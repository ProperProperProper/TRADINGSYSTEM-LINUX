use anyhow::Result;
use clap::{Parser, Subcommand};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
mod board;
mod deploy;
mod headroom;
mod improve_cmd;
mod model_distill;
mod model_eval;
mod model_gate;
mod momentum_backtest;
mod momentum_forward;
mod msgboard;
mod objectives;
mod orb;
mod ops;
mod paper_cmd;
mod portable;
mod portal;
mod select;
mod symbol_tune;
mod validation;
mod watchdog;
use thomas_core::{candles, export, extra, merge, protodiff, reconcile, store, symbols};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum MsgCmd {
    /// Put a message on the board
    Post {
        #[arg(long)]
        from: String,
        #[arg(long, default_value = "*")]
        to: String,
        #[arg(long, default_value = "")]
        topic: String,
        #[arg(long, default_value = "note")]
        kind: String,
        /// The goal (O1-O6) the message serves
        #[arg(long, default_value = "")]
        goal: String,
        #[arg(long)]
        reply_to: Option<i64>,
        text: String,
    },
    /// Read what is new for an agent (marks it read); --all includes what it has already read
    Read {
        #[arg(long = "as")]
        r#as: String,
        #[arg(long)]
        all: bool,
        #[arg(long, default_value_t = 15)]
        limit: usize,
    },
    /// The newest messages of the whole board
    List {
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
}

#[derive(Subcommand)]
enum Cmd {
    /// Research-only 1-minute NASDAQ opening-range breakout replay; no orders.
    OrbBacktest {
        #[arg(long, default_value = "papercompmoomoo/data/common1m.sqlite3")] db: PathBuf,
        #[arg(long, default_value = "data/thomas.sqlite3")] paper_store: PathBuf,
        #[arg(long, default_value_t = 1.0)] risk_pct: f64,
        #[arg(long, default_value_t = 10.0)] cost_bps: f64,
    },
    /// Emit the frozen causal momentum candidates as JSON; research only
    MomentumSelect {
        #[arg(long, default_value = "papercompmoomoo/data/common1m.sqlite3")]
        db: String,
        #[arg(long, default_value_t = 20)]
        lookback_sessions: usize,
        #[arg(long, default_value_t = 15)]
        limit: usize,
        /// Per-symbol tuned parameters (stored in `symbol_params`) instead of the single 20-session lookback
        #[arg(long)]
        tuned: bool,
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        #[arg(long, default_value = "papercompmoomoo/data/nasdaq.json")]
        universe: PathBuf,
        #[arg(long, default_value_t = 100_000)]
        trials: usize,
    },
    /// Backtest causal cross-sectional 1-minute momentum; research only
    MomentumBacktest {
        #[arg(long, default_value = "papercompmoomoo/data/common1m.sqlite3")]
        db: String,
        /// Durable research-result store shown by the read-only portal
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        #[arg(long, default_value_t = 20)]
        lookback_sessions: usize,
        /// Nasdaq-listed symbols MooComp may trade; the backtest ranks only these (matches live)
        #[arg(long, default_value = "papercompmoomoo/data/nasdaq.json")]
        universe: PathBuf,
        /// Rank by each symbol's own causal track record (earlier sessions only) instead of raw momentum
        #[arg(long)]
        track_record: bool,
        #[arg(long, default_value_t = 15)]
        slots: usize,
        #[arg(long, default_value_t = 10.0)]
        cost_bps: f64,
        #[arg(long, default_value_t = 100_000.0)]
        capital: f64,
    },
    /// Per-symbol walk-forward parameter search on stored real data; stores fits in symbol_params (research only)
    MomentumTune {
        #[arg(long, default_value = "papercompmoomoo/data/common1m.sqlite3")]
        db: String,
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        #[arg(long, default_value = "papercompmoomoo/data/nasdaq.json")]
        universe: PathBuf,
        /// Random-search trials per symbol (capped at the size of the grid)
        #[arg(long, default_value_t = 100_000)]
        trials: usize,
        #[arg(long, default_value_t = 15)]
        slots: usize,
        #[arg(long, default_value_t = 10.0)]
        cost_bps: f64,
        #[arg(long, default_value_t = 100_000.0)]
        capital: f64,
    },
    /// Persist causal next-session outcomes for every frozen momentum candidate
    MomentumForward {
        #[arg(long, default_value = "papercompmoomoo/data/journal.sqlite3")]
        journal: PathBuf,
        #[arg(long, default_value = "papercompmoomoo/data/common1m.sqlite3")]
        db: PathBuf,
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        #[arg(long, default_value = "data/symbol_map.csv")]
        symbol_map: PathBuf,
        #[arg(long, default_value_t = 10.0)]
        cost_bps: f64,
    },
    /// Load the candle snapshot, validate it, and record the report in the store
    DataCheck {
        #[arg(long, default_value = "data/candles.sqlite3")]
        candles: PathBuf,
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
    },
    /// Cross-check moomoo against Yahoo, fill zero-volume bars with real Yahoo bars, record gaps
    Reconcile {
        #[arg(long, default_value = "data/candles.sqlite3")]
        candles: PathBuf,
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        /// Only these symbols (default: all)
        #[arg(long, value_delimiter = ',')]
        symbols: Option<Vec<String>>,
    },
    /// Rebuild the symbol translation list (which symbols the live bot may trade)
    Symbols {
        #[arg(long, default_value = "data/candles.sqlite3")]
        bot_snapshot: PathBuf,
        #[arg(long, default_value = "bot/data/spy_holdings.csv")]
        holdings: PathBuf,
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
    },
    /// Fetch real Yahoo 1m bars for SPY holdings moomoo has no candles for (into candles_extra)
    FetchExtra {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
    },
    /// Fetch the real US trading calendar since the research start (2026-01-02) into sessions_calendar
    FetchSessions {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
    },
    /// Refresh data/candles.sqlite3 from the bot's store (read-only source, SQLite backup), e.g. after new moomoo pulls; then run `merge`
    Snapshot {
        /// Default: the bot's store inside this project (state/ClaudeMoomooBotArchive/moomoo.sqlite3)
        #[arg(long)]
        from: Option<PathBuf>,
        #[arg(long, default_value = "data/candles.sqlite3")]
        to: PathBuf,
    },
    /// Merge moomoo and Yahoo into candles_merged (moomoo primary; Yahoo only where moomoo lacks a real bar)
    Merge {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        #[arg(long, default_value = "data/candles.sqlite3")]
        bot_snapshot: PathBuf,
    },
    /// Write data/symbol_map.csv (both spellings, status, match evidence)
    ExportMap {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        #[arg(long, default_value = "data/symbol_map.csv")]
        out: PathBuf,
    },
    /// Check every data invariant; exits non-zero if any fails
    Verify {
        #[arg(long, default_value = "papercompmoomoo/data/common1m.sqlite3")]
        db: PathBuf,
        #[arg(long, default_value = "data/symbol_map.csv")]
        symbol_map: PathBuf,
        #[arg(long, default_value = "versions.lock")]
        lock: PathBuf,
    },
    /// Compare the bot's proto files with the SDK's field by field; full report to --report
    ProtoDiff {
        #[arg(long, default_value = "bot/proto")]
        old: PathBuf,
        /// The SDK's proto folder (default: inside the pinned SDK in tools/moomoo-venv)
        #[arg(long)]
        new: Option<PathBuf>,
        #[arg(long, default_value = "docs/PROTO_DIFF.md")]
        report: PathBuf,
    },
    /// Git hook logic (hooks/ are one-line stubs that call this): pre-commit | post-commit
    Hook { name: String },
    /// Phase 2: overlapping 14-session periods, a new one every 3 sessions, across the scored part of the 40-session window (all symbols allowed in); each new period is searched once with the bot's own search, candidates re-run on all periods. IN-SAMPLE except the forward tests; stored.
    Search {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        #[arg(long, default_value = "data/candles.sqlite3")]
        bot_snapshot: PathBuf,
        #[arg(long, default_value = "bot/data/spy_holdings.csv")]
        holdings: PathBuf,
        /// Combinations drawn per period (the bot's live search draws 200,000 per window)
        #[arg(long, default_value_t = 200_000)]
        samples: usize,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Usability floor of closed trades per 14-session period (default: thomas_core::config::MIN_TRADES_SEARCH = 3; the bot's own is 8)
        #[arg(long)]
        min_trades: Option<usize>,
    },
    /// Walk-forward validation on real data: several expanding-window folds, each choosing its setting only from candles before its test block and judged on
    /// the block after it; out-of-sample blocks are stitched and compared with buy-and-hold of SPY. Two selection rules run on the same folds.
    Walkforward {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        #[arg(long, default_value = "data/candles.sqlite3")]
        bot_snapshot: PathBuf,
        #[arg(long, default_value = "bot/data/spy_holdings.csv")]
        holdings: PathBuf,
        /// Settings drawn per fold at the 5-hour size (the bot's live search draws 200,000); finer candles draw proportionally fewer, because a backtest on 1-minute candles costs
        /// about 13 times more, so every run takes about the same time
        #[arg(long, default_value_t = 50_000)]
        samples: usize,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Fold layouts as "training-sessions x block-sessions", comma separated: each layout is a separate walk-forward
        #[arg(long, default_value = "8x3,10x2")]
        layouts: String,
        /// Candle sizes to run, comma separated (all built from the real 1-minute bars): 1m 30m 45m 1h 90m 2h 150m 3h 4h 5h 6h 1d
        #[arg(long, default_value = "4h,3h,150m,2h,90m,1h,45m,30m,1m")]
        intervals: String,
        /// Longest holding time in minutes, or "live" for the live holding range of 1-2 sessions; each value is a separate run (the owner's upper limit is 4h15 = 255)
        #[arg(long, default_value = "255,240,180")]
        hold_minutes: String,
        /// Closed trades a setting needs to count as usable in a training window (owner: 2 is the lower limit; few trades are fine when the profit is higher)
        #[arg(long, default_value_t = 2)]
        min_trades: usize,
        /// Most positions held at once. The owner's rule is exactly 1 (LIVE_TOP); anything else is research that could never go live
        #[arg(long, default_value_t = 1)]
        top: usize,
        /// Use the DEEP market: only symbols with 60+ sessions of real history (the bot's ~95 moomoo symbols), over all of it. Needs the reference symbol's history (quota-gated)
        #[arg(long)]
        deep: bool,
        /// Universe picks to compare, comma separated: all, turnover:N, volatility:N, reversal:N. Each fold picks its symbols from its training window only; picks other than
        /// `all` run the robust rule only
        #[arg(long, default_value = "all")]
        universe: String,
        /// Trend filter settings to try: on, off
        #[arg(long, default_value = "on,off")]
        trend: String,
    },
    /// The weekly symbol pick: rank every tradable symbol on the last 14 sessions and write data/selection.json for the bot (its importer fetches these first, its backtest
    /// and paper trading use the ones with candles). Runs at most once a week unless --force.
    Select {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        #[arg(long, default_value = "data/candles.sqlite3")]
        bot_snapshot: PathBuf,
        #[arg(long, default_value = "bot/data/spy_holdings.csv")]
        holdings: PathBuf,
        #[arg(long, default_value = "data/selection.json")]
        out: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// The local paper trader's live feed: the last days of real Yahoo 1-minute bars (closed bars only) for every tradable symbol; run `merge` after it
    FetchPaper {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        #[arg(long, default_value_t = 3)]
        days: i64,
    },
    /// The local paper trader: steps the engine's own paper portfolio (5,000 USD, the measured fees, lot sizes and slippage) through every newly closed candle of the
    /// weekly pick; no order leaves the machine. The first run opens the account, later runs trade candles after it
    Paper {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        #[arg(long, default_value = "data/candles.sqlite3")]
        bot_snapshot: PathBuf,
        #[arg(long, default_value = "bot/data/spy_holdings.csv")]
        holdings: PathBuf,
        /// Settings drawn when the setting is re-searched (the bot's live search draws 200,000)
        #[arg(long, default_value_t = 200_000)]
        samples: usize,
    },
    /// Pause the local paper trader for some hours (it resumes by itself and skips the candles of the pause); the competition bot, the data imports and all research carry on.
    /// Refuses while the paper account holds a position, because nothing would manage it
    PaperPause {
        #[arg(long, default_value_t = 72)]
        hours: i64,
        #[arg(long, default_value = "owner asked for a pause")]
        reason: String,
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
    },
    /// End a pause of the paper trader now
    PaperResume,
    /// The paper trader's whole pass in one light job (run by launchd at minutes 4, 19, 34 and 49 of every hour, after the sibling project's candle converter): inside US trading hours it pulls the closed bars of every tradable symbol,
    /// merges them and steps the account; outside them it does nothing. Its own lock keeps two runs from overlapping
    PaperRun {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        #[arg(long, default_value = "data/candles.sqlite3")]
        bot_snapshot: PathBuf,
        #[arg(long, default_value = "bot/data/spy_holdings.csv")]
        holdings: PathBuf,
        /// Run outside trading hours too
        #[arg(long)]
        force: bool,
    },
    /// Phase 4: one unattended pass (refresh the data, verify, search, learn, improve, write the report); every step is logged in cycle_runs
    Cycle {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        /// Improvement-loop experiments per cycle (0 = none)
        #[arg(long, default_value_t = 2)]
        rounds: usize,
        /// Skip the Yahoo/calendar refresh
        #[arg(long)]
        no_refresh: bool,
    },
    /// Pack the irreplaceable data (databases, picks, the bot's candle store) into data/bundles/thomas-data-<date>.tar.gz for another machine; --publish also uploads it as a release of the private repository
    Bundle {
        #[arg(long)]
        publish: bool,
    },
    /// Unpack a data bundle (a file, or the newest release of the repository) into this checkout; refuses to overwrite without --force
    Restore {
        file: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    /// Read the new 1-minute bars from the SQLite candle store that papercompmoomoo keeps (the one store both bots use) into candles_extra, checked row by
    /// row; run `merge` after it
    ImportBars {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        #[arg(long, default_value = thomas_core::ingest::EXTERNAL_DB)]
        db: PathBuf,
        #[arg(long, default_value = thomas_core::ingest::SYMBOL_MAP)]
        symbol_map: PathBuf,
    },
    /// The overnight caretaker: one competition-bot chain, current local code, and a live candle feed; it never pushes.
    /// what it did goes to data/watchdog.log
    Watchdog,
    /// Everything from a fresh clone to a running system: tools, the local model, data, build, scheduled jobs, the bot's importer, verification
    Setup {
        /// A data bundle file instead of the newest release
        #[arg(long)]
        bundle: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
    },
    /// The read-only web portal (paper trading, AI testing, bot backtests) on port 8790; loopback and private IPv4 clients only
    Portal {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        #[arg(long, default_value = "data/backtests.sqlite3")]
        archive: PathBuf,
        #[arg(long, default_value_t = portal::PORT)]
        port: u16,
    },
    /// The agents' message board (papercompmoomoo/data/board.sqlite3): post, read what is for an agent, list, and see the commander's rules and the council's decisions
    Msg {
        #[command(subcommand)]
        cmd: MsgCmd,
    },
    /// Attach a Rust evaluator result to a ThomasAi model validation record
    Validation {
        #[arg(long, default_value = "papercompmoomoo/data/board.sqlite3")]
        database: PathBuf,
        #[arg(long)]
        id: i64,
        #[arg(long)]
        status: String,
    },
    /// Check a trained ThomasAi candidate against provenance and Rust validation before controlled activation
    ModelGate {
        #[arg(long)]
        candidate: PathBuf,
        #[arg(long, default_value = "papercompmoomoo/data/board.sqlite3")]
        database: PathBuf,
        #[arg(long)]
        validation_id: i64,
    },
    /// Evaluate recorded model decision envelopes against the real causal replay and write evaluation.json
    ModelEvaluate {
        #[arg(long)]
        decisions: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Export real closed events from the converted common candle database for a local model adapter
    ModelEvents {
        #[arg(long)]
        output: PathBuf,
    },
    /// Export a causal Qwen decision corpus from real closed events
    ModelDecisionCorpus {
        #[arg(long)]
        output: PathBuf,
    },
    /// Export closed shadow decisions as a causal Qwen feedback corpus
    ModelShadowCorpus {
        #[arg(long)]
        output: PathBuf,
    },
    /// Distill real recorded teacher envelopes into a consensus ThomasAi corpus
    ModelDistill {
        #[arg(long = "trace", required = true)]
        traces: Vec<PathBuf>,
        #[arg(long)]
        output: PathBuf,
    },
    /// The project's objectives, scored from the real files and tables, and the agents that serve them
    Objectives {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
    },
    /// Check the 16% free CPU/RAM/disk rule before a heavy job (exit 1 when it would break it)
    Headroom {
        /// RAM the job will use, GB
        #[arg(long, default_value_t = 0.0)]
        ram_gb: f64,
        /// Disk the job will use, GB
        #[arg(long, default_value_t = 0.0)]
        disk_gb: f64,
    },
    /// Phase 4: the honest report from the store (printed and written to data/reports/<date>.md)
    Report {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
    },
    /// Phase 4: what is wrong (stale cycles, failed steps, old candles, code older than the files on disk, uncommitted work); exits 1 on an error
    Doctor {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
    },
    /// Phase 4: install, remove or show the launchd agent that runs `thomas cycle` every hour at the lowest priority
    Schedule {
        /// install | remove | status
        action: String,
        #[arg(long, default_value_t = 3600)]
        interval: u32,
    },
    /// A3.3: the local model picks search-space experiments from a fixed list; the harness runs, guards and logs them
    Improve {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        #[arg(long, default_value = "data/candles.sqlite3")]
        bot_snapshot: PathBuf,
        #[arg(long, default_value = "bot/data/spy_holdings.csv")]
        holdings: PathBuf,
        /// Combinations drawn per period for each experiment
        #[arg(long, default_value_t = 50_000)]
        samples: usize,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Closed trades a setting needs to be usable in a training window (owner: 2 is the lower limit)
        #[arg(long, default_value_t = 2)]
        min_trades: usize,
        /// Candle size the experiments run on
        #[arg(long, default_value = "1m")]
        interval: String,
        /// Run on the DEEP market (the bot's ~95 moomoo symbols, ~43 scored sessions) instead of all symbols over 20 scored sessions
        #[arg(long)]
        deep: bool,
        /// Experiments to run
        #[arg(long, default_value_t = 3)]
        rounds: usize,
    },
    /// A2.4: judge the numeric learner (gradient-boosted trees on causal features) on each period, against single features and a shuffled-target null
    Learn {
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        #[arg(long, default_value = "data/candles.sqlite3")]
        bot_snapshot: PathBuf,
        #[arg(long, default_value = "bot/data/spy_holdings.csv")]
        holdings: PathBuf,
        /// Shuffled-target control runs per period
        #[arg(long, default_value_t = 8)]
        null_runs: usize,
    },
    /// Append a line to JOURNAL.md (the shared project memory every AI reads and writes)
    Log {
        message: String,
        /// Who is writing: claude, qwen, codex, deepseek-coder-15k, owner...
        #[arg(long, default_value = "claude")]
        by: String,
        #[arg(long, default_value = "JOURNAL.md")]
        file: PathBuf,
    },
    /// Exit non-zero unless every given symbol is tradable on the live bot
    CheckPromotable {
        symbols: Vec<String>,
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
    },
    /// Apple Intelligence & Apple Silicon autonomous learning bridge: debriefs closed trades and derives neurogenic lessons
    AppleTeacher {
        #[arg(long, default_value = "papercompmoomoo/data/brain.sqlite3")]
        brain: PathBuf,
        #[arg(long, default_value = "papercompmoomoo/data/journal.sqlite3")]
        journal: PathBuf,
    },
    /// ThomasAi autonomous neural step: senses market environment, ingests test/backtest data, candle & temporal patterns, learns skills, self-prunes, and updates synaptic weights
    NeuralStep {
        #[arg(long, default_value = "papercompmoomoo/data/brain.sqlite3")]
        brain: PathBuf,
        #[arg(long, default_value = "papercompmoomoo/data/journal.sqlite3")]
        journal: PathBuf,
        #[arg(long, default_value = "data/thomas.sqlite3")]
        store: PathBuf,
        #[arg(long, default_value = "papercompmoomoo/data/common1m.sqlite3")]
        common: PathBuf,
        #[arg(long, default_value = "papercompmoomoo/data/orb.sqlite3")]
        orb_db: PathBuf,
        #[arg(long, default_value = "papercompmoomoo/data/asx_journal.sqlite3")]
        asx_journal: PathBuf,
        #[arg(long, default_value = "papercompmoomoo/data/hk_journal.sqlite3")]
        hk_journal: PathBuf,
    },
    /// Multi-Model Deliberation (Antigravity, Claude, Codex, Apple AI) -> Gemini Verdict -> Walkforward Backtest & Autonomous Learning
    Deliberate,
    /// Execute one momentum scan and rebalance cycle for ASX Standard Paper Portfolio ($5,000)
    AsxCycle {
        #[arg(long, default_value = "papercompmoomoo/data/common1m.sqlite3")]
        common: PathBuf,
        #[arg(long, default_value = "papercompmoomoo/data/asx_journal.sqlite3")]
        journal: PathBuf,
        #[arg(long, default_value_t = 5000.0)]
        capital: f64,
    },
    /// Execute one momentum scan and rebalance cycle for HK Standard Paper Portfolio ($5,000 USD / HK$39,000 HKD)
    HkCycle {
        #[arg(long, default_value = "papercompmoomoo/data/common1m.sqlite3")]
        common: PathBuf,
        #[arg(long, default_value = "papercompmoomoo/data/hk_journal.sqlite3")]
        journal: PathBuf,
        #[arg(long, default_value_t = 5000.0)]
        capital: f64,
    },
    /// Inspect all empirical learnings, council consensus, and neural synaptic weights for Thomas
    Learnings {
        #[arg(long, default_value = "papercompmoomoo/data/brain.sqlite3")]
        brain: PathBuf,
        #[arg(long, default_value = "papercompmoomoo/data/journal.sqlite3")]
        journal: PathBuf,
        #[arg(long, default_value = "papercompmoomoo/data/asx_journal.sqlite3")]
        asx_journal: PathBuf,
        #[arg(long, default_value = "papercompmoomoo/data/hk_journal.sqlite3")]
        hk_journal: PathBuf,
    },
}


/// The proto files of the pinned moomoo SDK: `tools/moomoo-venv/lib/python*/site-packages/moomoo/common/pb`.
fn sdk_proto_dir() -> anyhow::Result<PathBuf> {
    std::fs::read_dir("tools/moomoo-venv/lib")?
        .flatten()
        .map(|py| py.path().join("site-packages/moomoo/common/pb"))
        .find(|p| p.is_dir())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "the moomoo SDK is not installed in tools/moomoo-venv: run `thomas setup`"
            )
        })
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::OrbBacktest { db, paper_store, risk_pct, cost_bps } => {
            println!("{}", orb::run(&db, &paper_store, risk_pct, cost_bps)?);
        }
        Cmd::MomentumSelect {
            db,
            lookback_sessions,
            limit,
            tuned,
            store,
            universe,
            trials,
        } => {
            if tuned {
                println!(
                    "{}",
                    symbol_tune::select_next(
                        &db,
                        &universe,
                        &store,
                        trials,
                        limit,
                        10.0,
                        ops::now_ms()
                    )?
                );
            } else {
                println!(
                    "{}",
                    momentum_backtest::select(&db, lookback_sessions, limit)?
                );
            }
        }
        Cmd::MomentumBacktest {
            db,
            store,
            universe,
            track_record,
            lookback_sessions,
            slots,
            cost_bps,
            capital,
        } => {
            print!(
                "{}",
                momentum_backtest::run_and_record(
                    &db,
                    &store,
                    Some(universe.as_path()),
                    track_record,
                    lookback_sessions,
                    slots,
                    cost_bps,
                    capital,
                    ops::now_ms(),
                )?
            );
        }
        Cmd::MomentumTune {
            db,
            store,
            universe,
            trials,
            slots,
            cost_bps,
            capital,
        } => {
            print!(
                "{}",
                symbol_tune::run(
                    &db,
                    &universe,
                    &store,
                    trials,
                    slots,
                    cost_bps,
                    capital,
                    ops::now_ms()
                )?
            );
        }
        Cmd::MomentumForward {
            journal,
            db,
            store,
            symbol_map,
            cost_bps,
        } => {
            println!(
                "{}",
                momentum_forward::run(&journal, &db, &store, &symbol_map, cost_bps, ops::now_ms())?
            );
        }
        Cmd::DataCheck {
            candles: db,
            store: sdb,
        } => {
            let c = candles::load(&db)?;
            let r = candles::validate(&c);
            let conn = store::open(&sdb)?;
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
            conn.execute(
                "INSERT INTO data_checks(ts_ms, source, symbols, bars, bad_symbols, report_json) VALUES(?,?,?,?,?,?)",
                (now, db.display().to_string(), r.symbols, r.bars, r.bad_symbols.len(), serde_json::to_string(&r)?),
            )?;
            let sum = |f: fn(&candles::SymbolReport) -> usize| {
                r.per_symbol.values().map(f).sum::<usize>()
            };
            println!("symbols {}  bars {}", r.symbols, r.bars);
            println!(
                "bad_ohlc {}  non_positive {}  zero_volume {}  intraday_gaps {}",
                sum(|s| s.bad_ohlc),
                sum(|s| s.non_positive_price),
                sum(|s| s.zero_volume),
                sum(|s| s.intraday_gaps)
            );
            let steps: std::collections::BTreeSet<_> = r
                .per_symbol
                .values()
                .map(|s| s.median_step_ms / 60000)
                .collect();
            println!("median bar step (minutes) across symbols: {steps:?}");
            println!("symbols with bad price data: {:?}", r.bad_symbols);
        }
        Cmd::Reconcile {
            candles: db,
            store: sdb,
            symbols,
        } => {
            let c = candles::load(&db)?;
            let conn = store::open(&sdb)?;
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
            let s = reconcile::run(&c, &conn, now, symbols.as_deref())?;
            println!("{s:#?}");
        }
        Cmd::Symbols {
            bot_snapshot,
            holdings,
            store: sdb,
        } => {
            let conn = store::open(&sdb)?;
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
            for (k, n) in symbols::build(&conn, &bot_snapshot, &holdings, now)? {
                println!("{k:<32} {n}");
            }
        }
        Cmd::FetchExtra { store: sdb } => {
            let conn = store::open(&sdb)?;
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
            println!("{:#?}", extra::run(&conn, now)?);
        }
        Cmd::FetchSessions { store: sdb } => {
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
            println!(
                "{} sessions stored",
                extra::fetch_sessions(&store::open(&sdb)?, 1_767_312_000, now)?
            );
        }
        Cmd::Snapshot { from, to } => {
            merge::snapshot(
                &from.unwrap_or_else(moomoo_bot::platform::bot_store_path),
                &to,
            )?;
            println!("snapshot written to {} (integrity ok)", to.display());
        }
        Cmd::Merge {
            store: sdb,
            bot_snapshot,
        } => {
            println!("{:#?}", merge::run(&store::open(&sdb)?, &bot_snapshot)?);
        }
        Cmd::ExportMap { store: sdb, out } => {
            let n = export::symbol_map_csv(&store::open(&sdb)?, &out)?;
            println!("wrote {n} rows to {}", out.display());
        }
        Cmd::Verify {
            db,
            symbol_map,
            lock,
        } => {
            let checks = momentum_backtest::verify(&db, &symbol_map, &lock)?;
            let mut failed = 0;
            for c in &checks {
                if c.failures.is_empty() {
                    println!("PASS  {}", c.name);
                } else {
                    failed += 1;
                    println!(
                        "FAIL  {}: {} (e.g. {:?})",
                        c.name,
                        c.failures.len(),
                        &c.failures[..c.failures.len().min(5)]
                    );
                }
            }
            if failed > 0 {
                std::process::exit(2);
            }
        }
        Cmd::ProtoDiff { old, new, report } => {
            let r = protodiff::compare_dirs(&old, &new.map_or_else(sdk_proto_dir, Ok)?)?;
            println!(
                "shared {}  identical {}  additive-only {}  breaking {}  only in SDK {}  only in bot {}",
                r.shared,
                r.identical,
                r.additive.len(),
                r.breaking.len(),
                r.only_new.len(),
                r.only_old.len()
            );
            let mut md = String::from(
                "# Proto diff: bot proto/ (old) -> moomoo SDK 10.11.7108 (new)\nGenerated by `thomas proto-diff`. Breaking = a field removed, retyped or renumbered.\n\n",
            );
            md += &format!(
                "shared {} | identical {} | additive-only {} | breaking {} | only in SDK {} | only in bot {}\n\n",
                r.shared,
                r.identical,
                r.additive.len(),
                r.breaking.len(),
                r.only_new.len(),
                r.only_old.len()
            );
            for (title, list) in [
                ("Breaking", &r.breaking),
                ("Additive or renamed only", &r.additive),
            ] {
                md += &format!("## {title}\n");
                for (f, d) in list.iter() {
                    md += &format!("### {f}\n");
                    for x in &d.removed {
                        md += &format!("- removed `{x}`\n");
                    }
                    for (a, b) in &d.renamed {
                        md += &format!("- renamed `{a}` -> `{b}` (same number and type)\n");
                    }
                    for (k, a, b) in &d.changed {
                        md += &format!("- changed `{k}`: `{a}` -> `{b}`\n");
                    }
                    for x in &d.added {
                        md += &format!("- added `{x}`\n");
                    }
                }
            }
            md += &format!(
                "\n## Only in the SDK ({})\n{}\n",
                r.only_new.len(),
                r.only_new.join(", ")
            );
            std::fs::write(&report, md)?;
            println!("report: {}", report.display());
        }
        Cmd::Hook { name } => match name.as_str() {
            "pre-commit" => {
                let out = std::process::Command::new("git")
                    .args(["diff", "--cached", "--name-only"])
                    .output()?;
                let changed: Vec<String> = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(String::from)
                    .collect();
                // A commit that changes code must not break the fast unit tests (the slow real-data tests are run by hand).
                if changed.iter().any(|f| f.starts_with("crates/")) {
                    let ok = std::process::Command::new("cargo")
                        .args(["test", "--workspace", "--release", "--lib", "--quiet"])
                        .stdout(std::process::Stdio::null())
                        .status()?
                        .success();
                    if !ok {
                        eprintln!(
                            "BLOCKED: the unit tests fail (cargo test --workspace --release --lib). Fix them before committing."
                        );
                        std::process::exit(1);
                    }
                }
                // Project memory must move with the work so another AI can resume.
                let touches_work = changed.iter().any(|f| {
                    f.starts_with("crates/")
                        || f == "data/symbol_map.csv"
                        || f == "PLAN.md"
                        || f == "Cargo.toml"
                        || f == "versions.lock"
                });
                let has = |n: &str| changed.iter().any(|f| f == n);
                if touches_work && !(has("JOURNAL.md") && has("TASKS.md")) {
                    eprintln!(
                        "BLOCKED: update the project memory so another AI can resume: target/release/thomas log \"<what changed and why>\" and edit TASKS.md (next step), then git add JOURNAL.md TASKS.md."
                    );
                    std::process::exit(1);
                }
            }
            "post-commit" => {
                if !std::process::Command::new("git")
                    .args(["push", "-q", "origin", "HEAD"])
                    .status()?
                    .success()
                {
                    eprintln!("WARNING: push to origin failed");
                }
            }
            other => anyhow::bail!("unknown hook '{other}'"),
        },
        Cmd::Search {
            store: sdb,
            bot_snapshot,
            holdings,
            samples,
            seed,
            min_trades,
        } => {
            use thomas_engine::search;
            let search::Prepared {
                loaded: l,
                built: b,
                scores: sc,
                first_scored: first,
                ..
            } = search::prepare(&bot_snapshot, &sdb, &holdings)?;
            let scored = sc.iter().filter(|r| r.iter().any(|x| x.is_some())).count();
            println!(
                "market {} symbols, {} candles; search span = candles {first}..{} ({} sessions); {scored} symbols scored at least once (all allowed in)",
                b.market.symbols.len(),
                b.market.ts.len(),
                b.market.ts.len(),
                (b.market.ts.len() - first) / 2
            );
            let floor = min_trades.unwrap_or(thomas_core::config::MIN_TRADES_SEARCH);
            let cfg = search::Settings {
                first_scored: first,
                candles_per_session: search::ENGINE_INTERVAL.per_session(),
                interval: search::ENGINE_INTERVAL,
                equity: moomoo_bot::engine::START_CAPITAL,
                samples,
                min_trades: floor,
                seed,
            };
            let conn = store::open(&sdb)?;
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
            let rep = search::run_periods(&conn, &b, &sc, &l.calendar, cfg, now)?;
            println!(
                "{} overlapping 14-session periods in the window (a new one every 3 sessions); {} searched now, {} already stored",
                rep.periods.len(),
                rep.new_periods(),
                rep.periods.len() - rep.new_periods()
            );
            for (p, spy) in rep.periods.iter().zip(&rep.reference_return_pct) {
                let tag = if p.new {
                    format!("searched: {} draws in {:.1} s", p.evaluated, p.seconds)
                } else {
                    "stored earlier".to_string()
                };
                println!(
                    "  period {:?} [{tag}] usable {} (floor {floor} trades) | own winner robust {} vs best single {} | SPY {}",
                    p.range,
                    p.usable,
                    p.robust_score.map_or("none".into(), |x| format!("{x:.2}")),
                    p.raw_best_score
                        .map_or("none".into(), |x| format!("{x:.2}")),
                    spy.map_or("n/a".into(), |x| format!("{x:.2}%")),
                );
            }
            let n = rep.periods.len();
            let all = rep
                .candidates
                .iter()
                .filter(|c| c.periods_usable() == n)
                .count();
            println!(
                "{} candidates re-run on all {n} periods; usable in every period: {all}",
                rep.candidates.len()
            );
            let show = |label: &str, c: &search::Candidate| {
                println!(
                    "{label}: usable in {}/{n} | worst-period objective {:.2}",
                    c.periods_usable(),
                    c.worst_objective()
                );
                for (k, m) in c.per_period.iter().enumerate() {
                    println!(
                        "    period {}: return {:.2}%  drawdown {:.2}%  trades {}  wins {}  {}",
                        k + 1,
                        m.return_pct,
                        m.max_drawdown_pct,
                        m.trades,
                        m.wins,
                        if m.usable { "usable" } else { "not usable" }
                    );
                }
            };
            match rep.best() {
                Some(c) => {
                    show(
                        &format!(
                            "PREFERRED (usable in every period at a floor of {floor} trades: LOW CONFIDENCE, IN-SAMPLE and overlapping, not evidence of an edge)"
                        ),
                        c,
                    );
                    println!("params: {}", serde_json::to_string(&c.params)?);
                    let cx = thomas_engine::guards::Context {
                        market: &b,
                        scores: &sc,
                        periods: &rep.periods,
                        reference: &rep.reference_return_pct,
                        forward: &rep.forward,
                        equity: cfg.equity,
                        min_trades: cfg.min_trades,
                        seed: cfg.seed,
                    };
                    let g = thomas_engine::guards::evaluate(&cx, c);
                    println!("OVERFIT GUARDS:");
                    for (i, (rate, ok)) in g.win_rate.iter().enumerate() {
                        println!(
                            "  period {}: win rate {:.0}% ({}) | beats SPY: {} | objective is {} of the best single sampled",
                            i + 1,
                            rate * 100.0,
                            if *ok {
                                "meets the bot's 60% rule"
                            } else {
                                "BELOW the bot's 60% rule"
                            },
                            g.beats_reference[i].map_or("n/a".into(), |x| x.to_string()),
                            g.share_of_best[i]
                                .map_or("n/a".into(), |x| format!("{:.0}%", x * 100.0))
                        );
                    }
                    println!(
                        "  stability: {} of {} perturbed copies (+/-15% on decimal values) stay usable in every period ({:.0}%)",
                        g.perturbed_usable_in_all,
                        g.perturbed,
                        g.stable_share() * 100.0
                    );
                    println!(
                        "  forward tests for this setting: {} ({} positive)",
                        g.forward_tests, g.forward_positive
                    );
                    let f = g.failures();
                    if f.is_empty() {
                        println!("  VERDICT: credible");
                    } else {
                        println!("  VERDICT: UNPROVEN. {}", f.join("; "));
                    }
                    let out =
                        thomas_engine::registry::update(&conn, &c.params, &g, &rep.forward, now)?;
                    println!("REGISTRY: {} is {} ({})", out.name, out.status, out.reason);
                    for line in thomas_engine::registry::show(&conn, &rep.forward)? {
                        println!("  {line}");
                    }
                }
                None => {
                    println!("RESULT: no setting held up in every period. Closest:");
                    for c in rep.candidates.iter().take(2) {
                        show("  candidate", c);
                    }
                }
            }
            if rep.forward.is_empty() {
                println!(
                    "FORWARD (out-of-sample): none yet. A stored winner is tested on a later period that starts after its own ended; with 14-session periods every 3 sessions that needs 15 more sessions after a period's start."
                );
            } else {
                let n = rep.forward.len();
                let pos = rep
                    .forward
                    .iter()
                    .filter(|f| f.metrics.return_pct > 0.0)
                    .count();
                let mean = rep
                    .forward
                    .iter()
                    .map(|f| f.metrics.return_pct)
                    .sum::<f64>()
                    / n as f64;
                println!(
                    "FORWARD (out-of-sample): {n} tests of stored winners on later periods; {pos} positive ({:.0}%); mean return {mean:.2}%",
                    100.0 * pos as f64 / n as f64
                );
            }
            let sid = search::record(&conn, &b, &rep, cfg, now)?;
            println!("stored as strategy id {sid}");
        }
        Cmd::Walkforward {
            store: sdb,
            bot_snapshot,
            holdings,
            samples,
            seed,
            layouts,
            intervals,
            hold_minutes,
            min_trades,
            top,
            deep,
            universe,
            trend,
        } => {
            use thomas_engine::{search, universe::Pick, walkfwd};
            let conn = store::open(&sdb)?;
            let run_ms = ops::now_ms();
            for name in intervals.split(',').map(str::trim) {
                let interval = moomoo_bot::engine::Interval::ALL
                    .iter()
                    .copied()
                    .find(|i| i.label() == name)
                    .ok_or_else(|| anyhow::anyhow!("unknown candle size '{name}'"))?;
                let search::Prepared {
                    built: b,
                    scores: sc,
                    first_scored: first,
                    per_session: cps,
                    ..
                } = if deep {
                    search::prepare_deep(&bot_snapshot, &sdb, &holdings, interval, 60)?
                } else {
                    search::prepare_for(&bot_snapshot, &sdb, &holdings, interval)?
                };
                for layout in layouts.split(',') {
                    let (a, c) = layout
                        .split_once('x')
                        .ok_or_else(|| anyhow::anyhow!("layout '{layout}' is not like 8x3"))?;
                    let (min_train, block): (usize, usize) = (a.trim().parse()?, c.trim().parse()?);
                    let folds = walkfwd::folds(first, b.market.ts.len(), cps, min_train, block);
                    if folds.is_empty() {
                        println!("{name} {layout}: the scored span is too short for one fold");
                        continue;
                    }
                    let mut seen = std::collections::BTreeSet::new();
                    for hold in hold_minutes.split(',').map(str::trim).map(|h| {
                        if h == "live" {
                            Ok(None)
                        } else {
                            h.parse::<usize>().map(Some)
                        }
                    }) {
                        let hold = hold?;
                        // on long candles several hour limits mean the same single candle: run it once
                        if !seen.insert(hold.map(|h| (h / (interval.bars() * 15)).max(1))) {
                            continue;
                        }
                        for tf in trend.split(',').map(str::trim) {
                            let space = walkfwd::space_for(
                                interval,
                                (samples * 2 / cps).max(2000),
                                min_trades,
                                hold,
                                top,
                                tf == "on",
                            );
                            // what the improvement loop currently confirms is part of the bot's setting in every test of the bot
                            let options = thomas_engine::improve::in_force(&conn)?;
                            let space = thomas_engine::improve::stack(space, &options)?;
                            for upick in universe.split(',').map(str::trim).filter(|u| *u != "all")
                            {
                                let pick = Pick::parse(upick)?;
                                let (results, sizes) = walkfwd::run_universe(
                                    &b,
                                    &folds,
                                    space,
                                    moomoo_bot::engine::START_CAPITAL,
                                    seed,
                                    pick,
                                )?;
                                let label = format!(
                                    "{name} hold{} universe={} trend={tf} min_trades={min_trades} {layout}{}",
                                    hold.map_or("live".to_string(), |h| format!("<={h}min")),
                                    pick.name(),
                                    if options.is_empty() {
                                        String::new()
                                    } else {
                                        format!(" +{}", options.join("+"))
                                    }
                                );
                                walkfwd::record(
                                    &conn, run_ms, &label, &b, &results, samples, min_trades, seed,
                                )?;
                                let oos = walkfwd::stitched(results.iter().map(|r| r.return_pct()));
                                let spy = walkfwd::stitched(
                                    results.iter().filter_map(|r| r.reference_return_pct),
                                );
                                let trades: usize = results
                                    .iter()
                                    .map(|r| r.test_metrics.map_or(0, |m| m.trades))
                                    .sum();
                                let wins: usize = results
                                    .iter()
                                    .map(|r| r.test_metrics.map_or(0, |m| m.wins))
                                    .sum();
                                let positive =
                                    results.iter().filter(|r| r.return_pct() > 0.0).count();
                                println!(
                                    "WALK-FORWARD {label}\n  universe {} symbols (per fold {:?}): out-of-sample stitched {oos:+.2}% vs SPY {spy:+.2}% | {positive} of {} folds positive | {trades} trades, {wins} wins",
                                    pick.name(),
                                    sizes,
                                    results.len()
                                );
                            }
                            if !universe.split(',').map(str::trim).any(|u| u == "all") {
                                continue;
                            }
                            let results = walkfwd::run(
                                &b,
                                &sc,
                                &folds,
                                space,
                                moomoo_bot::engine::START_CAPITAL,
                                seed,
                            )?;
                            let label = format!(
                                "{name} hold{} top<={top} trend={tf} min_trades={min_trades} {layout}",
                                hold.map_or("live".to_string(), |h| format!("<={h}min"))
                            );
                            walkfwd::record(
                                &conn, run_ms, &label, &b, &results, samples, min_trades, seed,
                            )?;
                            println!(
                                "WALK-FORWARD {label} ({} folds, each setting chosen from candles before its block only)",
                                folds.len()
                            );
                            for rule in [
                                walkfwd::Rule::Robust,
                                walkfwd::Rule::StableHalves,
                                walkfwd::Rule::Rolling,
                            ] {
                                let rs: Vec<_> =
                                    results.iter().filter(|r| r.rule == rule).collect();
                                let oos = walkfwd::stitched(rs.iter().map(|r| r.return_pct()));
                                let spy = walkfwd::stitched(
                                    rs.iter().filter_map(|r| r.reference_return_pct),
                                );
                                let positive = rs.iter().filter(|r| r.return_pct() > 0.0).count();
                                let trades: usize = rs
                                    .iter()
                                    .map(|r| r.test_metrics.map_or(0, |m| m.trades))
                                    .sum();
                                let wins: usize = rs
                                    .iter()
                                    .map(|r| r.test_metrics.map_or(0, |m| m.wins))
                                    .sum();
                                println!(
                                    "  {:<13} out-of-sample stitched {oos:+.2}% vs SPY {spy:+.2}% | {positive} of {} folds positive | {trades} trades, {wins} wins",
                                    rule.name(),
                                    rs.len()
                                );
                            }
                        }
                    }
                }
            }
        }
        Cmd::Select {
            store: sdb,
            bot_snapshot,
            holdings,
            out,
            force,
        } => {
            use thomas_engine::search;
            let conn = store::open(&sdb)?;
            let now = ops::now_ms();
            if !force && !select::due(&conn, now) {
                println!("a pick was already made in the last 7 days; use --force to make another");
                return Ok(());
            }
            let search::Prepared {
                built: b,
                per_session,
                ..
            } = search::prepare(&bot_snapshot, &sdb, &holdings)?;
            let tradable: std::collections::BTreeSet<String> = conn
                .prepare("SELECT moomoo_symbol FROM symbols WHERE bot_status='tradable'")?
                .query_map([], |r| r.get::<_, String>(0))?
                .filter_map(Result::ok)
                .collect();
            let pick = select::build(&b, per_session, &tradable, now);
            anyhow::ensure!(
                pick.symbols.len() >= select::PICK,
                "only {} tradable symbols could be ranked",
                pick.symbols.len()
            );
            select::save(&conn, &out, &pick)?;
            println!(
                "{}: {} symbols ranked ({}); the first {} are this week's pick; written to {}",
                pick.week,
                pick.symbols.len(),
                pick.rule,
                pick.pick,
                out.display()
            );
            println!(
                "first ten: {}",
                pick.symbols
                    .iter()
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Cmd::FetchPaper { store: sdb, days } => {
            let conn = store::open(&sdb)?;
            let bars = paper_cmd::fetch(&conn, days, ops::now_ms())?;
            println!("{bars} closed 1-minute bars stored for the paper universe");
        }
        Cmd::Paper {
            store: sdb,
            bot_snapshot,
            holdings,
            samples,
        } => {
            let conn = store::open(&sdb)?;
            println!(
                "{}",
                paper_cmd::run(
                    &conn,
                    &bot_snapshot,
                    &sdb,
                    &holdings,
                    samples,
                    ops::now_ms()
                )?
            );
        }
        Cmd::PaperRun {
            store: sdb,
            bot_snapshot,
            holdings,
            force,
        } => {
            let now = ops::now_ms();
            if !force && !ops::in_trading_window(now) {
                println!("outside US trading hours: nothing to do");
                return Ok(());
            }
            let _lock = ops::Lock::take(std::path::Path::new("data/paper.lock"))?;
            let conn = store::open(&sdb)?;
            // no Yahoo request while downloads are off: the account steps on whatever candles are already stored
            let bars = if thomas_core::config::YAHOO_DOWNLOADS {
                paper_cmd::fetch(&conn, 3, now)?
            } else {
                let db = std::path::Path::new(thomas_core::ingest::EXTERNAL_DB);
                if db.exists() {
                    let n = thomas_core::ingest::import_store_mapped(
                        &conn,
                        db,
                        std::path::Path::new(thomas_core::ingest::SYMBOL_MAP),
                        now,
                    )?
                    .bars;
                    thomas_core::ingest::derive_sessions(&conn, now)?;
                    n
                } else {
                    0
                }
            };
            thomas_core::merge::run(&conn, &bot_snapshot)?;
            println!(
                "{bars} bars imported; {}",
                paper_cmd::run(
                    &conn,
                    &bot_snapshot,
                    &sdb,
                    &holdings,
                    200_000,
                    ops::now_ms()
                )?
            );
        }
        Cmd::PaperPause {
            hours,
            reason,
            store: sdb,
        } => {
            let conn = store::open(&sdb)?;
            if let Some(acc) = thomas_engine::paper::load(&conn)? {
                anyhow::ensure!(
                    acc.portfolio.positions.is_empty(),
                    "the paper account holds {} position(s): nothing would manage them during a pause",
                    acc.portfolio.positions.len()
                );
            }
            let now = ops::now_ms();
            let p = paper_cmd::Pause {
                from_ms: now,
                until_ms: now + hours * 3_600_000,
                reason,
            };
            paper_cmd::write_pause(std::path::Path::new("."), &p)?;
            println!(
                "paper trader paused from {} until {} UTC",
                ops::utc(p.from_ms),
                ops::utc(p.until_ms)
            );
        }
        Cmd::PaperResume => {
            paper_cmd::clear_pause(std::path::Path::new("."));
            println!("paper trader pause ended");
        }
        Cmd::Watchdog => {
            let root = std::env::current_dir()?;
            let now = ops::now_ms();
            let actions = watchdog::run(&root, now);
            watchdog::record(&root, now, &actions)?;
            if !actions.is_empty() {
                msgboard::say(
                    &root,
                    now,
                    "watchdog",
                    "commander",
                    "watchdog",
                    "alert",
                    &format!("Watchdog acted: {}", actions.join("; ")),
                    "O3",
                );
            }
            println!(
                "watchdog: {} action(s){}",
                actions.len(),
                actions
                    .iter()
                    .map(|a| format!("\n  {a}"))
                    .collect::<String>()
            );
        }
        Cmd::ImportBars {
            store: sdb,
            db,
            symbol_map,
        } => {
            if !db.exists() {
                println!(
                    "import-bars: {} does not exist yet, nothing to import",
                    db.display()
                );
                return Ok(());
            }
            let conn = store::open(&sdb)?;
            let s =
                thomas_core::ingest::import_store_mapped(&conn, &db, &symbol_map, ops::now_ms())?;
            let sessions = thomas_core::ingest::derive_sessions(&conn, ops::now_ms())?;
            println!(
                "import-bars: {} symbols with new bars, {} bars stored, {} tickers not in our map, {} rows refused, {} new trading session(s) from SPY's bars",
                s.symbols, s.bars, s.unknown_tickers, s.rejected_rows, sessions
            );
        }
        Cmd::Bundle { publish } => {
            let repo = std::env::current_dir()?;
            let stamp = ops::utc(ops::now_ms()).replace([' ', ':'], "-");
            let tar = portable::bundle(
                &repo,
                &repo.join("data/bundles"),
                &stamp,
                &portable::bot_archive(),
            )?;
            println!(
                "bundle written: {} ({} MB)",
                tar.display(),
                std::fs::metadata(&tar)?.len() / 1_000_000
            );
            if publish {
                println!("{}", portable::publish(&repo, &tar, &stamp)?);
            }
        }
        Cmd::Restore { file, force } => {
            let repo = std::env::current_dir()?;
            let tar = match file {
                Some(f) => f,
                None => portable::download_latest(&repo, &repo.join("data/bundles"))?,
            };
            let placed = portable::restore(&repo, &tar, &portable::bot_archive(), force)?;
            println!("restored {} file(s) from {}", placed.len(), tar.display());
        }
        Cmd::Setup { bundle, dry_run } => {
            let failed = portable::run_steps(
                &portable::setup_steps(&std::env::current_dir()?, bundle),
                dry_run,
            );
            anyhow::ensure!(
                failed == 0,
                "{failed} setup step(s) failed: fix the cause and run `thomas setup` again (finished steps are skipped)"
            );
        }
        Cmd::Portal {
            store,
            archive,
            port,
        } => {
            portal::serve(&store, &archive, &std::env::current_dir()?, port)?;
        }
        Cmd::Msg { cmd } => {
            let root = std::env::current_dir()?;
            let c = msgboard::open(&root)?;
            match cmd {
                MsgCmd::Post {
                    from,
                    to,
                    topic,
                    kind,
                    goal,
                    reply_to,
                    text,
                } => {
                    let id = msgboard::post(
                        &c,
                        ops::now_ms(),
                        &from,
                        &to,
                        &topic,
                        &kind,
                        &text,
                        reply_to,
                        &goal,
                        "claude",
                    )?;
                    println!("posted #{id}");
                }
                MsgCmd::Read { r#as, all, limit } => {
                    let ms = msgboard::inbox(&c, &r#as, limit, !all)?;
                    for m in &ms {
                        println!("{}", msgboard::line(m));
                    }
                    if ms.is_empty() {
                        println!("(nothing new for {})", r#as);
                    }
                    msgboard::mark_read(
                        &c,
                        ops::now_ms(),
                        &r#as,
                        &ms.iter().map(|m| m.id).collect::<Vec<_>>(),
                    )?;
                }
                MsgCmd::List { limit } => {
                    for m in msgboard::recent(&c, limit)? {
                        println!("{}", msgboard::line(&m));
                    }
                }
            }
        }
        Cmd::Validation {
            database,
            id,
            status,
        } => {
            let c = rusqlite::Connection::open(&database)?;
            validation::set_rust_status(&c, id, &status)?;
            println!("validation #{id}: Rust status {status}");
        }
        Cmd::ModelGate {
            candidate,
            database,
            validation_id,
        } => {
            let root = std::env::current_dir()?;
            println!(
                "{}",
                model_gate::check(&root, &candidate, &database, validation_id)?
            );
        }
        Cmd::ModelEvaluate { decisions, output } => {
            let root = std::env::current_dir()?;
            let command = std::env::args().collect::<Vec<_>>().join(" ");
            println!(
                "{}",
                model_eval::run(&root, &decisions, &output, ops::now_ms(), &command)?
            );
        }
        Cmd::ModelEvents { output } => {
            let root = std::env::current_dir()?;
            println!("{}", model_eval::events(&root, &output)?);
        }
        Cmd::ModelDecisionCorpus { output } => {
            let root = std::env::current_dir()?;
            println!("{}", model_eval::decision_corpus(&root, &output)?);
        }
        Cmd::ModelShadowCorpus { output } => {
            let root = std::env::current_dir()?;
            println!("{}", model_eval::shadow_corpus(&root, &output)?);
        }
        Cmd::ModelDistill { traces, output } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&model_distill::run(&traces, &output)?)?
            );
        }
        Cmd::Objectives { store: sdb } => {
            let conn = store::open(&sdb)?;
            let root = std::env::current_dir()?;
            let journal = rusqlite::Connection::open_with_flags(
                root.join("papercompmoomoo/data/journal.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .ok();
            print!(
                "{}",
                objectives::text(&objectives::score(
                    &conn,
                    journal.as_ref(),
                    &root,
                    ops::now_ms()
                ))
            );
        }
        Cmd::Headroom { ram_gb, disk_gb } => {
            let h = headroom::measure()?;
            println!(
                "free now: RAM {:.0}%, CPU idle {:.0}%, disk {:.0}%",
                h.ram_free_percent, h.cpu_idle_percent, h.disk_free_percent
            );
            headroom::allow(&h, ram_gb, disk_gb)?;
            println!(
                "ok: the job leaves at least {}% free",
                headroom::MIN_FREE_PERCENT
            );
        }
        Cmd::Cycle {
            store,
            rounds,
            no_refresh,
        } => {
            let _lock = ops::Lock::take(std::path::Path::new("data/cycle.lock"))?;
            let conn = store::open(&store)?;
            let exe = std::env::current_exe()?;
            let failed = ops::cycle(
                &conn,
                &exe,
                &ops::plan(&conn, ops::now_ms(), !no_refresh, rounds),
            )?;
            let text = ops::report(&conn);
            std::fs::create_dir_all("data/reports")?;
            std::fs::write(
                format!("data/reports/{}.md", &ops::utc(ops::now_ms())[..10]),
                &text,
            )?;
            if !failed.is_empty() {
                msgboard::say(
                    &std::env::current_dir()?,
                    ops::now_ms(),
                    "cycle",
                    "commander",
                    "cycle",
                    "alert",
                    &format!(
                        "The research cycle finished with failed steps: {}.",
                        failed.join(", ")
                    ),
                    "O3",
                );
                anyhow::bail!("failed steps: {}", failed.join(", "));
            }
        }
        Cmd::Report { store } => {
            let text = ops::report(&store::open(&store)?);
            std::fs::create_dir_all("data/reports")?;
            std::fs::write(
                format!("data/reports/{}.md", &ops::utc(ops::now_ms())[..10]),
                &text,
            )?;
            print!("{text}");
        }
        Cmd::Doctor { store } => {
            let dirty = std::process::Command::new("git")
                .args(["status", "--porcelain"])
                .output()
                .map_or(0, |o| String::from_utf8_lossy(&o.stdout).lines().count());
            let mut problems = ops::doctor(&store::open(&store)?, ops::now_ms(), dirty);
            problems.extend(deploy::problems(&std::env::current_dir()?));
            if problems.is_empty() {
                println!("healthy: no problems found");
            }
            for p in &problems {
                println!("{}: {}", if p.error { "ERROR" } else { "warning" }, p.text);
            }
            if problems.iter().any(|p| p.error) {
                std::process::exit(1);
            }
        }
        Cmd::Schedule { action, interval } => match action.as_str() {
            "install" => {
                let repo = std::env::current_dir()?;
                ops::install(&std::env::current_exe()?.canonicalize()?, &repo, interval)?;
                println!(
                    "installed {} (every {interval} s, nice 19; log: data/cycle.log)",
                    ops::LABEL
                );
            }
            "remove" => {
                ops::remove()?;
                println!("removed {}", ops::LABEL);
            }
            "status" => {
                for (label, on) in [
                    (ops::LABEL, ops::installed()),
                    (ops::PORTAL_LABEL, ops::agent_installed(ops::PORTAL_LABEL)),
                ] {
                    println!(
                        "{label} is {}",
                        if on { "installed" } else { "not installed" }
                    );
                }
            }
            other => anyhow::bail!("unknown action '{other}': use install, remove or status"),
        },
        Cmd::Improve {
            store,
            bot_snapshot,
            holdings,
            samples,
            seed,
            min_trades,
            interval,
            deep,
            rounds,
        } => {
            improve_cmd::run(
                &store,
                &bot_snapshot,
                &holdings,
                samples,
                seed,
                min_trades,
                &interval,
                deep,
                rounds,
            )?;
        }
        Cmd::Learn {
            store: sdb,
            bot_snapshot,
            holdings,
            null_runs,
        } => {
            use thomas_engine::{learner, search};
            let search::Prepared {
                loaded: l,
                built: b,
                scores: _,
                first_scored: first,
                ..
            } = search::prepare(&bot_snapshot, &sdb, &holdings)?;
            let rows = learner::build_rows(&b);
            println!(
                "{} rows ({} features, {} symbols x candles, target = next session's return relative to the market)",
                rows.x.len(),
                learner::FEATURES.len(),
                b.market.symbols.len()
            );
            let conn = store::open(&sdb)?;
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
            let ps = learner::periods(&b, &l.calendar, first, 2);
            let gb = learner::gbdt_trainer(thomas_engine::gbdt::Params::default());
            // the bot's own small neural network, judged by exactly the same test
            let mlp = learner::mlp_trainer(7, 16, 20, 0.01);
            // the same network with its settings chosen from an enumerated grid on held-back training rows
            let tuned = learner::mlp_tuned_trainer(7);
            let mut signals = 0;
            let models: [(&'static str, learner::Trainer); 3] =
                [("gbdt", &gb), ("mlp", &mlp), ("mlp-tuned", &tuned)];
            for (name, trainer) in models {
                let mut n = 0;
                for (start_ms, range) in &ps {
                    match learner::evaluate_with(&rows, range.clone(), name, trainer, null_runs)? {
                        None => {
                            println!("{name} period {range:?}: not enough earlier data to train")
                        }
                        Some(r) => {
                            learner::record(&conn, *start_ms, &r, now)?;
                            n += usize::from(r.has_signal());
                            println!(
                                "{name} period {range:?}: trained on {} rows, tested on {} candles | IC {:+.3} (t {:+.1}) | top-minus-bottom decile {:+.2}% | best single feature {} IC {:+.3} | shuffled-target null {:+.3} +/- {:.3} | {}",
                                r.train_rows,
                                r.test_candles,
                                r.ic_mean,
                                r.ic_t,
                                r.spread_pct,
                                r.best_baseline.0,
                                r.best_baseline.1,
                                r.null_mean,
                                r.null_sd,
                                if r.has_signal() {
                                    "SIGNAL"
                                } else {
                                    "no signal"
                                }
                            );
                        }
                    }
                }
                println!("{name}: {n} of {} periods show a signal", ps.len());
                signals += n;
            }
            println!(
                "VERDICT: {signals} of {} model-periods show a signal that beats the best single feature and the null. {}",
                ps.len() * models.len(),
                if signals == ps.len() * models.len() && !ps.is_empty() {
                    "Consistent in every period; still in-sample and overlapping, so it needs forward confirmation."
                } else {
                    "No evidence that the learner finds anything the simple features do not."
                }
            );
        }
        Cmd::Log { message, by, file } => {
            use std::io::Write;
            let stamp = std::process::Command::new("date")
                .args(["-u", "+%Y-%m-%d %H:%M UTC"])
                .output()?;
            let line = format!(
                "- {} [{}] {}\n",
                String::from_utf8_lossy(&stamp.stdout).trim(),
                by,
                message.replace('\n', " ")
            );
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file)?
                .write_all(line.as_bytes())?;
            print!("{line}");
        }
        Cmd::CheckPromotable {
            symbols: wanted,
            store: sdb,
        } => {
            let bad = symbols::check_promotable(&store::open(&sdb)?, &wanted)?;
            if bad.is_empty() {
                println!("ok: all {} symbols tradable", wanted.len());
            } else {
                for (t, s) in &bad {
                    println!("BLOCKED {t}: {s}");
                }
                std::process::exit(2);
            }
        }
        Cmd::AppleTeacher { brain, journal } => {
            let now = ops::now_ms();
            if !brain.exists() {
                println!("brain {} does not exist yet", brain.display());
                return Ok(());
            }
            if !journal.exists() {
                println!("journal {} does not exist yet", journal.display());
                return Ok(());
            }
            let bconn = rusqlite::Connection::open(&brain)?;
            let jconn = rusqlite::Connection::open_with_flags(
                &journal,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            let report = thomas_engine::apple_teacher::run_apple_teacher_cycle(&bconn, &jconn, now)?;
            println!(
                "apple-teacher: ok; {} trades evaluated, {} new neurogenic lessons generated, {} total Apple memories in bank (accelerator: {})",
                report.trades_evaluated, report.new_lessons_created, report.total_apple_memories, report.hardware_accelerator
            );
        }
        Cmd::NeuralStep { brain, journal, store, common, orb_db, asx_journal, hk_journal } => {
            let now = ops::now_ms();
            if !brain.exists() {
                println!("brain {} does not exist yet", brain.display());
                return Ok(());
            }
            let bconn = rusqlite::Connection::open(&brain)?;
            bconn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=15000;")?;
            let tx_started = bconn.execute("BEGIN IMMEDIATE", []).is_ok();
            thomas_engine::neural::seed_neural_topology(&bconn, now)?;

            // Open store DB if available
            let sconn = if store.exists() {
                rusqlite::Connection::open_with_flags(
                    &store,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                ).ok()
            } else {
                None
            };

            // Open common candles DB if available
            let cconn = if common.exists() {
                rusqlite::Connection::open_with_flags(
                    &common,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                ).ok()
            } else {
                None
            };
            let orb_conn = if orb_db.exists() {
                rusqlite::Connection::open_with_flags(&orb_db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()
            } else { None };

            // Open journals
            let jconn = if journal.exists() {
                rusqlite::Connection::open_with_flags(
                    &journal,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                ).ok()
            } else {
                None
            };

            let _ = (&asx_journal, &hk_journal);

            // Run Apple Intelligence teacher evaluation if journal is present
            if let Some(ref jc) = jconn {
                let _ = thomas_engine::apple_teacher::run_apple_teacher_cycle(&bconn, jc, now);
            }

            // Execute full biological cognitive research & learning cycle on US NASDAQ:
            let report = thomas_engine::cognitive_researcher::run_full_cognitive_cycle_with_journals(
                &bconn,
                sconn.as_ref(),
                cconn.as_ref(),
                jconn.as_ref(),
                orb_conn.as_ref(),
                None,
                None,
                now,
            )?;

            // Also read all live closed trades from US NASDAQ journal to apply magnitude-scaled Hebbian causal reinforcement
            let mut live_trades_processed = 0;
            if let Some(ref jc) = jconn {
                if let Ok(mut stmt) = jc.prepare(
                    "SELECT id, symbol, pnl_pct, closed_ms FROM trades WHERE closed_ms IS NOT NULL ORDER BY id ASC"
                ) {
                    let rows = stmt.query_map([], |r| Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, f64>(2)?,
                        r.get::<_, i64>(3)?,
                    )))?;
                    for r in rows.flatten() {
                        let (_id, sym, pnl, _closed_ms) = r;
                        let success = pnl > 0.0;
                        let magnitude = 1.0 + (pnl.abs() * 0.5).min(2.0);

                        // 1. Causal outcome reinforcement: Rank 1 Alpha Execution -> Capital Growth or Floor
                        let _ = thomas_engine::neural::hebbian_update_scaled(
                            &bconn,
                            "node:decision:rank1_execute",
                            if success { "node:outcome:capital_growth" } else { "node:reflex:break_even_floor" },
                            magnitude,
                            success,
                            now,
                        );

                        // 2. Sensory symbol association: Symbol -> Rank 1 Execution
                        let clean_sym = sym.replace('.', "_").replace('-', "_");
                        let sym_node = format!("node:symbol:{clean_sym}");
                        let _ = thomas_engine::neural::hebbian_update_scaled(
                            &bconn,
                            &sym_node,
                            "node:decision:rank1_execute",
                            magnitude,
                            success,
                            now,
                        );

                        // 3. Tri-Tier Risk Profiles & Adversarial Sentry tuning
                        if pnl > 0.30 {
                            let _ = thomas_engine::neural::hebbian_update_scaled(
                                &bconn,
                                "node:decision:rank1_execute",
                                "node:reflex:risk_aggressive_expansion",
                                magnitude,
                                true,
                                now,
                            );
                        } else if pnl > 0.0 {
                            let _ = thomas_engine::neural::hebbian_update_scaled(
                                &bconn,
                                "node:decision:rank1_execute",
                                "node:reflex:risk_neutral_allocator",
                                magnitude,
                                true,
                                now,
                            );
                        } else {
                            let _ = thomas_engine::neural::hebbian_update_scaled(
                                &bconn,
                                "node:reflex:adversarial_bear_sentry",
                                "node:outcome:drawdown_protection",
                                magnitude,
                                true,
                                now,
                            );
                            let _ = thomas_engine::neural::hebbian_update_scaled(
                                &bconn,
                                "node:decision:rank1_execute",
                                "node:reflex:risk_conservative_floor",
                                magnitude,
                                false,
                                now,
                            );
                        }
                        live_trades_processed += 1;
                    }
                }
            }
            if tx_started {
                let _ = bconn.execute("COMMIT", []);
            }

            let summary = thomas_engine::neural::summary(&bconn)?;
            let accel_info = thomas_engine::ane_accelerator::accelerator_info();
            println!(
                "neural-step: ok [{accel_info}]; {} sensors ({} discovered, {} activated), {} nodes, {} synapses (avg wt {:.2}, {} reinf, {} pen) | Ingested: {} test decisions, {} forward cand, {} backtests, {} bars, {} patterns, {} skills, {} live trades, {} council scout lessons | Apoptosis: {} synapses pruned",
                summary.total_sensors, report.sensors_discovered, report.sensors_activated, summary.total_nodes, summary.total_synapses, summary.avg_synapse_weight,
                summary.total_reinforcements, summary.total_penalties,
                report.test_decisions_processed, report.forward_candidates_processed, report.backtests_processed,
                report.candle_bars_analyzed, report.patterns_discovered, report.skills_evaluated, live_trades_processed,
                report.council_scout_learnings_ingested,
                report.synapses_pruned
            );
        }
        Cmd::Deliberate => {
            println!("Convening Multi-Model Deliberation Council (Antigravity, Claude, Codex, Apple AI)...");
            let home = std::env::var("HOME").unwrap_or_default();
            let orig_path = std::env::var("PATH").unwrap_or_default();
            let new_path = format!("/opt/homebrew/bin:/usr/local/bin:{home}/.local/bin:{orig_path}");
            let py = if std::path::Path::new("./papercompmoomoo/.venv/bin/python3").exists() {
                "./papercompmoomoo/.venv/bin/python3"
            } else {
                "./papercompmoomoo/.venv/bin/python"
            };
            let status = std::process::Command::new(py)
                .arg("-m")
                .arg("papercompmoomoo.deliberation")
                .env("PYTHONPATH", "papercompmoomoo/src:papercompmoomoo")
                .env("PATH", new_path)
                .status()?;
            if !status.success() {
                anyhow::bail!("Deliberation failed with exit code: {:?}", status.code());
            }
        }
        Cmd::AsxCycle { common, journal, capital } => {
            println!("Executing ASX Standard Paper Momentum Cycle (Allocation: ${:.2})...", capital);
            let now = ops::now_ms();
            let cconn = rusqlite::Connection::open_with_flags(
                &common,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            let jconn = rusqlite::Connection::open(&journal)?;

            let mut positions = std::collections::HashMap::new();
            let res = thomas_engine::market_replicator::execute_native_cycle(
                thomas_engine::market_replicator::MarketType::Asx,
                &cconn,
                &jconn,
                capital,
                &mut positions,
                now,
            )?;

            println!(
                "ASX Cycle Completed. Balance: ${:.2} | Cumulative PnL: ${:+.2} | {} open positions, {} closed, {} new entries",
                res.current_balance, res.cumulative_pnl, res.open_positions_count, res.closed_in_cycle.len(), res.new_entries_count
            );
        }
        Cmd::HkCycle { common, journal, capital } => {
            println!("Executing HK Standard Paper Momentum Cycle (Allocation: ${:.2} USD / HK$39,000 HKD)...", capital);
            let now = ops::now_ms();
            let cconn = rusqlite::Connection::open_with_flags(
                &common,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            let jconn = rusqlite::Connection::open(&journal)?;

            let mut positions = std::collections::HashMap::new();
            let res = thomas_engine::market_replicator::execute_native_cycle(
                thomas_engine::market_replicator::MarketType::Hk,
                &cconn,
                &jconn,
                capital,
                &mut positions,
                now,
            )?;

            println!(
                "HK Cycle Completed. Balance: ${:.2} | Cumulative PnL: ${:+.2} | {} open positions, {} closed, {} new entries",
                res.current_balance, res.cumulative_pnl, res.open_positions_count, res.closed_in_cycle.len(), res.new_entries_count
            );
        }
        Cmd::Learnings { brain, journal, asx_journal: _, hk_journal: _ } => {
            println!("=== ThomasAi Multi-Model Learnings & Council Knowledge Base (NASDAQ) ===");
            if brain.exists() {
                let bconn = rusqlite::Connection::open_with_flags(&brain, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
                let total_memories: i64 = bconn.query_row("SELECT count(*) FROM memories", [], |r| r.get(0)).unwrap_or(0);
                let apple_memories: i64 = bconn.query_row("SELECT count(*) FROM memories WHERE source='apple_intelligence'", [], |r| r.get(0)).unwrap_or(0);
                let council_memories: i64 = bconn.query_row("SELECT count(*) FROM memories WHERE source='council_deliberation'", [], |r| r.get(0)).unwrap_or(0);
                let skills_mastered: i64 = bconn.query_row("SELECT count(*) FROM brain_skills WHERE status='mastered'", [], |r| r.get(0)).unwrap_or(0);
                println!("• Brain Synapses & Bank: {} total memories ({} Apple AI reflections, {} Council Deliberation syntheses, {} mastered skills)", total_memories, apple_memories, council_memories, skills_mastered);
            }
            if journal.exists() {
                let jconn = rusqlite::Connection::open_with_flags(&journal, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
                let total_scout: i64 = jconn.query_row("SELECT count(*) FROM scout_learnings", [], |r| r.get(0)).unwrap_or(0);
                let approved_runs: i64 = jconn.query_row("SELECT count(*) FROM scout_learnings WHERE exit_reason='walkforward_passed'", [], |r| r.get(0)).unwrap_or(0);
                println!("• Council Scout Learnings: {} total empirical runs ({} walk-forward approved & active)", total_scout, approved_runs);
            }
        }
    }
    Ok(())
}

