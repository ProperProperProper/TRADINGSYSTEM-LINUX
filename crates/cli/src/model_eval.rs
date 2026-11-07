//! Rust-owned ThomasAi corpus and evaluation for the sole 20-session momentum strategy.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::File,
    hash::{Hash, Hasher},
    io::{BufRead, BufReader, Write},
    path::Path,
};
use thomas_engine::model_decision::{self, Decision, Envelope};

const STRATEGY_VERSION: &str = "cross-sectional-momentum-1m:v1:lookback20";

#[derive(Clone, Debug)]
struct Event {
    session: String,
    built_ms: i64,
    cutoff_day: i64,
    moomoo: String,
    external: String,
    rank: i64,
    score: f64,
    observations: i64,
    outcome: Option<f64>,
}

fn store_path(root: &Path) -> std::path::PathBuf {
    root.join("data/thomas.sqlite3")
}

fn load(root: &Path, completed_only: bool) -> Result<Vec<Event>> {
    let path = store_path(root);
    let db =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| {
                format!(
                    "cannot open {}; run thomas momentum-forward first",
                    path.display()
                )
            })?;
    let condition = if completed_only {
        " WHERE outcome_complete=1"
    } else {
        ""
    };
    let mut q = db.prepare(&format!("SELECT session,built_ms,cutoff_day,moomoo_symbol,external_symbol,rank,score,observations,net_return_pct FROM momentum_forward_candidates{condition} ORDER BY session,rank"))
        .context("momentum forward tables are absent; run thomas momentum-forward first")?;
    let rows = q
        .query_map([], |r| {
            Ok(Event {
                session: r.get(0)?,
                built_ms: r.get(1)?,
                cutoff_day: r.get(2)?,
                moomoo: r.get(3)?,
                external: r.get(4)?,
                rank: r.get(5)?,
                score: r.get(6)?,
                observations: r.get(7)?,
                outcome: r.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        !rows.is_empty(),
        "no {}momentum events are stored",
        if completed_only { "completed " } else { "" }
    );
    Ok(rows)
}

fn observed(event: &Event) -> Value {
    json!({"session":event.session,"rank":event.rank,"score":event.score,"observations":event.observations,"lookback_sessions":20,"data_cutoff_day":event.cutoff_day})
}

fn prompt(event: &Event) -> String {
    format!(
        "FROZEN_MOMENTUM_CANDIDATE {}\nOUTPUT_JSON schema decision confidence reason",
        observed(event)
    )
}

pub fn events(root: &Path, output: &Path) -> Result<String> {
    // Shadow replay is allowed only once the Rust ledger has closed the whole
    // session.  The prompt still contains frozen pre-session fields only.
    let rows = load(root, true)?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = File::create(output)?;
    for e in &rows {
        writeln!(
            file,
            "{}",
            json!({"schema":1,"strategy":STRATEGY_VERSION,"session":e.session,"signal_ts_ms":e.built_ms,"symbol":{"moomoo":e.moomoo,"external":e.external},"rank":e.rank,"score":e.score,"observations":e.observations,"data_cutoff":e.cutoff_day.to_string()})
        )?;
    }
    Ok(format!(
        "wrote {} frozen momentum events from {}",
        rows.len(),
        store_path(root).display()
    ))
}

fn metrics(events: &[&Event]) -> (f64, f64, usize, usize) {
    let mut sessions: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for event in events {
        if let Some(outcome) = event.outcome {
            sessions.entry(&event.session).or_default().push(outcome);
        }
    }
    let mut equity = 1.0_f64;
    let mut peak = 1.0_f64;
    let mut drawdown = 0.0_f64;
    let mut wins = 0;
    let mut trades = 0;
    for values in sessions.values() {
        let day = values.iter().sum::<f64>() / values.len() as f64;
        equity *= 1.0 + day / 100.0;
        peak = peak.max(equity);
        drawdown = drawdown.max(100.0 * (peak - equity) / peak);
        wins += values.iter().filter(|x| **x > 0.0).count();
        trades += values.len();
    }
    (100.0 * (equity - 1.0), drawdown, trades, wins)
}

pub fn run(
    root: &Path,
    decisions_path: &Path,
    output: &Path,
    now_ms: i64,
    command: &str,
) -> Result<String> {
    let events = load(root, true)?;
    let mut decisions = Vec::new();
    let mut raw = Vec::new();
    for (line_no, line) in BufReader::new(File::open(decisions_path)?)
        .lines()
        .enumerate()
    {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let envelope = model_decision::parse(&line, now_ms)
            .with_context(|| format!("invalid decision line {}", line_no + 1))?;
        ensure!(
            matches!(envelope.decision, Decision::Enter | Decision::Abstain),
            "momentum shadow decisions must be enter or abstain"
        );
        ensure!(
            envelope.strategy_version == STRATEGY_VERSION,
            "decision has the wrong momentum strategy version"
        );
        decisions.push(envelope);
        raw.push(line);
    }
    ensure!(!decisions.is_empty(), "decision stream is empty");
    let model_version = decisions[0].model_version.clone();
    ensure!(
        decisions.iter().all(|x| x.model_version == model_version),
        "decision stream mixes model versions"
    );
    let by_key: HashMap<(&str, i64), &Event> = events
        .iter()
        .map(|e| ((e.external.as_str(), e.built_ms), e))
        .collect();
    let mut seen = HashSet::new();
    let mut selected_sessions = HashSet::new();
    for d in &decisions {
        let event = by_key
            .get(&(d.symbol.external.as_str(), d.signal_ts_ms))
            .context("decision does not match a completed frozen momentum candidate")?;
        ensure!(
            d.symbol.moomoo == event.moomoo,
            "decision symbol pair does not match the canonical map"
        );
        ensure!(
            d.data_cutoff == event.cutoff_day.to_string(),
            "decision cutoff does not match its frozen selection"
        );
        ensure!(
            seen.insert((d.symbol.external.as_str(), d.signal_ts_ms)),
            "duplicate momentum decision"
        );
        selected_sessions.insert(event.session.as_str());
    }
    let evaluated_events: Vec<_> = events
        .iter()
        .filter(|e| selected_sessions.contains(e.session.as_str()))
        .collect();
    ensure!(
        seen.len() == evaluated_events.len(),
        "evaluation requires one decision for every candidate in each submitted session (got {}, need {})",
        seen.len(),
        evaluated_events.len()
    );
    let base_events = evaluated_events;
    let entered: HashSet<_> = decisions
        .iter()
        .filter(|d| d.decision == Decision::Enter)
        .map(|d| (d.symbol.external.as_str(), d.signal_ts_ms))
        .collect();
    let candidate_events: Vec<_> = base_events
        .iter()
        .copied()
        .filter(|e| entered.contains(&(e.external.as_str(), e.built_ms)))
        .collect();
    let base = metrics(&base_events);
    let candidate = metrics(&candidate_events);
    let document = json!({
        "source":decisions_path.display().to_string(),"data_cutoff":base_events.iter().map(|e|e.cutoff_day).max().unwrap_or_default().to_string(),
        "strategy_version":STRATEGY_VERSION,"model_version":model_version,"rust_command":command,
        "self_capability":{"valid_envelopes":decisions.len(),"enter_envelopes":candidate.2,"causal_violations":0},
        "base":{"return_pct":base.0,"max_drawdown_pct":base.1,"trades":base.2,"wins":base.3},
        "candidate":{"return_pct":candidate.0,"max_drawdown_pct":candidate.1,"trades":candidate.2,"wins":candidate.3}
    });
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output, serde_json::to_vec_pretty(&document)?)?;
    persist(root, &document, &decisions, &raw, &by_key, now_ms)?;
    Ok(format!(
        "wrote momentum evaluation: {} (base {:.3}%/{} candidates; ThomasAi {:.3}%/{})",
        output.display(),
        base.0,
        base.2,
        candidate.0,
        candidate.2
    ))
}

fn persist(
    root: &Path,
    document: &Value,
    decisions: &[Envelope],
    raw: &[String],
    events: &HashMap<(&str, i64), &Event>,
    now_ms: i64,
) -> Result<()> {
    let mut db = rusqlite::Connection::open(store_path(root))?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS model_evaluation_runs(id INTEGER PRIMARY KEY,created_ms INTEGER NOT NULL,source TEXT NOT NULL,data_cutoff TEXT NOT NULL,strategy_version TEXT NOT NULL,rust_command TEXT NOT NULL,evaluation_json TEXT NOT NULL); CREATE TABLE IF NOT EXISTS model_decisions(id INTEGER PRIMARY KEY,run_id INTEGER NOT NULL REFERENCES model_evaluation_runs(id),signal_ts_ms INTEGER NOT NULL,external_symbol TEXT NOT NULL,decision TEXT NOT NULL,confidence REAL NOT NULL,raw_json TEXT NOT NULL); CREATE TABLE IF NOT EXISTS model_decision_outcomes(decision_id INTEGER PRIMARY KEY REFERENCES model_decisions(id),deterministic_eligible INTEGER NOT NULL,agreement INTEGER NOT NULL,outcome_pct REAL,exit_reason TEXT,evaluated_ms INTEGER NOT NULL);")?;
    let tx = db.transaction()?;
    tx.execute("INSERT INTO model_evaluation_runs(created_ms,source,data_cutoff,strategy_version,rust_command,evaluation_json) VALUES(?,?,?,?,?,?)", rusqlite::params![now_ms,document["source"].as_str().unwrap_or(""),document["data_cutoff"].as_str().unwrap_or(""),STRATEGY_VERSION,document["rust_command"].as_str().unwrap_or(""),serde_json::to_string(document)?])?;
    let run_id = tx.last_insert_rowid();
    for (decision, line) in decisions.iter().zip(raw) {
        tx.execute("INSERT INTO model_decisions(run_id,signal_ts_ms,external_symbol,decision,confidence,raw_json) VALUES(?,?,?,?,?,?)", rusqlite::params![run_id,decision.signal_ts_ms,decision.symbol.external,format!("{:?}",decision.decision).to_lowercase(),decision.confidence,line])?;
        let id = tx.last_insert_rowid();
        let event = events[&(decision.symbol.external.as_str(), decision.signal_ts_ms)];
        tx.execute("INSERT INTO model_decision_outcomes(decision_id,deterministic_eligible,agreement,outcome_pct,exit_reason,evaluated_ms) VALUES(?,1,?,?,?,?)", rusqlite::params![id,(decision.decision==Decision::Enter) as i64,event.outcome,"next-session close after costs",now_ms])?;
    }
    tx.commit()?;
    Ok(())
}

fn brain_id(root: &Path) -> Result<i64> {
    let db = rusqlite::Connection::open_with_flags(
        root.join("papercompmoomoo/data/brain.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    db.query_row("SELECT id FROM memories WHERE status='active' AND strategy_version IS NOT NULL AND trim(strategy_version)<>'' ORDER BY id LIMIT 1", [], |r| r.get(0)).context("no active versioned brain evidence exists")
}

fn corpus_rows(root: &Path, from_shadow: bool) -> Result<Vec<(String, Value)>> {
    let events = load(root, true)?;
    let brain = brain_id(root)?;
    let mut rows = Vec::new();
    for event in events {
        let outcome = event
            .outcome
            .context("completed momentum event has no outcome")?;
        let enter = outcome > 0.0;
        let answer = json!({"schema":1,"decision":if enter{"enter"}else{"abstain"},"confidence":if enter{1.0}else{0.0},"reason":"Rust-scored next-session outcome after costs"}).to_string();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        format!("{}:{}:{}", event.session, event.external, event.built_ms).hash(&mut hasher);
        rows.push((event.session.clone(),json!({"messages":[{"role":"user","content":prompt(&event)},{"role":"assistant","content":answer}],"source":{"brain_ids":[brain],"board_ids":[],"data_cutoff":event.cutoff_day.to_string(),"strategy_version":STRATEGY_VERSION,"source_hash":format!("{:016x}",hasher.finish()),"source":"rust","via":if from_shadow{"momentum_shadow_outcome"}else{"momentum_forward_outcome"},"topic":"momentum_decision"},"label":{"status":"accepted","evidence":"real next-session close after costs; outcome excluded from prompt","validated_by":["rust"]}})));
    }
    Ok(rows)
}

fn write_corpus(root: &Path, output: &Path, from_shadow: bool) -> Result<String> {
    let rows = corpus_rows(root, from_shadow)?;
    let days: Vec<_> = rows
        .iter()
        .map(|x| x.0.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    ensure!(
        days.len() >= 3,
        "need at least three completed momentum sessions for causal train/valid/test splits"
    );
    let train_end = (days.len() * 70 / 100).max(1);
    let valid_end = (days.len() * 85 / 100)
        .max(train_end + 1)
        .min(days.len() - 1);
    std::fs::create_dir_all(output)?;
    let mut counts = BTreeMap::new();
    for (name, range) in [
        ("train", 0..train_end),
        ("valid", train_end..valid_end),
        ("test", valid_end..days.len()),
    ] {
        let selected: Vec<_> = rows
            .iter()
            .filter(|(day, _)| days[range.clone()].contains(day))
            .collect();
        let mut file = File::create(output.join(format!("{name}.jsonl")))?;
        let mut enter = 0;
        let mut abstain = 0;
        for (_, row) in &selected {
            let answer = row["messages"][1]["content"].as_str().unwrap_or("");
            enter += usize::from(answer.contains("\"decision\":\"enter\""));
            abstain += usize::from(answer.contains("\"decision\":\"abstain\""));
            writeln!(file, "{row}")?;
        }
        ensure!(
            enter > 0 && abstain > 0,
            "{name} split needs real enter and abstain outcomes (enter {enter}, abstain {abstain})"
        );
        counts.insert(
            name,
            json!({"rows":selected.len(),"enter":enter,"abstain":abstain}),
        );
    }
    std::fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(
            &json!({"schema":2,"model":"ThomasAi","capability":"momentum_decision","strategy_version":STRATEGY_VERSION,"counts":counts,"rule":"frozen pre-session inputs only; real next-session outcomes after costs are labels; chronological session splits; Rust evaluation required","promotion":"shadow only; never grants order authority"}),
        )?,
    )?;
    Ok(format!(
        "wrote {} causal momentum examples to {}",
        rows.len(),
        output.display()
    ))
}

pub fn decision_corpus(root: &Path, output: &Path) -> Result<String> {
    write_corpus(root, output, false)
}
pub fn shadow_corpus(root: &Path, output: &Path) -> Result<String> {
    write_corpus(root, output, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metrics_compound_sessions_and_measure_drawdown() {
        let a = Event {
            session: "a".into(),
            built_ms: 1,
            cutoff_day: 1,
            moomoo: "US.A".into(),
            external: "A".into(),
            rank: 1,
            score: 1.0,
            observations: 20,
            outcome: Some(10.0),
        };
        let b = Event {
            session: "b".into(),
            outcome: Some(-10.0),
            ..a.clone()
        };
        let result = metrics(&[&a, &b]);
        assert!((result.0 + 1.0).abs() < 1e-9);
        assert!((result.1 - 10.0).abs() < 1e-9);
        assert_eq!((result.2, result.3), (2, 1));
    }
}
