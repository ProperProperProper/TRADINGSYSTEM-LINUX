//! A2.3 champion/challenger registry. The preferred setting of each search is registered as a challenger together with its guard verdict. A setting becomes
//! champion only on forward (out-of-sample) evidence: tested on at least `MIN_FORWARD_PERIODS` later periods, all overlap guards passing, and, when a champion
//! exists, a mean forward return at least `PROMOTION_MARGIN_PCT` points above the champion's (the bot's own "+1 point better" promotion rule). It is rejected when
//! it has forward tests in that many periods and their mean is not positive. Every decision is written to `experiments` with its reason. Nothing is handed to the
//! live bot by this module (rule 9).
use anyhow::Result;
use moomoo_bot::engine::xs::XsParams;
use rusqlite::{Connection, OptionalExtension};
use thomas_core::config::{MIN_FORWARD_PERIODS, PROMOTION_MARGIN_PCT};

use crate::guards::Guards;
use crate::search::Forward;

/// Forward (out-of-sample) results of one setting.
#[derive(Debug, Clone, PartialEq)]
pub struct ForwardSummary {
    pub tests: usize,
    pub positive: usize,
    /// Distinct later periods it was tested on.
    pub periods: usize,
    pub mean_return_pct: f64,
}

pub fn forward_summary(forward: &[Forward], params: &XsParams) -> ForwardSummary {
    let me = serde_json::to_string(params).unwrap_or_default();
    let mine: Vec<&Forward> = forward
        .iter()
        .filter(|f| serde_json::to_string(&f.params).unwrap_or_default() == me)
        .collect();
    let periods: std::collections::BTreeSet<i64> =
        mine.iter().map(|f| f.on_period_start_ms).collect();
    ForwardSummary {
        tests: mine.len(),
        positive: mine.iter().filter(|f| f.metrics.return_pct > 0.0).count(),
        periods: periods.len(),
        mean_return_pct: if mine.is_empty() {
            0.0
        } else {
            mine.iter().map(|f| f.metrics.return_pct).sum::<f64>() / mine.len() as f64
        },
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Not enough forward evidence either way.
    Challenger(String),
    Promote(String),
    Reject(String),
}

/// The decision for one setting. `champion` is the current champion's forward summary, if there is one.
pub fn decide(guards: &Guards, fwd: &ForwardSummary, champion: Option<&ForwardSummary>) -> Verdict {
    if fwd.periods < MIN_FORWARD_PERIODS {
        return Verdict::Challenger(format!(
            "forward-tested on {} later period(s); {MIN_FORWARD_PERIODS} needed before it can be promoted or rejected",
            fwd.periods
        ));
    }
    if fwd.mean_return_pct <= 0.0 {
        return Verdict::Reject(format!(
            "mean forward return {:.2}% over {} tests in {} periods is not positive",
            fwd.mean_return_pct, fwd.tests, fwd.periods
        ));
    }
    let failing = guards.failures();
    if !failing.is_empty() {
        return Verdict::Challenger(format!(
            "forward evidence is positive ({:.2}% mean) but guards fail: {}",
            fwd.mean_return_pct,
            failing.join("; ")
        ));
    }
    match champion {
        Some(c) if fwd.mean_return_pct < c.mean_return_pct + PROMOTION_MARGIN_PCT => {
            Verdict::Challenger(format!(
                "forward mean {:.2}% is not at least {PROMOTION_MARGIN_PCT} point above the champion's {:.2}%",
                fwd.mean_return_pct, c.mean_return_pct
            ))
        }
        _ => Verdict::Promote(format!(
            "forward mean {:.2}% over {} tests in {} periods, all guards pass",
            fwd.mean_return_pct, fwd.tests, fwd.periods
        )),
    }
}

pub fn hash_of(json: &str) -> String {
    format!(
        "{:016x}",
        json.bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |h, x| (h ^ u64::from(x))
                .wrapping_mul(0x0100_0000_01b3))
    )
}

/// What `update` did.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub strategy_id: i64,
    pub name: String,
    pub status: String,
    pub reason: String,
    pub champion_id: Option<i64>,
}

/// Registers `params` (when new), applies the decision, and logs it. `forward` are the forward tests of all stored winners.
pub fn update(
    conn: &Connection,
    params: &XsParams,
    guards: &Guards,
    forward: &[Forward],
    now_ms: i64,
) -> Result<Outcome> {
    let json = serde_json::to_string(params)?;
    let hash = hash_of(&json);
    let name = format!("xs-setting {}", &hash[..8]);
    let existing: Option<i64> = conn
        .query_row(
            "SELECT strategy_id FROM strategy_params WHERE params_hash=?",
            [&hash],
            |r| r.get(0),
        )
        .optional()?;
    let id = match existing {
        Some(id) => id,
        None => {
            conn.execute("INSERT INTO strategies(name, version, git_commit, status, created_ms, notes) VALUES(?, 1, ?, 'challenger', ?, 'registered by the search')", (&name, crate::bot_commit(), now_ms))?;
            let id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO strategy_params(strategy_id, params_hash, params_json) VALUES(?,?,?)",
                (id, &hash, &json),
            )?;
            conn.execute(
                "INSERT INTO experiments(ts_ms, strategy_id, proposer, description, decision, reason) VALUES(?,?,'search','preferred setting of the period search','registered','new challenger')",
                (now_ms, id),
            )?;
            id
        }
    };
    let status: String = conn.query_row("SELECT status FROM strategies WHERE id=?", [id], |r| {
        r.get(0)
    })?;
    // the current champion (if any, and not this one) and its forward summary
    let champion: Option<(i64, XsParams)> = conn
        .query_row("SELECT s.id, p.params_json FROM strategies s JOIN strategy_params p ON p.strategy_id = s.id WHERE s.status='champion' AND s.id != ?", [id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .optional()?
        .map(|(cid, j)| serde_json::from_str(&j).map(|p| (cid, p)))
        .transpose()?;
    let fwd = forward_summary(forward, params);
    let champ_fwd = champion.as_ref().map(|(_, p)| forward_summary(forward, p));
    let verdict = if status == "champion" {
        Verdict::Challenger("already champion".into())
    } else {
        decide(guards, &fwd, champ_fwd.as_ref())
    };
    let (new_status, decision, reason) = match &verdict {
        Verdict::Promote(r) => ("champion", "promoted", r.clone()),
        Verdict::Reject(r) => ("rejected", "rejected", r.clone()),
        Verdict::Challenger(r) => (
            if status == "champion" {
                "champion"
            } else {
                "challenger"
            },
            "kept",
            r.clone(),
        ),
    };
    if new_status != status {
        conn.execute(
            "UPDATE strategies SET status=?, notes=? WHERE id=?",
            (new_status, &reason, id),
        )?;
        if new_status == "champion"
            && let Some((cid, _)) = &champion
        {
            conn.execute("UPDATE strategies SET status='retired', notes='replaced by a better champion' WHERE id=?", [cid])?;
        }
    }
    conn.execute(
        "INSERT INTO experiments(ts_ms, strategy_id, parent_id, proposer, description, oos_score, incumbent_score, decision, reason) VALUES(?,?,NULL,'search','registry decision',?,?,?,?)",
        rusqlite::params![now_ms, id, (fwd.tests > 0).then_some(fwd.mean_return_pct), champ_fwd.as_ref().map(|c| c.mean_return_pct), decision, reason],
    )?;
    let champion_id = conn
        .query_row(
            "SELECT id FROM strategies WHERE status='champion' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(Outcome {
        strategy_id: id,
        name,
        status: new_status.to_string(),
        reason,
        champion_id,
    })
}

/// One line per registered setting: id, name, status, forward summary computed from `forward`.
pub fn show(conn: &Connection, forward: &[Forward]) -> Result<Vec<String>> {
    let mut st = conn.prepare("SELECT s.id, s.name, s.status, p.params_json FROM strategies s JOIN strategy_params p ON p.strategy_id = s.id ORDER BY CASE s.status WHEN 'champion' THEN 0 WHEN 'challenger' THEN 1 ELSE 2 END, s.id")?;
    let rows: Vec<(i64, String, String, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<std::result::Result<_, _>>()?;
    rows.into_iter()
        .map(|(id, name, status, json)| {
            let p: XsParams = serde_json::from_str(&json)?;
            let f = forward_summary(forward, &p);
            Ok(format!(
                "#{id} {name} [{status}] forward: {} tests in {} periods, {} positive, mean {:.2}%",
                f.tests, f.periods, f.positive, f.mean_return_pct
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::PeriodMetrics;
    use thomas_core::store;

    fn params(stop: f64) -> XsParams {
        serde_json::from_value(serde_json::json!({"signal":"TrendScore","flip":true,"lookback":0,"hold":2,"top":1,"gross_leverage":1.0,"stop_pct":stop,"risk":{"add_pct":8.4,"take_profit_pct":49.6,"breaker_pct":null},"long_only":true})).expect("params parse")
    }

    fn fwd(p: &XsParams, on: i64, ret: f64) -> Forward {
        Forward {
            winner_period_start_ms: 0,
            on_period_start_ms: on,
            params: p.clone(),
            metrics: PeriodMetrics {
                return_pct: ret,
                max_drawdown_pct: 1.0,
                trades: 4,
                wins: 3,
                usable: ret > 0.0,
            },
        }
    }

    fn clean() -> Guards {
        Guards {
            win_rate: vec![(0.8, true)],
            share_of_best: vec![None],
            perturbed: 50,
            perturbed_usable_in_all: 45,
            beats_reference: vec![Some(true)],
            forward_tests: 2,
            forward_positive: 2,
        }
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::init(&c).unwrap();
        c
    }

    fn status(c: &Connection, id: i64) -> String {
        c.query_row("SELECT status FROM strategies WHERE id=?", [id], |r| {
            r.get(0)
        })
        .unwrap()
    }

    #[test]
    fn without_forward_evidence_a_setting_stays_a_challenger() {
        let c = db();
        let o = update(&c, &params(5.0), &clean(), &[], 1).unwrap();
        assert_eq!((o.status.as_str(), o.champion_id), ("challenger", None));
        assert!(o.reason.contains("0 later period"));
        // registering twice does not duplicate
        update(&c, &params(5.0), &clean(), &[], 2).unwrap();
        let n: i64 = c
            .query_row("SELECT count(*) FROM strategies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn forward_evidence_in_two_periods_with_clean_guards_promotes() {
        let c = db();
        let p = params(5.0);
        let f = vec![fwd(&p, 10, 2.0), fwd(&p, 20, 1.0)];
        let o = update(&c, &p, &clean(), &f, 1).unwrap();
        assert_eq!(o.status, "champion");
        assert_eq!(o.champion_id, Some(o.strategy_id));
    }

    #[test]
    fn failing_guards_block_promotion_even_with_positive_forward_tests() {
        let c = db();
        let p = params(5.0);
        let bad = Guards {
            win_rate: vec![(0.4, false)],
            ..clean()
        };
        let o = update(&c, &p, &bad, &[fwd(&p, 10, 2.0), fwd(&p, 20, 1.0)], 1).unwrap();
        assert_eq!(o.status, "challenger");
        assert!(o.reason.contains("guards fail"));
    }

    #[test]
    fn a_challenger_must_beat_the_champion_by_a_point_to_replace_it() {
        let c = db();
        let (a, b) = (params(5.0), params(6.0));
        let champ = update(&c, &a, &clean(), &[fwd(&a, 10, 2.0), fwd(&a, 20, 2.0)], 1).unwrap();
        // b is better by only 0.5 points: kept as challenger
        let f1 = vec![
            fwd(&a, 10, 2.0),
            fwd(&a, 20, 2.0),
            fwd(&b, 10, 2.5),
            fwd(&b, 20, 2.5),
        ];
        let o1 = update(&c, &b, &clean(), &f1, 2).unwrap();
        assert_eq!(o1.status, "challenger");
        assert_eq!(status(&c, champ.strategy_id), "champion");
        // b better by 1.5 points: promoted, a retired
        let f2 = vec![
            fwd(&a, 10, 2.0),
            fwd(&a, 20, 2.0),
            fwd(&b, 10, 3.5),
            fwd(&b, 20, 3.5),
        ];
        let o2 = update(&c, &b, &clean(), &f2, 3).unwrap();
        assert_eq!(o2.status, "champion");
        assert_eq!(status(&c, champ.strategy_id), "retired");
    }

    #[test]
    fn a_setting_that_loses_money_forward_in_two_periods_is_rejected() {
        let c = db();
        let p = params(5.0);
        let o = update(&c, &p, &clean(), &[fwd(&p, 10, -1.0), fwd(&p, 20, 0.5)], 1).unwrap();
        assert_eq!(o.status, "rejected");
        let logged: i64 = c
            .query_row(
                "SELECT count(*) FROM experiments WHERE decision='rejected'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(logged, 1);
    }
}
