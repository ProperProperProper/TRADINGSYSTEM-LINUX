//! Persistent online research brain. It selects a valid, untested candle experiment;
//! deterministic lab tools execute and judge it. Every decision and outcome is durable SQLite state.

use crate::{brain_roles, context::Ctx};
use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const AUTHOR: &str = "brain-strategist";

pub fn init(ctx: &Ctx<'_>) -> Result<()> {
    ctx.mem.execute_batch("CREATE TABLE IF NOT EXISTS online_turns (
        id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, evidence TEXT NOT NULL, response TEXT NOT NULL,
        candidate TEXT, status TEXT NOT NULL, result TEXT NOT NULL DEFAULT '');
        CREATE TABLE IF NOT EXISTS online_memory (
        id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, subject TEXT NOT NULL UNIQUE, lesson TEXT NOT NULL,
        source_turn INTEGER, applied INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS online_skills (
        skill TEXT PRIMARY KEY, updated_ts INTEGER NOT NULL, evidence_count INTEGER NOT NULL DEFAULT 0,
        wins INTEGER NOT NULL DEFAULT 0, losses INTEGER NOT NULL DEFAULT 0, confidence REAL NOT NULL DEFAULT 0,
        guidance TEXT NOT NULL, uses INTEGER NOT NULL DEFAULT 0, last_used_ts INTEGER);
        -- Training-only successor. Legacy skills are preserved for audit because
        -- their labels came from holdout-derived verdicts.
        CREATE TABLE IF NOT EXISTS online_training_skills (
        skill TEXT PRIMARY KEY, updated_ts INTEGER NOT NULL, evidence_count INTEGER NOT NULL DEFAULT 0,
        wins INTEGER NOT NULL DEFAULT 0, losses INTEGER NOT NULL DEFAULT 0, confidence REAL NOT NULL DEFAULT 0,
        guidance TEXT NOT NULL, uses INTEGER NOT NULL DEFAULT 0, last_used_ts INTEGER);
        CREATE TABLE IF NOT EXISTS online_brain (
        name TEXT PRIMARY KEY, updated_ts INTEGER NOT NULL, state TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS online_turn_status_idx ON online_turns(status, candidate);")?;
    // One-way compatibility migration. Old tables are left untouched as a recoverable archive.
    for (old, copy) in [
        ("qwen_turns", "INSERT OR IGNORE INTO online_turns SELECT * FROM qwen_turns"),
        ("qwen_memory", "INSERT OR IGNORE INTO online_memory SELECT * FROM qwen_memory"),
        ("qwen_skills", "INSERT OR IGNORE INTO online_skills SELECT * FROM qwen_skills"),
        ("qwen_brain", "INSERT OR IGNORE INTO online_brain SELECT * FROM qwen_brain"),
    ] {
        let exists: bool = ctx.mem.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [old],
            |r| r.get(0),
        )?;
        if exists {
            ctx.mem.execute(copy, [])?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TrainingOutcome { paired_gain: f64, clean_return: f64, noisy_return: f64 }

impl TrainingOutcome {
    fn promising(self) -> bool { self.paired_gain > 0.0 && self.clean_return > 0.0 && self.noisy_return > 0.0 }
    fn lesson(self, candidate: &str) -> String {
        format!("Experiment {candidate} training evidence: paired gain {:+.2} points/block, clean return {:+.2} points/block, noisy return {:+.2} points/block; {} under the training-only shortlist rule.",
            self.paired_gain, self.clean_return, self.noisy_return, if self.promising() { "promising" } else { "not promising" })
    }
}

fn training_outcome(row: &Value, paired_gain: f64) -> Option<TrainingOutcome> {
    Some(TrainingOutcome { paired_gain, clean_return: row["a"]["h1"].as_f64()?, noisy_return: row["a"]["nh1"].as_f64()? })
}

fn update_skill(mem: &Connection, token: &str, outcome: TrainingOutcome) -> Result<()> {
    let won = i64::from(outcome.promising());
    let lost = i64::from(!outcome.promising());
    mem.execute(
        "INSERT INTO online_training_skills(skill,updated_ts,evidence_count,wins,losses,confidence,guidance)
         VALUES(?1,?2,1,?3,?4,?5,?6)
         ON CONFLICT(skill) DO UPDATE SET updated_ts=excluded.updated_ts,
         evidence_count=evidence_count+1,wins=wins+excluded.wins,losses=losses+excluded.losses,
         confidence=CAST(wins+excluded.wins AS REAL)/(evidence_count+1),guidance=excluded.guidance",
        params![token, crate::util::now_ts(), won, lost, won as f64,
            if won == 1 { format!("Prefer {token} only in a valid untested experiment; its latest training evidence was positive on clean and noisy data and beat the training baseline.") }
            else { format!("Treat {token} cautiously; its latest training evidence did not pass the clean, noisy, and paired-gain screen. Require new independent training evidence.") }],
    )?;
    Ok(())
}

fn skills(mem: &Connection, candidates: &[String]) -> Result<Vec<String>> {
    let wanted: std::collections::BTreeSet<&str> =
        candidates.iter().flat_map(|c| c.split('+')).collect();
    let mut st = mem.prepare("SELECT skill,evidence_count,wins,losses,confidence,guidance FROM online_training_skills ORDER BY confidence DESC,evidence_count DESC LIMIT 100")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, f64>(4)?,
            r.get::<_, String>(5)?,
        ))
    })?;
    Ok(rows
        .flatten()
        .filter(|r| wanted.contains(r.0.as_str()))
        .take(20)
        .map(|(skill, n, w, l, c, g)| {
            format!("{skill}: evidence={n}, wins={w}, losses={l}, confidence={c:.2}; {g}")
        })
        .collect())
}

fn update_brain(ctx: &Ctx<'_>, learned: usize) -> Result<String> {
    let turns: i64 = ctx
        .mem
        .query_row("SELECT count(*) FROM online_turns", [], |r| r.get(0))?;
    let memories: i64 = ctx
        .mem
        .query_row("SELECT count(*) FROM online_memory", [], |r| r.get(0))?;
    let skill_count: i64 = ctx
        .mem
        .query_row("SELECT count(*) FROM online_training_skills", [], |r| r.get(0))?;
    let state = format!("paper research only; {turns} decision turns, {memories} verified outcome memories, {skill_count} learned option skills; reconciled {learned} new outcomes this turn; objective: select useful untested stored-candle experiments while obeying walk-forward gates");
    ctx.mem.execute("INSERT INTO online_brain(name,updated_ts,state) VALUES('research',?1,?2) ON CONFLICT(name) DO UPDATE SET updated_ts=excluded.updated_ts,state=excluded.state", params![crate::util::now_ts(), state])?;
    Ok(state)
}

fn reconcile(ctx: &Ctx<'_>) -> Result<usize> {
    let mut st = ctx.mem.prepare("SELECT q.id, q.candidate FROM online_turns q JOIN test_queue t ON t.tokens=q.candidate WHERE q.status='queued' AND t.author=?1 AND t.status='tested'")?;
    let rows = st
        .query_map([AUTHOR], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let board = crate::strategy::leaderboard(ctx.mem)?;
    let paired: BTreeMap<String, f64> = crate::quant::paired_all(ctx.mem, "5h", crate::quant::Half::Training)?
        .into_iter().map(|p| (p.tokens, p.mean_diff)).collect();
    let mut learned = 0;
    for (id, candidate) in &rows {
        let outcome = board
            .iter()
            .find(|r| r["interval"] == "5h" && r["tokens"] == candidate.as_str())
            .and_then(|r| training_outcome(r, *paired.get(candidate)?));
        let Some(outcome) = outcome else {
            ctx.mem.execute("UPDATE online_turns SET status='training-evidence-unavailable', result='completed test has incomplete training evidence; no lesson recorded' WHERE id=?1", [id])?;
            continue;
        };
        let lesson = outcome.lesson(candidate);
        ctx.mem.execute("INSERT INTO online_memory(ts,subject,lesson,source_turn,applied) VALUES(?1,?2,?3,?4,1) ON CONFLICT(subject) DO UPDATE SET ts=excluded.ts,lesson=excluded.lesson,applied=1", params![crate::util::now_ts(), format!("training-experiment:{candidate}"), lesson, id])?;
        ctx.mem.execute(
            "UPDATE online_turns SET status='learned', result=?1 WHERE id=?2",
            params![lesson, id],
        )?;
        brain_roles::remember(
            ctx.mem,
            "strategist",
            "lesson",
            candidate,
            &lesson,
            "online training reconciliation",
        )?;
        for token in candidate.split('+') {
            update_skill(ctx.mem, token, outcome)?;
        }
        learned += 1;
    }
    Ok(learned)
}

/// Runs one learning/action turn. Failure is non-fatal so the deterministic strategist still runs.
pub fn run(ctx: &Ctx<'_>) -> Result<Vec<String>> {
    init(ctx)?;
    crate::teacher_engine::init(ctx.mem)?;
    let learned = reconcile(ctx)?;
    let brain_state = update_brain(ctx, learned)?;
    let candidates = crate::labtools::untested(ctx.mem, "5h", 12)?;
    if candidates.is_empty() {
        return Ok(vec![format!(
            "Online brain: reconciled {learned} outcomes; no valid untested candle experiments available"
        )]);
    }
    let lessons: Vec<String> = {
        let mut st = ctx
            .mem
            .prepare("SELECT lesson FROM online_memory WHERE subject LIKE 'training-experiment:%' ORDER BY id DESC LIMIT 12")?;
        let rows = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        rows
    };
    let inherited: Vec<String> = ctx.mem.prepare("SELECT role,kind,text FROM brain_facts WHERE kind IN ('lesson','skill') AND source != 'online outcome reconciliation' ORDER BY id DESC LIMIT 12")?
        .query_map([], |r| Ok(format!("[{} {}] {}", r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?.replace('\n', " ").chars().take(260).collect::<String>())))?
        .collect::<Result<_, _>>()?;
    let learned_skills = skills(ctx.mem, &candidates)?;
    let recent_actions: Vec<String> = {
        let mut st = ctx.mem.prepare("SELECT coalesce(candidate,'wait'),status,result FROM online_turns WHERE status != 'learned' OR result LIKE 'Experiment % training evidence:%' ORDER BY id DESC LIMIT 8")?;
        let rows = st
            .query_map([], |r| {
                Ok(format!(
                    "{}: {} ({})",
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?
                ))
            })?
            .collect::<Result<_, _>>()?;
        rows
    };
    // Every model sees the same persistent record: earlier answers from every model, so the
    // ensemble's memory is shared rather than per-model.
    let model_history: Vec<String> = {
        let mut st = ctx.mem.prepare(
            "SELECT model, substr(response,1,140) FROM online_model_responses WHERE evidence LIKE '%training-side evidence only v2%' ORDER BY id DESC LIMIT 12",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok(format!(
                    "{} answered {}",
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?
                ))
            })?
            .collect::<Result<_, _>>()?;
        rows
    };
    let evidence = json!({"source":"stored candle backtests, training-side evidence only v2","brain_state":brain_state,"candidates":candidates,"verified_outcome_memory":lessons,"verified_option_skills":learned_skills,"recent_actions_and_refusals":recent_actions,"earlier_model_answers":model_history,"inherited_lab_lessons":inherited});
    let (decision, teachers, validator) =
        crate::teacher_engine::decide(ctx.mem, &evidence, &candidates)?;
    let raw = serde_json::to_string(&decision)?;
    let reason = format!(
        "{} [teachers: {teachers}; validator: {validator}]",
        decision.reason
    );
    let candidate = decision.candidate.map(|i| candidates[i].clone());
    let status = if candidate.is_some() {
        "proposed"
    } else {
        "idle"
    };
    ctx.mem.execute(
        "INSERT INTO online_turns(ts,evidence,response,candidate,status) VALUES(?1,?2,?3,?4,?5)",
        params![
            crate::util::now_ts(),
            evidence.to_string(),
            raw,
            candidate,
            status
        ],
    )?;
    let turn = ctx.mem.last_insert_rowid();
    let Some(candidate) = candidate else {
        return Ok(vec![format!(
            "Online brain: reconciled {learned} outcomes; chose to wait"
        )]);
    };
    for token in candidate.split('+') {
        ctx.mem.execute(
            "UPDATE online_training_skills SET uses=uses+1,last_used_ts=?1 WHERE skill=?2",
            params![crate::util::now_ts(), token],
        )?;
    }
    let options: Vec<String> = candidate.split('+').map(str::to_string).collect();
    let why = if reason.trim().is_empty() {
        "the validated free-model ensemble selected this untested candle experiment using prior stored outcomes"
    } else {
        reason.trim()
    };
    let result = crate::labtools::call(
        ctx,
        "lab_queue_test",
        &json!({"interval":"5h","options":options,"why":why,"author":AUTHOR}),
    );
    match result {
        Ok(msg) => {
            let queued = msg.starts_with("queued");
            ctx.mem.execute(
                "UPDATE online_turns SET status=?1,result=?2 WHERE id=?3",
                params![if queued { "queued" } else { "rejected" }, msg, turn],
            )?;
            Ok(vec![format!(
                "Online brain: reconciled {learned} outcomes; {candidate} -> {msg}"
            )])
        }
        Err(e) => {
            let msg = format!("{e:#}");
            ctx.mem.execute(
                "UPDATE online_turns SET status='rejected',result=?1 WHERE id=?2",
                params![msg, turn],
            )?;
            Ok(vec![format!(
                "Online proposal {candidate} was safely refused: {e:#}"
            )])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn training_outcome_ignores_holdout_status_and_requires_robust_training_evidence() {
        let strong = TrainingOutcome { paired_gain: 1.2, clean_return: 2.0, noisy_return: 0.4 };
        assert!(strong.promising());
        assert!(!TrainingOutcome { noisy_return: -0.1, ..strong }.promising());
        let validated = json!({"status":"validated", "a":{"h1":2.0,"nh1":0.4}});
        let failed = json!({"status":"failed-holdout", "a":{"h1":2.0,"nh1":0.4}});
        assert_eq!(training_outcome(&validated, 1.2), training_outcome(&failed, 1.2));
        let lesson = strong.lesson("obv");
        assert!(lesson.contains("training evidence") && !lesson.contains("validated") && !lesson.contains("holdout"));
    }

    #[test]
    fn verified_outcomes_build_and_update_a_persistent_skill() {
        let mem = Connection::open_in_memory().unwrap();
        mem.execute_batch("CREATE TABLE online_training_skills (skill TEXT PRIMARY KEY, updated_ts INTEGER NOT NULL, evidence_count INTEGER NOT NULL DEFAULT 0, wins INTEGER NOT NULL DEFAULT 0, losses INTEGER NOT NULL DEFAULT 0, confidence REAL NOT NULL DEFAULT 0, guidance TEXT NOT NULL, uses INTEGER NOT NULL DEFAULT 0, last_used_ts INTEGER);").unwrap();
        update_skill(&mem, "obv", TrainingOutcome { paired_gain: 1.0, clean_return: 2.0, noisy_return: 0.5 }).unwrap();
        update_skill(&mem, "obv", TrainingOutcome { paired_gain: 1.0, clean_return: 2.0, noisy_return: -0.5 }).unwrap();
        let row: (i64, i64, i64, f64) = mem
            .query_row(
                "SELECT evidence_count,wins,losses,confidence FROM online_training_skills WHERE skill='obv'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((row.0, row.1, row.2), (2, 1, 1));
        assert!((row.3 - 0.5).abs() < 1e-9);
    }
}
