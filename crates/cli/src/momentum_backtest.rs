//! Causal cross-sectional momentum proxy on real 1-minute sessions.
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use thomas_engine::momentum::Selector;

/// A symbol needs this many earlier positive-momentum sessions before its track record may rank it.
const MIN_TRACK_SESSIONS: usize = 10;
const EXPECTED_SYMBOLS: usize = 518;

pub(crate) struct Report {
    pub(crate) symbols: usize,
    pub(crate) universe: String,
    pub(crate) first_day: i64,
    pub(crate) last_day: i64,
    pub(crate) sessions: usize,
    pub(crate) trades: usize,
    pub(crate) wins: usize,
    pub(crate) pnl: f64,
    pub(crate) return_pct: f64,
    pub(crate) max_drawdown_pct: f64,
    pub(crate) blocks: Vec<f64>,
}

fn query_strings(c: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut q = c.prepare(sql)?;
    Ok(q.query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn map_symbols(text: &str) -> (HashSet<String>, Vec<String>) {
    let mut symbols = HashSet::new();
    let mut bad = Vec::new();
    for (line_no, line) in text.lines().enumerate().skip(1) {
        let fields: Vec<_> = line.splitn(4, ',').collect();
        if fields.len() < 3 || fields[0].is_empty() || fields[1].is_empty() || fields[2].is_empty()
        {
            bad.push(format!("line {}", line_no + 1));
        } else if !symbols.insert(fields[0].to_owned()) {
            bad.push(format!("duplicate {}", fields[0]));
        }
    }
    (symbols, bad)
}

fn yahoo_to_moomoo(text: &str) -> HashMap<String, String> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<_> = line.splitn(4, ',').collect();
            (fields.len() >= 3 && !fields[0].is_empty() && !fields[2].is_empty())
                .then(|| (fields[2].to_owned(), fields[0].to_owned()))
        })
        .collect()
}

/// Current-system verification: the one canonical symbol map and converter-owned 1-minute store.
pub fn verify(
    db: &Path,
    symbol_map: &Path,
    lock: &Path,
) -> Result<Vec<thomas_core::export::Check>> {
    let map_text = std::fs::read_to_string(symbol_map)?;
    let lock_text = std::fs::read_to_string(lock)?;
    let c = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    verify_inputs(&c, &map_text, &lock_text)
}

fn verify_inputs(
    c: &Connection,
    map_text: &str,
    lock_text: &str,
) -> Result<Vec<thomas_core::export::Check>> {
    let (mapped, malformed) = map_symbols(map_text);
    let mut checks = Vec::new();
    let mut add = |name, failures| checks.push(thomas_core::export::Check { name, failures });
    add(
        "every symbol-map row carries both symbol spellings",
        malformed,
    );
    add(
        "the canonical map contains all 518 symbols",
        if mapped.len() == EXPECTED_SYMBOLS {
            Vec::new()
        } else {
            vec![format!(
                "found {}, expected {EXPECTED_SYMBOLS}",
                mapped.len()
            )]
        },
    );
    let stored: HashSet<String> = query_strings(c, "SELECT DISTINCT symbol FROM bars")?
        .into_iter()
        .collect();
    let aliases = yahoo_to_moomoo(map_text);
    add(
        "every mapped symbol has real converted bars",
        aliases
            .iter()
            .filter(|(yahoo, _)| !stored.contains(*yahoo))
            .map(|(_, moomoo)| moomoo.clone())
            .collect(),
    );
    add(
        "all converted bars are aligned real 1-minute OHLCV rows",
        query_strings(
            c,
            "SELECT DISTINCT symbol FROM bars WHERE ts%60000!=0 OR open<=0 OR close<=0 OR high<low OR high<max(open,close) OR low>min(open,close) OR volume<0",
        )?,
    );
    add(
        "every short-history symbol has a recorded full-history attempt",
        aliases
            .iter()
            .filter(|(yahoo, _)| {
                let sessions = c
                    .query_row(
                        "SELECT count(DISTINCT ts/86400000) FROM bars WHERE symbol=?",
                        [yahoo],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(0);
                let recorded = c
                    .query_row(
                        "SELECT 1 FROM meta WHERE key=?",
                        [format!("backfilled:{yahoo}")],
                        |_| Ok(()),
                    )
                    .is_ok();
                sessions < 20 && !recorded
            })
            .map(|(_, moomoo)| moomoo.clone())
            .collect(),
    );
    add(
        "the common candle database has no duplicate symbol/timestamp rows",
        query_strings(
            c,
            "SELECT symbol||'@'||ts FROM bars GROUP BY symbol,ts HAVING count(*)>1",
        )?,
    );
    add("sqlite integrity", {
        let mut failures = query_strings(c, "PRAGMA integrity_check")?;
        failures.retain(|x| x != "ok");
        failures
    });
    add(
        "moomoo OpenD and SDK are the pinned version (versions.lock)",
        thomas_core::export::moomoo_version_failures(lock_text),
    );
    Ok(checks)
}

pub(crate) fn load_days(db: &str) -> Result<BTreeMap<i64, Vec<(String, f64)>>> {
    let mapping = yahoo_to_moomoo(&std::fs::read_to_string("data/symbol_map.csv")?);
    let c = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut sessions: BTreeMap<String, Vec<(i64, f64, f64, f64, f64)>> = BTreeMap::new();
    let mut last = HashMap::new();
    let mut no = HashMap::new();
    let mut q = c.prepare("select symbol,ts,open,high,low,close from bars order by symbol,ts")?;
    let mut rows = q.query([])?;
    while let Some(r) = rows.next()? {
        let s: String = r.get(0)?;
        let canonical_owned = if s.starts_with("US.") || s.ends_with(".AX") || s.contains('.') {
            s.clone()
        } else {
            format!("US.{s}")
        };
        let canonical = mapping.get(&s).unwrap_or(&canonical_owned);

        let ts: i64 = r.get(1)?;
        let n = if last.get(&s).is_none_or(|p| ts - p > 6 * 3_600_000) {
            let x = no.entry(s.clone()).or_insert(0);
            *x += 1;
            *x
        } else {
            *no.get(&s).unwrap_or(&1)
        };
        last.insert(s.clone(), ts);
        sessions
            .entry(format!("{canonical}#{n:08}"))
            .or_default()
            .push((ts, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?));
    }
    let mut days: BTreeMap<i64, Vec<(String, f64)>> = BTreeMap::new();
    for (key, bars) in sessions {
        if bars.len() < 2 {
            continue;
        }
        let symbol = key.split('#').next().unwrap_or_default().to_owned();
        let entry = bars[0].1;
        if entry <= 0.0 {
            continue;
        }
        // Dynamic exit execution modeled after swings.py / live trading bot:
        // Take-profit target (+2.2%) and stop-loss threshold (-0.8%), with intraday high/low trajectory
        let tp_pct = 0.022;
        let sl_pct = 0.008;
        let mut ret = None;
        for b in &bars {
            let h = b.2;
            let l = b.3;
            if (h - entry) / entry >= tp_pct {
                ret = Some(tp_pct);
                break;
            }
            if (l - entry) / entry <= -sl_pct {
                ret = Some(-sl_pct);
                break;
            }
        }
        let realized = ret.unwrap_or_else(|| bars.last().unwrap().4 / entry - 1.0);
        days.entry(bars[0].0 / 86_400_000)
            .or_default()
            .push((symbol, realized));
    }
    Ok(days)
}

pub fn select(db: &str, lookback: usize, limit: usize) -> Result<String> {
    anyhow::ensure!(
        lookback > 0 && limit > 0,
        "invalid momentum selector settings"
    );
    let mut selector = Selector::new(lookback);
    for (day, returns) in load_days(db)? {
        selector.observe(day, returns);
    }
    let selection = selector.select(limit);
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "schema": 1,
        "strategy": "cross_sectional_momentum_1m_proxy",
        "data_cutoff_day": selection.data_cutoff_day,
        "lookback_sessions": selection.lookback_sessions,
        "candidates": selection.candidates.iter().map(|c| serde_json::json!({"symbol": c.symbol, "score": c.score, "observations": c.observations})).collect::<Vec<_>>()
    }))?)
}

/// Nasdaq-listed universe written by MooComp (`US.`-prefixed or bare symbols); the same set it may trade.
pub(crate) fn load_universe(path: &Path) -> Result<HashSet<String>> {
    let names: Vec<String> = serde_json::from_str(&std::fs::read_to_string(path)?)
        .with_context(|| format!("invalid universe file {}", path.display()))?;
    let set: HashSet<String> = names
        .into_iter()
        .map(|n| n.strip_prefix("US.").unwrap_or(&n).to_owned())
        .collect();
    anyhow::ensure!(!set.is_empty(), "universe file is empty");
    Ok(set)
}

fn execute(
    db: &str,
    universe: Option<&HashSet<String>>,
    track: bool,
    lookback: usize,
    slots: usize,
    cost_bps: f64,
    capital: f64,
) -> Result<Report> {
    anyhow::ensure!(
        lookback > 0 && slots > 0 && capital > 0.0,
        "invalid momentum settings"
    );
    let mut days = load_days(db)?;
    if let Some(u) = universe {
        for rows in days.values_mut() {
            rows.retain(|(s, _)| u.contains(s.strip_prefix("US.").unwrap_or(s)));
        }
        days.retain(|_, rows| !rows.is_empty());
    }
    let symbols = days
        .values()
        .flatten()
        .map(|(s, _)| s.as_str())
        .collect::<HashSet<_>>()
        .len();
    let first_day = *days
        .keys()
        .next()
        .context("no mapped sessions in common candle database")?;
    let last_day = *days
        .keys()
        .next_back()
        .context("no mapped sessions in common candle database")?;
    let mut selector = Selector::new(lookback);
    let mut equity = capital;
    let mut trades = 0;
    let mut wins = 0;
    let mut daily_returns = Vec::new();
    let mut peak = capital;
    let mut max_drawdown: f64 = 0.0;
    // Per-symbol track record, built only from earlier sessions: the net next-session return of each
    // session in which the symbol's own 20-session momentum score was positive.
    let mut stats: HashMap<String, (usize, f64)> = HashMap::new();

    // ThomasAi Neural Connectome Guidance: load learned causal weights for candidate symbols
    let brain_path = Path::new("papercompmoomoo/data/brain.sqlite3");
    let (brain_weights, bear_traps): (HashMap<String, f64>, HashMap<String, f64>) = if brain_path.exists() {
        rusqlite::Connection::open_with_flags(
            brain_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .and_then(|bc| {
            let mut stmt = bc.prepare(
                "SELECT source_id, weight FROM brain_synapses WHERE target_id = 'node:decision:rank1_execute'"
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?;
            let rank1_map: HashMap<String, f64> = rows.flatten().collect();

            let mut bear_stmt = bc.prepare(
                "SELECT source_id, weight FROM brain_synapses WHERE target_id = 'node:reflex:adversarial_bear_sentry'"
            )?;
            let bear_rows = bear_stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?;
            let bear_map: HashMap<String, f64> = bear_rows.flatten().collect();

            Ok((rank1_map, bear_map))
        })
        .unwrap_or_default()
    } else {
        (HashMap::new(), HashMap::new())
    };

    for (day, mut rows) in days {
        let everyone = selector.select(usize::MAX);
        let mut selection = everyone.clone();
        if track {
            let mut ranked: Vec<_> = everyone
                .candidates
                .iter()
                .filter_map(|c| {
                    let (n, sum) = *stats.get(&c.symbol)?;
                    (n >= MIN_TRACK_SESSIONS && sum > 0.0).then(|| (sum / n as f64, c.clone()))
                })
                .collect();
            ranked.sort_by(|a, b| {
                b.0.total_cmp(&a.0)
                    .then_with(|| a.1.symbol.cmp(&b.1.symbol))
            });
            selection.candidates = ranked.into_iter().map(|(_, c)| c).collect();
        } else if !brain_weights.is_empty() {
            // Apply ThomasAi neural conviction and Adversarial Bear Sentry trap filtering
            selection.candidates.sort_by(|a, b| {
                let clean_a = format!("node:symbol:{}", a.symbol.replace('.', "_").replace('-', "_"));
                let clean_b = format!("node:symbol:{}", b.symbol.replace('.', "_").replace('-', "_"));
                let wa = brain_weights.get(&clean_a).copied().unwrap_or(1.0);
                let wb = brain_weights.get(&clean_b).copied().unwrap_or(1.0);
                let bear_a = bear_traps.get(&clean_a).copied().unwrap_or(1.0);
                let bear_b = bear_traps.get(&clean_b).copied().unwrap_or(1.0);
                let score_a = if bear_a > 1.5 { (a.score * wa) / bear_a } else { a.score * wa };
                let score_b = if bear_b > 1.5 { (b.score * wb) / bear_b } else { b.score * wb };
                score_b.total_cmp(&score_a).then_with(|| a.symbol.cmp(&b.symbol))
            });
        }
        selection.candidates.truncate(slots);
        let current: HashMap<_, _> = rows.iter().cloned().collect();
        let before = equity;
        let alloc = equity / slots as f64;
        for candidate in &selection.candidates {
            let Some(r) = current.get(&candidate.symbol) else {
                continue;
            };
            let net = *r - cost_bps / 10_000.0;
            equity += alloc * net;
            trades += 1;
            wins += usize::from(net > 0.0);
        }
        for c in everyone.candidates.iter().filter(|c| c.score > 0.0) {
            if let Some(r) = current.get(&c.symbol) {
                let e = stats.entry(c.symbol.clone()).or_insert((0, 0.0));
                e.0 += 1;
                e.1 += *r - cost_bps / 10_000.0;
            }
        }
        selector.observe(day, rows.drain(..));
        daily_returns.push((day, equity / before - 1.0));
        peak = peak.max(equity);
        max_drawdown = max_drawdown.max((peak - equity) / peak);
    }
    let mut blocks = Vec::new();
    for i in 0..5 {
        let start = i * daily_returns.len() / 5;
        let end = (i + 1) * daily_returns.len() / 5;
        let mut value = 1.0;
        for (_, r) in &daily_returns[start..end] {
            value *= 1.0 + r;
        }
        blocks.push(value - 1.0);
    }
    Ok(Report {
        symbols,
        universe: format!(
            "{}{}",
            if universe.is_some() {
                "nasdaq"
            } else {
                "all mapped"
            },
            if track { ", track-record rank" } else { "" }
        ),
        first_day,
        last_day,
        sessions: daily_returns.len(),
        trades,
        wins,
        pnl: equity - capital,
        return_pct: (equity / capital - 1.0) * 100.0,
        max_drawdown_pct: max_drawdown * 100.0,
        blocks,
    })
}

pub(crate) fn render(
    report: &Report,
    lookback: usize,
    slots: usize,
    cost_bps: f64,
    capital: f64,
) -> String {
    format!(
        "momentum_proxy lookback_sessions={} slots={} cost={:.1}bps capital=${:.2}\ntrades={} wins={} pnl=${:+.2} return={:+.3}% max_drawdown={:.3}%\nforward_blocks={}\n",
        lookback,
        slots,
        cost_bps,
        capital,
        report.trades,
        report.wins,
        report.pnl,
        report.return_pct,
        report.max_drawdown_pct,
        report
            .blocks
            .iter()
            .map(|r| format!("{:+.3}% ", r * 100.0))
            .collect::<String>(),
    )
}

fn schema(store: &Connection) -> Result<()> {
    store.execute_batch(
        "CREATE TABLE IF NOT EXISTS momentum_backtest_runs(
           id INTEGER PRIMARY KEY, run_ms INTEGER NOT NULL, strategy TEXT NOT NULL,
           lookback_sessions INTEGER NOT NULL, slots INTEGER NOT NULL, cost_bps REAL NOT NULL,
           capital REAL NOT NULL, first_day INTEGER NOT NULL, last_day INTEGER NOT NULL,
           sessions INTEGER NOT NULL, trades INTEGER NOT NULL, wins INTEGER NOT NULL,
           pnl REAL NOT NULL, return_pct REAL NOT NULL, max_drawdown_pct REAL NOT NULL,
           forward_blocks_json TEXT NOT NULL, universe TEXT, symbols INTEGER);
         CREATE INDEX IF NOT EXISTS momentum_backtest_runs_latest ON momentum_backtest_runs(run_ms DESC);",
    )?;
    // Older ledgers predate the universe columns; add them in place (their old rows stay NULL = unrecorded).
    for (col, def) in [("universe", "TEXT"), ("symbols", "INTEGER")] {
        let has: bool = store.query_row(
            "SELECT count(*) > 0 FROM pragma_table_info('momentum_backtest_runs') WHERE name = ?",
            [col],
            |r| r.get(0),
        )?;
        if !has {
            store.execute(
                &format!("ALTER TABLE momentum_backtest_runs ADD COLUMN {col} {def}"),
                [],
            )?;
        }
    }
    Ok(())
}

pub(crate) fn record(
    store: &Connection,
    report: &Report,
    lookback: usize,
    slots: usize,
    cost_bps: f64,
    capital: f64,
    now_ms: i64,
) -> Result<()> {
    schema(store)?;
    store.execute(
        "INSERT INTO momentum_backtest_runs(run_ms,strategy,lookback_sessions,slots,cost_bps,capital,first_day,last_day,sessions,trades,wins,pnl,return_pct,max_drawdown_pct,forward_blocks_json,universe,symbols) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        rusqlite::params![now_ms, "cross_sectional_momentum_1m_proxy", lookback as i64, slots as i64, cost_bps, capital, report.first_day, report.last_day, report.sessions as i64, report.trades as i64, report.wins as i64, report.pnl, report.return_pct, report.max_drawdown_pct, serde_json::to_string(&report.blocks)?, report.universe.as_str(), report.symbols as i64],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
/// Run only on the converter-owned real candles and preserve the result for the read-only portal.
pub fn run_and_record(
    db: &str,
    store_path: &Path,
    universe_path: Option<&Path>,
    track: bool,
    lookback: usize,
    slots: usize,
    cost_bps: f64,
    capital: f64,
    now_ms: i64,
) -> Result<String> {
    let universe = universe_path.map(load_universe).transpose()?;
    let report = execute(
        db,
        universe.as_ref(),
        track,
        lookback,
        slots,
        cost_bps,
        capital,
    )?;
    let store = Connection::open(store_path)?;
    record(&store, &report, lookback, slots, cost_bps, capital, now_ms)?;
    Ok(render(&report, lookback, slots, cost_bps, capital))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_verifier_rejects_missing_map_history_and_bad_bars() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE bars(symbol TEXT, ts INTEGER, open REAL, high REAL, low REAL, close REAL, volume REAL, PRIMARY KEY(symbol,ts));
             CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO bars VALUES('AAA',60000,2,1,2,1,1);",
        )
        .unwrap();
        let checks = verify_inputs(
            &c,
            "moomoo_symbol,market,yahoo_symbol\nAAA,US,AAA\nBBB,US,BBB\n",
            "",
        )
        .unwrap();
        let failed: Vec<_> = checks
            .iter()
            .filter(|x| !x.failures.is_empty())
            .map(|x| x.name)
            .collect();
        assert!(failed.contains(&"the canonical map contains all 518 symbols"));
        assert!(failed.contains(&"every mapped symbol has real converted bars"));
        assert!(failed.contains(&"all converted bars are aligned real 1-minute OHLCV rows"));
        assert!(failed.contains(&"every short-history symbol has a recorded full-history attempt"));
    }

    #[test]
    fn yahoo_aliases_are_translated_to_canonical_moomoo_symbols() {
        let aliases =
            yahoo_to_moomoo("moomoo_symbol,market,yahoo_symbol\nBRK.B,US,BRK-B\nBF.B,US,BF-B\n");
        assert_eq!(aliases.get("BRK-B").map(String::as_str), Some("BRK.B"));
        assert_eq!(aliases.get("BF-B").map(String::as_str), Some("BF.B"));
    }

    #[test]
    fn persisted_backtest_keeps_real_research_metrics() {
        let store = Connection::open_in_memory().unwrap();
        let report = Report {
            symbols: 169,
            universe: "nasdaq".into(),
            first_day: 20_000,
            last_day: 20_010,
            sessions: 11,
            trades: 22,
            wins: 13,
            pnl: 123.45,
            return_pct: 0.123,
            max_drawdown_pct: 2.5,
            blocks: vec![0.01, -0.02],
        };
        record(&store, &report, 20, 2, 10.0, 100_000.0, 123).unwrap();
        let row: (i64, i64, f64, String) = store
            .query_row(
                "SELECT sessions,trades,return_pct,forward_blocks_json FROM momentum_backtest_runs",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((row.0, row.1), (11, 22));
        assert_eq!(row.2, 0.123);
        assert_eq!(row.3, "[0.01,-0.02]");
        let u: (String, i64) = store
            .query_row(
                "SELECT universe,symbols FROM momentum_backtest_runs",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(u, ("nasdaq".to_owned(), 169));
    }
}
