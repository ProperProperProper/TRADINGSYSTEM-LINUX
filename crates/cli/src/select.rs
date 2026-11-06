//! The weekly symbol pick. Once a week ThomasAi ranks every tradable symbol from the last 14 sessions (past data only) and writes the ordered list to
//! `data/selection.json`. The bot reads that file: its historical-data importer fetches moomoo candles for these symbols first (within the 100-stock weekly
//! quota), then its established backtest and paper trading run on the symbols that have candles. The ranking rule is an unproven prior until the deep
//! walk-forward has real history for the symbols it picked (PLAN A3.13); every pick is stored in `selections` so it can be judged later.
use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeSet;
use std::path::Path;
use thomas_engine::market::Built;
use thomas_engine::universe::{self, Pick};

/// Symbols that make up one week's pick (the quota is 100 stocks, 6 kept in reserve by the bot).
pub const PICK: usize = 94;
/// Symbols written to the file: the pick first, then the next best, so the bot has a queue when old pulls expire during the week.
pub const LIST: usize = 150;
/// Sessions of the window the ranking looks at.
pub const WINDOW_SESSIONS: usize = 14;
pub const WEEK_MS: i64 = 7 * 86_400_000;

/// "2026-W43" from epoch milliseconds (ISO 8601 week).
pub fn week_id(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    // 1970-01-01 was a Thursday; weekday 0 = Monday
    let weekday = (days + 3).rem_euclid(7);
    // the Thursday of this ISO week decides the ISO year
    let thursday = days - weekday + 3;
    let (y, m, d) = civil(thursday);
    let jan1 = days_from_civil(y, 1, 1);
    let _ = (m, d);
    let week = (thursday - jan1) / 7 + 1;
    format!("{y:04}-W{week:02}")
}

fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Epoch milliseconds of midnight UTC on a civil date (test helper).
#[cfg(test)]
pub fn week_start_for_tests(y: i64, m: i64, d: i64) -> i64 {
    days_from_civil(y, m, d) * 86_400_000
}

#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    pub generated_ms: i64,
    pub week: String,
    pub rule: String,
    pub pick: usize,
    pub symbols: Vec<String>,
    pub basis: String,
}

impl Selection {
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "generated_ms": self.generated_ms,
            "week": self.week,
            "rule": self.rule,
            "pick": self.pick,
            "symbols": self.symbols,
            "basis": self.basis,
        })
        .to_string()
    }
}

/// Ranks the tradable symbols on the last `WINDOW_SESSIONS` sessions of `b` and returns the ordered list (best first, `LIST` long).
pub fn build(b: &Built, per_session: usize, tradable: &BTreeSet<String>, now_ms: i64) -> Selection {
    let total = b.market.ts.len();
    let window = total.saturating_sub(WINDOW_SESSIONS * per_session)..total;
    let ranked = universe::ranked(&b.market, window, Pick::Composite(LIST));
    let symbols: Vec<String> = ranked
        .into_iter()
        .map(|i| b.market.symbols[i].clone())
        .filter(|s| tradable.contains(s))
        .take(LIST)
        .collect();
    Selection {
        generated_ms: now_ms,
        week: week_id(now_ms),
        rule: Pick::Composite(LIST).name(),
        pick: PICK,
        symbols,
        basis: format!(
            "average rank of turnover, volatility and reversal over the last {WINDOW_SESSIONS} sessions of real 1-minute candles; an unproven prior"
        ),
    }
}

/// True when no selection was stored in the last seven days.
pub fn due(c: &Connection, now_ms: i64) -> bool {
    c.query_row("SELECT max(generated_ms) FROM selections", [], |r| {
        r.get::<_, Option<i64>>(0)
    })
    .ok()
    .flatten()
    .is_none_or(|ms| now_ms - ms >= WEEK_MS)
}

/// Stores the pick and writes the hand-off file atomically (a temp file renamed into place), so the bot never reads half a file.
pub fn save(c: &Connection, out: &Path, s: &Selection) -> Result<()> {
    c.execute("BEGIN IMMEDIATE", [])?;
    let result = (|| -> Result<()> {
        c.execute(
        "INSERT INTO selections(generated_ms, week, rule, n_pick, symbols_json, basis) VALUES(?,?,?,?,?,?)",
        rusqlite::params![s.generated_ms, s.week, s.rule, s.pick as i64, serde_json::to_string(&s.symbols)?, s.basis],
    )?;
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = out.with_extension("json.tmp");
        if std::fs::symlink_metadata(&tmp).is_ok_and(|m| m.file_type().is_symlink()) {
            anyhow::bail!("refusing to write through symlink {}", tmp.display());
        }
        let _ = std::fs::remove_file(&tmp);
        use std::io::Write;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?
            .write_all(s.to_json().as_bytes())?;
        std::fs::rename(&tmp, out)?;
        c.execute("COMMIT", [])?;
        Ok(())
    })();
    if result.is_err() {
        let _ = c.execute("ROLLBACK", []);
        let _ = std::fs::remove_file(out.with_extension("json.tmp"));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_weeks_match_known_dates() {
        let ms = |y, m, d| days_from_civil(y, m, d) * 86_400_000;
        assert_eq!(week_id(ms(2026, 10, 19)), "2026-W43", "a Monday");
        assert_eq!(
            week_id(ms(2026, 10, 25)),
            "2026-W43",
            "the Sunday of the same week"
        );
        assert_eq!(week_id(ms(2026, 10, 26)), "2026-W44");
        assert_eq!(
            week_id(ms(2026, 1, 1)),
            "2026-W01",
            "2026-01-01 is a Thursday"
        );
        assert_eq!(
            week_id(ms(2025, 12, 29)),
            "2026-W01",
            "belongs to the next ISO year"
        );
        assert_eq!(week_id(ms(2021, 1, 3)), "2020-W53");
    }

    #[test]
    fn a_pick_is_due_once_a_week_and_saving_writes_the_store_and_the_file_whole() {
        let c = Connection::open_in_memory().unwrap();
        thomas_core::store::init(&c).unwrap();
        let now = 1_792_323_480_000_i64;
        assert!(due(&c, now), "nothing stored yet");
        let s = Selection {
            generated_ms: now,
            week: week_id(now),
            rule: "composite:150".into(),
            pick: PICK,
            symbols: vec!["NVDA".into(), "AAPL".into()],
            basis: "test".into(),
        };
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("data/selection.json");
        save(&c, &out, &s).unwrap();
        assert!(!due(&c, now + WEEK_MS - 1) && due(&c, now + WEEK_MS));
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(v["symbols"], serde_json::json!(["NVDA", "AAPL"]));
        assert_eq!(
            (
                v["week"].as_str(),
                v["pick"].as_u64(),
                v["generated_ms"].as_i64()
            ),
            (Some("2026-W42"), Some(94), Some(now))
        );
        assert!(
            !out.with_extension("json.tmp").exists(),
            "no temp file left behind"
        );
        let n: i64 = c
            .query_row("SELECT count(*) FROM selections", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
