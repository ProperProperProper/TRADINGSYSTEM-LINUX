//! Rust-native, research-only NASDAQ opening-range breakout replay.
//! It never submits orders and does not share state with the competition bot.
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone)]
struct Bar { ts: i64, open: f64, high: f64, low: f64, close: f64, volume: f64 }

pub fn run(path: &Path, _paper_path: &Path, risk_pct: f64, cost_bps: f64) -> Result<String> {
    let snapshot = path.parent().unwrap_or(Path::new(".")).join("orb_account.json");
    let snapshot: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&snapshot)
        .with_context(|| format!("refresh shared Competition account snapshot before running ORB; missing {}", snapshot.display()))?)?;
    anyhow::ensure!(snapshot["account_kind"] == "COMPETITION", "ORB refuses non-Competition account snapshot");
    let equity = snapshot["equity"].as_f64().context("Competition paper account snapshot has no equity")?;
    anyhow::ensure!(equity.is_finite() && equity > 0.0, "Competition paper account equity is invalid: {equity}");
    let db = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    let universe_path = path.parent().unwrap_or(Path::new(".")).join("nasdaq.json");
    let universe: std::collections::HashSet<String> = serde_json::from_str::<Vec<String>>(&std::fs::read_to_string(&universe_path)
        .with_context(|| format!("read NASDAQ universe {}", universe_path.display()))?)?
        .into_iter().map(|s| s.trim_start_matches("US.").to_string()).collect();
    let mut q = db.prepare("SELECT symbol,ts,open,high,low,close,volume FROM bars ORDER BY symbol,ts")?;
    let mut symbols: BTreeMap<String, Vec<Bar>> = BTreeMap::new();
    let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, Bar { ts:r.get(1)?, open:r.get(2)?, high:r.get(3)?, low:r.get(4)?, close:r.get(5)?, volume:r.get(6)? })))?;
    for row in rows { let (s,b) = row?; symbols.entry(s).or_default().push(b); }
    anyhow::ensure!(!symbols.is_empty(), "ORB backtest refused: common1m has no bars");
    let mut trades = Vec::new();
    for (symbol, bars) in symbols {
        if !universe.contains(&symbol) { continue; }
        let mut days: BTreeMap<i64, Vec<&Bar>> = BTreeMap::new();
        // The stored sample is in the New York daylight-saving window; shift
        // UTC timestamps by four hours before grouping regular sessions.
        for b in &bars { days.entry((b.ts - 4 * 3_600_000) / 86_400_000).or_default().push(b); }
        let day_keys: Vec<i64> = days.keys().copied().collect();
        for (day_index, day_key) in day_keys.iter().enumerate() {
            let session = &days[day_key];
            if session.len() < 6 { continue; }
            if day_index == 0 { continue; }
            let prior = &days[&day_keys[day_index - 1]];
            let pre_open_score = (prior.last().unwrap().close / prior.first().unwrap().open - 1.0)
                + 0.05 * (prior.iter().map(|b| b.volume).sum::<f64>() / prior.len() as f64 + 1.0).ln();
            let orh = session[..5].iter().map(|b| b.high).fold(f64::MIN, f64::max);
            let orl = session[..5].iter().map(|b| b.low).fold(f64::MAX, f64::min);
            let stop = (orh + orl) / 2.0;
            if orh <= orl || (orh / orl - 1.0) > 0.10 { continue; }
            let mut pv = 0.0; let mut vv = 0.0;
            for i in 5..session.len() {
                let b = session[i]; pv += ((b.high+b.low+b.close)/3.0)*b.volume; vv += b.volume;
                let vwap = if vv > 0.0 { pv/vv } else { b.close };
                if b.close <= orh + 0.01 || b.close <= vwap || b.volume <= session[i-1].volume { continue; }
                let entry = orh + 0.01; let risk = entry-stop; if risk <= 0.0 { continue; }
                let allocation = equity / 20.0;
                let risk_qty = ((equity*risk_pct/100.0)/risk).floor() as i64;
                let allocation_qty = (allocation / entry).floor() as i64;
                let qty = risk_qty.min(allocation_qty); if qty < 1 { continue; }
                let target = entry + 1.5*risk; let mut exit = b.close; let mut reason = "10:30 cutoff";
                for next in &session[i..] { exit = next.close; if next.low <= stop { exit=stop; reason="midpoint stop"; break; } if next.high >= target { exit=target; reason="1.5R target"; break; } }
                let pnl = (exit-entry)*(qty as f64) - (entry+exit)*(qty as f64)*cost_bps/10_000.0;
                trades.push(json!({"symbol":symbol,"session":day_key,"pre_open_score":pre_open_score,"entry_ts":b.ts,"qty":qty,"entry":entry,"exit":exit,"pnl_usd":pnl,"pnl_pct":(exit/entry-1.0)*100.0,"reason":reason,"orh":orh,"orl":orl,"vwap":vwap}));
                break;
            }
        }
    }
    let mut selected = Vec::new();
    let sessions: std::collections::BTreeSet<i64> = trades.iter().filter_map(|t| t["session"].as_i64()).collect();
    for session in sessions {
        let mut candidates: Vec<_> = trades.iter().filter(|t| t["session"].as_i64() == Some(session)).cloned().collect();
        candidates.sort_by(|a,b| b["pre_open_score"].as_f64().partial_cmp(&a["pre_open_score"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
        selected.extend(candidates.into_iter().take(20));
    }
    let total: f64 = selected.iter().map(|t| t["pnl_usd"].as_f64().unwrap_or(0.0)).sum();
    let wins = selected.iter().filter(|t| t["pnl_usd"].as_f64().unwrap_or(0.0)>0.0).count();
    Ok(serde_json::to_string_pretty(&json!({"strategy":"nasdaq_orb_1m_rust","research_only":true,"session_timezone":"America/New_York (EDT data)","top_n_per_session":20,"equity":equity,"risk_pct":risk_pct,"trades":selected.len(),"wins":wins,"win_rate":if selected.is_empty(){0.0}else{wins as f64/selected.len() as f64},"total_pnl_usd":total,"trade_details":selected}))?)
}
