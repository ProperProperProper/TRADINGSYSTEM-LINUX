//! Durable, idempotent forward evidence for the sole momentum strategy.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path};

pub const STRATEGY: &str = "cross_sectional_momentum_1m_proxy";
use crate::symbol_tune::{LOOKBACK_MAX, TUNED_STRATEGY};

fn known(strategy: Option<&str>) -> bool {
    strategy == Some(STRATEGY) || strategy == Some(TUNED_STRATEGY)
}

fn mapping(text: &str) -> HashMap<String, String> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<_> = line.splitn(4, ',').collect();
            (fields.len() >= 3).then(|| (fields[0].to_owned(), fields[2].to_owned()))
        })
        .collect()
}

fn schema(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS momentum_forward_candidates(
           session TEXT NOT NULL, built_ms INTEGER NOT NULL, cutoff_day INTEGER NOT NULL,
           strategy TEXT NOT NULL, moomoo_symbol TEXT NOT NULL, yahoo_symbol TEXT NOT NULL,
           rank INTEGER NOT NULL, score REAL NOT NULL, observations INTEGER NOT NULL,
           bars INTEGER NOT NULL, gross_return_pct REAL, net_return_pct REAL,
           actual_trades INTEGER NOT NULL, actual_pnl_pct REAL, actual_reason TEXT,
           outcome_complete INTEGER NOT NULL, updated_ms INTEGER NOT NULL,
           PRIMARY KEY(session,moomoo_symbol));
         CREATE INDEX IF NOT EXISTS momentum_forward_cutoff ON momentum_forward_candidates(cutoff_day,session);
         CREATE TABLE IF NOT EXISTS momentum_forward_runs(
           session TEXT PRIMARY KEY, built_ms INTEGER NOT NULL, cutoff_day INTEGER NOT NULL,
           candidates INTEGER NOT NULL, completed INTEGER NOT NULL, net_return_pct REAL,
           actual_trades INTEGER NOT NULL, actual_pnl_pct REAL, equity REAL,
           drawdown_pct REAL, updated_ms INTEGER NOT NULL);",
    )?;
    let columns: std::collections::HashSet<String> = db
        .prepare("PRAGMA table_info(momentum_forward_runs)")?
        .query_map([], |row| row.get(1))?
        .collect::<rusqlite::Result<_>>()?;
    for (name, kind) in [("equity", "REAL"), ("drawdown_pct", "REAL")] {
        if !columns.contains(name) {
            db.execute(
                &format!("ALTER TABLE momentum_forward_runs ADD COLUMN {name} {kind}"),
                [],
            )?;
        }
    }
    Ok(())
}

fn session_prices(
    candles: &Connection,
    yahoo: &str,
    session: &str,
) -> Result<(i64, Option<f64>, Option<f64>, bool)> {
    let has_hl: bool = candles
        .query_row(
            "SELECT count(*) = 2 FROM pragma_table_info('bars') WHERE name IN ('high', 'low')",
            [],
            |r| r.get(0),
        )
        .unwrap_or(false);
    let rows: Vec<(i64, f64, f64, f64, f64)> = if has_hl {
        let mut q = candles.prepare(
            "SELECT ts,open,high,low,close FROM bars WHERE symbol=? AND date(ts/1000,'unixepoch')=? ORDER BY ts",
        )?;
        q.query_map(params![yahoo, session], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<rusqlite::Result<_>>()?
    } else {
        let mut q = candles.prepare(
            "SELECT ts,open,close FROM bars WHERE symbol=? AND date(ts/1000,'unixepoch')=? ORDER BY ts",
        )?;
        q.query_map(params![yahoo, session], |r| {
            let (ts, open, close): (i64, f64, f64) = (r.get(0)?, r.get(1)?, r.get(2)?);
            Ok((ts, open, open.max(close), open.min(close), close))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    let later: bool = candles.query_row(
        "SELECT EXISTS(SELECT 1 FROM bars WHERE symbol=? AND date(ts/1000,'unixepoch')>?)",
        params![yahoo, session],
        |r| r.get(0),
    )?;
    let complete = rows
        .first()
        .zip(rows.last())
        .is_some_and(|(a, b)| later || (rows.len() >= 24 && b.0 - a.0 >= 5 * 3_600_000));
    if rows.is_empty() {
        return Ok((0, None, None, false));
    }
    let entry = rows[0].1;
    let tp_pct = 0.022;
    let sl_pct = 0.008;
    let mut exit_price = rows.last().unwrap().4;
    for b in &rows {
        if (b.2 - entry) / entry >= tp_pct {
            exit_price = entry * (1.0 + tp_pct);
            break;
        }
        if (b.3 - entry) / entry <= -sl_pct {
            exit_price = entry * (1.0 - sl_pct);
            break;
        }
    }
    Ok((
        rows.len() as i64,
        Some(entry),
        Some(exit_price),
        complete,
    ))
}

fn actual_outcome(
    journal: &Connection,
    symbol: &str,
    session: &str,
) -> Result<(i64, Option<f64>, Option<String>)> {
    journal
        .query_row(
            "SELECT count(*), CASE WHEN count(*)>0 THEN sum(pnl_pct) END,
                CASE WHEN count(*)>0 THEN group_concat(reason,'; ') END
           FROM trades WHERE symbol=? AND date(opened_ms/1000,'unixepoch')=?",
            params![symbol, session],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(Into::into)
}

pub fn run(
    journal_path: &Path,
    candles_path: &Path,
    store_path: &Path,
    map_path: &Path,
    cost_bps: f64,
    now_ms: i64,
) -> Result<String> {
    ensure!(
        cost_bps.is_finite() && cost_bps >= 0.0,
        "cost must be finite and non-negative"
    );
    let journal = Connection::open_with_flags(journal_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("cannot open {}", journal_path.display()))?;
    let candles = Connection::open_with_flags(candles_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("cannot open {}", candles_path.display()))?;
    let mut store = Connection::open(store_path)?;
    schema(&store)?;
    let aliases = mapping(&std::fs::read_to_string(map_path)?);
    let plans: Vec<(String, i64, Value)> = {
        let mut q = journal.prepare(
            "SELECT p.session,p.ts_ms,p.detail FROM plan_events p
             WHERE p.event='plan_built' AND p.id=(
               SELECT max(newest.id) FROM plan_events newest
               WHERE newest.event='plan_built' AND newest.session=p.session)
             ORDER BY p.session",
        )?;
        q.query_map([], |r| {
            let raw: String = r.get(2)?;
            Ok((
                r.get(0)?,
                r.get(1)?,
                serde_json::from_str(&raw).unwrap_or(Value::Null),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    let tx = store.transaction()?;
    let mut imported = 0_i64;
    let mut completed = 0_i64;
    for (session, built_ms, plan) in plans {
        if !known(plan["strategy"].as_str()) {
            continue;
        }
        let cutoff = plan["data_cutoff_day"]
            .as_i64()
            .context("momentum plan has no cutoff")?;
        let candidates: Vec<(String, Value)> = {
            let mut q = journal.prepare(
                "SELECT symbol,detail FROM plan_events WHERE session=? AND event='candidate' AND ts_ms>=? ORDER BY id",
            )?;
            q.query_map(params![session, built_ms], |r| {
                let raw: String = r.get(1)?;
                Ok((r.get(0)?, serde_json::from_str(&raw).unwrap_or(Value::Null)))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        let mut net = Vec::new();
        let mut run_actual_n = 0_i64;
        let mut run_actual = 0.0;
        for (moomoo_code, detail) in candidates {
            if !known(detail["strategy"].as_str()) {
                continue;
            }
            let moomoo = moomoo_code.strip_prefix("US.").unwrap_or(&moomoo_code);
            let yahoo = aliases
                .get(moomoo)
                .context("candidate is absent from canonical symbol map")?;
            let rank = detail["rank"]
                .as_i64()
                .context("momentum candidate has no rank")?;
            let score = detail["score"]
                .as_f64()
                .context("momentum candidate has no score")?;
            let observations = detail["observations"]
                .as_i64()
                .context("momentum candidate has no observations")?;
            if detail["strategy"].as_str() == Some(TUNED_STRATEGY) {
                ensure!(
                    (1..=LOOKBACK_MAX as i64).contains(&observations),
                    "tuned candidate lookback is outside the searched range"
                );
            } else {
                ensure!(
                    observations == 20,
                    "momentum candidate does not have 20 observations"
                );
            }
            let (bars, first, last, done) = session_prices(&candles, yahoo, &session)?;
            let gross = first
                .zip(last)
                .filter(|_| done)
                .map(|(a, b)| 100.0 * (b / a - 1.0));
            let net_pct = gross.map(|r| r - cost_bps / 100.0);
            if let Some(value) = net_pct {
                net.push(value);
            }
            let (actual_n, actual_pnl, reason) = actual_outcome(&journal, &moomoo_code, &session)?;
            run_actual_n += actual_n;
            run_actual += actual_pnl.unwrap_or(0.0);
            tx.execute(
                "INSERT INTO momentum_forward_candidates(session,built_ms,cutoff_day,strategy,moomoo_symbol,yahoo_symbol,rank,score,observations,bars,gross_return_pct,net_return_pct,actual_trades,actual_pnl_pct,actual_reason,outcome_complete,updated_ms)
                 VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
                 ON CONFLICT(session,moomoo_symbol) DO UPDATE SET bars=excluded.bars,gross_return_pct=excluded.gross_return_pct,net_return_pct=excluded.net_return_pct,actual_trades=excluded.actual_trades,actual_pnl_pct=excluded.actual_pnl_pct,actual_reason=excluded.actual_reason,outcome_complete=excluded.outcome_complete,updated_ms=excluded.updated_ms",
                params![session,built_ms,cutoff,detail["strategy"].as_str().unwrap_or(STRATEGY),moomoo_code,yahoo,rank,score,observations,bars,gross,net_pct,actual_n,actual_pnl,reason,done as i64,now_ms],
            )?;
            imported += 1;
            completed += i64::from(done);
        }
        let candidate_count: i64 = tx.query_row(
            "SELECT count(*) FROM momentum_forward_candidates WHERE session=?",
            [&session],
            |r| r.get(0),
        )?;
        let complete_count: i64 = tx.query_row("SELECT count(*) FROM momentum_forward_candidates WHERE session=? AND outcome_complete=1", [&session], |r| r.get(0))?;
        let mean = (!net.is_empty()).then(|| net.iter().sum::<f64>() / net.len() as f64);
        tx.execute(
            "INSERT INTO momentum_forward_runs(session,built_ms,cutoff_day,candidates,completed,net_return_pct,actual_trades,actual_pnl_pct,equity,drawdown_pct,updated_ms) VALUES(?,?,?,?,?,?,?,?,NULL,NULL,?)
             ON CONFLICT(session) DO UPDATE SET candidates=excluded.candidates,completed=excluded.completed,net_return_pct=excluded.net_return_pct,actual_trades=excluded.actual_trades,actual_pnl_pct=excluded.actual_pnl_pct,updated_ms=excluded.updated_ms",
            params![session,built_ms,cutoff,candidate_count,complete_count,mean,run_actual_n,(run_actual_n>0).then_some(run_actual),now_ms],
        )?;
    }
    let returns: Vec<(String, f64)> = {
        let mut q = tx.prepare("SELECT session,net_return_pct FROM momentum_forward_runs WHERE candidates>0 AND completed=candidates ORDER BY session")?;
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    tx.execute(
        "UPDATE momentum_forward_runs SET equity=NULL,drawdown_pct=NULL",
        [],
    )?;
    let mut equity = 1.0_f64;
    let mut peak = 1.0_f64;
    for (session, return_pct) in returns {
        equity *= 1.0 + return_pct / 100.0;
        peak = peak.max(equity);
        let drawdown = 100.0 * (peak - equity) / peak;
        tx.execute(
            "UPDATE momentum_forward_runs SET equity=?,drawdown_pct=? WHERE session=?",
            params![equity, drawdown, session],
        )?;
    }
    tx.commit()?;
    let sessions: i64 = store.query_row("SELECT count(*) FROM momentum_forward_runs", [], |r| {
        r.get(0)
    })?;
    let mature_sessions: i64 = store.query_row(
        "SELECT count(*) FROM momentum_forward_runs WHERE candidates>0 AND completed=candidates",
        [],
        |r| r.get(0),
    )?;
    let summary = json!({"strategy":STRATEGY,"sessions":sessions,"mature_sessions":mature_sessions,"candidates_imported":imported,"completed_candidates":completed,"cost_bps":cost_bps});
    Ok(serde_json::to_string_pretty(&summary)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn imports_only_versioned_momentum_plans_and_scores_real_next_session_bars() {
        let dir = tempdir().unwrap();
        let journal_path = dir.path().join("journal.sqlite3");
        let candles_path = dir.path().join("candles.sqlite3");
        let store_path = dir.path().join("store.sqlite3");
        let map_path = dir.path().join("map.csv");
        std::fs::write(
            &map_path,
            "moomoo_symbol,market,yahoo_symbol\nBRK.B,US,BRK-B\n",
        )
        .unwrap();
        let journal = Connection::open(&journal_path).unwrap();
        journal.execute_batch("CREATE TABLE plan_events(id INTEGER PRIMARY KEY,ts_ms INTEGER,session TEXT,symbol TEXT,event TEXT,detail TEXT); CREATE TABLE trades(symbol TEXT,opened_ms INTEGER,pnl_pct REAL,reason TEXT);").unwrap();
        journal
            .execute(
                "INSERT INTO plan_events VALUES(1,100,'2026-10-22',NULL,'plan_built',?)",
                [json!({"strategy":STRATEGY,"data_cutoff_day":20747}).to_string()],
            )
            .unwrap();
        journal
            .execute(
                "INSERT INTO plan_events VALUES(2,100,'2026-10-22','US.BRK.B','candidate',?)",
                [json!({"strategy":STRATEGY,"rank":1,"score":0.2,"observations":20}).to_string()],
            )
            .unwrap();
        journal
            .execute(
                "INSERT INTO trades VALUES('US.BRK.B',1792675800000,1.0,'close')",
                [],
            )
            .unwrap();
        drop(journal);
        let candles = Connection::open(&candles_path).unwrap();
        candles.execute_batch("CREATE TABLE bars(symbol TEXT,ts INTEGER,open REAL,close REAL); INSERT INTO bars VALUES('BRK-B',1792675800000,100,100),('BRK-B',1792696500000,100,102),('BRK-B',1792762200000,103,103);").unwrap();
        drop(candles);
        run(
            &journal_path,
            &candles_path,
            &store_path,
            &map_path,
            10.0,
            200,
        )
        .unwrap();
        let store = Connection::open(store_path).unwrap();
        let row: (String,f64,f64,i64) = store.query_row("SELECT yahoo_symbol,gross_return_pct,net_return_pct,actual_trades FROM momentum_forward_candidates", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(row.0, "BRK-B");
        assert!((row.1 - 2.0).abs() < 1e-9);
        assert!((row.2 - 1.9).abs() < 1e-9);
        assert_eq!(row.3, 1);
    }

    #[test]
    fn migrates_the_first_forward_run_schema_without_losing_rows() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE momentum_forward_runs(
               session TEXT PRIMARY KEY, built_ms INTEGER NOT NULL, cutoff_day INTEGER NOT NULL,
               candidates INTEGER NOT NULL, completed INTEGER NOT NULL, net_return_pct REAL,
               actual_trades INTEGER NOT NULL, actual_pnl_pct REAL, updated_ms INTEGER NOT NULL);
             INSERT INTO momentum_forward_runs VALUES('2026-10-22',1,1,1,0,NULL,0,NULL,1);",
        )
        .unwrap();
        schema(&db).unwrap();
        let columns: Vec<String> = db
            .prepare("PRAGMA table_info(momentum_forward_runs)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(columns.contains(&"equity".to_owned()));
        assert!(columns.contains(&"drawdown_pct".to_owned()));
        assert_eq!(
            db.query_row("SELECT count(*) FROM momentum_forward_runs", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            1
        );
    }
}
