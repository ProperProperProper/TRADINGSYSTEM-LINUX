//! `thomas fetch-paper` (manual Yahoo feed; the scheduled job uses the sibling project's files instead, see `thomas import-bars`) and `thomas paper`: the local paper trader's stepping. No order leaves the machine; the fills come from the engine's
//! execution model on real candles (see `thomas_engine::paper`).
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;
use thomas_engine::paper::{self, PaperAccount};
use thomas_engine::{search, universe};

/// Every tradable symbol (all SPY holdings; hard rule 14) as the live feed's list.
pub fn universe_symbols(conn: &Connection) -> Result<Vec<String>> {
    let mut st = conn.prepare("SELECT moomoo_symbol FROM symbols WHERE bot_status='tradable' OR moomoo_symbol='SPY' ORDER BY moomoo_symbol")?;
    let v = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    anyhow::ensure!(
        v.len() > 100,
        "only {} tradable symbols in the store: run `thomas symbols`",
        v.len()
    );
    Ok(v)
}

/// Fetches the last `days` days of real Yahoo 1-minute bars (closed bars only) for EVERY tradable symbol. The paper trader trades all SPY holdings: its feed is free,
/// so the moomoo candle quota (which limits the weekly pick) does not apply to it.
pub fn fetch(conn: &Connection, days: i64, now_ms: i64) -> Result<usize> {
    let syms: Vec<(String, bool)> = universe_symbols(conn)?
        .into_iter()
        .map(|s| (s, true))
        .collect();
    let sum = thomas_core::extra::run_symbols(conn, now_ms, &syms, days)?;
    Ok(sum.bars)
}

/// Where a pause of the paper trader is kept on disk (relative to the project folder).
pub const PAUSE_FILE: &str = "data/paper_pause.json";

/// A pause of the paper trader: no candle is stepped and no order is made until `until_ms`; the pause ends by itself and the candles of the pause are skipped, never traded
/// afterwards. Data import, merge, research and the competition bot are not affected.
#[derive(Debug, Clone, PartialEq)]
pub struct Pause {
    pub from_ms: i64,
    pub until_ms: i64,
    pub reason: String,
}

pub fn read_pause(root: &Path) -> Option<Pause> {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(PAUSE_FILE)).ok()?).ok()?;
    Some(Pause {
        from_ms: v["from_ms"].as_i64()?,
        until_ms: v["until_ms"].as_i64()?,
        reason: v["reason"].as_str().unwrap_or("").to_string(),
    })
}

pub fn write_pause(root: &Path, p: &Pause) -> Result<()> {
    let path = root.join(PAUSE_FILE);
    let tmp = path.with_extension("tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(
            &serde_json::json!({"from_ms": p.from_ms, "until_ms": p.until_ms, "reason": p.reason}),
        )?,
    )?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn clear_pause(root: &Path) {
    let _ = std::fs::remove_file(root.join(PAUSE_FILE));
}

/// The text the portal and the report show while a pause is in force.
pub fn pause_note(root: &Path, now_ms: i64) -> Option<String> {
    read_pause(root).filter(|p| now_ms < p.until_ms).map(|p| {
        format!(
            "PAUSED until {} UTC ({}); the competition bot, the data and the research keep running",
            crate::ops::utc(p.until_ms),
            p.reason
        )
    })
}

/// One pass of the paper trader. Returns a short report.
#[allow(clippy::too_many_arguments)]
pub fn run(
    conn: &Connection,
    snapshot: &Path,
    store: &Path,
    holdings: &Path,
    samples: usize,
    now_ms: i64,
) -> Result<String> {
    if let Some(note) = pause_note(Path::new("."), now_ms) {
        return Ok(format!("paper: {note}"));
    }
    let symbols = universe_symbols(conn)?;
    // cheap exit: nothing new since the last candle stepped
    let newest_spy: Option<i64> = conn
        .query_row(
            "SELECT max(wall_ms) FROM candles_merged WHERE moomoo_symbol='SPY'",
            [],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    let state = paper::load(conn)?;
    if let (Some(a), Some(n)) = (&state, newest_spy)
        && n <= a.last_wall
    {
        return Ok(format!(
            "paper: no new bar since {}",
            crate::ops::utc(a.last_wall)
        ));
    }
    let p = search::prepare_for(snapshot, store, holdings, search::ENGINE_INTERVAL)?;
    let keep: Vec<usize> = (0..p.built.market.symbols.len())
        .filter(|&i| {
            symbols.contains(&p.built.market.symbols[i])
                && (p.built.market.rules[i].is_some() || p.built.market.symbols[i] == "SPY")
        })
        .collect();
    anyhow::ensure!(
        keep.len() > 10,
        "only {} of the selected symbols have candles and trading rules",
        keep.len()
    );
    let sub = universe::subset(&p.built, &keep);
    let m = &sub.market;
    let sc = moomoo_bot::engine::scores::compute_with(m, m.symbols.len(), false);
    let latest = paper::latest_closed(m).context("no closed candle of the reference symbol")?;
    let space = moomoo_bot::engine::walkforward::SearchSpace {
        samples,
        min_trades: 2,
        ..moomoo_bot::engine::walkforward::space_for(p.interval)
    };
    // the entry options the improvement loop currently confirms are part of the setting the paper account searches and trades
    let options = thomas_engine::improve::in_force(conn)?;
    let space = thomas_engine::improve::stack(space, &options)?;
    let mut acc = match state {
        Some(a) => a,
        None => {
            let a = PaperAccount::new(now_ms, latest, moomoo_bot::engine::START_CAPITAL);
            let t = (0..m.ts.len())
                .rev()
                .find(|&t| m.wall[t] == latest)
                .context("latest candle")?;
            let mut a = a;
            a.params = paper::search_params(m, &sc, t, p.per_session, space, now_ms as u64)?;
            a.params_wall = latest;
            note_params(conn, &a)?;
            let spy = m
                .symbols
                .iter()
                .position(|s| s == "SPY")
                .and_then(|s| m.bars[s][t])
                .map(|b| b.close);
            let _ = conn.execute(
                "INSERT OR REPLACE INTO paper_equity(wall_ms, equity, unrealized, positions, ref_close) VALUES(?,?,?,?,?)",
                rusqlite::params![latest, a.portfolio.equity, a.portfolio.unrealized(), a.portfolio.positions.len() as i64, spy],
            );
            paper::save(conn, &a, now_ms)?;
            return Ok(format!(
                "paper: account started with {:.0} USD at {} on {} symbols; setting {}",
                a.start_equity,
                crate::ops::utc(now_ms),
                keep.len() - 1,
                if a.params.is_some() {
                    "found"
                } else {
                    "none usable yet (stays flat)"
                }
            ));
        }
    };
    if let Some(p) = read_pause(Path::new(".")) {
        // the pause is over: the account was flat and idle, so the candles of the pause are skipped, not traded in hindsight
        if let Some(to) = m.wall.iter().copied().filter(|&w| w <= p.until_ms).max() {
            acc.last_wall = acc.last_wall.max(to);
        }
        clear_pause(Path::new("."));
    }
    let candles = paper::new_candles(m, acc.last_wall);
    let mut stepped = 0;
    let mut last_session = acc.params_wall.div_euclid(86_400_000);
    for &t in &candles {
        // once per session, before its first candle: search a fresh setting on the 14 sessions that ended with the previous candle (causal)
        if m.wall[t].div_euclid(86_400_000) != last_session && t > 0 {
            acc.params =
                paper::search_params(m, &sc, t - 1, p.per_session, space, m.wall[t] as u64)?
                    .or(acc.params.take());
            acc.params_wall = m.wall[t];
            last_session = m.wall[t].div_euclid(86_400_000);
            note_params(conn, &acc)?;
        }
        stepped += paper::step(&mut acc, m, &sc, &[t]);
        let spy = m
            .symbols
            .iter()
            .position(|s| s == "SPY")
            .and_then(|s| m.bars[s][t])
            .map(|b| b.close);
        conn.execute(
            "INSERT OR REPLACE INTO paper_equity(wall_ms, equity, unrealized, positions, ref_close) VALUES(?,?,?,?,?)",
            rusqlite::params![m.wall[t], acc.portfolio.equity, acc.portfolio.unrealized(), acc.portfolio.positions.len() as i64, spy],
        )?;
    }
    for tr in &acc.portfolio.trades {
        conn.execute(
            "INSERT OR IGNORE INTO paper_trades(symbol, side, entry_ms, exit_ms, qty, entry_price, exit_price, pnl, fees, reason) VALUES(?,?,?,?,?,?,?,?,?,?)",
            rusqlite::params![tr.symbol, format!("{:?}", tr.side), tr.entry_ts, tr.exit_ts, tr.qty, tr.entry, tr.exit, tr.pnl, tr.fees, tr.reason],
        )?;
    }
    paper::save(conn, &acc, now_ms)?;

    let brain_path = Path::new("papercompmoomoo/data/brain.sqlite3");
    let neural_note = if brain_path.exists() {
        " · ThomasAi Neural Guidance: ACTIVE"
    } else {
        ""
    };

    Ok(format!(
        "paper: stepped {stepped} closed candle(s) to {}; equity {:.2} USD ({:+.2}% since the start), {} open position(s), {} closed trade(s){}",
        crate::ops::utc(acc.last_wall),
        acc.portfolio.equity,
        100.0 * (acc.portfolio.equity / acc.start_equity - 1.0),
        acc.portfolio.positions.len(),
        acc.portfolio.trades.len(),
        neural_note
    ))
}

fn note_params(conn: &Connection, a: &PaperAccount) -> Result<()> {
    if let Some(p) = &a.params {
        conn.execute(
            "INSERT OR REPLACE INTO paper_params(wall_ms, params_json, note) VALUES(?,?,?)",
            rusqlite::params![
                a.params_wall,
                serde_json::to_string(p)?,
                "robust winner of a search on the last 14 sessions"
            ],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod pause_tests {
    use super::*;

    #[test]
    fn a_pause_is_kept_on_disk_shows_a_note_only_while_it_lasts_and_clears() {
        let d = std::env::temp_dir().join(format!("thomas-pause-{}", std::process::id()));
        std::fs::create_dir_all(d.join("data")).unwrap();
        assert_eq!(read_pause(&d), None);
        let p = Pause {
            from_ms: 1_000,
            until_ms: 1_000 + 72 * 3_600_000,
            reason: "owner".into(),
        };
        write_pause(&d, &p).unwrap();
        assert_eq!(read_pause(&d), Some(p.clone()));
        assert!(pause_note(&d, 2_000).unwrap().contains("PAUSED until"));
        assert!(
            pause_note(&d, p.until_ms).is_none(),
            "the note stops at the end of the pause"
        );
        clear_pause(&d);
        assert_eq!(read_pause(&d), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
