//! Checked online research ensemble. Models propose stored-candle experiments only;
//! Claude or an OpenCode free model vets them, and deterministic walk-forward evidence is authoritative.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, process::Command};

pub const FREE_MODELS: [&str; 10] = [
    "opencode/beam-free",
    "opencode/big-pickle",
    "opencode/deepseek-v4.1-flash-free",
    "opencode/glm-5.3-flash-free",
    "opencode/longcat-2.5-preview-free",
    "opencode/mimo-v2.6-flash-free",
    "opencode/nemotron-3-ultra-free",
    "opencode/nemotron-3.5-lightning-free",
    "opencode/nemotron-3.5-super-vl-free",
    "opencode/space-bunny-free",
];

/// Free hosted models are deliberately restricted to OpenCode's own provider namespace.
/// This prevents a database row or future configuration change from sending free-tier work
/// directly to a provider SDK or API endpoint.
fn ensure_opencode_free_model(model: &str) -> Result<()> {
    if !model.starts_with("opencode/") || !FREE_MODELS.contains(&model) {
        anyhow::bail!("refusing non-OpenCode free model route: {model}");
    }
    Ok(())
}

const OPENCODE_PROVENANCE_ERROR: &str =
    "opencode's free tier can only be used from within opencode";

/// Provider credentials, endpoint overrides, and generic proxies must never leak into the
/// OpenCode child. Free-tier traffic is allowed to use only OpenCode's own saved session and
/// built-in provider route.
const BLOCKED_OPENCODE_ENV: [&str; 29] = [
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "OPENAI_COMPATIBLE_BASE_URL",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_GENERATIVE_AI_API_KEY",
    "GROQ_API_KEY",
    "MISTRAL_API_KEY",
    "COHERE_API_KEY",
    "DEEPSEEK_API_KEY",
    "XAI_API_KEY",
    "OPENROUTER_API_KEY",
    "AZURE_OPENAI_API_KEY",
    "AZURE_OPENAI_ENDPOINT",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "OPENCODE_CONFIG",
    "OPENCODE_CONFIG_CONTENT",
    "OPENCODE_CONFIG_DIR",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];

fn is_opencode_provenance_error(message: &str) -> bool {
    message.to_ascii_lowercase().contains(OPENCODE_PROVENANCE_ERROR)
}

pub fn init(mem: &Connection) -> Result<()> {
    mem.execute_batch("CREATE TABLE IF NOT EXISTS teacher_models (
        model TEXT PRIMARY KEY, calls INTEGER NOT NULL DEFAULT 0, failures INTEGER NOT NULL DEFAULT 0,
        active INTEGER NOT NULL DEFAULT 1, cooldown_until INTEGER NOT NULL DEFAULT 0, last_used INTEGER);
        CREATE TABLE IF NOT EXISTS online_model_responses (
        id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, model TEXT NOT NULL, evidence TEXT NOT NULL,
        response TEXT NOT NULL, accepted INTEGER NOT NULL DEFAULT 0, validator TEXT NOT NULL DEFAULT '');")?;
    for model in FREE_MODELS {
        mem.execute(
            "INSERT OR IGNORE INTO teacher_models(model) VALUES(?1)",
            [model],
        )?;
    }
    Ok(())
}

/// Least-used healthy teacher, with failures weighted heavily to spread work and avoid bad endpoints.
pub fn pick(mem: &Connection, now: i64) -> Result<Option<String>> {
    init(mem)?;
    Ok(mem.query_row("SELECT model FROM teacher_models WHERE active=1 AND cooldown_until<=?1 ORDER BY calls+failures*4,last_used NULLS FIRST,model LIMIT 1", [now], |r| r.get(0)).ok())
}

pub fn record_call(mem: &Connection, model: &str, ok: bool, now: i64) -> Result<()> {
    init(mem)?;
    mem.execute("UPDATE teacher_models SET calls=calls+1,failures=failures+?1,last_used=?2,cooldown_until=CASE WHEN ?1=1 THEN ?2+3600 ELSE cooldown_until END WHERE model=?3", params![i64::from(!ok), now, model])?;
    Ok(())
}

fn approved(s: &str) -> bool {
    s.lines()
        .any(|line| line.trim().eq_ignore_ascii_case("approve"))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Decision {
    pub candidate: Option<usize>,
    pub reason: String,
}

/// Raw model reply. `candidate` is kept as free JSON because models answer with either the
/// candidate's exact name (most common), its index, or null. Resolving happens against the
/// supplied list so a name and an index mean the same thing.
#[derive(Debug, Deserialize)]
struct RawDecision {
    #[serde(default)]
    candidate: Option<serde_json::Value>,
    #[serde(default)]
    reason: String,
}

/// The model's answer text from an `opencode --format json` transcript: the last event that
/// carries `part.text`, else the raw output.
fn last_text(output: &str) -> String {
    for line in output.lines().rev() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(s) = v
                .get("part")
                .and_then(|p| p.get("text"))
                .and_then(|x| x.as_str())
            {
                return s.into();
            }
        }
    }
    output.to_string()
}

fn extract(output: &str) -> Result<RawDecision> {
    let text = last_text(output);
    // Free models often wrap the answer in commentary (for example a "maximum steps" summary)
    // that contains its own braces. Scan every balanced `{...}` span and keep the last one that
    // parses as an object carrying the fields we asked for.
    let bytes = text.as_bytes();
    let mut best: Option<RawDecision> = None;
    for start in 0..bytes.len() {
        if bytes[start] != b'{' {
            continue;
        }
        for end in (start + 1..bytes.len()).rev() {
            if bytes[end] != b'}' {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&text[start..=end]) else {
                continue;
            };
            if value.get("candidate").is_none() && value.get("reason").is_none() {
                continue;
            }
            if let Ok(decision) = serde_json::from_value::<RawDecision>(value) {
                best = Some(decision);
            }
            break;
        }
    }
    best.context("teacher returned no usable JSON")
}

/// Maps a model's reply to an index into `names`: `Some(None)` means a deliberate wait,
/// `Some(Some(i))` a valid index, and `None` an out-of-range or unusable answer.
fn resolve(candidate: Option<&serde_json::Value>, names: &[String]) -> Option<Option<usize>> {
    match candidate.unwrap_or(&serde_json::Value::Null) {
        serde_json::Value::Null => Some(None),
        serde_json::Value::Number(n) => {
            n.as_u64().filter(|i| (*i as usize) < names.len()).map(|i| Some(i as usize))
        }
        serde_json::Value::String(s) => {
            let s = s.trim();
            if s.is_empty() || s.eq_ignore_ascii_case("null") || s.eq_ignore_ascii_case("wait") {
                return Some(None);
            }
            if let Some(i) = names.iter().position(|x| x == s) {
                return Some(Some(i));
            }
            s.parse::<usize>().ok().filter(|i| *i < names.len()).map(Some)
        }
        _ => None,
    }
}

/// The candidate (a real index, else the deliberate wait) that at least two models agree on.
/// A real candidate is preferred over "wait" on a tie.
fn agreed(answers: &[Decision]) -> Option<Option<usize>> {
    let mut best: Option<(usize, Option<usize>)> = None;
    for a in answers {
        let c = a.candidate;
        let votes = answers.iter().filter(|x| x.candidate == c).count();
        if votes < 2 {
            continue;
        }
        let better = match best {
            None => true,
            Some((bv, bc)) => votes > bv || (votes == bv && bc.is_none() && c.is_some()),
        };
        if better {
            best = Some((votes, c));
        }
    }
    best.map(|(_, c)| c)
}

fn home_tool(rel: &str) -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(rel)
}

/// The free model that decides and approves when neither paid CLI session is available.
pub const FALLBACK_VALIDATOR: &str = "opencode/deepseek-v4.1-flash-free";

enum Vetted {
    Approve(String),
    Reject(String),
    Unavailable,
}

/// A CLI that cannot run, exits non-zero, or reports a quota/auth/availability problem is
/// treated as unavailable so validation can move on; a clear REJECT from a working session
/// is final.
fn looks_unavailable(s: &str) -> bool {
    let l = s.to_lowercase();
    [
        "weekly limit",
        "quota",
        "rate limit",
        "usage limit",
        "session limit",
        "not signed in",
        "not logged in",
        "login",
        "authentication",
        "unauthorized",
        "api key",
        "expired",
        "no such file",
        "command not found",
        "cannot find",
    ]
    .iter()
    .any(|k| l.contains(k))
}

fn run_session(bin: PathBuf, args: &[&str], label: &str) -> Vetted {
    match Command::new(bin).args(args).output() {
        Ok(o) => {
            let s = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            if o.status.success() && approved(&s) {
                Vetted::Approve(label.into())
            } else if !o.status.success() || looks_unavailable(&s) {
                Vetted::Unavailable
            } else {
                Vetted::Reject(label.into())
            }
        }
        Err(_) => Vetted::Unavailable,
    }
}

/// One answer from a locked-down OpenCode agent, as plain text.
fn opencode_reply(agent: &str, model: &str, prompt: &str) -> Result<String> {
    ensure_opencode_free_model(model)?;
    let role = match agent {
        "online-validator" => "Approve or reject one strategy experiment using only the supplied evidence. Do not use tools. Reply with APPROVE or REJECT on its own line, then at most one short sentence.",
        _ => "Choose one stored-candle strategy experiment using only the supplied evidence. Do not use tools, place orders, or change exits. Return exactly the requested JSON and nothing else.",
    };
    let request = format!("{role}\n\n{prompt}");
    let o = opencode_command()?
        .args([
            "run",
            "--standalone",
            "--agent",
            "build",
            "--model",
            model,
            "--format",
            "json",
            &request,
        ])
        .output()?;
    let stdout = String::from_utf8_lossy(&o.stdout);
    let stderr = String::from_utf8_lossy(&o.stderr);
    let combined = format!("{stdout}\n{stderr}");
    if is_opencode_provenance_error(&combined) {
        anyhow::bail!("{OPENCODE_PROVENANCE_ERROR}; run `opencode auth login opencode` and retry");
    }
    if !o.status.success() {
        anyhow::bail!(
            "opencode {model} exited with {}: {}",
            o.status,
            combined.chars().take(2000).collect::<String>()
        );
    }
    Ok(last_text(&stdout))
}

/// An `opencode` command rooted in an empty runtime directory. Keeping both cwd and `PWD` away
/// from the repositories prevents the stock agent from discovering project files. Removing
/// inherited provider/proxy configuration guarantees that OpenCode uses its native route.
fn opencode_command() -> Result<Command> {
    let root = home_tool(".local/share/ClaudeMoomooBot/opencode-free-runtime");
    std::fs::create_dir_all(&root)?;
    let mut cmd = Command::new(home_tool(".opencode/bin/opencode"));
    cmd.current_dir(&root).env("PWD", &root);
    for key in BLOCKED_OPENCODE_ENV {
        cmd.env_remove(key);
    }
    Ok(cmd)
}

/// Final validation. Claude is tried first; when it is unavailable the fastest OpenCode free
/// model decides. Codex is deliberately reserved for interactive use and is never launched by
/// unattended automation.
fn vet(prompt: &str) -> Result<String> {
    match run_session(
        home_tool(".local/bin/claude"),
        &["-p", prompt, "--no-session-persistence"],
        "claude",
    ) {
        Vetted::Approve(v) => return Ok(v),
        Vetted::Reject(v) => anyhow::bail!("{v} rejected proposal"),
        Vetted::Unavailable => {}
    }
    let text = opencode_reply("online-validator", FALLBACK_VALIDATOR, prompt)?;
    if approved(&text) {
        Ok(format!("{FALLBACK_VALIDATOR} (Claude unavailable)"))
    } else {
        anyhow::bail!("{FALLBACK_VALIDATOR} rejected proposal or gave no verdict")
    }
}

pub fn decide(
    mem: &Connection,
    evidence: &serde_json::Value,
    candidates: &[String],
) -> Result<(Decision, String, String)> {
    init(mem)?;
    let now = crate::util::now_ts();
    let mut answers: Vec<(Decision, i64, String)> = Vec::new();
    let mut used = Vec::new();
    // Ask distinct, least-loaded models until two of them agree. Six attempts leave room for
    // the free tier's flaky endpoints without letting one model dominate.
    for _ in 0..6 {
        let Some(model) = pick(mem, now)? else { break };
        if used.contains(&model) {
            break;
        }
        used.push(model.clone());
        ensure_opencode_free_model(&model)?;
        let prompt = format!(
            "Pick the best next strategy experiment from the evidence. \
             Return JSON only: {{\"candidate\":\"<exact name copied from candidates>\" or null,\"reason\":\"short hypothesis\"}}. \
             Evidence: {evidence}"
        );
        match opencode_reply("online-researcher", &model, &prompt) {
            Ok(reply) => {
                match extract(&reply).ok().and_then(|raw| {
                    resolve(raw.candidate.as_ref(), candidates).map(|idx| Decision {
                        candidate: idx,
                        reason: raw.reason,
                    })
                }) {
                    Some(decision) => {
                        record_call(mem, &model, true, now)?;
                        mem.execute("INSERT INTO online_model_responses(ts,model,evidence,response) VALUES(?1,?2,?3,?4)", params![now, model, evidence.to_string(), serde_json::to_string(&decision)?])?;
                        let row = mem.last_insert_rowid();
                        answers.push((decision, row, model.clone()));
                        if agreed(&answers.iter().map(|(d, _, _)| d.clone()).collect::<Vec<_>>()).is_some() {
                            break;
                        }
                    }
                    None => record_call(mem, &model, false, now)?,
                }
            }
            Err(error) => {
                record_call(mem, &model, false, now)?;
                if is_opencode_provenance_error(&error.to_string()) {
                    anyhow::bail!("OpenCode rejected the free-tier session; re-authenticate with `opencode auth login opencode` before the next strategist run");
                }
            }
        }
    }
    if answers.len() < 2 {
        anyhow::bail!("fewer than two free online models answered");
    }
    let decisions: Vec<Decision> = answers.iter().map(|(d, _, _)| d.clone()).collect();
    let Some(candidate) = agreed(&decisions) else {
        anyhow::bail!("free online models disagreed; proposal quarantined");
    };
    // The models that actually agreed, not every model that was asked.
    let agreed_by: Vec<String> = answers
        .iter()
        .filter(|(d, _, _)| d.candidate == candidate)
        .map(|(_, _, m)| m.clone())
        .collect();
    let (representative, row, _) = answers
        .iter()
        .find(|(d, _, _)| d.candidate == candidate)
        .cloned()
        .context("agreed candidate vanished")?;
    // A deliberate wait is a valid, complete decision and needs no external validation.
    if candidate.is_none() {
        return Ok((representative, agreed_by.join(","), String::new()));
    }
    let index = candidate.expect("a real candidate");
    let proposal = serde_json::json!({
        "candidate": candidates.get(index).cloned().unwrap_or_default(),
        "reason": representative.reason,
    });
    let vp = format!("Approve this strategy experiment only if it is grounded in the evidence and names a valid candidate; else reject. Reply APPROVE or REJECT on its own line. Evidence: {evidence}; proposal: {proposal}");
    let validator = vet(&vp)?;
    mem.execute(
        "UPDATE online_model_responses SET accepted=1,validator=?1 WHERE id=?2",
        params![validator, row],
    )?;
    Ok((representative, agreed_by.join(","), validator))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn balances_and_cools_down_failed_teachers() {
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let first = pick(&db, 10).unwrap().unwrap();
        record_call(&db, &first, false, 10).unwrap();
        assert_ne!(pick(&db, 10).unwrap().unwrap(), first);
        assert!(FREE_MODELS.contains(&pick(&db, 4000).unwrap().unwrap().as_str()));
    }
    #[test]
    fn approval_must_be_an_exact_line() {
        assert!(approved("APPROVE\n"));
        assert!(!approved("do not approve"));
        assert!(!approved("REJECT"));
    }

    #[test]
    fn availability_problems_are_distinguished_from_rejections() {
        assert!(looks_unavailable("You've hit your weekly limit"));
        assert!(looks_unavailable("not signed in"));
        assert!(looks_unavailable("no such file or directory"));
        assert!(!looks_unavailable("REJECT: the proposal is not grounded"));
    }

    #[test]
    fn free_models_are_allowlisted_and_must_use_the_opencode_namespace() {
        for model in FREE_MODELS {
            ensure_opencode_free_model(model).unwrap();
        }
        assert!(ensure_opencode_free_model("anthropic/claude-free").is_err());
        assert!(ensure_opencode_free_model("openai/gpt-free").is_err());
        assert!(ensure_opencode_free_model("opencode/unreviewed-free").is_err());
    }

    #[test]
    fn provenance_rejection_is_detected_for_the_circuit_breaker() {
        assert!(is_opencode_provenance_error(
            "Error: OpenCode's free tier can only be used from within OpenCode."
        ));
        assert!(!is_opencode_provenance_error("temporary endpoint failure"));
    }

    #[test]
    fn extract_finds_the_json_even_wrapped_in_an_essay() {
        let essay = "**Maximum steps for this agent have been reached.**\n\n\
            Summary of work done {so far}: nothing.\n\n```json\n\
            {\"candidate\":\"obv+top13\",\"reason\":\"only candidate with validated memory\"}\n```\n\n\
            Remaining tasks: hand the decision to the ensemble.";
        let line = serde_json::json!({"type":"text","part":{"type":"text","text":essay}}).to_string();
        let d = extract(&line).unwrap();
        assert_eq!(d.candidate, Some(serde_json::json!("obv+top13")));
        // A reply with a top-level JSON object and no envelope still works.
        let d = extract(r#"{"candidate":null,"reason":"no evidence"}"#).unwrap();
        assert!(d.candidate.is_none());
        assert!(extract("no json at all").is_err());
    }

    #[test]
    fn a_name_an_index_and_null_all_resolve() {
        let names = vec!["obv+top13".to_string(), "feegate+top13".to_string()];
        let two = serde_json::json!(2);
        let s = serde_json::json!("feegate+top13");
        let n = serde_json::json!(null);
        let wait = serde_json::json!("wait");
        assert_eq!(resolve(Some(&s), &names), Some(Some(1)));
        assert_eq!(resolve(Some(&two), &names), None);
        assert_eq!(resolve(Some(&serde_json::json!(1)), &names), Some(Some(1)));
        assert_eq!(resolve(Some(&n), &names), Some(None));
        assert_eq!(resolve(Some(&wait), &names), Some(None));
        assert_eq!(resolve(None, &names), Some(None));
        assert_eq!(resolve(Some(&serde_json::json!("unknown")), &names), None);
    }

    #[test]
    fn two_agreeing_models_are_required_and_wait_is_valid() {
        let d = |c: Option<usize>| Decision {
            candidate: c,
            reason: String::new(),
        };
        assert_eq!(agreed(&[d(Some(0)), d(Some(1))]), None);
        assert_eq!(agreed(&[d(Some(1)), d(None), d(Some(1))]), Some(Some(1)));
        assert_eq!(agreed(&[d(None), d(None)]), Some(None));
        assert_eq!(agreed(&[d(Some(0))]), None);
        // A real candidate wins over a tied wait.
        assert_eq!(
            agreed(&[d(None), d(None), d(Some(0)), d(Some(0))]),
            Some(Some(0))
        );
    }
}
