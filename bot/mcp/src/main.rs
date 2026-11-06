//! MCP server (stdio, JSON-RPC 2.0) giving Claude Code COMPACT views of the moomoo paper bot, so
//! checks cost a few lines instead of raw logs and JSON. Read-only: never changes the bot, never
//! talks to OpenD, never places orders.
//!
//! Tools: moomoo_check (health = `bot-ops check`), moomoo_results (interval winners and
//! out-of-sample tests), moomoo_orders (real paper orders, holdings, recent paper trades).
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const CONSOLE: &str = "127.0.0.1:8789";

/// GET a console API path; Err when the console does not answer.
fn get(path: &str) -> Result<Value, String> {
    let mut s = TcpStream::connect_timeout(&CONSOLE.parse().map_err(|e| format!("{e}"))?, Duration::from_secs(3))
        .map_err(|e| format!("console {CONSOLE} not answering: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok();
    s.write_all(format!("GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).map_err(|e| e.to_string())?;
    let body = buf.split_once("\r\n\r\n").map(|x| x.1).unwrap_or("");
    serde_json::from_str(body).map_err(|e| format!("{path}: bad JSON: {e}"))
}

fn utc(ms: &Value) -> String {
    ms.as_i64()
        .filter(|m| *m > 0)
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map_or("-".into(), |d| d.format("%m-%d %H:%M UTC").to_string())
}

fn pct(v: &Value) -> String {
    v.as_f64().map_or("-".into(), |x| format!("{x:+.2}%"))
}

/// The bot's `bot-ops` binary: `MOOMOO_BOT_DIR/target/release`, else `~/Projects/moomoo-bot/target/release`, else the installed copy in `~/.local/bin`
/// (`.exe` on Windows; macOS, Linux and Windows are all supported).
fn bot_ops() -> Option<std::path::PathBuf> {
    let name = if cfg!(windows) { "bot-ops.exe" } else { "bot-ops" };
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from)?;
    let repo = std::env::var_os("MOOMOO_BOT_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| home.join("Projects/moomoo-bot"));
    [repo.join("target/release").join(name), home.join(".local/bin").join(name)].into_iter().find(|p| p.is_file())
}

fn check(minutes: i64) -> (String, bool) {
    let Some(bin) = bot_ops() else { return ("bot-ops was not found: run `cargo build --release --bin bot-ops` in the bot repo".into(), true) };
    match std::process::Command::new(bin).arg("check").arg(minutes.to_string()).output() {
        Ok(o) => (String::from_utf8_lossy(&o.stdout).trim().to_string(), false),
        Err(e) => (format!("bot-ops check failed: {e}"), true),
    }
}

fn results() -> (String, bool) {
    let w = match get("/api/winners") {
        Ok(w) => w,
        Err(e) => return (e, true),
    };
    let mut out = vec![format!("paper trades: {} | {}", w["paper_interval"], w["note"].as_str().unwrap_or(""))];
    for r in w["rows"].as_array().into_iter().flatten() {
        out.push(format!(
            "{}: {} sessions, usable {} | winner {} (last 14 {}), {} trades, maxDD {} | walk-forward {}{} | stability {}/{} positive | searched {}",
            r["label"].as_str().unwrap_or("?"), r["sessions"], r["usable"], pct(&r["return_pct"]), pct(&r["common_return_pct"]),
            r["trades"], pct(&r["max_drawdown_pct"]), pct(&r["walk_forward_pct"]),
            r["walk_forward_error"].as_str().map_or(String::new(), |e| format!(" (failed: {})", e.chars().take(80).collect::<String>())),
            r["stability_positive"], r["stability_found"], utc(&r["searched_ms"])
        ));
    }
    (out.join("\n"), false)
}

fn orders() -> (String, bool) {
    let mut out = Vec::new();
    match get("/api/orders") {
        Ok(o) => {
            out.push(format!("next orders planned for {}", utc(&o["planned_for"])));
            for x in o["holdings"].as_array().into_iter().flatten() {
                out.push(format!("holding {x}"));
            }
            for x in o["orders"].as_array().into_iter().flatten().rev().take(12) {
                out.push(format!("order {x}"));
            }
        }
        Err(e) => return (e, true),
    }
    if let Ok(s) = get("/api/status") {
        for p in s["paper_positions"].as_array().into_iter().flatten() {
            out.push(format!("paper position {} qty {} entry {} mark {}", p["symbol"].as_str().unwrap_or("?"), p["qty"], p["entry"], p["mark"]));
        }
        for t in s["paper_recent_trades"].as_array().into_iter().flatten().take(10) {
            out.push(format!("paper trade {} pnl {} ({}) closed {}", t["symbol"].as_str().unwrap_or("?"), t["pnl"], t["reason"].as_str().unwrap_or(""), utc(&t["exit_ts"])));
        }
    }
    (out.join("\n"), false)
}

fn tools() -> Value {
    json!([
        {"name": "moomoo_check", "description": "Health of the moomoo paper bot in a few lines: OpenD, service, mode, paper equity, tasks, interval winners, de-duplicated WARN/ERROR lines of the last N minutes, last bar.", "inputSchema": {"type": "object", "properties": {"minutes": {"type": "integer", "default": 60}}}},
        {"name": "moomoo_results", "description": "Per interval (15m, 1h, 4h, 1 session): usable settings, winner return, last-14-session return, trades, drawdown, walk-forward and stability results, and which interval paper trades.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "moomoo_orders", "description": "Real moomoo paper orders and holdings, the paper positions and the last 10 paper trades, one line each.", "inputSchema": {"type": "object", "properties": {}}}
    ])
}

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = req.get("id").cloned() else { continue };
        let result = match req["method"].as_str().unwrap_or("") {
            "initialize" => json!({
                "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "moomoo-bot", "version": env!("CARGO_PKG_VERSION")}
            }),
            "tools/list" => json!({"tools": tools()}),
            "tools/call" => {
                let args = &req["params"]["arguments"];
                let (text, is_error) = match req["params"]["name"].as_str().unwrap_or("") {
                    "moomoo_check" => check(args["minutes"].as_i64().unwrap_or(60)),
                    "moomoo_results" => results(),
                    "moomoo_orders" => orders(),
                    other => (format!("unknown tool {other}"), true),
                };
                json!({"content": [{"type": "text", "text": text}], "isError": is_error})
            }
            "ping" => json!({}),
            m => {
                let _ = writeln!(stdout, "{}", json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {m}")}}));
                let _ = stdout.flush();
                continue;
            }
        };
        let _ = writeln!(stdout, "{}", json!({"jsonrpc": "2.0", "id": id, "result": result}));
        let _ = stdout.flush();
    }
}
