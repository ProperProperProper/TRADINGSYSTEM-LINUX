//! Synthetic exporter (spec 4.2). Used ONLY to test the machinery: every file is flagged `synthetic` and the ingester
//! refuses to mix synthetic and real data. It can plant a known effect (to test recall) or plant none (to test that
//! noise does not produce discoveries).

use crate::contract::{trades_header, ManifestEntry, Metrics, RunHeader, SCHEMA_VERSION};
use crate::util::sha256_file;
use anyhow::Result;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::io::Write;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct SynthSpec {
    pub windows: usize,
    pub days_per_window: usize,
    pub entries_per_day: usize,
    pub runs_per_window: usize,
    /// (feature name, effect per standard deviation, in return-on-margin units)
    pub planted: Option<(String, f64)>,
    pub seed: u64,
    pub first_window: usize,
}

impl Default for SynthSpec {
    /// Default synthetic spec (tests only): 3 windows of 21 days.
    fn default() -> Self {
        Self { windows: 3, days_per_window: 21, entries_per_day: 12, runs_per_window: 6, planted: None, seed: 1, first_window: 1 }
    }
}

/// Feature names used by the test fixtures (the production registry is dynamic).
pub const FIXTURE_FEATURES: &[&str] = &[
    "btc_dist_sma", "btc_ret_1h", "btc_atr_pct", "breadth", "dispersion", "funding", "vol_ratio", "vol_spike",
    "wick_ratio", "body_ratio", "close_loc", "vwap_dist", "coin_ret_24h", "coin_atr_pct", "hour", "weekday",
];

const T0: i64 = 1_700_000_000 - 1_700_000_000 % 86_400;

/// Standard normal random number (Box-Muller).
fn normal(rng: &mut ChaCha8Rng) -> f64 {
    let u1: f64 = rng.gen_range(1e-12..1.0);
    let u2: f64 = rng.gen();
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

struct Entry {
    symbol: String,
    ts: i64,
    feats: Vec<f64>,
    y: f64,
}

/// Writes synthetic export files for tests (never used on a real lab).
pub fn generate(out: &Path, spec: &SynthSpec) -> Result<usize> {
    let tmp = out.join(".tmp");
    std::fs::create_dir_all(&tmp)?;
    let mut manifest = Vec::new();
    let mut runs = 0usize;
    for wi in 0..spec.windows {
        let w = spec.first_window + wi;
        let label = format!("w{w}");
        let start = T0 + (w as i64 - 1) * spec.days_per_window as i64 * 86_400;
        let end = start + spec.days_per_window as i64 * 86_400 - 1;
        let mut rng = ChaCha8Rng::seed_from_u64(spec.seed * 1_000_003 + w as u64);
        let mut entries = Vec::new();
        for d in 0..spec.days_per_window {
            let shock = 0.02 * normal(&mut rng);
            for k in 0..spec.entries_per_day {
                let ts = start + d as i64 * 86_400 + (k as i64 * 7 % 96) * 900;
                let mut feats: Vec<f64> = FIXTURE_FEATURES.iter().map(|_| normal(&mut rng)).collect();
                for (i, f) in FIXTURE_FEATURES.iter().enumerate() {
                    if *f == "hour" { feats[i] = ((ts / 3600) % 24) as f64; }
                    if *f == "weekday" { feats[i] = ((ts / 86_400 + 4) % 7) as f64; }
                }
                let mut y = shock + 0.03 * normal(&mut rng);
                if let Some((name, eff)) = &spec.planted {
                    if let Some(i) = FIXTURE_FEATURES.iter().position(|f| f == name) { y += eff * feats[i]; }
                }
                entries.push(Entry { symbol: format!("S{}", k % 20), ts, feats, y });
            }
        }
        for r in 0..spec.runs_per_window {
            let run_id = format!("{label}-r{r}");
            let mut rr = ChaCha8Rng::seed_from_u64(spec.seed * 7_919 + (w * 1000 + r) as u64);
            let hold = rr.gen_range(1..8) as i64;
            let mut rows: Vec<Vec<String>> = Vec::new();
            let (mut wins, mut gross_win, mut gross_loss, mut fees) = (0u64, 0.0, 0.0, 0.0);
            let mut day_pnl: std::collections::BTreeMap<i64, f64> = Default::default();
            for e in &entries {
                if rr.gen::<f64>() > 0.6 { continue; }
                let ret = e.y + 0.005 * normal(&mut rr);
                let margin = 100.0;
                let pnl = ret * margin;
                let fee = 0.1;
                fees += fee;
                if pnl > 0.0 { wins += 1; gross_win += pnl } else { gross_loss += -pnl }
                let exit_ts = e.ts + hold * 3600;
                *day_pnl.entry(exit_ts / 86_400).or_insert(0.0) += pnl;
                let mut row = vec![
                    e.symbol.clone(), "long".into(), e.ts.to_string(), exit_ts.to_string(), "100".into(),
                    format!("{}", 100.0 * (1.0 + ret / 3.0)), "1".into(), "100".into(), fee.to_string(), "0".into(),
                    format!("{pnl}"), format!("{}", pnl / margin), "synthetic".into(),
                ];
                row.extend(e.feats.iter().map(|v| format!("{v}")));
                rows.push(row);
            }
            let n = rows.len() as u64;
            let trades_name = format!("run_{run_id}_trades.csv");
            let daily_name = format!("run_{run_id}_daily.csv");
            let header_name = format!("run_{run_id}.json");
            {
                let mut wtr = csv::Writer::from_path(tmp.join(&trades_name))?;
                wtr.write_record(trades_header(FIXTURE_FEATURES))?;
                for row in &rows { wtr.write_record(row)?; }
                wtr.flush()?;
                let mut wtr = csv::Writer::from_path(tmp.join(&daily_name))?;
                wtr.write_record(["day", "equity", "pnl"])?;
                let mut equity = 1000.0;
                for (d, p) in &day_pnl { equity += p; wtr.write_record([d.to_string(), equity.to_string(), p.to_string()])?; }
                wtr.flush()?;
            }
            let total: f64 = day_pnl.values().sum();
            let header = RunHeader {
                schema_version: SCHEMA_VERSION, run_id: run_id.clone(), synthetic: true, engine_commit: "SYNTHETIC".into(),
                window_label: label.clone(), window_start: start, window_end: end, data_fingerprint: format!("synthetic-{}-{w}", spec.seed),
                cost_model_version: "synthetic-1".into(), role: if r < 20 { "search_winner".into() } else { "study".into() },
                rank: if r < 20 { Some(r as i64 + 1) } else { None }, leverage: 3.0, start_equity: 1000.0,
                params: serde_json::json!({"p_hold": hold, "p_gate": rr.gen_range(0.0..1.0f64)}),
                metrics: Metrics { return_pct: total / 10.0, max_dd_pct: 5.0, trades: n, wins, liquidations: 0,
                    profit_factor: if gross_loss > 0.0 { gross_win / gross_loss } else { 0.0 }, fees, funding: 0.0 },
                seed: Some(spec.seed),
            };
            std::fs::write(tmp.join(&header_name), serde_json::to_vec_pretty(&header)?)?;
            for (name, kind) in [(&header_name, "header"), (&trades_name, "trades"), (&daily_name, "daily")] {
                let p = tmp.join(name);
                manifest.push(ManifestEntry { file: name.clone(), sha256: sha256_file(&p)?, bytes: std::fs::metadata(&p)?.len(), schema_version: SCHEMA_VERSION, kind: kind.into() });
                std::fs::rename(&p, out.join(name))?;
            }
            runs += 1;
        }
    }
    // The manifest is appended last, so a half-written run is never ingested (spec section 4).
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(out.join("MANIFEST.jsonl"))?;
    for m in manifest { writeln!(f, "{}", serde_json::to_string(&m)?)?; }
    f.sync_all()?;
    Ok(runs)
}
