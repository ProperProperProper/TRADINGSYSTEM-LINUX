//! The export contract (spec section 4): file formats, manifest, and validation of one run before it is ingested.

use crate::db::valid_feature_name;
use std::collections::BTreeMap;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metrics {
    pub return_pct: f64,
    pub max_dd_pct: f64,
    pub trades: u64,
    pub wins: u64,
    pub liquidations: u64,
    pub profit_factor: f64,
    pub fees: f64,
    pub funding: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunHeader {
    pub schema_version: u32,
    pub run_id: String,
    #[serde(default)]
    pub synthetic: bool,
    pub engine_commit: String,
    pub window_label: String,
    pub window_start: i64,
    pub window_end: i64,
    pub data_fingerprint: String,
    pub cost_model_version: String,
    /// search_winner | recorded_top | study | walkforward | other
    pub role: String,
    #[serde(default)]
    pub rank: Option<i64>,
    pub leverage: f64,
    pub start_equity: f64,
    pub params: Value,
    pub metrics: Metrics,
    #[serde(default)]
    pub seed: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub file: String,
    pub sha256: String,
    pub bytes: u64,
    pub schema_version: u32,
    pub kind: String, // header | trades | daily
}

#[derive(Clone, Debug)]
pub struct TradeRow {
    pub symbol: String,
    pub side: String,
    pub entry_ts: i64,
    pub exit_ts: i64,
    pub entry_px: f64,
    pub exit_px: f64,
    pub qty: f64,
    pub margin: f64,
    pub fees: f64,
    pub funding: f64,
    pub pnl: f64,
    pub ret_margin: f64,
    pub reason: String,
    /// Feature name (without the `f_` prefix) to value; empty cells are simply absent.
    pub features: BTreeMap<String, f64>,
}

#[derive(Clone, Debug)]
pub struct DailyRow {
    pub day: i64,
    pub equity: f64,
    pub pnl: f64,
}

/// Column names of a trades CSV: the fixed columns plus one `f_<name>` per feature.
pub fn trades_header(features: &[&str]) -> Vec<String> {
    let mut h: Vec<String> = ["symbol", "side", "entry_ts", "exit_ts", "entry_px", "exit_px", "qty", "margin", "fees", "funding", "pnl", "ret_margin", "reason"]
        .iter().map(|s| s.to_string()).collect();
    h.extend(features.iter().map(|f| format!("f_{f}")));
    h
}

/// Words that must never appear as a JSON key or CSV column: the lab holds no credentials or account amounts.
const FORBIDDEN: &[&str] = &["api_key", "apikey", "secret", "passphrase", "token", "wallet", "balance", "password"];

/// Refuses text that contains a forbidden field (credentials, balances) as a JSON key or CSV column.
pub fn scrub(text: &str) -> Result<()> {
    let lower = text.to_ascii_lowercase();
    for w in FORBIDDEN {
        if lower.contains(&format!("\"{w}\"")) || lower.lines().next().is_some_and(|l| l.split(',').any(|c| c.trim() == *w)) {
            bail!("forbidden field `{w}` (privacy scrub)");
        }
    }
    Ok(())
}

/// Parses a trades CSV into rows; features come from its `f_*` columns. Also returns true (degraded) when the file has no feature columns.
pub fn parse_trades(text: &str) -> Result<(Vec<TradeRow>, bool)> {
    let mut rdr = csv::ReaderBuilder::new().flexible(false).from_reader(text.as_bytes());
    let headers: Vec<String> = rdr.headers()?.iter().map(|s| s.to_string()).collect();
    let col = |name: &str| headers.iter().position(|h| h == name);
    let need = |name: &str| col(name).ok_or_else(|| anyhow::anyhow!("missing column {name}"));
    let (c_sym, c_side, c_ent, c_ext, c_epx, c_xpx, c_qty, c_mar, c_fee, c_fun, c_pnl, c_ret, c_rsn) = (
        need("symbol")?, need("side")?, need("entry_ts")?, need("exit_ts")?, need("entry_px")?, need("exit_px")?,
        need("qty")?, need("margin")?, need("fees")?, need("funding")?, need("pnl")?, need("ret_margin")?, need("reason")?,
    );
    // Features are discovered from the `f_<name>` columns present in the file (dynamic registry).
    let fcols: Vec<(String, usize)> = headers.iter().enumerate()
        .filter_map(|(i, h)| h.strip_prefix("f_").filter(|n| valid_feature_name(n)).map(|n| (n.to_string(), i))).collect();
    let degraded = fcols.is_empty();
    let mut out = Vec::new();
    for (i, rec) in rdr.records().enumerate() {
        let r = rec?;
        let f = |c: usize| -> Result<f64> {
            let v: f64 = r[c].trim().parse().map_err(|_| anyhow::anyhow!("row {i}: bad number `{}`", &r[c]))?;
            if !v.is_finite() { bail!("row {i}: non-finite number"); }
            Ok(v)
        };
        let n = |c: usize| -> Result<i64> { r[c].trim().parse().map_err(|_| anyhow::anyhow!("row {i}: bad integer `{}`", &r[c])) };
        let features: BTreeMap<String, f64> = fcols.iter()
            .filter_map(|(n, c)| r[*c].trim().parse::<f64>().ok().filter(|v| v.is_finite()).map(|v| (n.clone(), v))).collect();
        out.push(TradeRow {
            symbol: r[c_sym].to_string(), side: r[c_side].to_string(), entry_ts: n(c_ent)?, exit_ts: n(c_ext)?,
            entry_px: f(c_epx)?, exit_px: f(c_xpx)?, qty: f(c_qty)?, margin: f(c_mar)?, fees: f(c_fee)?, funding: f(c_fun)?,
            pnl: f(c_pnl)?, ret_margin: f(c_ret)?, reason: r[c_rsn].to_string(), features,
        });
    }
    Ok((out, degraded))
}

/// Parses a daily CSV (day, equity, pnl), rejecting non-finite numbers.
pub fn parse_daily(text: &str) -> Result<Vec<DailyRow>> {
    let mut rdr = csv::Reader::from_reader(text.as_bytes());
    let mut out = Vec::new();
    for rec in rdr.records() {
        let r = rec?;
        if r.len() < 3 { bail!("daily row needs day,equity,pnl"); }
        let p = |s: &str| -> Result<f64> { let v: f64 = s.trim().parse()?; if v.is_finite() { Ok(v) } else { bail!("non-finite") } };
        out.push(DailyRow { day: r[0].trim().parse()?, equity: p(&r[1])?, pnl: p(&r[2])? });
    }
    Ok(out)
}

/// Quality gate (spec 4.1). Err = reject the whole run (quarantine).
pub fn validate(h: &RunHeader, trades: &[TradeRow], daily: &[DailyRow]) -> Result<()> {
    if h.schema_version != SCHEMA_VERSION { bail!("unsupported schema_version {}", h.schema_version); }
    if h.window_end <= h.window_start { bail!("window_end must be after window_start"); }
    if h.metrics.trades as usize != trades.len() { bail!("header says {} trades, file has {}", h.metrics.trades, trades.len()); }
    if h.run_id.is_empty() || h.run_id.contains('/') || h.run_id.contains("..") { bail!("bad run_id"); }
    for (i, t) in trades.iter().enumerate() {
        if t.side != "long" && t.side != "short" { bail!("trade {i}: side must be long or short"); }
        if t.exit_ts <= t.entry_ts { bail!("trade {i}: exit must be after entry"); }
        if t.entry_ts < h.window_start || t.entry_ts > h.window_end { bail!("trade {i}: entry outside the window"); }
        if !(t.margin > 0.0) || !(t.entry_px > 0.0) || !(t.exit_px > 0.0) { bail!("trade {i}: non-positive margin or price"); }
        if t.fees < 0.0 { bail!("trade {i}: negative fees"); }
        let implied = t.pnl / t.margin;
        if (implied - t.ret_margin).abs() > 1e-6 + 1e-6 * t.ret_margin.abs() { bail!("trade {i}: ret_margin does not equal pnl/margin"); }
    }
    for w in daily.windows(2) {
        if w[1].day <= w[0].day { bail!("daily rows must be strictly increasing by day"); }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test fixture: a valid run header.
    fn header() -> RunHeader {
        RunHeader {
            schema_version: 1, run_id: "r1".into(), synthetic: true, engine_commit: "SYNTHETIC".into(), window_label: "w1".into(),
            window_start: 0, window_end: 1000, data_fingerprint: "x".into(), cost_model_version: "c1".into(), role: "study".into(),
            rank: None, leverage: 3.0, start_equity: 1000.0, params: serde_json::json!({"a": 1}),
            metrics: Metrics { return_pct: 1.0, max_dd_pct: 1.0, trades: 1, wins: 1, liquidations: 0, profit_factor: 1.0, fees: 0.1, funding: 0.0 },
            seed: None,
        }
    }

    /// Test fixture: a valid trade row.
    fn trade() -> TradeRow {
        TradeRow { symbol: "AAA".into(), side: "long".into(), entry_ts: 10, exit_ts: 20, entry_px: 1.0, exit_px: 1.1, qty: 1.0, margin: 100.0,
            fees: 0.1, funding: 0.0, pnl: 5.0, ret_margin: 0.05, reason: "tp".into(), features: BTreeMap::new() }
    }

    /// Test: valid run passes and each defect is rejected.
    #[test]
    fn valid_run_passes_and_each_defect_is_rejected() {
        let h = header();
        assert!(validate(&h, &[trade()], &[]).is_ok());
        let mut t = trade(); t.exit_ts = 5;
        assert!(validate(&h, &[t], &[]).is_err());
        let mut t = trade(); t.ret_margin = 0.5;
        assert!(validate(&h, &[t], &[]).is_err());
        let mut t = trade(); t.entry_ts = 5000;
        assert!(validate(&h, &[t], &[]).is_err());
        assert!(validate(&h, &[], &[]).is_err()); // header promises one trade
    }

    /// Test: scrub blocks credentials and account fields.
    #[test]
    fn scrub_blocks_credentials_and_account_fields() {
        assert!(scrub("{\"api_key\": \"x\"}").is_err());
        assert!(scrub("symbol,balance,pnl\nA,1,2").is_err());
        assert!(scrub("{\"params\": {\"lookback\": 3}}").is_ok());
    }
}
