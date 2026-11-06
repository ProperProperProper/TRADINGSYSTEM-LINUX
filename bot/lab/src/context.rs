//! The lab's shared context (config and the two databases) and the data windows. Weekly blocks of the bot's backtest archive are windows only when COMPLETE.

use crate::config::Config;
use crate::db;
use anyhow::Result;
use rusqlite::Connection;

pub struct Ctx<'a> {
    pub cfg: &'a Config,
    pub mem: &'a Connection,
    pub data: &'a Connection,
}


// Weekly blocks cut by entry time are windows only when COMPLETE (the data frontier has passed their end) and the bot's runs
// COVER at least six sevenths of their length. Coverage is measured on the runs' time spans, not on days with trades: the strategy
// trades on few days, and a quiet week is still a valid week of evidence. Windows from the file contract keep their labels.

/// A window with fewer trades than this is ignored: it cannot support cut points or a test (the archive's oldest weeks hold only a
/// handful of trades because older runs were stored without them).
pub const MIN_WINDOW_TRADES: i64 = 50;

/// Every window the lab may use, with whether it is COMPLETE. A window with enough trades that is still growing, or that the bot's
/// runs cover only partly (the archive started part-way through that week), is PROVISIONAL: it is used for exploration and
/// re-assessed as it grows, but a claim whose TEST windows include one can never be called replicated (veto `V_PROVISIONAL`).
pub fn windows_info(data: &Connection) -> Result<Vec<(String, bool)>> {
    let block = db::block_secs(data);
    let min_covered = block * 6 / 7;
    let newest_trade: i64 = data.query_row("SELECT coalesce(max(entry_ts), 0) FROM trades", [], |r| r.get(0))?;
    let newest_bar: i64 = data.query_row("SELECT coalesce(max(window_end), 0) FROM runs", [], |r| r.get(0))?;
    let frontier = newest_trade.max(newest_bar);
    let mut spans: Vec<(i64, i64)> = {
        let mut s = data.prepare("SELECT DISTINCT window_start, window_end FROM runs ORDER BY window_start")?;
        let v = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        v
    };
    spans.sort();
    let covered = |lo: i64, hi: i64| -> i64 {
        let (mut total, mut cursor) = (0i64, lo);
        for (a, b) in &spans {
            let (a, b) = ((*a).max(cursor).max(lo), (*b).min(hi));
            if b > a { total += b - a; cursor = b; }
        }
        total
    };
    let mut s = data.prepare("SELECT window, min(entry_ts), count(*) FROM trades GROUP BY window ORDER BY min(entry_ts)")?;
    let rows: Vec<(String, i64, i64)> = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
    let mut out = Vec::new();
    for (w, _first, n_trades) in rows {
        if n_trades < MIN_WINDOW_TRADES { continue; }
        match w.strip_prefix('b').and_then(|n| n.parse::<i64>().ok()) {
            Some(n) => {
                // Complete: past its end AND fully covered by the bot's runs. Anything else with enough trades is still usable but
                // PROVISIONAL: still growing, or only partly covered because the archive began part-way through the week.
                let complete = (n + 1) * block <= frontier && covered(n * block, (n + 1) * block) >= min_covered;
                out.push((w, complete));
            }
            None => out.push((w, true)),
        }
    }
    Ok(out)
}

/// Complete windows only (what lessons and replication rest on).
pub fn windows(data: &Connection) -> Result<Vec<String>> {
    Ok(windows_info(data)?.into_iter().filter(|(_, c)| *c).map(|(w, _)| w).collect())
}

