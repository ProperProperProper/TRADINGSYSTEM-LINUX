//! Rust-owned admission gate for a trained ThomasAi adapter.
//!
//! Training happens outside Rust, but activation does not: the candidate must
//! be complete, provenance-backed, and attached to an accepted validation row
//! whose Rust status is `pass`.
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::Value;
use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

fn validate_corpus_file(path: &Path, name: &str) -> Result<usize> {
    let file = File::open(path).with_context(|| format!("cannot read {name}"))?;
    let mut count = 0;
    for (line, raw) in BufReader::new(file).lines().enumerate() {
        let raw = raw.with_context(|| format!("cannot read {name} line {}", line + 1))?;
        let item: Value = serde_json::from_str(&raw)
            .with_context(|| format!("{name} line {} is not JSON", line + 1))?;
        let source = item
            .get("source")
            .context(format!("{name} line {} has no source", line + 1))?;
        anyhow::ensure!(
            source["brain_ids"]
                .as_array()
                .is_some_and(|ids| !ids.is_empty()),
            "{name} line {} has no brain provenance",
            line + 1
        );
        for field in ["data_cutoff", "strategy_version", "source_hash"] {
            anyhow::ensure!(
                source[field]
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty()),
                "{name} line {} source.{field} is missing",
                line + 1
            );
        }
        let label = item
            .get("label")
            .context(format!("{name} line {} has no label", line + 1))?;
        anyhow::ensure!(
            matches!(
                label["status"].as_str(),
                Some("accepted" | "rejected" | "uncertain")
            ),
            "{name} line {} has invalid label.status",
            line + 1
        );
        count += 1;
    }
    anyhow::ensure!(count > 0, "{name} is empty");
    Ok(count)
}

pub fn check(root: &Path, candidate: &Path, database: &Path, validation_id: i64) -> Result<String> {
    let manifest_path = candidate.join("manifest.json");
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("missing {}", manifest_path.display()))?,
    )?;
    anyhow::ensure!(
        manifest["schema"].as_i64() == Some(3),
        "candidate manifest schema must be 3"
    );
    anyhow::ensure!(
        manifest["model"].as_str() == Some("ThomasAi")
            && manifest["base_checkpoint"]["name"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty())
            && manifest["base_checkpoint"]["files"]
                .as_object()
                .is_some_and(|files| !files.is_empty()),
        "candidate manifest must identify the imported ThomasAi base checkpoint"
    );
    anyhow::ensure!(
        manifest["promotion"]
            .as_str()
            .is_some_and(|s| s.contains("never auto-activate")),
        "candidate manifest lacks the no-auto-activation rule"
    );
    let brain_ids = manifest["brain_ids"].as_array().map_or(0, Vec::len);
    let capability = manifest["capability"].as_str().unwrap_or("research");
    if capability == "decision" {
        let examples = manifest["counts"]
            .as_object()
            .map(|counts| counts.values().filter_map(Value::as_u64).sum::<u64>())
            .unwrap_or(0);
        anyhow::ensure!(
            examples >= 20,
            "decision candidate has only {examples} corpus examples; need at least 20"
        );
    } else {
        anyhow::ensure!(
            brain_ids >= 20,
            "candidate provenance has only {brain_ids} brain rows; need at least 20"
        );
    }
    for name in ["train.jsonl", "valid.jsonl", "test.jsonl"] {
        anyhow::ensure!(
            candidate.join(name).is_file(),
            "candidate is missing {name}"
        );
        validate_corpus_file(&candidate.join(name), name)?;
    }
    anyhow::ensure!(
        candidate.join("adapter/adapters.safetensors").is_file(),
        "candidate adapter weights are missing"
    );
    let evaluation_path = candidate.join("evaluation.json");
    let evaluation: Value = serde_json::from_str(
        &std::fs::read_to_string(&evaluation_path)
            .with_context(|| format!("missing evaluator output {}", evaluation_path.display()))?,
    )?;
    anyhow::ensure!(
        evaluation["self_capability"]["valid_envelopes"]
            .as_u64()
            .is_some_and(|n| n > 0)
            && evaluation["self_capability"]["causal_violations"].as_u64() == Some(0),
        "evaluation does not prove valid causal model envelopes"
    );
    for field in ["source", "data_cutoff", "strategy_version", "rust_command"] {
        anyhow::ensure!(
            evaluation[field]
                .as_str()
                .is_some_and(|value| !value.trim().is_empty()),
            "evaluation.{field} must identify real provenance"
        );
    }
    let base = &evaluation["base"];
    let challenger = &evaluation["candidate"];
    let base_return = base["return_pct"]
        .as_f64()
        .context("evaluation.base.return_pct is missing")?;
    let candidate_return = challenger["return_pct"]
        .as_f64()
        .context("evaluation.candidate.return_pct is missing")?;
    let base_drawdown = base["max_drawdown_pct"]
        .as_f64()
        .context("evaluation.base.max_drawdown_pct is missing")?;
    let candidate_drawdown = challenger["max_drawdown_pct"]
        .as_f64()
        .context("evaluation.candidate.max_drawdown_pct is missing")?;
    let candidate_trades = challenger["trades"]
        .as_u64()
        .context("evaluation.candidate.trades is missing")?;
    anyhow::ensure!(
        candidate_return >= base_return,
        "candidate return {candidate_return:.4} is below base {base_return:.4}"
    );
    anyhow::ensure!(
        candidate_drawdown <= base_drawdown,
        "candidate drawdown {candidate_drawdown:.4} is above base {base_drawdown:.4}"
    );
    anyhow::ensure!(candidate_trades > 0, "candidate has no evaluated trades");
    let c = Connection::open(database)
        .with_context(|| format!("cannot open {}", database.display()))?;
    let row: (String, String, String) = c
        .query_row(
            "SELECT verdict, rust_status, model_version FROM validations WHERE id=?",
            [validation_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .with_context(|| format!("validation #{validation_id} does not exist"))?;
    anyhow::ensure!(
        row.0 == "accept",
        "validation #{validation_id} verdict is {:?}, not accept",
        row.0
    );
    anyhow::ensure!(
        row.1 == "pass",
        "validation #{validation_id} Rust status is {:?}, not pass",
        row.1
    );
    anyhow::ensure!(
        !row.2.is_empty(),
        "validation #{validation_id} has no model version"
    );
    let _ = root;
    Ok(format!(
        "candidate admitted for controlled activation: {} (validation #{validation_id})",
        candidate.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use tempfile::tempdir;

    #[test]
    fn refuses_without_a_rust_pass() {
        let dir = tempdir().unwrap();
        let candidate = dir.path().join("candidate");
        std::fs::create_dir_all(candidate.join("adapter")).unwrap();
        std::fs::write(candidate.join("manifest.json"), r#"{"schema":3,"model":"ThomasAi","base_checkpoint":{"name":"qwen","files":{"config.json":{"sha256":"x"}}},"promotion":"held-out causal evaluation and Rust safety checks required; never auto-activate","brain_ids":[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20]}"#).unwrap();
        for name in ["train.jsonl", "valid.jsonl", "test.jsonl"] {
            std::fs::write(candidate.join(name), "{}\n").unwrap();
        }
        std::fs::write(candidate.join("adapter/adapters.safetensors"), "candidate").unwrap();
        std::fs::write(candidate.join("evaluation.json"), r#"{"source":"test fixture","data_cutoff":"test cutoff","strategy_version":"test strategy","rust_command":"test evaluator","base":{"return_pct":1.0,"max_drawdown_pct":5.0},"candidate":{"return_pct":0.5,"max_drawdown_pct":4.0,"trades":2}}"#).unwrap();
        let db = dir.path().join("board.sqlite3");
        let c = Connection::open(&db).unwrap();
        c.execute_batch("CREATE TABLE validations(id INTEGER PRIMARY KEY, verdict TEXT, rust_status TEXT, model_version TEXT);") .unwrap();
        c.execute(
            "INSERT INTO validations VALUES(1,'accept','pending','v1')",
            params![],
        )
        .unwrap();
        assert!(check(dir.path(), &candidate, &db, 1).is_err());
    }
}
