//! ACTION ITEMS for the quant supervisor : every verdict and open problem the lab dashboard shows, from
//! its JSON snapshot on stdin. Used by supervisor/quant_supervisor.sh; prints one "- item" per line.

use serde_json::Value;

/// First value stored under key `k` anywhere in nested objects (depth-first, objects only, like the original). A JSON null counts as absent.
fn w<'a>(o: &'a Value, k: &str) -> Option<&'a Value> {
    let m = o.as_object()?;
    if let Some(v) = m.get(k) { return if v.is_null() { None } else { Some(v) }; }
    m.values().find_map(|v| w(v, k))
}
fn truthy(v: &Value) -> bool {
    match v { Value::Null => false, Value::Bool(b) => *b, Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0), Value::String(s) => !s.is_empty(), Value::Array(a) => !a.is_empty(), Value::Object(o) => !o.is_empty() }
}
/// `w(d, k) or default`
fn wo(d: &Value, k: &str, default: Value) -> Value { match w(d, k) { Some(v) if truthy(v) => v.clone(), _ => default } }
fn s(v: &Value) -> &str { v.as_str().unwrap_or("") }
fn clip(t: &str, n: usize) -> String { t.chars().take(n).collect() }

pub fn items(d: &Value) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    let eg = wo(d, "entry_gating", Value::String(String::new()));
    if let Some(v) = s(&eg).lines().find(|l| l.starts_with("VERDICT")) {
        items.push(format!("ENTRY GATING: {v}"));
        if v.contains("new code") || v.contains("NEEDS") {
            items.push("  -> YOUR JOB: design and BUILD a new entry gate in code (a research-only option, off in the live search; engine entry_* field drawn only when its token is on, unit test, variant_study token, lab VOCAB + exclusive group + FEATURE_OPTIONS mapping). Motivate it by data: a confirmed win/lose finding, the quant review risk edge, or a research idea that needs new code. The loop then tests it automatically on both cuts.".into());
        }
    }
    let q = wo(d, "quant", Value::String(String::new()));
    if let Some(v) = s(&q).lines().map(str::trim).find(|l| l.contains("VERDICT")) { items.push(format!("QUANT (unseen half): {v}")); }
    let ri = wo(d, "research_ideas", Value::Array(vec![]));
    let nc: Vec<&Value> = ri.as_array().into_iter().flatten().filter(|x| x.is_object() && x.get("status").map(s).unwrap_or("") == "needs new code").collect();
    if !nc.is_empty() {
        let titles: Vec<String> = nc.iter().take(3).map(|x| clip(x.get("title").map(s).unwrap_or(""), 70)).collect();
        items.push(format!("RESEARCH: {} recent idea(s) need new code: {}", nc.len(), titles.join("; ")));
    }
    let ps = wo(d, "promotion_status", Value::Object(Default::default()));
    for l in ps.get("not").and_then(Value::as_array).into_iter().flatten().take(3) { items.push(format!("PROMOTION: {}", s(l))); }
    if items.is_empty() { items.push("none".into()); }
    items
}

pub fn main() {
    let mut input = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
    match serde_json::from_str::<Value>(&input) {
        Ok(d) => println!("{}", items(&d).iter().map(|i| format!("- {i}")).collect::<Vec<_>>().join("\n")),
        Err(e) => { eprintln!("action-items: snapshot is not JSON: {e}"); std::process::exit(1); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_snapshot_says_none() { assert_eq!(items(&json!({})), vec!["none"]); }

    #[test]
    fn verdicts_and_problems_become_items() {
        let d = json!({"a": {"entry_gating": "intro\nVERDICT: NEEDS improving, no gate validated"}, "quant": "x\n  VERDICT: no gain survives",
            "research_ideas": [{"status": "needs new code", "title": "Idea A"}, {"status": "ok", "title": "B"}],
            "promotion_status": {"not": ["p1"]}});
        let it = items(&d);
        assert_eq!(it[0], "ENTRY GATING: VERDICT: NEEDS improving, no gate validated");
        assert!(it[1].starts_with("  -> YOUR JOB: design and BUILD"));
        assert_eq!(it[2], "QUANT (unseen half): VERDICT: no gain survives");
        assert!(it.contains(&"RESEARCH: 1 recent idea(s) need new code: Idea A".to_string()));
        assert!(it.contains(&"PROMOTION: p1".to_string()));
    }

    #[test]
    fn null_values_do_not_stop_the_search_in_siblings() {
        assert_eq!(items(&json!({"x": {"quant": null}, "y": {"quant": "VERDICT: ok"}})), vec!["QUANT (unseen half): VERDICT: ok"]);
    }
}
