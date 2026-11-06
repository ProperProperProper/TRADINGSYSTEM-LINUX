use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufRead, BufReader, Write},
    path::Path,
};

const TEACHERS: [&str; 3] = ["claude", "codex", "gemini"];

fn read(path: &Path) -> Result<Vec<Value>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    BufReader::new(file)
        .lines()
        .enumerate()
        .filter_map(|(n, line)| {
            let line = match line {
                Ok(value) => value,
                Err(error) => return Some(Err(error.into())),
            };
            if line.trim().is_empty() {
                return None;
            }
            Some(
                serde_json::from_str(&line)
                    .with_context(|| format!("invalid JSON at {}:{}", path.display(), n + 1)),
            )
        })
        .collect()
}

fn validate(row: &Value) -> Result<(String, String, Value)> {
    let teacher = row["teacher"].as_str().unwrap_or("");
    if !TEACHERS.contains(&teacher) || !row["event"].is_object() || !row["answer"].is_object() {
        bail!("missing teacher, event, or answer");
    }
    for key in [
        "ticker",
        "ts_ms",
        "day",
        "slot",
        "price",
        "rvol",
        "move_pct",
        "close_pos",
        "bar_ret_pct",
        "range_pct",
        "crossed",
    ] {
        if row["event"][key].is_null() {
            bail!("event missing {key}");
        }
    }
    if row.get("forward").is_some() || row.get("outcome").is_some() || row.get("pnl").is_some() {
        bail!("outcome/forward data cannot enter prompt");
    }
    let decision = row["answer"]["decision"].as_str().unwrap_or("");
    if decision != "enter" && decision != "abstain" {
        bail!("invalid decision");
    }
    let confidence = row["answer"]["confidence"]
        .as_f64()
        .filter(|x| (0.0..=1.0).contains(x))
        .ok_or_else(|| anyhow::anyhow!("invalid confidence"))?;
    let reason = row["answer"]["reason"].as_str().unwrap_or("").trim();
    if reason.is_empty() || reason.len() > 80 {
        bail!("invalid reason");
    }
    let key = format!("{}|{}", row["event"]["ticker"], row["event"]["ts_ms"]);
    Ok((
        key,
        teacher.to_string(),
        json!({"decision": decision, "confidence": confidence, "reason": reason}),
    ))
}

pub fn run(inputs: &[std::path::PathBuf], output: &Path) -> Result<Value> {
    let mut groups: BTreeMap<String, Vec<(String, Value)>> = BTreeMap::new();
    for path in inputs {
        for row in read(path)? {
            let (key, teacher, answer) =
                validate(&row).with_context(|| path.display().to_string())?;
            groups
                .entry(key)
                .or_default()
                .push((teacher, json!({"event": row["event"], "answer": answer})));
        }
    }
    let mut examples = Vec::new();
    for (key, rows) in groups {
        let mut votes: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, (_, row)) in rows.iter().enumerate() {
            votes
                .entry(row["answer"]["decision"].as_str().unwrap().to_string())
                .or_default()
                .push(index);
        }
        let (decision, winning) = votes
            .into_iter()
            .max_by_key(|(_, indexes)| indexes.len())
            .unwrap();
        let teachers: Vec<String> = winning.iter().map(|i| rows[*i].0.clone()).collect();
        if teachers.len() < 2 || winning.len() * 2 <= rows.len() {
            continue;
        }
        let event = &rows[winning[0]].1["event"];
        let observed = json!({"t": event["ticker"], "d": event["day"], "s": event["slot"], "p": event["price"], "r": event["rvol"], "m": event["move_pct"], "c": event["close_pos"], "b": event["bar_ret_pct"], "g": event["range_pct"], "x": event["crossed"]});
        let mut hasher = Sha256::new();
        hasher.update(serde_json::to_vec(
            &winning.iter().map(|i| &rows[*i]).collect::<Vec<_>>(),
        )?);
        let target = json!({"schema": 1, "decision": decision, "confidence": rows[winning[0]].1["answer"]["confidence"], "reason": rows[winning[0]].1["answer"]["reason"]});
        examples.push(json!({"messages": [{"role":"user", "content": format!("Decide enter or abstain from this closed 1-minute event. Return the schema-1 decision envelope and do not invent facts. EVENT {}", serde_json::to_string(&observed)?)}, {"role":"assistant", "content": serde_json::to_string(&target)?}], "source":{"event_key":key,"signal_ts_ms":event["ts_ms"],"teachers":teachers,"teacher_trace_hash":format!("{:x}", hasher.finalize())}, "label":{"status":"teacher-consensus","decision":decision,"validated_by":teachers}}));
    }
    if examples.is_empty() {
        bail!("refusing distillation: no event has consensus from two distinct teachers");
    }
    fs::create_dir_all(output)?;
    let mut file = File::create(output.join("train.jsonl"))?;
    for example in &examples {
        writeln!(file, "{}", serde_json::to_string(example)?)?;
    }
    let manifest = json!({"schema":1,"model":"ThomasAi","capability":"decision_distillation","examples":examples.len(),"teachers":TEACHERS,"rule":"same closed event, two-teacher consensus, no outcome data in prompt","promotion":"Rust causal evaluation required; never auto-activate"});
    fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let error = run(&[], dir.path()).unwrap_err().to_string();
        assert!(error.contains("no event"));
    }
}
