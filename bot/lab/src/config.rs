//! `lab.toml`: every threshold and budget lives here and is snapshotted into the memory database per run.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub root: PathBuf,
    /// `real` or `synthetic`; the two never mix (spec 4.2).
    pub mode: String,
    pub limits: Limits,
    pub evidence: Evidence,
    pub paper: PaperConfig,
    pub backtest_archive: BacktestArchiveConfig,
    pub schedule: ScheduleConfig,
    pub dashboard: DashboardConfig,
    pub strategy: StrategyConfig,
}

/// The strategy-improvement loop (`qwen-lab improve`): the bot-side scoring tool and what it is run over.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct StrategyConfig {
    /// `variant_study` example binary built in the moomoo bot repo.
    pub study_bin: PathBuf,
    /// Start capitals (USD) every variant is scored at, in addition to the bot's own capital when `include_account_capital` is set.
    pub equities: Vec<f64>,
    /// Also score at the paper account's available cash. Off by default: the bot trades a 5,000 USD account (moomoo-bot
    /// `START_CAPITAL`, user 2026-10-09), which `equities` covers; the ~1M USD paper balance is not what it trades.
    pub include_account_capital: bool,
    /// The bot's store, read-only, for the account's latest available cash.
    pub store_path: PathBuf,
    pub intervals: Vec<String>,
    /// Variants the explorer picks per round when nothing else is due.
    pub per_round: usize,
    /// Study processes run at the same time (the machine also runs the bot).
    pub max_parallel: usize,
    /// Share of the CPU cores all running studies together may use (owner 2026-10-12: the lab must never max out the machine).
    pub cpu_share: f64,
    /// Pause between rounds of `improve --forever`, seconds.
    pub pause_secs: u64,
}

impl Default for StrategyConfig {
    /// Default strategy-loop settings: the bot's variant_study binary, a 5,000 USD capital, all research intervals, 4 variants per round.
    fn default() -> Self {
        Self {
            study_bin: crate::platform::bot_repo().join("target/release/examples").join(crate::platform::exe("variant_study")),
            equities: vec![5_000.0],
            include_account_capital: false,
            store_path: crate::platform::bot_store_path(),
            intervals: vec!["5h".into()],
            per_round: 4,
            max_parallel: 3,
            cpu_share: 0.4,
            pause_secs: 60,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DashboardConfig {
    /// Localhost port of the read-only monitoring page (the bot's console uses 8787).
    pub port: u16,
}

impl Default for DashboardConfig {
    /// Default dashboard port 8790.
    fn default() -> Self { Self { port: 8790 } }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleConfig {
    /// Local hour (0-23) of the nightly full tick.
    pub full_hour: u32,
    pub min_hours_between_full: i64,
    /// When set, a full tick (backup, size check) runs whenever this many hours have passed, at any hour, instead of once a night at `full_hour`.
    pub full_every_hours: Option<i64>,
    pub max_load_per_cpu: f64,
    pub lock_stale_hours: i64,
}

impl Default for ScheduleConfig {
    /// Default schedule: a full tick at 03:00 at most every 20 hours, with load and memory guards.
    fn default() -> Self {
        Self { full_hour: 3, min_hours_between_full: 20, full_every_hours: None, max_load_per_cpu: 0.85, lock_stale_hours: 3 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BacktestArchiveConfig {
    pub path: PathBuf,
}

impl Default for BacktestArchiveConfig {
    /// Default backtest-archive path: the SPY bot's archive.
    fn default() -> Self {
        Self { path: crate::platform::bot_backtest_archive_path() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PaperConfig {
    pub archive_path: PathBuf,
    /// Which bot wrote the archive: "moomoo" (US stocks, USD, long-only; default). "bybit" is a legacy format kept only so old
    /// test fixtures still parse; the lab never uses crypto data.
    pub source: String,
    /// Currency the paper amounts are in (shown in prompts and reports).
    pub currency: String,
    pub usable_trades: i64,
    pub usable_days: i64,
    pub rich_trades: i64,
    pub rich_days: i64,
    pub stale_secs: i64,
}

impl Default for PaperConfig {
    /// Default paper-archive settings: the SPY bot's store (moomoo, USD).
    fn default() -> Self {
        Self {
            archive_path: crate::platform::bot_store_path(),
            source: "moomoo".into(),
            currency: "USD".into(),
            usable_trades: 30,
            usable_days: 14,
            rich_trades: 200,
            rich_days: 28,
            stale_secs: 7200,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub data_db_limit_bytes: u64,
    pub data_db_target_bytes: u64,
    pub memory_db_alert_bytes: u64,
    pub newest_batches_protected: usize,
}

/// How the bot's archive is cut into windows.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Evidence {
    /// Length in days of the independent windows cut from the bot's archive (fixed per lab at the first import).
    pub block_days: i64,
    /// Fewest windows (complete plus one provisional) needed to start learning from the archive.
    pub min_windows: usize,
}

impl Default for Config {
    /// Default lab configuration rooted at ~/Projects/qwen-lab in real mode.
    fn default() -> Self {
        Self {
            root: crate::platform::lab_root(),
            mode: "real".into(),
            limits: Limits::default(),
            evidence: Evidence::default(),
            paper: PaperConfig::default(),
            backtest_archive: BacktestArchiveConfig::default(),
            schedule: ScheduleConfig::default(),
            dashboard: DashboardConfig::default(),
            strategy: StrategyConfig::default(),
        }
    }
}

impl Default for Limits {
    /// Default database size limits: cull data above 10 GB down to 8 GB, alert on memory above 2 GB.
    fn default() -> Self {
        Self {
            data_db_limit_bytes: 10 * 1024 * 1024 * 1024,
            data_db_target_bytes: 8 * 1024 * 1024 * 1024,
            memory_db_alert_bytes: 2 * 1024 * 1024 * 1024,
            newest_batches_protected: 3,
        }
    }
}

impl Default for Evidence {
    /// Default windows: 7-day blocks, three needed.
    fn default() -> Self { Self { block_days: 7, min_windows: 3 } }
}

impl Config {
    /// Loads lab.toml; refuses anything but real mode and a cull target below the limit.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let cfg: Config = toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        // Real data only (user rule, 2026-10-05): the lab refuses to run on anything else. Synthetic fixtures exist only
        // inside the automated test build and never reach a lab folder.
        anyhow::ensure!(cfg.mode == "real", "this lab runs on real data only (mode must be \"real\")");
        anyhow::ensure!(cfg.limits.data_db_target_bytes < cfg.limits.data_db_limit_bytes, "cull target must be below the limit");
        Ok(cfg)
    }

    /// Configuration rooted at `root` in `mode`, everything else default.
    pub fn for_root(root: &Path, mode: &str) -> Self {
        Self { root: root.to_path_buf(), mode: mode.into(), ..Self::default() }
    }

    /// Folder the bot's exports are read from.
    pub fn exports_dir(&self) -> PathBuf { self.root.join(if self.mode == "synthetic" { "exports-synthetic" } else { "exports" }) }
    /// Folder where rejected imports are kept with their reason.
    pub fn quarantine_dir(&self) -> PathBuf { self.root.join("quarantine") }
    /// Folder for requests to the bot session.
    pub fn requests_dir(&self) -> PathBuf { self.root.join("requests") }
    /// Folder for daily reports.
    pub fn reports_dir(&self) -> PathBuf { self.root.join("reports") }
    /// Folder for logs.
    pub fn logs_dir(&self) -> PathBuf { self.root.join("logs") }
    /// Path of the data database (imported runs and trades).
    pub fn data_db(&self) -> PathBuf { self.root.join("data").join("data.sqlite3") }
    pub fn candles_db(&self) -> PathBuf { self.root.join("data").join("candles.sqlite3") }
    /// Path of the memory database (persistent learning).
    pub fn memory_db(&self) -> PathBuf { self.root.join("memory").join("memory.sqlite3") }
    /// Folder for memory backups.
    pub fn backup_dir(&self) -> PathBuf { self.root.join("memory").join("backups") }
    /// Path of the kill-switch file: while it exists, cycles refuse to run.
    pub fn stop_file(&self) -> PathBuf { self.root.join("STOP") }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: partial toml uses defaults and validates.
    #[test]
    fn partial_toml_uses_defaults_and_validates() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lab.toml");
        std::fs::write(&p, "[evidence]\nmin_windows = 4\n").unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.mode, "real");
        assert_eq!(c.evidence.min_windows, 4);
        assert_eq!(c.evidence.block_days, 7);
        std::fs::write(&p, "mode = \"bogus\"\n").unwrap();
        assert!(Config::load(&p).is_err());
        std::fs::write(&p, "mode = \"synthetic\"\n").unwrap();
        assert!(Config::load(&p).is_err(), "synthetic labs must be impossible to load");
    }
}
