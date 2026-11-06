//! Meta-labelling (owner 2026-10-13, item 2): train on the bot's OWN backtest trades. The bot's strategy decides to enter; the model learns, from the candle
//! context at the decision bar, which of those entries do badly, so it can veto them. Two labels are tried: the return itself, and a TAIL label (the trade
//! loses more than `tail`), because a veto exists to avoid the bad outcomes, not to rank the good ones.
//! The candle context is the lab's features at the decision bar (the latest bar OPEN at least 15 minutes before the trade's recorded decision time, so it is
//! strictly past even if the recorded time is a bar close).

use crate::mltrade::Trade;
use rusqlite::Connection;
use std::collections::HashMap;

/// The bot's long backtest trades of the unselected runs (random/oos) with the candle features at their decision bar. `lookup(symbol, ts_ms)` returns the
/// feature vector of a bar. Rows without candle context are dropped. `block` = the run window start (trades of one block share the market).
pub fn trades_with_context(data: &Connection, lookup: &dyn Fn(&str, i64) -> Option<Vec<f64>>, with_trade_features: bool) -> rusqlite::Result<Vec<Trade>> {
    let cols = crate::mltrade::FEATURES;
    let extra = if with_trade_features { format!(", {}", cols.iter().map(|c| format!("f.{c}")).collect::<Vec<_>>().join(", ")) } else { String::new() };
    let join = if with_trade_features { "JOIN trade_features f ON f.run_id = t.run_id AND f.idx = t.idx" } else { "" };
    let sql = format!("SELECT r.window_start, t.symbol, t.decided_ts, t.ret_margin{extra} FROM trades t JOIN runs r ON r.run_id = t.run_id {join} \
        WHERE r.role IN ('random', 'oos') AND t.side = 'long' AND t.decided_ts IS NOT NULL ORDER BY r.window_start, t.entry_ts");
    let mut st = data.prepare(&sql)?;
    let n = if with_trade_features { cols.len() } else { 0 };
    let mut cache: HashMap<(String, i64), Option<Vec<f64>>> = HashMap::new();
    let rows = st.query_map([], |r| {
        let mut tf = Vec::with_capacity(n);
        for i in 0..n { tf.push(r.get::<_, Option<f64>>(4 + i)?.unwrap_or(f64::NAN)); }
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, f64>(3)?, tf))
    })?;
    let mut out = vec![];
    for row in rows {
        let (block, sym, decided, ret, tf) = row?;
        let ts = decided * 1000 - 900_000;
        let ctx = cache.entry((sym.clone(), ts)).or_insert_with(|| lookup(&sym, ts)).clone();
        if let Some(mut x) = ctx { x.extend(tf); out.push(Trade { block, x, win: ret > 0.0, ret }); }
    }
    Ok(out)
}

/// Tail label: `win` = the trade did NOT lose more than `tail` (so a classifier's score is the probability of avoiding the tail).
pub fn with_tail_label(t: &[Trade], tail: f64) -> Vec<Trade> { t.iter().map(|x| Trade { win: x.ret > -tail, ..x.clone() }).collect() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trades_get_the_context_of_the_bar_before_the_decision_and_missing_context_is_dropped() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE runs(run_id TEXT, role TEXT, window_start INTEGER); CREATE TABLE trades(run_id TEXT, idx INTEGER, symbol TEXT, side TEXT, entry_ts INTEGER, decided_ts INTEGER, ret_margin REAL);
            INSERT INTO runs VALUES ('r1','random',100), ('r2','chosen',100);
            INSERT INTO trades VALUES ('r1',0,'AAA','long',2000,1000,0.05), ('r1',1,'BBB','long',2000,1000,-0.02), ('r1',2,'AAA','short',2000,1000,0.1), ('r2',0,'AAA','long',2000,1000,0.9);").unwrap();
        let look = |s: &str, ts: i64| if s == "AAA" && ts == 1000 * 1000 - 900_000 { Some(vec![1.0, 2.0]) } else { None };
        let t = trades_with_context(&db, &look, false).unwrap();
        assert_eq!(t.len(), 1, "only AAA's long trade of an unselected run has context");
        assert_eq!((t[0].block, t[0].x.clone(), t[0].win, t[0].ret), (100, vec![1.0, 2.0], true, 0.05));
        let tail = with_tail_label(&[Trade { block: 0, x: vec![], win: true, ret: -0.03 }, Trade { block: 0, x: vec![], win: false, ret: -0.005 }], 0.02);
        assert!(!tail[0].win && tail[1].win, "-3% is a tail loss at 2%, -0.5% is not");
    }
}
