//! Small helper commands that used to be inline Python in shell scripts (owner 2026-10-13: "everything is rust only"):
//!   agent-results  (stdin = dashboard snapshot)   the "Agents: tests and results" block of the quant supervisor's bundle
//!   market                                        OPEN / CLOSED for the US equity session (New York clock, 09:25-16:05 on weekdays)

use anyhow::Result;
use serde_json::Value;

// ---------------------------------------------------------------- Python-style text for numbers, dicts and JSON (the reports keep their old look)
pub fn py_float(f: f64) -> String {
    let d = format!("{f:?}");
    match d.split_once('e') {
        Some((m, e)) => { let n: i32 = e.parse().unwrap_or(0); format!("{m}e{}{:02}", if n < 0 { '-' } else { '+' }, n.abs()) }
        None => d,
    }
}
pub fn py_quote(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut o = String::from(q);
    for c in s.chars() { match c { '\\' => o.push_str("\\\\"), '\n' => o.push_str("\\n"), '\r' => o.push_str("\\r"), '\t' => o.push_str("\\t"), c if c == q => { o.push('\\'); o.push(c) } c => o.push(c) } }
    o.push(q); o
}
/// Python `repr` of a JSON value (None/True/False, single-quoted strings, `{'k': v}`).
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(), Value::Bool(b) => if *b { "True".into() } else { "False".into() },
        Value::Number(n) => if n.is_f64() { py_float(n.as_f64().unwrap_or(0.0)) } else { n.to_string() },
        Value::String(s) => py_quote(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!("{{{}}}", o.iter().map(|(k, v)| format!("{}: {}", py_quote(k), py_repr(v))).collect::<Vec<_>>().join(", ")),
    }
}
/// Python `str()` of an optional JSON value (strings unquoted, missing = None).
pub fn py_str_pub(v: Option<&Value>) -> String { py_str(v) }
fn py_str(v: Option<&Value>) -> String { match v { None | Some(Value::Null) => "None".into(), Some(Value::String(s)) => s.clone(), Some(o) => py_repr(o) } }
/// Python `json.dumps` (", " and ": " separators, non-ASCII escaped).
pub fn py_json(v: &Value) -> String {
    match v {
        Value::String(s) => {
            let mut o = String::from('"');
            for c in s.chars() { match c { '"' => o.push_str("\\\""), '\\' => o.push_str("\\\\"), '\n' => o.push_str("\\n"), '\r' => o.push_str("\\r"), '\t' => o.push_str("\\t"),
                c if (c as u32) < 0x20 || (c as u32) > 0x7e => { let mut b = [0u16; 2]; for u in c.encode_utf16(&mut b) { o.push_str(&format!("\\u{:04x}", u)); } } c => o.push(c) } }
            o.push('"'); o
        }
        Value::Array(a) => format!("[{}]", a.iter().map(py_json).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!("{{{}}}", m.iter().map(|(k, v)| format!("{}: {}", py_json(&Value::String(k.clone())), py_json(v))).collect::<Vec<_>>().join(", ")),
        Value::Number(n) if n.is_f64() => py_float(n.as_f64().unwrap_or(0.0)),
        other => other.to_string(),
    }
}

fn w<'a>(o: &'a Value, k: &str) -> Option<&'a Value> {
    let m = o.as_object()?;
    if let Some(v) = m.get(k) { return if v.is_null() { None } else { Some(v) }; }
    m.values().find_map(|v| w(v, k))
}
fn truthy(v: &Value) -> bool { match v { Value::Null => false, Value::Bool(b) => *b, Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0), Value::String(s) => !s.is_empty(), Value::Array(a) => !a.is_empty(), Value::Object(o) => !o.is_empty() } }

pub fn agent_results(d: &Value) -> Vec<String> {
    let mut out = vec![];
    let ar = w(d, "agent_results").filter(|v| truthy(v)).cloned().unwrap_or(Value::Object(Default::default()));
    for a in ar.get("summary").and_then(Value::as_array).into_iter().flatten() {
        let b = a.get("best").filter(|v| truthy(v)).cloned().unwrap_or(Value::Object(Default::default()));
        out.push(format!("- {} ({}): tested {}, validated {}, avg gain {}, best {} ({}: {})", py_str(a.get("source")), py_str(a.get("kind")), py_str(a.get("tested")), py_str(a.get("validated")),
            py_str(a.get("avg_gain_a")), py_str(b.get("variant")), py_str(b.get("status")), py_str(b.get("verdict"))));
    }
    let ps = py_json(w(d, "promotion_status").unwrap_or(&Value::Null));
    out.push(format!("- promotion status: {}", ps.chars().take(600).collect::<String>()));
    out
}

/// OPEN / CLOSED from New York's clock (weekday number 1=Mon..7=Sun, HHMM).
pub fn market_state(dow: u32, hhmm: u32) -> &'static str { if dow <= 5 && (925..=1605).contains(&hhmm) { "OPEN" } else { "CLOSED" } }

/// OPEN / CLOSED for a custom weekday window given as HHMM bounds (inclusive, to the minute; the Python versions compared to the second, so the
/// closing minute can read OPEN up to 59 s longer).
pub fn market_window_now(from: u32, to: u32) -> &'static str {
    let (d, h) = crate::platform::new_york_clock(chrono::Utc::now().timestamp());
    if d <= 5 && (from..=to).contains(&h) { "OPEN" } else { "CLOSED" }
}

pub fn market_now() -> &'static str {
    let (d, h) = crate::platform::new_york_clock(chrono::Utc::now().timestamp());
    market_state(d, h)
}

// ---------------------------------------------------------------- small JSON file helpers used by the bot's ops scripts
/// Number of rows in the fill probe's JSON list.
pub fn json_len(path: &str) -> Result<usize> { Ok(serde_json::from_str::<Value>(&std::fs::read_to_string(path)?)?.as_array().ok_or_else(|| anyhow::anyhow!("not a list"))?.len()) }
/// `data[key]` of a JSON object file as plain text (strings unquoted).
pub fn json_get(path: &str, key: &str) -> Result<String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(match v.get(key).ok_or_else(|| anyhow::anyhow!("no key {key}"))? { Value::String(s) => s.clone(), o => py_repr(o) })
}
/// `' '.join(data[key])` for a list of strings.
pub fn json_join(path: &str, key: &str) -> Result<String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(v.get(key).and_then(Value::as_array).ok_or_else(|| anyhow::anyhow!("no list {key}"))?.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join(" "))
}
/// The fill probe's decision from its real measurements only: every size filled completely both ways and the worst slippage against the quote is small.
pub fn fill_probe_verdict(path: &str) -> Vec<String> {
    let Ok(Ok(Value::Array(rows))) = std::fs::read_to_string(path).map(|t| serde_json::from_str::<Value>(&t)) else { return vec!["no data".into()] };
    if rows.len() < 4 { return vec![format!("only {} sizes measured", rows.len())]; }
    let f = |r: &Value, k: &str| r.get(k).and_then(Value::as_f64);
    let mut worst = 0f64; let mut big = f64::MIN; let mut bad = 0usize;
    for r in &rows {
        let (Some(buy), Some(sell), Some(n)) = (f(r, "buy_slip_bps_vs_ask"), f(r, "sell_slip_bps_vs_bid"), f(r, "notional")) else { return vec![] };
        if f(r, "sell_qty") != f(r, "qty") { bad += 1; }
        worst = worst.max(buy.abs()).max(sell.abs()); big = big.max(n);
    }
    vec![(if bad == 0 && worst <= 20.0 { "OK" } else { "NO" }).into(), format!("largest {big:.0} USD, worst slippage {worst:.2} bps, {bad} incomplete")]
}

/// First line of an MCP reply -> `result.content[0].text`.
pub fn mcp_text(reply_line: &str) -> Result<String> {
    let v: Value = serde_json::from_str(reply_line)?;
    Ok(v["result"]["content"][0]["text"].as_str().ok_or_else(|| anyhow::anyhow!("no result.content[0].text in the reply"))?.to_string())
}

pub fn main_mcp_text() -> i32 {
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    match mcp_text(&line) { Ok(t) => { println!("{t}"); 0 } Err(e) => { eprintln!("mcp-text: {e}"); 1 } }
}

pub fn main_agent_results() -> i32 {
    let mut input = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
    match serde_json::from_str::<Value>(&input) { Ok(d) => { for l in agent_results(&d) { println!("{l}"); } 0 } Err(e) => { eprintln!("agent-results: snapshot is not JSON: {e}"); 1 } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn python_style_text() {
        assert_eq!(py_repr(&json!({"a": 1, "b": 0.5, "c": null, "d": true, "e": "x", "f": [1, 2.0]})), "{'a': 1, 'b': 0.5, 'c': None, 'd': True, 'e': 'x', 'f': [1, 2.0]}");
        assert_eq!(py_repr(&json!("it's")), "\"it's\"");
        // keys come out sorted (serde_json's map); Python keeps insertion order, which for the dashboard snapshot is already sorted
        assert_eq!(py_json(&json!({"not": ["a", "é"], "n": null, "f": 1.5})), "{\"f\": 1.5, \"n\": null, \"not\": [\"a\", \"\\u00e9\"]}");
        assert_eq!(py_float(1e-5), "1e-05");
        assert_eq!(py_float(0.6666666666666666), "0.6666666666666666");
    }

    #[test]
    fn agent_results_block_has_the_original_shape() {
        let d = json!({"agent_results": {"summary": [{"source": "brain-tuner", "kind": "Brain (own models)", "tested": 25, "validated": 0, "avg_gain_a": -0.3468,
            "best": {"variant": "5h|breadth", "status": "failed-holdout", "verdict": "a block lost more than 8%"}}]},
            "promotion_status": {"not": ["p"]}});
        assert_eq!(agent_results(&d), vec![
            "- brain-tuner (Brain (own models)): tested 25, validated 0, avg gain -0.3468, best 5h|breadth (failed-holdout: a block lost more than 8%)",
            "- promotion status: {\"not\": [\"p\"]}"]);
        assert_eq!(agent_results(&json!({})), vec!["- promotion status: null"]);
    }

    #[test]
    fn mcp_reply_text_is_extracted() {
        assert_eq!(mcp_text(r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"VERDICT: x"}]}}"#).unwrap(), "VERDICT: x");
        assert!(mcp_text("not json").is_err() && mcp_text(r#"{"result":{}}"#).is_err());
    }

    #[test]
    fn json_helpers_and_the_fill_probe_verdict() {
        let d = tempfile::tempdir().unwrap(); let p = d.path().join("f.json");
        std::fs::write(&p, r#"[{"qty":1,"sell_qty":1,"buy_slip_bps_vs_ask":1.5,"sell_slip_bps_vs_bid":-2.25,"notional":300},{"qty":2,"sell_qty":2,"buy_slip_bps_vs_ask":0,"sell_slip_bps_vs_bid":0,"notional":30000},
            {"qty":3,"sell_qty":3,"buy_slip_bps_vs_ask":3,"sell_slip_bps_vs_bid":-1,"notional":150000},{"qty":4,"sell_qty":4,"buy_slip_bps_vs_ask":4.5,"sell_slip_bps_vs_bid":-5,"notional":500000}]"#).unwrap();
        let ps = p.to_str().unwrap();
        assert_eq!(json_len(ps).unwrap(), 4);
        assert_eq!(fill_probe_verdict(ps), vec!["OK", "largest 500000 USD, worst slippage 5.00 bps, 0 incomplete"]);
        std::fs::write(&p, r#"[{"qty":1,"sell_qty":0,"buy_slip_bps_vs_ask":25,"sell_slip_bps_vs_bid":0,"notional":1},{"qty":1,"sell_qty":1,"buy_slip_bps_vs_ask":0,"sell_slip_bps_vs_bid":0,"notional":1},{"qty":1,"sell_qty":1,"buy_slip_bps_vs_ask":0,"sell_slip_bps_vs_bid":0,"notional":1},{"qty":1,"sell_qty":1,"buy_slip_bps_vs_ask":0,"sell_slip_bps_vs_bid":0,"notional":1}]"#).unwrap();
        assert_eq!(fill_probe_verdict(ps), vec!["NO", "largest 1 USD, worst slippage 25.00 bps, 1 incomplete"]);
        std::fs::write(&p, "[]").unwrap(); assert_eq!(fill_probe_verdict(ps), vec!["only 0 sizes measured"]);
        assert_eq!(fill_probe_verdict("/nonexistent"), vec!["no data"]);
        std::fs::write(&p, r#"{"variant":"5h|obv","new_options":["a","b:c"]}"#).unwrap();
        assert_eq!(json_get(ps, "variant").unwrap(), "5h|obv"); assert_eq!(json_join(ps, "new_options").unwrap(), "a b:c"); assert!(json_get(ps, "missing").is_err());
    }

    #[test]
    fn market_hours_match_the_original_window() {
        assert_eq!(market_state(2, 925), "OPEN"); assert_eq!(market_state(2, 1605), "OPEN"); assert_eq!(market_state(2, 924), "CLOSED");
        assert_eq!(market_state(2, 1606), "CLOSED"); assert_eq!(market_state(6, 1100), "CLOSED");
    }

}
