//! ThomasAi's own results database. Everything the system learns is written here.
use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

pub const SCHEMA_VERSION: i64 = 30;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS data_checks(
    id INTEGER PRIMARY KEY, ts_ms INTEGER NOT NULL, source TEXT NOT NULL,
    symbols INTEGER NOT NULL, bars INTEGER NOT NULL, bad_symbols INTEGER NOT NULL,
    report_json TEXT NOT NULL);

-- Translation list between sources. Every row carries both spellings.
-- moomoo_symbol = the security code the bot sends to OpenD (plain ticker) together with moomoo_market
-- (QotMarket US = 11, from the bot's src/engine/moomoo.rs). yahoo_match: verified | mismatch | unverified.
-- bot_status: tradable (in the bot's moomoo-verified universe) | unverified | research_only.
CREATE TABLE IF NOT EXISTS symbols(
    moomoo_symbol TEXT PRIMARY KEY, moomoo_market TEXT NOT NULL, yahoo_symbol TEXT NOT NULL UNIQUE,
    name TEXT, bot_status TEXT NOT NULL, in_spy INTEGER NOT NULL, in_bot_universe INTEGER NOT NULL,
    has_moomoo_candles INTEGER NOT NULL, yahoo_returned_symbol TEXT, yahoo_name TEXT, yahoo_match TEXT NOT NULL,
    overlap_bars INTEGER, close_diff_bps_median REAL, close_diff_bps_p99 REAL, volume_ratio_median REAL,
    evidence TEXT NOT NULL, verified_ms INTEGER NOT NULL);

-- Raw Yahoo 1m bars (last ~58 days) for the symbols fetched. An accumulating archive: never dropped on upgrade. The merged base
-- (candles_merged) takes from it only what moomoo lacks.
CREATE TABLE IF NOT EXISTS candles_extra(
    moomoo_symbol TEXT NOT NULL, yahoo_symbol TEXT NOT NULL, wall_ms INTEGER NOT NULL, open REAL NOT NULL,
    high REAL NOT NULL, low REAL NOT NULL, close REAL NOT NULL, volume REAL NOT NULL, source TEXT NOT NULL,
    fetched_ms INTEGER NOT NULL, PRIMARY KEY(moomoo_symbol, wall_ms)) WITHOUT ROWID;

-- The merged candle base: moomoo is primary; Yahoo fills only what moomoo lacks (symbols without candles, missing bars, zero-volume bars).
-- Rebuilt by `thomas merge` from the moomoo snapshot + candles_extra (raw Yahoo archive). turnover for Yahoo rows is volume*(high+low+close)/3
-- (median error 0.03% against moomoo's real turnover on 89,753 overlapping bars); volume_reliable=0 for Yahoo's first/last bar of a
-- session, where its volume disagrees with moomoo's (auction handling).
-- When each outside candle store was last read by `thomas import-bars` (the time the read began): rows the other project changed after it are read again next time.
CREATE TABLE IF NOT EXISTS import_state(source TEXT PRIMARY KEY, last_ms INTEGER NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS candles_merged(
    moomoo_symbol TEXT NOT NULL, yahoo_symbol TEXT NOT NULL, wall_ms INTEGER NOT NULL, open REAL NOT NULL, high REAL NOT NULL,
    low REAL NOT NULL, close REAL NOT NULL, volume REAL NOT NULL, turnover REAL NOT NULL, source TEXT NOT NULL,
    turnover_estimated INTEGER NOT NULL, volume_reliable INTEGER NOT NULL, PRIMARY KEY(moomoo_symbol, wall_ms)) WITHOUT ROWID;

-- The real US trading calendar (regular sessions) from the research start. The bot's trading clock starts at the first session of its calendar, and
-- rebalance timing depends on that origin, so the market must be built on the full calendar, not just on the days that have candles.
-- Open times are Yahoo's SPY daily-bar stamps (09:30 ET in UTC); close = open + 6.5 h (no early-close day falls in 2026 before November).
CREATE TABLE IF NOT EXISTS sessions_calendar(open_ms INTEGER PRIMARY KEY, close_ms INTEGER NOT NULL, source TEXT NOT NULL, fetched_ms INTEGER NOT NULL);

-- What real history each symbol has, against the market calendar (timestamps most symbols traded).
CREATE TABLE IF NOT EXISTS symbol_coverage(
    moomoo_symbol TEXT PRIMARY KEY, yahoo_symbol TEXT NOT NULL, first_ms INTEGER NOT NULL, last_ms INTEGER NOT NULL,
    moomoo_bars INTEGER NOT NULL, filled_from_yahoo INTEGER NOT NULL, gap_bars INTEGER NOT NULL,
    expected_bars INTEGER NOT NULL, complete INTEGER NOT NULL, checked_ms INTEGER NOT NULL);

-- Real bars from a secondary source that fill gaps in the primary (moomoo) snapshot.
CREATE TABLE IF NOT EXISTS candles_supplement(
    moomoo_symbol TEXT NOT NULL, yahoo_symbol TEXT NOT NULL, wall_ms INTEGER NOT NULL, open REAL NOT NULL,
    high REAL NOT NULL, low REAL NOT NULL, close REAL NOT NULL, volume REAL NOT NULL, source TEXT NOT NULL,
    fetched_ms INTEGER NOT NULL, PRIMARY KEY(moomoo_symbol, wall_ms)) WITHOUT ROWID;

-- Spans with no real data from any source; the engine must exclude them. kind: placeholder | no_trade | unavailable.
CREATE TABLE IF NOT EXISTS data_gaps(
    moomoo_symbol TEXT NOT NULL, yahoo_symbol TEXT NOT NULL, first_ms INTEGER NOT NULL, last_ms INTEGER NOT NULL,
    kind TEXT NOT NULL, bars INTEGER NOT NULL, detail TEXT NOT NULL, checked_ms INTEGER NOT NULL,
    PRIMARY KEY(moomoo_symbol, first_ms)) WITHOUT ROWID;

-- Moomoo vs Yahoo agreement per symbol over the overlap window.
CREATE TABLE IF NOT EXISTS source_checks(
    id INTEGER PRIMARY KEY, ts_ms INTEGER NOT NULL, moomoo_symbol TEXT NOT NULL, yahoo_symbol TEXT NOT NULL,
    yahoo_returned_symbol TEXT, yahoo_name TEXT, overlap_bars INTEGER NOT NULL, missing_in_yahoo INTEGER NOT NULL,
    close_diff_bps_median REAL, close_diff_bps_p99 REAL, close_diff_bps_max REAL, volume_ratio_median REAL);

-- Versioned strategies; status is champion | challenger | rejected | retired.
CREATE TABLE IF NOT EXISTS strategies(
    id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL,
    git_commit TEXT, status TEXT NOT NULL, created_ms INTEGER NOT NULL, notes TEXT,
    UNIQUE(name, version));

-- The exact setting behind a registry strategy (name 'xs-setting <hash8>'), so a champion or challenger can be rebuilt and tested again.
CREATE TABLE IF NOT EXISTS strategy_params(
    strategy_id INTEGER PRIMARY KEY REFERENCES strategies(id), params_hash TEXT NOT NULL UNIQUE, params_json TEXT NOT NULL);

-- One backtest of one parameter set over one window.
CREATE TABLE IF NOT EXISTS backtest_runs(
    id INTEGER PRIMARY KEY, ts_ms INTEGER NOT NULL, strategy_id INTEGER NOT NULL REFERENCES strategies(id),
    params_hash TEXT NOT NULL, params_json TEXT NOT NULL, window_label TEXT NOT NULL,
    first_bar_ts INTEGER NOT NULL, last_bar_ts INTEGER NOT NULL, start_equity REAL NOT NULL,
    return_pct REAL NOT NULL, max_drawdown_pct REAL NOT NULL, trades INTEGER NOT NULL, wins INTEGER NOT NULL,
    profit_factor REAL NOT NULL, fees REAL NOT NULL, robust_score REAL, seed INTEGER,
    engine_version TEXT NOT NULL,
    UNIQUE(strategy_id, params_hash, window_label, first_bar_ts));
CREATE INDEX IF NOT EXISTS backtest_runs_strategy ON backtest_runs(strategy_id, ts_ms);

-- The overlapping search periods already searched, keyed by their first session's open time and the trade floor used (a different floor is a different search). Periods are never re-searched (new data adds new ones),
-- and a stored winner can later be tested on a period that starts after its own ended: genuine forward evidence.
CREATE TABLE IF NOT EXISTS search_periods(
    period_start_ms INTEGER NOT NULL, min_trades INTEGER NOT NULL, period_end_ms INTEGER NOT NULL, searched_ms INTEGER NOT NULL, samples INTEGER NOT NULL,
    seed INTEGER NOT NULL, usable INTEGER NOT NULL, params_json TEXT, robust_score REAL, raw_best_score REAL, PRIMARY KEY(period_start_ms, min_trades));

-- A2.4: the numeric learner judged on each period (trained only on candles before it): rank IC of prediction vs outcome, against the best single feature and a shuffled-target null.
CREATE TABLE IF NOT EXISTS learner_periods(
    period_start_ms INTEGER NOT NULL, train_rows INTEGER NOT NULL, test_candles INTEGER NOT NULL, ic_mean REAL NOT NULL, ic_t REAL NOT NULL, spread_pct REAL NOT NULL,
    best_baseline TEXT NOT NULL, best_baseline_ic REAL NOT NULL, null_mean REAL NOT NULL, null_sd REAL NOT NULL, has_signal INTEGER NOT NULL, computed_ms INTEGER NOT NULL,
    model TEXT NOT NULL DEFAULT 'gbdt', PRIMARY KEY(period_start_ms, model));

-- EVERY winning (usable) setting a period's search drew, kept for good: they are the pool for cross-period checks and for forward tests as data grows.
CREATE TABLE IF NOT EXISTS winning_settings(
    period_start_ms INTEGER NOT NULL, min_trades INTEGER NOT NULL, params_hash TEXT NOT NULL, params_json TEXT NOT NULL, objective REAL NOT NULL,
    PRIMARY KEY(period_start_ms, min_trades, params_hash)) WITHOUT ROWID;

-- Every change an agent proposes and what happened to it.
CREATE TABLE IF NOT EXISTS experiments(
    id INTEGER PRIMARY KEY, ts_ms INTEGER NOT NULL, strategy_id INTEGER REFERENCES strategies(id),
    parent_id INTEGER REFERENCES experiments(id), proposer TEXT NOT NULL, description TEXT NOT NULL,
    patch_sha TEXT, compiled INTEGER, tests_passed INTEGER, oos_score REAL, incumbent_score REAL,
    decision TEXT NOT NULL, reason TEXT NOT NULL);

-- Phase 4: one row per step of every `thomas cycle` (the unattended 24x7 run), so what ran, what failed and how long it took is on disk.
CREATE TABLE IF NOT EXISTS cycle_runs(
    id INTEGER PRIMARY KEY, cycle_ms INTEGER NOT NULL, step TEXT NOT NULL, ok INTEGER NOT NULL, seconds REAL NOT NULL, detail TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS cycle_runs_cycle ON cycle_runs(cycle_ms);

-- Walk-forward validation (real data only): one row per fold and selection rule; the setting was chosen from candles before the test block.
CREATE TABLE IF NOT EXISTS walkforward_runs(
    id INTEGER PRIMARY KEY, run_ms INTEGER NOT NULL, layout TEXT NOT NULL, rule TEXT NOT NULL, fold INTEGER NOT NULL, train_from_ms INTEGER NOT NULL, train_to_ms INTEGER NOT NULL,
    test_from_ms INTEGER NOT NULL, test_to_ms INTEGER NOT NULL, return_pct REAL NOT NULL, max_drawdown_pct REAL NOT NULL, trades INTEGER NOT NULL, wins INTEGER NOT NULL,
    ref_return_pct REAL, params_json TEXT, min_trades INTEGER NOT NULL, samples INTEGER NOT NULL, seed INTEGER NOT NULL);

-- The weekly symbol pick handed to the bot (data/selection.json); kept so a pick can be judged once real history for it exists.
CREATE TABLE IF NOT EXISTS selections(
    id INTEGER PRIMARY KEY, generated_ms INTEGER NOT NULL, week TEXT NOT NULL, rule TEXT NOT NULL, n_pick INTEGER NOT NULL, symbols_json TEXT NOT NULL, basis TEXT NOT NULL);

-- The local paper trader (no orders leave the machine): its whole account as JSON, every closed trade and decision it made, its equity after every candle.
CREATE TABLE IF NOT EXISTS paper_state(id INTEGER PRIMARY KEY CHECK(id=1), json TEXT NOT NULL, updated_ms INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS paper_trades(
    id INTEGER PRIMARY KEY, symbol TEXT NOT NULL, side TEXT NOT NULL, entry_ms INTEGER NOT NULL, exit_ms INTEGER NOT NULL, qty REAL NOT NULL, entry_price REAL NOT NULL,
    exit_price REAL NOT NULL, pnl REAL NOT NULL, fees REAL NOT NULL, reason TEXT NOT NULL, UNIQUE(symbol, entry_ms, exit_ms));
CREATE TABLE IF NOT EXISTS paper_equity(wall_ms INTEGER PRIMARY KEY, equity REAL NOT NULL, unrealized REAL NOT NULL, positions INTEGER NOT NULL, ref_close REAL);
CREATE TABLE IF NOT EXISTS paper_params(wall_ms INTEGER PRIMARY KEY, params_json TEXT NOT NULL, note TEXT NOT NULL);

-- ThomasAi learning from the competition bot: each run of the improvement loop (the strategy in force, what was tried, what was kept) and its candidates.
CREATE TABLE IF NOT EXISTS comp_runs(
    id INTEGER PRIMARY KEY, ts_ms INTEGER NOT NULL, events INTEGER NOT NULL, days INTEGER NOT NULL, blocks INTEGER NOT NULL, base_json TEXT NOT NULL,
    base_total REAL NOT NULL, base_trades INTEGER NOT NULL, kept TEXT, applied INTEGER NOT NULL DEFAULT 0, summary TEXT NOT NULL);
-- Every hypothesis tried by hand against the judge before any code was written (`thomas comp --try`); they count as tests in the multiple-testing correction.
CREATE TABLE IF NOT EXISTS comp_hypotheses(
    id INTEGER PRIMARY KEY, ts_ms INTEGER NOT NULL, label TEXT NOT NULL, changes_json TEXT NOT NULL, better_blocks INTEGER NOT NULL, blocks INTEGER NOT NULL,
    base_total REAL NOT NULL, cand_total REAL NOT NULL, cand_trades INTEGER NOT NULL, keep INTEGER NOT NULL, reason TEXT NOT NULL);
-- The daily review of the learning by a headless Claude session (`thomas review`): whether it went well, how many hypotheses it pre-checked, what it said.
CREATE TABLE IF NOT EXISTS comp_reviews(
    id INTEGER PRIMARY KEY, ts_ms INTEGER NOT NULL, ok INTEGER NOT NULL, hypotheses INTEGER NOT NULL, seconds REAL NOT NULL, text TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS comp_candidates(
    run_id INTEGER NOT NULL REFERENCES comp_runs(id), name TEXT NOT NULL, keep INTEGER NOT NULL, better_blocks INTEGER NOT NULL, blocks INTEGER NOT NULL,
    base_total REAL NOT NULL, cand_total REAL NOT NULL, cand_trades INTEGER NOT NULL, reason TEXT NOT NULL, PRIMARY KEY(run_id, name));

";

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    // several scheduled jobs share this file (the research cycle, the paper trader, the reports): wait for a writer instead of failing
    conn.busy_timeout(std::time::Duration::from_secs(60))?;
    init(&conn)?;
    Ok(conn)
}

/// Tables rebuilt from sources by `thomas reconcile` / `thomas symbols`; safe to drop on upgrade.
const DERIVED: [&str; 5] = [
    "symbols",
    "symbol_coverage",
    "candles_supplement",
    "data_gaps",
    "source_checks",
];

pub fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA mmap_size=4294967296; PRAGMA cache_size=-65536; PRAGMA temp_store=MEMORY;",
    )?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version != 0 && version < 19 {
        conn.execute_batch("DROP TABLE IF EXISTS learner_periods;")?; // one row per period AND model since v19 (rebuilt by `thomas learn`)
    }
    if version != 0 && version < 28 {
        conn.execute_batch("DROP TABLE IF EXISTS agent_actions; DROP TABLE IF EXISTS escalations; DROP TABLE IF EXISTS playbooks;")?; // the local model and its agents were removed (2026-10-20)
    }
    if version != 0 && version < 26 {
        conn.execute_batch("DROP TABLE IF EXISTS volume_scale;")?; // a per-symbol volume ratio lived here for one day (v25); the converter owns it now
    }
    if version != 0 && version < 15 {
        conn.execute_batch(
            "DROP TABLE IF EXISTS winning_settings; DROP TABLE IF EXISTS search_periods;",
        )?; // search_periods lost top_json in v15
    }
    if version != 0 && version < 14 {
        conn.execute_batch("DROP TABLE IF EXISTS search_periods;")?; // reshaped in v13 and v14 (top candidates kept): it held test runs only
    }
    if version != 0 && version < 5 {
        for t in DERIVED {
            conn.execute_batch(&format!("DROP TABLE IF EXISTS {t};"))?;
        }
    }
    // Retired tables (owner 2026-10-18: no hourly-derived candles; no walk-forward chunks): dropped, with their data, if an older store still has them.
    conn.execute_batch(
        "DROP TABLE IF EXISTS candles_hourly; DROP TABLE IF EXISTS walkforward_blocks;",
    )?;
    conn.execute_batch(SCHEMA)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables(c: &Connection) -> Vec<String> {
        let mut s = c.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
        s.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn creates_all_tables_and_is_idempotent() {
        let c = Connection::open_in_memory().unwrap();
        init(&c).unwrap();
        init(&c).unwrap();
        assert_eq!(
            tables(&c),
            [
                "backtest_runs",
                "candles_extra",
                "candles_merged",
                "candles_supplement",
                "comp_candidates",
                "comp_hypotheses",
                "comp_reviews",
                "comp_runs",
                "cycle_runs",
                "data_checks",
                "data_gaps",
                "experiments",
                "import_state",
                "learner_periods",
                "paper_equity",
                "paper_params",
                "paper_state",
                "paper_trades",
                "search_periods",
                "selections",
                "sessions_calendar",
                "source_checks",
                "strategies",
                "strategy_params",
                "symbol_coverage",
                "symbols",
                "walkforward_runs",
                "winning_settings"
            ]
        );
        let v: i64 = c
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn duplicate_run_is_rejected_and_foreign_keys_enforced() {
        let c = Connection::open_in_memory().unwrap();
        init(&c).unwrap();
        let run = "INSERT INTO backtest_runs(ts_ms,strategy_id,params_hash,params_json,window_label,first_bar_ts,last_bar_ts,start_equity,return_pct,max_drawdown_pct,trades,wins,profit_factor,fees,engine_version) VALUES(1,1,'h','{}','w',0,1,5000,0,0,0,0,0,0,'t')";
        assert!(
            c.execute(run, []).is_err(),
            "run without a strategy must fail"
        );
        c.execute(
            "INSERT INTO strategies(name,version,status,created_ms) VALUES('s',1,'champion',0)",
            [],
        )
        .unwrap();
        c.execute(run, []).unwrap();
        assert!(c.execute(run, []).is_err(), "same run twice must fail");
    }
}
