//! Persistent record of the best parameters the search has found (user request 2026-10-04).
//!
//! The market cache and every other runtime file are wiped on each start/deploy (fresh-data rule),
//! but PARAMETERS are the product of the search, not market history: the top settings of every
//! search are kept in their own SQLite file outside the runtime folder so that (1) the most recent
//! best settings can still be used when a search finds nothing usable, (2) every recorded setting is
//! re-tested on each new search window as a candidate, and (3) each recorded setting accumulates a
//! track record on every window it has been tested on. Only parameters and their test results are
//! stored: no market data, no account data.

use super::xs::XsParams;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::Path;

/// Evaluations older than this are pruned.
const EVAL_KEEP_MS: i64 = 30 * 86_400_000;

pub struct Record {
    db: Connection,
}

/// One recorded setting with its track record, for the console.
#[derive(Debug, Clone, Serialize)]
pub struct RecordRow {
    pub id: i64,
    pub params: XsParams,
    pub times_in_top: i64,
    pub times_chosen: i64,
    pub best_score: f64,
    pub evals: i64,
    pub mean_return_pct: f64,
    pub positive_share_pct: f64,
    pub worst_return_pct: f64,
    pub last_seen_ms: i64,
}

impl Record {
    /// Opens (or creates) a parameter record database.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let db = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS params(
                 id INTEGER PRIMARY KEY,
                 json TEXT NOT NULL UNIQUE,
                 first_seen_ms INTEGER NOT NULL,
                 last_seen_ms INTEGER NOT NULL,
                 times_in_top INTEGER NOT NULL DEFAULT 0,
                 times_chosen INTEGER NOT NULL DEFAULT 0,
                 best_score REAL NOT NULL,
                 last_chosen_ms INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS evals(
                 id INTEGER PRIMARY KEY,
                 param_id INTEGER NOT NULL REFERENCES params(id),
                 ts_ms INTEGER NOT NULL,
                 label TEXT NOT NULL,
                 return_pct REAL NOT NULL,
                 max_drawdown_pct REAL NOT NULL,
                 trades INTEGER NOT NULL,
                 liquidations INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS evals_param ON evals(param_id, ts_ms);",
        )?;
        Ok(Record { db })
    }

    /// Id of settings `p` in the record, inserting them or updating last-seen time and best score.
    fn id_of(&self, p: &XsParams, now_ms: i64, score: f64) -> Result<i64> {
        let json = serde_json::to_string(p)?;
        self.db.execute(
            "INSERT INTO params(json, first_seen_ms, last_seen_ms, best_score) VALUES(?1, ?2, ?2, ?3)
             ON CONFLICT(json) DO UPDATE SET last_seen_ms=?2, best_score=MAX(best_score, ?3)",
            params![json, now_ms, score],
        )?;
        Ok(self
            .db
            .query_row("SELECT id FROM params WHERE json=?1", [json], |r| r.get(0))?)
    }

    /// Record a search's top settings (with their robust scores) and the one chosen.
    pub fn save_top(
        &self,
        top: &[(XsParams, f64)],
        chosen: Option<&XsParams>,
        now_ms: i64,
    ) -> Result<()> {
        for (p, score) in top {
            let id = self.id_of(p, now_ms, *score)?;
            self.db.execute(
                "UPDATE params SET times_in_top = times_in_top + 1 WHERE id=?1",
                [id],
            )?;
        }
        if let Some(c) = chosen {
            let id = self.id_of(c, now_ms, f64::MIN)?;
            self.db.execute(
                "UPDATE params SET times_chosen = times_chosen + 1, last_chosen_ms=?2 WHERE id=?1",
                params![id, now_ms],
            )?;
        }
        Ok(())
    }

    /// Recorded settings, best first (by best score, then most recently seen), at most `limit`.
    pub fn load_params(&self, limit: usize) -> Result<Vec<XsParams>> {
        let mut st = self.db.prepare(
            "SELECT json FROM params ORDER BY best_score DESC, last_seen_ms DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit as i64], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for j in rows {
            // A record this build cannot read (older/newer layout) is skipped, never guessed.
            if let Ok(p) = serde_json::from_str::<XsParams>(&j?) {
                out.push(p);
            }
        }
        Ok(out)
    }

    /// The most recently chosen settings: the "recent best" to use when nothing else is available.
    pub fn last_chosen(&self) -> Result<Option<XsParams>> {
        let j: Option<String> = self
            .db
            .query_row(
                "SELECT json FROM params WHERE last_chosen_ms > 0 ORDER BY last_chosen_ms DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok();
        Ok(j.and_then(|j| serde_json::from_str(&j).ok()))
    }

    /// Store one test result of a recorded setting on a window.
    #[allow(clippy::too_many_arguments)]
    pub fn add_eval(
        &self,
        p: &XsParams,
        now_ms: i64,
        label: &str,
        return_pct: f64,
        max_drawdown_pct: f64,
        trades: usize,
        liquidations: usize,
    ) -> Result<()> {
        let id = self.id_of(p, now_ms, f64::MIN)?;
        self.db.execute(
            "INSERT INTO evals(param_id, ts_ms, label, return_pct, max_drawdown_pct, trades, liquidations)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![id, now_ms, label, return_pct, max_drawdown_pct, trades as i64, liquidations as i64],
        )?;
        self.db.execute(
            "DELETE FROM evals WHERE ts_ms < ?1",
            [now_ms - EVAL_KEEP_MS],
        )?;
        Ok(())
    }

    /// Number of distinct settings in the record.
    pub fn count(&self) -> Result<i64> {
        Ok(self
            .db
            .query_row("SELECT COUNT(*) FROM params", [], |r| r.get(0))?)
    }

    /// The best-tracked settings (by mean return over their stored evaluations), at most `limit`.
    pub fn summary(&self, limit: usize) -> Result<Vec<RecordRow>> {
        let mut st = self.db.prepare(
            "SELECT p.id, p.json, p.times_in_top, p.times_chosen, p.best_score, p.last_seen_ms,
                    COUNT(e.id), COALESCE(AVG(e.return_pct), 0),
                    COALESCE(AVG(CASE WHEN e.return_pct > 0 THEN 100.0 ELSE 0.0 END), 0),
                    COALESCE(MIN(e.return_pct), 0)
             FROM params p LEFT JOIN evals e ON e.param_id = p.id
             GROUP BY p.id HAVING COUNT(e.id) > 0
             ORDER BY AVG(e.return_pct) DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, f64>(7)?,
                r.get::<_, f64>(8)?,
                r.get::<_, f64>(9)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, json, top, chosen, best, seen, evals, mean, pos, worst) = row?;
            if let Ok(params) = serde_json::from_str::<XsParams>(&json) {
                out.push(RecordRow {
                    id,
                    params,
                    times_in_top: top,
                    times_chosen: chosen,
                    best_score: best,
                    evals,
                    mean_return_pct: mean,
                    positive_share_pct: pos,
                    worst_return_pct: worst,
                    last_seen_ms: seen,
                });
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::walkforward::sample_params;

    /// Test fixture: a fresh parameter-record path in a temporary folder.
    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("bmr-record-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("params.sqlite3")
    }

    /// Test: records persist across reopen and rank by track record.
    #[test]
    fn records_persist_across_reopen_and_rank_by_track_record() {
        let path = tmp("persist");
        let ps = sample_params(1, 3, &crate::engine::walkforward::tests::test_band());
        {
            let r = Record::open(&path).unwrap();
            r.save_top(
                &[
                    (ps[0].clone(), 5.0),
                    (ps[1].clone(), 9.0),
                    (ps[2].clone(), 1.0),
                ],
                Some(&ps[1]),
                1000,
            )
            .unwrap();
            r.add_eval(&ps[0], 2000, "latest", 12.0, 5.0, 10, 0)
                .unwrap();
            r.add_eval(&ps[0], 2000, "window 1", -2.0, 9.0, 8, 0)
                .unwrap();
            r.add_eval(&ps[2], 2000, "latest", -30.0, 40.0, 3, 1)
                .unwrap();
        }
        // Reopen: nothing is lost across a restart.
        let r = Record::open(&path).unwrap();
        assert_eq!(r.count().unwrap(), 3);
        let loaded = r.load_params(10).unwrap();
        assert_eq!(loaded[0], ps[1], "best score first");
        assert_eq!(r.last_chosen().unwrap().unwrap(), ps[1]);
        let s = r.summary(10).unwrap();
        assert_eq!(s.len(), 2, "only settings with evaluations are summarised");
        assert_eq!(s[0].params, ps[0]);
        assert!(
            (s[0].mean_return_pct - 5.0).abs() < 1e-9
                && (s[0].positive_share_pct - 50.0).abs() < 1e-9
        );
        assert_eq!(s[1].worst_return_pct, -30.0);
        assert_eq!(s[0].times_in_top, 1);
    }

    /// Test: saving the same setting again counts it instead of duplicating.
    #[test]
    fn saving_the_same_setting_again_counts_it_instead_of_duplicating() {
        let path = tmp("dedupe");
        let ps = sample_params(2, 1, &crate::engine::walkforward::tests::test_band());
        let r = Record::open(&path).unwrap();
        for k in 0..3 {
            r.save_top(&[(ps[0].clone(), k as f64)], Some(&ps[0]), 1000 + k)
                .unwrap();
        }
        assert_eq!(r.count().unwrap(), 1);
        r.add_eval(&ps[0], 5000, "x", 1.0, 1.0, 1, 0).unwrap();
        let s = r.summary(5).unwrap();
        assert_eq!((s[0].times_in_top, s[0].times_chosen), (3, 3));
        assert_eq!(s[0].best_score, 2.0);
        // Old evaluations are pruned.
        r.add_eval(&ps[0], 5000 + EVAL_KEEP_MS + 1, "y", 2.0, 1.0, 1, 0)
            .unwrap();
        assert_eq!(r.summary(5).unwrap()[0].evals, 1);
    }
}
