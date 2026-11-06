//! The two JSON summaries inside the bot's check.sh (formerly inline Python): `qwen-tools bot-status` (stdin = the console's /api/status) and
//! `qwen-tools bot-winners` (stdin = /api/winners). The lab's `bot_status` tool and the scheduled checks call check.sh, which pipes into these.

use serde_json::Value;

fn py_str(v: Option<&Value>) -> String {
    match v { None | Some(Value::Null) => "None".into(), Some(Value::String(s)) => s.clone(), Some(Value::Bool(b)) => if *b { "True".into() } else { "False".into() }, Some(o) => crate::misc_tools::py_repr(o) }
}
fn truthy(v: Option<&Value>) -> bool { match v { None | Some(Value::Null) => false, Some(Value::Bool(b)) => *b, Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0), Some(Value::String(s)) => !s.is_empty(), Some(Value::Array(a)) => !a.is_empty(), Some(Value::Object(o)) => !o.is_empty() } }
fn num(v: Option<&Value>) -> f64 { v.and_then(Value::as_f64).unwrap_or(0.0) }

pub fn status(input: &str) -> Vec<String> {
    let Ok(d) = serde_json::from_str::<Value>(input) else { return vec!["console NOT answering".into()] };
    let tasks = d.get("tasks").filter(|t| truthy(Some(t))).and_then(Value::as_object).cloned().unwrap_or_default();
    let down: Vec<&str> = tasks.iter().filter(|(_, v)| !truthy(v.get("running"))).map(|(k, _)| k.as_str()).collect();
    vec![
        format!("mode: {} {} | symbols {}", py_str(d.get("mode")), if truthy(d.get("market_open")) { "| market open" } else { "| market closed" }, py_str(d.get("symbols"))),
        format!("paper equity {:.2} start {:.2} | open pos {} | trades {}", num(d.get("paper_equity")), num(d.get("paper_start_equity")), py_str(d.get("paper_open_positions")), py_str(d.get("paper_trades"))),
        format!("tasks: {}", if !tasks.is_empty() && down.is_empty() { "all running".to_string() } else { format!("DOWN: {}", down.join(",")) }),
    ]
}

pub fn winners(input: &str) -> Option<String> {
    let d = serde_json::from_str::<Value>(input).ok()?;
    let f = |x: Option<&Value>| match x.and_then(Value::as_f64) { None => "-".to_string(), Some(v) => format!("{v:+.2}") };
    let rows: Vec<String> = d.get("rows").and_then(Value::as_array).into_iter().flatten()
        .map(|r| format!("{} last14 {} WF {}", py_str(r.get("label")), f(r.get("common_return_pct")), f(r.get("walk_forward_pct")))).collect();
    Some(format!("paper interval {} | {}", py_str(d.get("paper_interval")), rows.join("; ")))
}

pub fn main(which: &str) -> i32 {
    let mut input = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
    if which == "status" { for l in status(&input) { println!("{l}"); } } else if let Some(l) = winners(&input) { println!("{l}"); }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines_match_the_original_format() {
        let s = r#"{"mode":"paper","market_open":false,"symbols":80,"paper_equity":5012.345,"paper_start_equity":5000,"paper_open_positions":2,"paper_trades":7,"tasks":{"a":{"running":true},"b":{"running":false}}}"#;
        assert_eq!(status(s), vec!["mode: paper | market closed | symbols 80", "paper equity 5012.35 start 5000.00 | open pos 2 | trades 7", "tasks: DOWN: b"]);
        assert_eq!(status("not json"), vec!["console NOT answering"]);
        assert_eq!(status("{}")[2], "tasks: DOWN: ");
    }

    #[test]
    fn winners_line() {
        let w = r#"{"paper_interval":"H5","rows":[{"label":"5h","common_return_pct":6.89,"walk_forward_pct":-2.31},{"label":"6h","common_return_pct":null}]}"#;
        assert_eq!(winners(w).unwrap(), "paper interval H5 | 5h last14 +6.89 WF -2.31; 6h last14 - WF -");
        assert!(winners("nope").is_none());
    }
}
