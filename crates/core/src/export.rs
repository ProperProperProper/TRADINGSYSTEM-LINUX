//! Exports the symbol map to CSV and verifies its invariants.
use anyhow::Result;
use rusqlite::Connection;
use std::{fmt::Write as _, fs, path::Path};

use crate::symbols::{MOOMOO_MARKET, external_symbol};

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Writes data/symbol_map.csv: one row per symbol with both spellings, status and match evidence.
pub fn symbol_map_csv(conn: &Connection, out: &Path) -> Result<usize> {
    let mut st = conn.prepare(
        "SELECT s.moomoo_symbol, s.moomoo_market, s.external_symbol, s.name, s.bot_status, s.external_match,
                s.external_returned_symbol, s.external_name, s.overlap_bars, s.close_diff_bps_median, s.close_diff_bps_p99,
                s.volume_ratio_median, c.moomoo_bars, c.filled_from_external, c.gap_bars,
                (SELECT count(*) FROM candles_extra e WHERE e.moomoo_symbol=s.moomoo_symbol)
         FROM symbols s LEFT JOIN symbol_coverage c USING(moomoo_symbol) ORDER BY s.moomoo_symbol")?;
    let mut text = String::from(
        "moomoo_symbol,moomoo_market,external_symbol,name,bot_status,external_match,external_returned_symbol,external_name,overlap_bars,close_diff_bps_median,close_diff_bps_p99,volume_ratio_median,moomoo_bars,external_filled_bars,gap_bars,external_extra_bars\n",
    );
    let mut n = 0;
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        let mut cells = Vec::new();
        for i in 0..16 {
            cells.push(match r.get_ref(i)? {
                rusqlite::types::ValueRef::Null => String::new(),
                rusqlite::types::ValueRef::Integer(v) => v.to_string(),
                rusqlite::types::ValueRef::Real(v) => format!("{v:.4}"),
                rusqlite::types::ValueRef::Text(t) => csv_field(std::str::from_utf8(t)?),
                rusqlite::types::ValueRef::Blob(_) => String::new(),
            });
        }
        writeln!(text, "{}", cells.join(","))?;
        n += 1;
    }
    fs::write(out, text)?;
    Ok(n)
}

pub struct Check {
    pub name: &'static str,
    pub failures: Vec<String>,
}

fn q1(conn: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut st = conn.prepare(sql)?;
    Ok(st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<_, _>>()?)
}

/// `key=value` lines of versions.lock.
pub fn parse_lock(text: &str) -> std::collections::BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// `~/x` becomes the home folder's x; a relative path stays relative to the project folder (where `thomas` runs).
fn expand_home(p: &str) -> std::path::PathBuf {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(h)) => std::path::PathBuf::from(h).join(rest),
        _ => std::path::PathBuf::from(p),
    }
}

/// Mismatches between versions.lock and what is installed: the OpenD app's Info.plist version (or Linux GTWLog version) and the SDK's dist-info.
pub fn moomoo_version_failures(lock_text: &str) -> Vec<String> {
    let lock = parse_lock(lock_text);
    let mut bad = Vec::new();
    let want = |k: &str| lock.get(k).cloned().unwrap_or_default();
    let app = expand_home(&want("opend_app"));
    let mut got = std::process::Command::new("defaults")
        .arg("read")
        .arg(app.join("Contents/Info"))
        .arg("CFBundleShortVersionString")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if got.is_empty() && cfg!(target_os = "linux") && app.to_string_lossy().contains("moomoo_OpenD.app") {
        let log_dir = expand_home("~/.com.moomoo.OpenD/Log");
        if let Ok(entries) = std::fs::read_dir(&log_dir) {
            let mut logs: Vec<std::path::PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("GTWLog_") && n.ends_with(".log"))
                })
                .collect();
            logs.sort();
            for log_path in logs.into_iter().rev() {
                if let Ok(text) = std::fs::read_to_string(&log_path)
                    && let Some((_, rest)) = text.split_once("moomoo OpenD version info:")
                    && let Some(ver) = rest.trim().split('(').next()
                {
                    got = ver.trim().to_string();
                    break;
                }
            }
        }
    }
    if got != want("moomoo_opend") {
        bad.push(format!(
            "OpenD app is '{got}', lock says '{}'",
            want("moomoo_opend")
        ));
    }
    let venv = expand_home(&want("sdk_venv"));
    let ver = want("moomoo_api");
    let wanted_moomoo = format!("moomoo_api-{ver}.dist-info");
    let wanted_futu = format!("futu_api-{ver}.dist-info");
    let has_sdk = |base: &Path| -> bool {
        ["lib", "lib64"].into_iter().any(|lib_dir| {
            std::fs::read_dir(base.join(lib_dir))
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .any(|py| {
                    let sp = py.path().join("site-packages");
                    sp.join(&wanted_moomoo).is_dir() || sp.join(&wanted_futu).is_dir()
                })
        })
    };
    let found = has_sdk(&venv)
        || (cfg!(target_os = "linux")
            && venv == Path::new("tools/moomoo-venv")
            && has_sdk(Path::new("papercompmoomoo/.venv")));
    if !found {
        bad.push(format!("SDK {wanted_moomoo} not found in {}", venv.display()));
    }
    bad
}

/// Every invariant the data must satisfy. `holdings` = the SPY holdings tickers.
pub fn verify(
    conn: &Connection,
    bot_snapshot: &Path,
    holdings: &std::collections::BTreeSet<String>,
    lock_text: &str,
) -> Result<Vec<Check>> {
    // The archived importer snapshot is optional after consolidation: the converter-owned common
    // database is now the live candle source. When an archive is present, retain its additional
    // cross-source checks; a fresh clone must still be verifiable without manufacturing that file.
    let bot = if bot_snapshot.exists() {
        Some(Connection::open_with_flags(
            bot_snapshot,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?)
    } else {
        None
    };
    let mut checks = Vec::new();
    let mut add = |name, failures| checks.push(Check { name, failures });

    // Both spellings present, derived consistently, unique.
    let mut f = Vec::new();
    for t in q1(conn, "SELECT moomoo_symbol FROM symbols")? {
        let (y, market): (String, String) = conn.query_row(
            "SELECT external_symbol, moomoo_market FROM symbols WHERE moomoo_symbol=?",
            [&t],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if t.is_empty() || y.is_empty() || y != external_symbol(&t) || market != MOOMOO_MARKET {
            f.push(t);
        }
    }
    add(
        "every symbol has a moomoo and a external spelling, consistently derived",
        f,
    );
    add(
        "external spellings are unique",
        q1(
            conn,
            "SELECT external_symbol FROM symbols GROUP BY external_symbol HAVING count(*)>1",
        )?,
    );

    // Coverage of the symbol universe.
    let known: std::collections::BTreeSet<String> = q1(conn, "SELECT moomoo_symbol FROM symbols")?
        .into_iter()
        .collect();
    add(
        "every SPY holding is in the map",
        holdings
            .iter()
            .filter(|h| !known.contains(*h))
            .cloned()
            .collect(),
    );
    let cs: Vec<String> = if let Some(bot) = &bot {
        let mut st = bot.prepare("SELECT DISTINCT symbol FROM candles")?;
        st.query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?
    } else {
        Vec::new()
    };
    add(
        "every moomoo candle symbol is in the map",
        cs.into_iter().filter(|s| !known.contains(s)).collect(),
    );

    // legacy provider agreement.
    add(
        "no symbol with a external_match of mismatch",
        q1(
            conn,
            "SELECT moomoo_symbol FROM symbols WHERE external_match='mismatch'",
        )?,
    );
    add(
        "external returned the symbol we asked for",
        q1(
            conn,
            "SELECT moomoo_symbol FROM symbols WHERE external_returned_symbol IS NOT NULL AND upper(external_returned_symbol)!=upper(external_symbol)",
        )?,
    );
    add(
        "every fetched symbol was checked (returned symbol recorded)",
        q1(
            conn,
            "SELECT moomoo_symbol FROM symbols s WHERE has_moomoo_candles=1 AND external_returned_symbol IS NULL OR (in_spy=1 AND NOT EXISTS (SELECT 1 FROM candles_extra e WHERE e.moomoo_symbol=s.moomoo_symbol) AND has_moomoo_candles=0)",
        )?,
    );

    // Every stored row carries the right pair.
    for t in [
        "candles_supplement",
        "data_gaps",
        "symbol_coverage",
        "candles_extra",
        "candles_merged",
        "source_checks",
    ] {
        let sql = format!(
            "SELECT DISTINCT x.moomoo_symbol FROM {t} x LEFT JOIN symbols s USING(moomoo_symbol) WHERE s.moomoo_symbol IS NULL OR x.external_symbol!=s.external_symbol"
        );
        let name: &'static str = match t {
            "candles_supplement" => "candles_supplement rows carry the mapped pair",
            "data_gaps" => "data_gaps rows carry the mapped pair",
            "symbol_coverage" => "symbol_coverage rows carry the mapped pair",
            "candles_extra" => "candles_extra rows carry the mapped pair",
            "candles_merged" => "candles_merged rows carry the mapped pair",
            _ => "source_checks rows carry the mapped pair",
        };
        add(name, q1(conn, &sql)?);
    }
    // Moomoo always wins: no legacy provider row may sit where the snapshot has a real moomoo bar.
    let mut clash = Vec::new();
    if let Some(bot) = &bot {
        let mut st =
            conn.prepare("SELECT moomoo_symbol, wall_ms FROM candles_merged WHERE source='external'")?;
        let mut q = bot.prepare(
            "SELECT 1 FROM candles WHERE symbol=? AND wall_ms=? AND kind='trades' AND volume>0",
        )?;
        for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (s, w) = row?;
            if q.exists((&s, w))? {
                clash.push(format!("{s}@{w}"));
            }
        }
    }
    add(
        "no legacy provider bar where moomoo has a real bar (moomoo is primary)",
        clash,
    );
    add(
        "every merged legacy provider row is flagged as an estimated-turnover row",
        q1(
            conn,
            "SELECT moomoo_symbol FROM candles_merged WHERE source='external' AND turnover_estimated!=1 UNION SELECT moomoo_symbol FROM candles_merged WHERE source='moomoo' AND turnover_estimated!=0",
        )?,
    );
    add(
        "carry bars are flat and have zero volume",
        q1(
            conn,
            "SELECT DISTINCT moomoo_symbol FROM candles_merged WHERE source='carry' AND (volume!=0 OR turnover!=0 OR open!=close OR high!=close OR low!=close)",
        )?,
    );
    add(
        "every SPY holding is in the merged candle base",
        holdings
            .iter()
            .filter(|h| {
                q1(
                    conn,
                    &format!(
                        "SELECT moomoo_symbol FROM candles_merged WHERE moomoo_symbol='{}' LIMIT 1",
                        h.replace('\'', "")
                    ),
                )
                .map_or(true, |v| v.is_empty())
            })
            .cloned()
            .collect(),
    );
    add(
        "every merged candle lies inside a session of the real trading calendar",
        q1(
            conn,
            "SELECT DISTINCT m.moomoo_symbol FROM candles_merged m WHERE NOT EXISTS (SELECT 1 FROM sessions_calendar s WHERE m.wall_ms >= s.open_ms AND m.wall_ms < s.close_ms)",
        )?,
    );
    add(
        "the trading calendar has no gap longer than 5 days and no overlapping sessions",
        q1(
            conn,
            "SELECT a.open_ms || '' FROM sessions_calendar a JOIN sessions_calendar b ON b.open_ms = (SELECT min(open_ms) FROM sessions_calendar WHERE open_ms > a.open_ms) WHERE b.open_ms - a.open_ms > 5*86400000 OR b.open_ms < a.close_ms",
        )?,
    );
    add(
        "every symbol in the map has legacy provider 1-minute bars in the archive (hard rule: all symbols)",
        q1(
            conn,
            "SELECT moomoo_symbol FROM symbols s WHERE NOT EXISTS (SELECT 1 FROM candles_extra e WHERE e.moomoo_symbol = s.moomoo_symbol)",
        )?,
    );
    add(
        "every symbol in the map has merged candles (hard rule: all symbols)",
        q1(
            conn,
            "SELECT moomoo_symbol FROM symbols s WHERE NOT EXISTS (SELECT 1 FROM candles_merged m WHERE m.moomoo_symbol = s.moomoo_symbol)",
        )?,
    );
    add(
        "every period keeps ALL its winning settings (winning_settings rows = search_periods.usable)",
        q1(
            conn,
            "SELECT p.period_start_ms || '/' || p.min_trades FROM search_periods p WHERE (SELECT count(*) FROM winning_settings w WHERE w.period_start_ms = p.period_start_ms AND w.min_trades = p.min_trades) != p.usable",
        )?,
    );
    add(
        "no invalid bars in candles_merged",
        q1(
            conn,
            "SELECT DISTINCT moomoo_symbol FROM candles_merged WHERE high<low OR high<max(open,close) OR low>min(open,close) OR open<=0 OR close<=0 OR volume<0 OR turnover<0",
        )?,
    );
    add(
        "no invalid bars in candles_extra",
        q1(
            conn,
            "SELECT DISTINCT moomoo_symbol FROM candles_extra WHERE high<low OR high<max(open,close) OR low>min(open,close) OR open<=0 OR close<=0 OR volume<0",
        )?,
    );
    add(
        "moomoo OpenD and SDK are the pinned version (versions.lock)",
        moomoo_version_failures(lock_text),
    );
    add("sqlite integrity and foreign keys", {
        let mut v = q1(conn, "PRAGMA integrity_check")?;
        v.retain(|s| s != "ok");
        v.extend(q1(
            conn,
            "SELECT 'fk:'||\"table\" FROM pragma_foreign_key_check",
        )?);
        v
    });
    Ok(checks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_parsing_and_mismatch_detection() {
        let m = parse_lock("a=1\nb = two \nno_equals\n");
        assert_eq!((m["a"].as_str(), m["b"].as_str(), m.len()), ("1", "two", 2));
        let bad = moomoo_version_failures(
            "moomoo_opend=0.0.1\nmoomoo_api=0.0.1\nopend_app=~/Applications/moomoo_OpenD.app\nsdk_venv=~/.venvs/moomoo\n",
        );
        assert_eq!(bad.len(), 2, "a wrong pin must fail both checks: {bad:?}");
    }

    #[test]
    fn verify_does_not_require_the_retired_importer_snapshot() {
        let conn = Connection::open_in_memory().unwrap();
        crate::store::init(&conn).unwrap();
        let missing = std::env::temp_dir().join(format!(
            "thomas-missing-snapshot-{}-{}.sqlite3",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(!missing.exists());
        let checks = verify(&conn, &missing, &std::collections::BTreeSet::new(), "").unwrap();
        assert!(!checks.is_empty());
    }
}
