//! `qwen-tools <command> ...` -- the lab's small helper commands, all Rust (they replace the Python scripts under hermes/ and tools/):
//!   action-items                                                                   quant-supervisor action items from the dashboard snapshot on stdin
//!   gen-function-docs <repo root>                                                  docs/functions/*.md from the Rust sources
//!   warnings scan|list|show|fixed|expected|report|counts   the bot's warnings ledger (log doctor)
//!   fix-report check|fallback|secrets|index ...             code checks for the daily fix report
//!   spy-holdings-xlsx <xlsx> <csv>                          SPY holdings spreadsheet -> CSV
//!   bot-status | bot-winners (JSON on stdin)                check.sh summaries
//!   agent-results (snapshot on stdin) | mcp-text (reply on stdin) | market   former inline Python of the shell scripts
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or(&[]);
    let code = match args.first().map(String::as_str) {
        Some("action-items") => { qwen_lab::action_items::main(); 0 }
        Some("gen-function-docs") => qwen_lab::fn_docs::main(rest),
        Some("agent-results") => qwen_lab::misc_tools::main_agent_results(),
        Some("warnings") => qwen_lab::warnings_ledger::main(rest),
        Some("fix-report") => qwen_lab::fix_report::main(rest),
        Some("spy-holdings-xlsx") => qwen_lab::holdings_xlsx::main(rest),
        Some("bot-status") => qwen_lab::bot_status::main("status"),
        Some("bot-winners") => qwen_lab::bot_status::main("winners"),
        Some("mcp-text") => qwen_lab::misc_tools::main_mcp_text(),
        Some("market") => { println!("{}", match (rest.first().and_then(|a| a.parse::<u32>().ok()), rest.get(1).and_then(|a| a.parse::<u32>().ok())) { (Some(f), Some(t)) => qwen_lab::misc_tools::market_window_now(f, t), _ => qwen_lab::misc_tools::market_now() }); 0 }
        Some("fill-probe-count") if rest.len() == 1 => match qwen_lab::misc_tools::json_len(&rest[0]) { Ok(n) => { println!("{n}"); 0 } Err(_) => 1 },
        Some("fill-probe-verdict") if rest.len() == 1 => { for l in qwen_lab::misc_tools::fill_probe_verdict(&rest[0]) { println!("{l}"); } 0 }
        Some("json-get") if rest.len() == 2 => match qwen_lab::misc_tools::json_get(&rest[0], &rest[1]) { Ok(v) => { println!("{v}"); 0 } Err(e) => { eprintln!("json-get: {e}"); 1 } },
        Some("json-join") if rest.len() == 2 => match qwen_lab::misc_tools::json_join(&rest[0], &rest[1]) { Ok(v) => { println!("{v}"); 0 } Err(e) => { eprintln!("json-join: {e}"); 1 } },
        _ => { eprintln!("usage: qwen-tools action-items | gen-function-docs | warnings | fix-report | spy-holdings-xlsx | bot-status | bot-winners | agent-results | mcp-text | market [FROM TO] | fill-probe-count | fill-probe-verdict | json-get | json-join ..."); 2 }
    };
    std::process::exit(code);
}
