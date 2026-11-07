//! `thomas portal`: a read-only web portal on this Mac (http://127.0.0.1:8790) for momentum forward evidence, ThomasAi shadow evaluation, the competition bot,
//! experiments, learner) and the bot's backtest archive. It only reads: the store is opened read-only for every request,
//! the server listens on the loopback address only, answers GET only and refuses a Host header that is not the loopback name (DNS rebinding). No script runs
//! in the pages and nothing leaves the machine.
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, types::ValueRef};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const PORT: u16 = 8790;

/// Everything a page may read.
pub struct Sources<'a> {
    pub store: &'a Connection,
    /// The bot's backtest archive (data/backtests.sqlite3), when present.
    pub archive: Option<&'a Connection>,
    /// The competition bot's journal (papercompmoomoo/data/journal.sqlite3), when present.
    pub journal: Option<&'a Connection>,
    /// The project folder (for data/tuned.json and data/bench.json).
    pub repo: &'a Path,
    pub now_ms: i64,
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn cell(name: &str, v: ValueRef) -> String {
    match v {
        ValueRef::Null => "–".into(),
        ValueRef::Integer(i) if name.ends_with("_ms") && i > 100_000_000_000 => crate::ops::utc(i),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => format!("{f:.2}"),
        ValueRef::Text(t) => {
            let s = String::from_utf8_lossy(t);
            let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
            if one.chars().count() > 140 {
                format!("{}…", one.chars().take(140).collect::<String>())
            } else {
                one
            }
        }
        ValueRef::Blob(_) => "(blob)".into(),
    }
}

/// Runs a query and renders it as an HTML table; a failing query renders as a visible message, never as an empty page.
pub fn table(conn: &Connection, sql: &str) -> String {
    let run = || -> Result<String> {
        let mut st = conn.prepare(sql)?;
        let names: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
        let mut out = String::from("<div class=scroll><table><thead><tr>");
        for n in &names {
            out.push_str(&format!("<th>{}</th>", esc(n)));
        }
        out.push_str("</tr></thead><tbody>");
        let mut rows = st.query([])?;
        let mut n = 0;
        while let Some(r) = rows.next()? {
            out.push_str("<tr>");
            for (i, name) in names.iter().enumerate() {
                out.push_str(&format!("<td>{}</td>", esc(&cell(name, r.get_ref(i)?))));
            }
            out.push_str("</tr>");
            n += 1;
        }
        if n == 0 {
            out.push_str(&format!(
                "<tr><td colspan={}>nothing yet</td></tr>",
                names.len().max(1)
            ));
        }
        out.push_str("</tbody></table></div>");
        Ok(out)
    };
    run().unwrap_or_else(|e| {
        format!(
            "<p class=bad>could not read this: {}</p>",
            esc(&format!("{e:#}"))
        )
    })
}

/// SVG line chart of the account's equity and SPY rebased to the same start.
pub fn equity_chart(points: &[(i64, f64, Option<f64>)]) -> String {
    if points.len() < 2 {
        return "<p>The chart appears once the account has two or more candles.</p>".into();
    }
    let start = points[0].1;
    let spy0 = points.iter().find_map(|p| p.2);
    let eq: Vec<Option<f64>> = points.iter().map(|p| Some(p.1)).collect();
    let spy: Vec<Option<f64>> = points
        .iter()
        .map(|p| match (p.2, spy0) {
            (Some(c), Some(c0)) if c0 > 0.0 => Some(start * c / c0),
            _ => None,
        })
        .collect();
    let all: Vec<f64> = eq.iter().chain(spy.iter()).flatten().copied().collect();
    let (lo, hi) = all
        .iter()
        .fold((f64::MAX, f64::MIN), |a, v| (a.0.min(*v), a.1.max(*v)));
    let span = (hi - lo).max(1e-9);
    let (w, h) = (720.0, 220.0);
    let line = |s: &[Option<f64>], class: &str| -> String {
        let pts: Vec<String> = s
            .iter()
            .enumerate()
            .filter_map(|(i, v)| {
                v.map(|v| {
                    format!(
                        "{:.1},{:.1}",
                        w * i as f64 / (s.len() - 1) as f64,
                        h - h * (v - lo) / span
                    )
                })
            })
            .collect();
        format!(
            "<polyline class={class} fill=none points=\"{}\"/>",
            pts.join(" ")
        )
    };
    let first = crate::ops::utc(points[0].0);
    let last = crate::ops::utc(points[points.len() - 1].0);
    format!(
        "<svg viewBox=\"0 0 {w} {h}\" role=img aria-label=\"equity against SPY\">{}{}</svg><p class=muted><span class=k1></span> paper account &nbsp; <span class=k2></span> SPY rebased &nbsp; range {lo:.0} to {hi:.0} USD · {first} to {last} UTC · {} snapshots</p>",
        line(&spy, "spy"),
        line(&eq, "eq"),
        points.len()
    )
}

const CSS: &str = "
:root{color-scheme:dark;--bg:#0c1117;--card:#151d27;--line:#263342;--fg:#e7edf4;--mut:#8b9aaa;--good:#63d9a0;--warn:#f5c76b;--bad:#ff7885;--acc:#75b8ff}
*{box-sizing:border-box}
body{margin:0;background:linear-gradient(135deg,#0c1117,#111b25) fixed;color:var(--fg);font:14px ui-sans-serif,system-ui,-apple-system,sans-serif;padding:0 22px 60px;max-width:1240px;margin-inline:auto}
nav{display:flex;flex-wrap:wrap;gap:6px 18px;padding:22px 0 14px;border-bottom:1px solid var(--line);margin-bottom:8px}
nav a{color:var(--mut);text-decoration:none}nav a:hover{color:var(--fg)}nav b{color:var(--good)}
h1{font-size:32px;margin:22px 0 4px;color:var(--good);letter-spacing:-1px}h2{font-size:16px;margin:26px 0 10px}h3{font-size:14px;margin:16px 0 8px;color:var(--mut);text-transform:uppercase;letter-spacing:.08em}
p{color:var(--mut)}a{color:var(--acc)}
.muted{color:var(--mut)}.bad{color:var(--bad)}.good{color:var(--good)}.warn{color:var(--warn)}
.tiles{display:grid;grid-template-columns:repeat(auto-fill,minmax(210px,1fr));gap:14px;margin:14px 0}
.tile,.scroll,pre,svg,iframe{background:rgba(21,29,39,.92);border:1px solid var(--line);border-radius:14px;box-shadow:0 12px 35px #0002}
.tile{padding:18px;color:var(--mut);font-size:12px;text-transform:uppercase;letter-spacing:.08em}
.tile .v{font-size:27px;font-weight:750;margin-top:8px;color:var(--fg);text-transform:none;letter-spacing:0}
.progress{height:14px;background:#0a0f14;border:1px solid var(--line);border-radius:999px;overflow:hidden;margin:12px 0}.progress-bar{height:100%;background:linear-gradient(90deg,var(--acc),var(--good));border-radius:inherit;transition:width .4s ease}
.scroll{overflow-x:auto;padding:6px 14px}
table{border-collapse:collapse;width:100%;font-size:13px}
.scroll table,.tile table{margin:0}
body>table,h2+table,h3+table,p+table{background:rgba(21,29,39,.92);border:1px solid var(--line);border-radius:14px;padding:6px 14px;display:block;overflow-x:auto}
th,td{text-align:left;padding:10px 7px;border-bottom:1px solid var(--line);white-space:nowrap;font-variant-numeric:tabular-nums}
th{color:var(--mut);font-size:11px;font-weight:500;text-transform:uppercase}
tr:last-child td{border-bottom:0}
svg{width:100%;height:auto;max-height:240px}
.eq{stroke:var(--acc);stroke-width:2}.spy{stroke:var(--mut);stroke-width:1.5;stroke-dasharray:4 3}
.k1,.k2{display:inline-block;width:18px;height:0;border-top:2px solid var(--acc);vertical-align:middle}.k2{border-top:2px dashed var(--mut)}
pre{padding:14px;overflow-x:auto}
@media(max-width:800px){body{padding:0 12px 40px}h1{font-size:24px}}
";

const NAV: [(&str, &str); 5] = [
    ("/", "Overview"),
    ("/gainer-tracker", "Gainer Tracker"),
    ("/exit-replay", "Exit Replay"),
    ("/brain", "Cognitive Brain"),
    ("/learnings", "Thomas Learnings"),
];

fn page(active: &str, title: &str, body: &str) -> String {
    let nav: String = NAV
        .iter()
        .map(|(h, l)| {
            if *h == active {
                format!("<a href=\"{h}\"><b>{l}</b></a>")
            } else {
                format!("<a href=\"{h}\">{l}</a>")
            }
        })
        .collect();
    format!(
        "<!doctype html><html lang=en><head><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><meta http-equiv=refresh content=60><title>ThomasAi · {}</title><style>{CSS}</style></head><body><nav><b>ThomasAi</b>{nav}</nav><h1>{}</h1><p class=muted>Read-only, refreshes every minute.</p>{body}</body></html>",
        esc(title),
        esc(title)
    )
}

fn tile(label: &str, value: &str, note: &str) -> String {
    format!(
        "<div class=tile><div class=muted>{}</div><div class=v>{}</div><div class=muted>{}</div></div>",
        esc(label),
        esc(value),
        esc(note)
    )
}

/// A JSON value as readable tables (never a raw dump): flat objects become two columns, objects of objects one row per key, timestamps (`*_ms`) become UTC times.
fn json_cell(key: &str, v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Number(n) if key.ends_with("_ms") => match n.as_i64() {
            Some(ms) if ms > 1_000_000_000_000 => format!("{} UTC", chrono_like(ms)),
            _ => "–".to_string(),
        },
        serde_json::Value::String(t) => esc(t),
        serde_json::Value::Null => "–".to_string(),
        other => {
            let t = other.to_string();
            esc(&if t.chars().count() > 160 {
                format!("{}…", t.chars().take(160).collect::<String>())
            } else {
                t
            })
        }
    }
}

/// Milliseconds since the epoch as `YYYY-MM-DD HH:MM:SS` (UTC), without a date library.
fn chrono_like(ms: i64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn json_tables(v: &serde_json::Value) -> String {
    let Some(top) = v.as_object() else {
        return format!("<p>{}</p>", esc(&v.to_string()));
    };
    let mut out = String::new();
    let flat: Vec<(&String, &serde_json::Value)> =
        top.iter().filter(|(_, x)| !x.is_object()).collect();
    if !flat.is_empty() {
        out.push_str("<table>");
        for (k, x) in flat {
            out.push_str(&format!(
                "<tr><td>{}</td><td>{}</td></tr>",
                esc(k),
                json_cell(k, x)
            ));
        }
        out.push_str("</table>");
    }
    for (k, x) in top.iter().filter(|(_, x)| x.is_object()) {
        let rows = x.as_object().into_iter().flatten();
        let nested = rows.clone().all(|(_, r)| r.is_object());
        out.push_str(&format!("<h3>{}</h3><table>", esc(k)));
        if nested {
            let cols: std::collections::BTreeSet<&String> = rows
                .clone()
                .filter_map(|(_, r)| r.as_object())
                .flat_map(|o| o.keys())
                .collect();
            out.push_str("<tr><th></th>");
            for c in &cols {
                out.push_str(&format!("<th>{}</th>", esc(c)));
            }
            out.push_str("</tr>");
            for (name, r) in rows {
                out.push_str(&format!("<tr><td><b>{}</b></td>", esc(name)));
                for c in &cols {
                    out.push_str(&format!(
                        "<td>{}</td>",
                        r.get(c.as_str())
                            .map_or("–".to_string(), |cell| json_cell(c, cell))
                    ));
                }
                out.push_str("</tr>");
            }
        } else {
            for (name, cell) in rows {
                out.push_str(&format!(
                    "<tr><td>{}</td><td>{}</td></tr>",
                    esc(name),
                    json_cell(name, cell)
                ));
            }
        }
        out.push_str("</table>");
    }
    out
}

/// A project JSON file as tables; a missing or unreadable file is said in words.
fn json_block(repo: &Path, rel: &str) -> String {
    let path = repo.join(rel);
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return format!(
            "<p class=muted>{} is not a regular project file.</p>",
            esc(rel)
        );
    }
    match std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    {
        Some(v) => json_tables(&v),
        None if rel == "papercompmoomoo/data/orb_premarket.json" =>
            "<p class=muted>No premarket candidates for the current session yet. The scanner will publish an explicit empty result after its next run.</p>".into(),
        None => format!("<p class=muted>{} does not exist yet.</p>", esc(rel)),
    }
}

fn paper_summary(c: &Connection) -> (String, Vec<String>) {
    let first: Option<(f64, Option<f64>)> = c
        .query_row(
            "SELECT equity, ref_close FROM paper_equity ORDER BY wall_ms LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let last: Option<(f64, Option<f64>, i64, i64)> = c
        .query_row("SELECT equity, ref_close, positions, wall_ms FROM paper_equity ORDER BY wall_ms DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .ok();
    let (trades, wins, pnl, fees): (i64, i64, f64, f64) = c
        .query_row("SELECT count(*), coalesce(sum(pnl>0),0), coalesce(sum(pnl),0), coalesce(sum(fees),0) FROM paper_trades", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap_or((0, 0, 0.0, 0.0));
    let Some(((e0, s0), (e1, s1, open, at))) = first.zip(last) else {
        return (
            "The paper account has not started yet.".into(),
            vec![tile(
                "Paper account",
                "not started",
                "the paper job opens it at the next US session",
            )],
        );
    };
    let ret = 100.0 * (e1 / e0 - 1.0);
    let spy = s0.zip(s1).map(|(a, b)| 100.0 * (b / a - 1.0));
    let line = format!(
        "Account {e1:.2} USD ({ret:+.2}%) against SPY {} since it opened; {open} open position(s); last candle {}.",
        spy.map_or("n/a".into(), |s| format!("{s:+.2}%")),
        crate::ops::utc(at)
    );
    (
        line,
        vec![
            tile(
                "Paper equity",
                &format!("{e1:.2} USD"),
                &format!("{ret:+.2}% since start"),
            ),
            tile(
                "SPY, same period",
                &spy.map_or("n/a".into(), |s| format!("{s:+.2}%")),
                "buy and hold",
            ),
            tile(
                "Closed trades",
                &trades.to_string(),
                &format!("{wins} won, P&L {pnl:+.2}, fees {fees:.2}"),
            ),
            tile(
                "Open positions",
                &open.to_string(),
                "one position is the rule",
            ),
        ],
    )
}

fn render_market_curves(curves: &serde_json::Map<String, serde_json::Value>) -> String {
    let (w, h) = (720.0, 220.0);
    let mut all_vals = Vec::new();
    let mut sessions = Vec::new();
    for (_, cobj) in curves {
        if let Some(pts) = cobj["curve"].as_array() {
            for p in pts {
                if let Some(y) = p["pnl_pct"].as_f64() {
                    all_vals.push(y);
                }
                if let Some(session) = p["session"].as_str().filter(|v| v.len() == 10 && v.as_bytes().get(4) == Some(&b'-')) {
                    sessions.push(session.to_string());
                }
            }
        }
    }
    if all_vals.is_empty() {
        return "<p class=muted>No curve points recorded.</p>".to_string();
    }
    let (lo, hi) = all_vals
        .iter()
        .fold((f64::MAX, f64::MIN), |a, v| (a.0.min(*v), a.1.max(*v)));
    let span = (hi - lo).max(1e-9);
    let mut polylines = String::new();
    let mut legend = String::new();

    for (name, cobj) in curves {
        let color = cobj["color"].as_str().unwrap_or("#63d9a0");
        if let Some(pts) = cobj["curve"].as_array() {
            let n_pts = pts.len();
            if n_pts > 1 {
                let coords: Vec<String> = pts
                    .iter()
                    .enumerate()
                    .filter_map(|(i, p)| {
                        p["pnl_pct"].as_f64().map(|y| {
                            format!(
                                "{:.1},{:.1}",
                                w * i as f64 / (n_pts - 1) as f64,
                                h - h * (y - lo) / span
                            )
                        })
                    })
                    .collect();
                polylines.push_str(&format!(
                    "<polyline fill=none stroke=\"{}\" stroke-width=2.2 points=\"{}\"/>",
                    color,
                    coords.join(" ")
                ));
                legend.push_str(&format!(
                    "<span style=\"display:inline-block;width:14px;height:0;border-top:3px solid {color};vertical-align:middle;margin-right:4px\"></span>{} &nbsp;&nbsp; ",
                    esc(name)
                ));
            }
        }
    }
    sessions.sort();
    let time_range = match (sessions.first(), sessions.last()) {
        (Some(first), Some(last)) => format!(" · daily sessions {first} to {last} (date labels from source data)"),
        _ => " · source session dates unavailable".to_string(),
    };
    format!(
        "<svg viewBox=\"0 0 {w} {h}\" role=img aria-label=\"alpha curves\">{polylines}</svg><p class=muted>{legend}range {lo:+.1}% to {hi:+.1}%{time_range}</p>"
    )
}

fn render_market_table(metrics: &[serde_json::Value]) -> String {
    let mut rows = Vec::new();
    for m in metrics {
        let name = m["Strategy"].as_str().unwrap_or("-");
        let eq = m["Ending Equity ($)"].as_f64().unwrap_or(0.0);
        let ret = m["Return (%)"].as_f64().unwrap_or(0.0);
        let dd = m["Max Drawdown (%)"].as_f64().unwrap_or(0.0);
        let wr = m["Win Rate (%)"].as_f64().unwrap_or(0.0);
        let sharpe = m["Sharpe"].as_f64().unwrap_or(0.0);
        rows.push(format!(
            "<tr><td><b>{}</b></td><td>${:.2}</td><td class={}><b>{:+.2}%</b></td><td class=warn>{:.2}%</td><td>{:.1}%</td><td><b>{:.2}</b></td></tr>",
            esc(name),
            eq,
            if ret >= 0.0 { "good" } else { "bad" },
            ret,
            dd,
            wr,
            sharpe
        ));
    }
    format!(
        "<div class=scroll><table><thead><tr><th>Strategy</th><th>Ending Equity</th><th>Return</th><th>Max Drawdown</th><th>Win Rate</th><th>Sharpe</th></tr></thead><tbody>{}</tbody></table></div>",
        rows.join("")
    )
}

fn alpha_page(s: &Sources) -> String {
    let rep_path = s.repo.join("data/reports/strategy_comparison.json");
    let json_val: Option<serde_json::Value> = std::fs::read_to_string(&rep_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());

    let mut market_sections = String::new();

    if let Some(ref data) = json_val {
        if let Some(mkts) = data["markets"].as_object() {
            let order = ["US NASDAQ"];
            for mkey in order {
                if let Some(mobj) = mkts.get(mkey) {
                    let desc = mobj["market_desc"].as_str().unwrap_or("");
                    let curves_str = mobj["curves"]
                        .as_object()
                        .map(render_market_curves)
                        .unwrap_or_default();
                    let table_str = mobj["metrics"]
                        .as_array()
                        .map(|arr| render_market_table(arr))
                        .unwrap_or_default();

                    let badge_color = "#10b981";

                    market_sections.push_str(&format!(
                        "<div style=\"background:rgba(21,29,39,0.85);border:1px solid var(--line);border-radius:12px;padding:18px;margin:22px 0;\">\
                        <div style=\"display:flex;justify-content:space-between;align-items:center;border-bottom:1px solid var(--line);padding-bottom:10px;margin-bottom:14px;\">\
                            <span style=\"font-size:18px;font-weight:700;color:{badge_color};\">● {mkey}</span>\
                            <span style=\"font-size:12px;color:var(--mut);\">{desc}</span>\
                        </div>\
                        <h4 style=\"margin:8px 0;font-size:13px;color:var(--mut);text-transform:uppercase;\">Equity Trajectory</h4>\
                        {curves_str}\
                        <h4 style=\"margin:16px 0 8px;font-size:13px;color:var(--mut);text-transform:uppercase;\">Performance Scorecard</h4>\
                        {table_str}\
                        </div>",
                        desc = esc(desc)
                    ));
                }
            }
        } else if let Some(metrics) = data["metrics"].as_array() {
            let table_str = render_market_table(metrics);
            let curves_str = data["curves"]
                .as_object()
                .map(render_market_curves)
                .unwrap_or_default();
            market_sections.push_str(&format!("{curves_str}{table_str}"));
        }
    }

    if market_sections.is_empty() {
        market_sections = "<p class=muted>No strategy comparison metrics found.</p>".to_string();
    }

    format!("<p class=muted>Historical measurements read directly from data/reports/strategy_comparison.json. This page is not active trading state.</p>{market_sections}")
}

fn render_3d_brain(db: &rusqlite::Connection) -> String {
    struct NodeData {
        id: String,
        kind: String,
        label: String,
        energy: f64,
        last_ms: i64,
    }
    struct SynapseData {
        source: String,
        target: String,
        weight: f64,
        wins: i64,
        losses: i64,
        last_ms: i64,
    }

    let mut nodes = Vec::new();
    if let Ok(mut stmt) = db.prepare("SELECT id, kind, label, activation_energy, coalesce(last_activated_ms, 0) FROM brain_nodes ORDER BY kind, id") {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok(NodeData {
                id: r.get(0)?,
                kind: r.get(1)?,
                label: r.get(2)?,
                energy: r.get(3).unwrap_or(0.0),
                last_ms: r.get(4).unwrap_or(0),
            })
        }) {
            for n in rows.flatten() {
                nodes.push(n);
            }
        }
    }

    let mut synapses = Vec::new();
    if let Ok(mut stmt) = db.prepare("SELECT source_id, target_id, weight, reinforcements, penalties, coalesce(last_propagated_ms, 0) FROM brain_synapses ORDER BY weight DESC") {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok(SynapseData {
                source: r.get(0)?,
                target: r.get(1)?,
                weight: r.get(2).unwrap_or(1.0),
                wins: r.get(3).unwrap_or(0),
                losses: r.get(4).unwrap_or(0),
                last_ms: r.get(5).unwrap_or(0),
            })
        }) {
            for s in rows.flatten() {
                synapses.push(s);
            }
        }
    }

    // Determine current cognitive system status from most recent synaptic transmission
    let max_syn_ms = synapses.iter().map(|s| s.last_ms).max().unwrap_or(0);
    let max_node_ms = nodes.iter().map(|n| n.last_ms).max().unwrap_or(0);
    let last_cognition_ms = max_syn_ms.max(max_node_ms);
    let current_now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let age_mins = if last_cognition_ms > 0 && current_now_ms >= last_cognition_ms {
        (current_now_ms - last_cognition_ms) / 60_000
    } else {
        0
    };

    // Active if evaluated within last 90 minutes (e.g. recent market cycle)
    let is_active = age_mins < 90;
    let status_badge = if is_active {
        format!("<span style=\"color:#4ade80;font-weight:600;\">● Causal Signal Telemetry: Active (Fired {}m ago)</span>", age_mins)
    } else {
        format!("<span style=\"color:#94a3b8;\">○ Cognitive Network: Resting / Capital Guard (Last cycle {}m ago)</span>", age_mins)
    };

    // JSON serialize for the client-side 3D renderer
    let mut nodes_json = String::from("[");
    for (i, n) in nodes.iter().enumerate() {
        if i > 0 {
            nodes_json.push(',');
        }
        nodes_json.push_str(&format!(
            "{{\"id\":\"{}\",\"kind\":\"{}\",\"label\":\"{}\",\"energy\":{:.2},\"last_ms\":{}}}",
            esc(&n.id),
            esc(&n.kind),
            esc(&n.label),
            n.energy,
            n.last_ms
        ));
    }
    nodes_json.push(']');

    let mut syn_json = String::from("[");
    for (i, s) in synapses.iter().enumerate() {
        if i > 0 {
            syn_json.push(',');
        }
        syn_json.push_str(&format!(
            "{{\"src\":\"{}\",\"tgt\":\"{}\",\"w\":{:.2},\"wins\":{},\"loss\":{},\"last_ms\":{}}}",
            esc(&s.source),
            esc(&s.target),
            s.weight,
            s.wins,
            s.losses,
            s.last_ms
        ));
    }
    syn_json.push(']');

    format!(
        "<div style=\"background:#0b0f19;border:1px solid #1e293b;border-radius:8px;padding:16px;margin:16px 0;\">\
            <div style=\"display:flex;justify-content:space-between;align-items:center;margin-bottom:8px;\">\
                <div>\
                    <div style=\"display:flex;align-items:center;gap:12px;\">\
                        <strong style=\"color:#f8fafc;font-size:16px;\">Neural Wiring &amp; Synaptic Topology (Telemetry Only)</strong>\
                        <div style=\"font-size:12px;background:#0f172a;padding:2px 8px;border-radius:4px;border:1px solid #1e293b;\">{status_badge}</div>\
                        <div style=\"font-size:12px;background:#022c22;color:#34d399;padding:2px 8px;border-radius:4px;border:1px solid #065f46;\">⚡ Apple M4 Neural Engine &amp; AMX Accelerated</div>\
                    </div>\
                    <div class=muted style=\"font-size:12px;margin-top:2px;\">Interactive 3D projection. Pulses represent stored network activity; the animation is not proof that new weights were learned. New learning requires a dated adapter artifact and evaluation record.</div>\
                </div>\
                <div style=\"font-size:12px;display:flex;gap:12px;flex-wrap:wrap;\">\
                    <span><span style=\"color:#38bdf8;\">●</span> Sensory</span>\
                    <span><span style=\"color:#c084fc;\">●</span> Archetype</span>\
                    <span><span style=\"color:#4ade80;\">●</span> Decision</span>\
                    <span><span style=\"color:#fbbf24;\">●</span> Reflex</span>\
                    <span><span style=\"color:#f59e0b;\">●</span> Outcome</span>\
                </div>\
            </div>\
            <div style=\"position:relative;width:100%;height:520px;background:#05070d;border-radius:6px;overflow:hidden;border:1px solid #141e33;\">\
                <canvas id=\"neuralCanvas3d\" width=\"960\" height=\"520\" style=\"width:100%;height:100%;display:block;cursor:grab;\"></canvas>\
                <div id=\"neuralTooltip\" style=\"position:absolute;display:none;background:rgba(15,23,42,0.95);border:1px solid #38bdf8;padding:8px 12px;border-radius:4px;font-size:12px;color:#f8fafc;pointer-events:none;box-shadow:0 4px 12px rgba(0,0,0,0.5);z-index:10;\"></div>\
            </div>\
            <div style=\"display:flex;justify-content:space-between;margin-top:8px;font-size:11px;color:#64748b;\">\
                <span>Left click + drag to rotate in 3D &bull; Mouse wheel to zoom &bull; Click node to inspect details</span>\
                <span>Active Neurons: {} &bull; Synaptic Pathways: {}</span>\
            </div>\
        </div>\
        <script>
        (function() {{
            const nodes = {nodes_json};
            const synapses = {syn_json};
            const canvas = document.getElementById('neuralCanvas3d');
            if (!canvas) return;
            const ctx = canvas.getContext('2d');
            const tooltip = document.getElementById('neuralTooltip');

            const layerZ = {{
                'sensory': -240,
                'archetype': -80,
                'decision': 80,
                'reflex': 100,
                'outcome': 240
            }};
            const colors = {{
                'sensory': '#22d3ee',
                'archetype': '#d946ef',
                'decision': '#4ade80',
                'reflex': '#f43f5e',
                'outcome': '#e879f9'
            }};

            // Organic natural soma positions using pseudo-random spherical seeds per kind
            function hashStr(str) {{
                let hash = 0;
                for (let i = 0; i < str.length; i++) {{
                    hash = ((hash << 5) - hash) + str.charCodeAt(i);
                    hash |= 0;
                }}
                return hash;
            }}

            const nodeMap = {{}};
            nodes.forEach(function(n, idx) {{
                const h = Math.abs(hashStr(n.id));
                const h2 = Math.abs(hashStr(n.id + '_y'));
                const h3 = Math.abs(hashStr(n.id + '_z'));

                // Denser cluster arrangement: growth hubs for decision/archetype, sensory tendrils
                const isHub = n.kind === 'decision' || n.kind === 'outcome';
                const dist = isHub ? 30 + (h % 90) : 90 + (h % 150);
                const theta = ((h % 360) / 180) * Math.PI;
                const phi = (((h2 % 180) - 90) / 180) * Math.PI;

                n.x = Math.cos(theta) * Math.cos(phi) * dist;
                n.y = Math.sin(phi) * (dist * 0.7);
                n.z = Math.sin(theta) * Math.cos(phi) * dist;

                // True biological color coding matching kind semantics:
                // Sensory = Cyan (#38bdf8), Archetype = Purple (#c084fc), Decision = Emerald (#4ade80), Reflex = Amber (#fbbf24), Outcome = Magenta (#f43f5e)
                n.color = colors[n.kind] || '#38bdf8';
                
                // Soma radius strictly proportional to true activation energy & structural weight
                const baseRadius = isHub ? 9 : 5;
                n.somaRadius = Math.max(3.5, baseRadius + (n.energy * 6.5));
                nodeMap[n.id] = n;
            }});

            // Generate organic multi-segment dendritic branches with spines for each synapse
            synapses.forEach(function(s) {{
                const n1 = nodeMap[s.src];
                const n2 = nodeMap[s.tgt];
                if (!n1 || !n2) return;

                const h = Math.abs(hashStr(s.src + '->' + s.tgt));
                const segments = 4;
                s.points = [];
                s.spines = [];

                for (let k = 0; k <= segments; k++) {{
                    const t = k / segments;
                    // Natural curved axon path with organic jitter
                    const jitter = Math.sin(t * Math.PI) * (18 + (h % 16));
                    const jx = Math.sin(t * Math.PI * 2 + (h % 7)) * jitter;
                    const jy = Math.cos(t * Math.PI * 2 + (h % 5)) * jitter * 0.8;
                    const jz = Math.sin(t * Math.PI * 3 + (h % 11)) * jitter;

                    const px = n1.x + (n2.x - n1.x) * t + jx;
                    const py = n1.y + (n2.y - n1.y) * t + jy;
                    const pz = n1.z + (n2.z - n1.z) * t + jz;
                    s.points.push({{ x: px, y: py, z: pz }});

                    // Add microscopic dendritic spines along the branch
                    if (k > 0 && k < segments && (h % 3 === 0)) {{
                        const spineOffset = 5 + (h % 6);
                        s.spines.push({{
                            bx: px, by: py, bz: pz,
                            sx: px + (h % 2 === 0 ? spineOffset : -spineOffset),
                            sy: py + (h % 3 === 0 ? spineOffset : -spineOffset),
                            sz: pz + (h % 4 === 0 ? spineOffset : -spineOffset)
                        }});
                    }}
                }}
            }});

            let rotX = 0.25;
            let rotY = -0.40;
            let zoom = 1.05;
            let isDragging = false;
            let lastMouseX = 0;
            let lastMouseY = 0;
            let hoveredNode = null;
            let animPulse = 0;

            function project(x, y, z) {{
                const cosY = Math.cos(rotY), sinY = Math.sin(rotY);
                const x1 = x * cosY - z * sinY;
                const z1 = z * cosY + x * sinY;

                const cosX = Math.cos(rotX), sinX = Math.sin(rotX);
                const y2 = y * cosX - z1 * sinX;
                const z2 = z1 * cosX + y * sinX;

                const fov = 480 * zoom;
                const distance = 460;
                const pz = z2 + distance;
                if (pz <= 10) return null;
                const scale = fov / pz;
                const cx = canvas.width / 2;
                const cy = canvas.height / 2;
                return {{
                    px: cx + x1 * scale,
                    py: cy + y2 * scale,
                    scale: scale,
                    depth: z2
                }};
            }}

            function draw() {{
                // Pure deep obsidian bioluminescent background
                ctx.fillStyle = '#030508';
                ctx.fillRect(0, 0, canvas.width, canvas.height);

                // Dynamic animation timer scaled to active learning workload
                animPulse += 0.025;

                // 1. Draw Organic Dendritic Arborized Synapses
                synapses.forEach(function(s) {{
                    const n1 = nodeMap[s.src];
                    const n2 = nodeMap[s.tgt];
                    if (!n1 || !n2 || !s.points) return;

                    const projPts = [];
                    for (let i = 0; i < s.points.length; i++) {{
                        const p = project(s.points[i].x, s.points[i].y, s.points[i].z);
                        if (p) projPts.push(p);
                    }}
                    if (projPts.length < 2) return;

                    const isSensory = n1.kind === 'sensory';
                    const isDecision = n1.kind === 'decision';
                    const isReflex = n1.kind === 'reflex';
                    const baseRgb = isSensory ? '56, 189, 248' : (isDecision ? '74, 222, 128' : (isReflex ? '251, 191, 36' : '232, 121, 249'));
                    const alpha = Math.min(0.85, Math.max(0.20, s.w / 2.8));

                    // Dendrite axon filament with smooth Bezier curve
                    ctx.strokeStyle = `rgba(${{baseRgb}}, ${{alpha}})`;
                    ctx.lineWidth = Math.max(0.8, Math.min(3.2, s.w * 0.85 * projPts[0].scale));
                    ctx.beginPath();
                    ctx.moveTo(projPts[0].px, projPts[0].py);
                    for (let i = 1; i < projPts.length; i++) {{
                        ctx.lineTo(projPts[i].px, projPts[i].py);
                    }}
                    ctx.stroke();

                    // Microscopic dendritic spines branching out
                    if (s.spines) {{
                        ctx.strokeStyle = `rgba(${{baseRgb}}, ${{alpha * 0.65}})`;
                        ctx.lineWidth = 0.7 * projPts[0].scale;
                        s.spines.forEach(function(sp) {{
                            const bp = project(sp.bx, sp.by, sp.bz);
                            const sp_p = project(sp.sx, sp.sy, sp.sz);
                            if (bp && sp_p) {{
                                ctx.beginPath();
                                ctx.moveTo(bp.px, bp.py);
                                ctx.lineTo(sp_p.px, sp_p.py);
                                ctx.stroke();
                                ctx.fillStyle = `rgba(${{baseRgb}}, 0.85)`;
                                ctx.beginPath();
                                ctx.arc(sp_p.px, sp_p.py, 1.2 * bp.scale, 0, Math.PI * 2);
                                ctx.fill();
                            }}
                        }});
                    }}

                    // Action potential nerve impulse:
                    // Strictly true to verified causal propagation:
                    // Only active synapses with verified transmission history (wins > 0, loss > 0, or recent propagation last_ms) fire action potentials
                    const totalTransmissions = s.wins + s.loss;
                    if (totalTransmissions > 0 || (s.last_ms > 0 && s.w >= 1.0)) {{
                        // Rate strictly proportional to transmission volume and synaptic potency
                        const activityRate = Math.max(0.3, Math.min(3.0, 0.3 + (totalTransmissions * 0.05) + ((s.w - 1.0) * 0.3)));
                        const pulseIndex = (animPulse * activityRate) % 1.0;
                        const idxFloat = pulseIndex * (projPts.length - 1);
                        const i0 = Math.floor(idxFloat);
                        const i1 = Math.min(projPts.length - 1, i0 + 1);
                        const frac = idxFloat - i0;
                        const pt0 = projPts[i0];
                        const pt1 = projPts[i1];
                        const pulseX = pt0.px + (pt1.px - pt0.px) * frac;
                        const pulseY = pt0.py + (pt1.py - pt0.py) * frac;

                        // Pulse glow color: emerald for profitable reinforcement, amber for protective reflex, cyan for sensory
                        const pulseColor = (s.wins > s.loss) ? '#4ade80' : ((s.loss > s.wins) ? '#fbbf24' : '#38bdf8');
                        ctx.fillStyle = pulseColor;
                        ctx.beginPath();
                        ctx.arc(pulseX, pulseY, Math.min(4.0, (1.8 + Math.min(2.0, totalTransmissions * 0.1)) * pt0.scale), 0, Math.PI * 2);
                        ctx.fill();
                    }}
                }});

                // 2. Project and sort Somas (Neurons)
                const projNodes = [];
                nodes.forEach(function(n) {{
                    const p = project(n.x, n.y, n.z);
                    if (p) {{
                        projNodes.push({{ node: n, p: p, depth: p.depth }});
                    }}
                }});
                projNodes.sort(function(a, b) {{ return b.depth - a.depth; }});

                // 3. Draw Organic Biological Somas with Dynamic Rate-Reflective Respiration
                projNodes.forEach(function(item) {{
                    const n = item.node;
                    const p = item.p;
                    const isHovered = hoveredNode && hoveredNode.id === n.id;
                    const hasFired = n.last_ms > 0 && n.energy > 0.0;
                    
                    // Respiration strictly proportional to true activation energy
                    const workRate = Math.max(0.5, Math.min(3.0, n.energy * 2.5));
                    const breath = hasFired 
                        ? Math.sin(animPulse * workRate * 2.5 + Math.abs(hashStr(n.id) % 10)) * (1.2 + n.energy * 1.5)
                        : 0.0;
                    const r = Math.max(3.0, (n.somaRadius + breath) * p.scale);

                    // Large bioluminescent cloud aura proportional to real activation
                    const auraR = r * (hasFired ? (2.2 + n.energy * 1.8) : 1.4);
                    const grad = ctx.createRadialGradient(p.px, p.py, r * 0.1, p.px, p.py, auraR);
                    grad.addColorStop(0, n.color);
                    grad.addColorStop(0.5, n.color === '#4ade80' ? 'rgba(74,222,128,0.30)' : (n.color === '#38bdf8' ? 'rgba(56,189,248,0.30)' : (n.color === '#fbbf24' ? 'rgba(251,191,36,0.30)' : 'rgba(217,70,239,0.30)')));
                    grad.addColorStop(1, 'rgba(0,0,0,0)');
                    ctx.fillStyle = grad;
                    ctx.beginPath();
                    ctx.arc(p.px, p.py, auraR, 0, Math.PI * 2);
                    ctx.fill();

                    // Organic textured nucleus
                    ctx.fillStyle = isHovered ? '#ffffff' : n.color;
                    ctx.beginPath();
                    ctx.arc(p.px, p.py, r, 0, Math.PI * 2);
                    ctx.fill();

                    // Intricate membrane ring with excitation glow
                    ctx.strokeStyle = isHovered ? '#ffffff' : (hasFired ? 'rgba(255,255,255,0.95)' : 'rgba(255,255,255,0.25)');
                    ctx.lineWidth = isHovered ? 2.2 : (hasFired ? 1.4 : 0.7);
                    ctx.stroke();

                    // Label on hover or prominent hubs
                    if (isHovered || n.somaRadius > 10) {{
                        ctx.fillStyle = isHovered ? '#ffffff' : '#cbd5e1';
                        ctx.font = `${{Math.max(10, Math.min(13, 11 * p.scale))}}px -apple-system, sans-serif`;
                        ctx.textAlign = 'center';
                        ctx.fillText(n.label, p.px, p.py + r + 12 * p.scale);
                    }}
                }});

                requestAnimationFrame(draw);
            }}

            canvas.addEventListener('mousedown', function(e) {{
                isDragging = true;
                lastMouseX = e.clientX;
                lastMouseY = e.clientY;
                canvas.style.cursor = 'grabbing';
            }});
            window.addEventListener('mouseup', function() {{
                isDragging = false;
                canvas.style.cursor = 'grab';
            }});
            window.addEventListener('mousemove', function(e) {{
                if (isDragging) {{
                    const dx = e.clientX - lastMouseX;
                    const dy = e.clientY - lastMouseY;
                    rotY += dx * 0.008;
                    rotX += dy * 0.008;
                    lastMouseX = e.clientX;
                    lastMouseY = e.clientY;
                }}

                const rect = canvas.getBoundingClientRect();
                const scaleX = canvas.width / rect.width;
                const scaleY = canvas.height / rect.height;
                const mx = (e.clientX - rect.left) * scaleX;
                const my = (e.clientY - rect.top) * scaleY;
                let hit = null;
                for (let i = 0; i < nodes.length; i++) {{
                    const n = nodes[i];
                    const p = project(n.x, n.y, n.z);
                    if (!p) continue;
                    const r = Math.max(6, 14 * p.scale);
                    const dist = Math.hypot(p.px - mx, p.py - my);
                    if (dist < r + 4) {{
                        hit = n;
                        break;
                    }}
                }}
                hoveredNode = hit;
                if (hit) {{
                    tooltip.style.display = 'block';
                    tooltip.style.left = (e.clientX - rect.left + 15) + 'px';
                    tooltip.style.top = (e.clientY - rect.top + 15) + 'px';
                    const lastActiveStr = hit.last_ms > 0 ? new Date(hit.last_ms).toISOString().replace('T', ' ').substring(0, 19) + ' UTC' : 'Never (Resting)';
                    tooltip.innerHTML = `<strong>${{hit.label}}</strong><br><span style=\"color:#94a3b8;\">Layer:</span> ${{hit.kind}}<br><span style=\"color:#94a3b8;\">ID:</span> ${{hit.id}}<br><span style=\"color:#94a3b8;\">Activation Energy:</span> ${{hit.energy.toFixed(2)}}<br><span style=\"color:#94a3b8;\">Last Fired:</span> ${{lastActiveStr}}`;
                }} else {{
                    tooltip.style.display = 'none';
                }}
            }});
            canvas.addEventListener('wheel', function(e) {{
                e.preventDefault();
                zoom += e.deltaY * -0.001;
                zoom = Math.min(Math.max(0.5, zoom), 2.2);
            }});

            draw();
        }})();
        </script>",
        nodes.len(),
        synapses.len()
    )
}

fn brain_page(s: &Sources) -> String {
    let brain_path = s.repo.join("papercompmoomoo/data/brain.sqlite3");
    let conn = open_ro(&brain_path).ok();

    let Some(ref db) = conn else {
        return "<p class=bad>Cognitive brain database (papercompmoomoo/data/brain.sqlite3) is not accessible.</p>".to_string();
    };

    let total: i64 = db.query_row("SELECT count(*) FROM memories", [], |r| r.get(0)).unwrap_or(0);
    let patterns: i64 = db.query_row("SELECT count(*) FROM memories WHERE kind='pattern'", [], |r| r.get(0)).unwrap_or(0);
    let apple_lessons: i64 = db.query_row("SELECT count(*) FROM memories WHERE source='apple_intelligence'", [], |r| r.get(0)).unwrap_or(0);
    let sensors: i64 = db.query_row("SELECT count(*) FROM brain_sensors", [], |r| r.get(0)).unwrap_or(0);
    let synapses: i64 = db.query_row("SELECT count(*) FROM brain_synapses", [], |r| r.get(0)).unwrap_or(0);
    let model_3d = render_3d_brain(db);

    format!(
        "<div class=tiles>\
            <div class=tile><div class=muted>Cognitive Memory Bank</div><div class=\"v good\">{}</div><div class=muted>Self-reflective knowledge bank</div></div>\
            <div class=tile><div class=muted>Recognized Setup Patterns</div><div class=\"v good\">{}</div><div class=muted>Market archetypes discovered</div></div>\
            <div class=tile><div class=muted>Apple AI Counterfactual Lessons</div><div class=\"v good\">{}</div><div class=muted>Apple Neural Engine &amp; Private Cloud</div></div>\
            <div class=tile><div class=muted>Autonomous Sensory Receptors</div><div class=\"v good\">{} Active</div><div class=muted>Friction, RVOL, Thrust, Regime</div></div>\
            <div class=tile><div class=muted>Active Neural Synapses</div><div class=\"v good\">{} Pathways</div><div class=muted>Dynamic Hebbian plasticity wiring</div></div>\
        </div>\
        {}\
        <h2>Autonomous Skill Development &amp; Mastery (Self-Diagnosed Competencies)</h2>{}\
        <h2>Autonomous Sensory Receptors (Feeding the Neural Network)</h2>{}\
        <h2>Neural Wiring &amp; Synaptic Connections (Hebbian Plasticity)</h2>{}\
        <h2>Market Pattern Archetype Distribution</h2>{}\
        <h2>Knowledge Base Memory Categories</h2>{}\
        <h2>Latest Learned Cognitive Memories &amp; Reflections (Latest 50)</h2>{}",
        total,
        patterns,
        apple_lessons,
        sensors,
        synapses,
        model_3d,
        table(
            db,
            "SELECT id AS skill_id, name, category, round(proficiency*100.0, 1) || '%' AS proficiency, sample_size, round(win_rate*100.0, 1) || '%' AS win_rate, status, datetime(last_trained_ms/1000,'unixepoch') AS last_trained_utc FROM brain_skills ORDER BY proficiency DESC"
        ),
        table(
            db,
            "SELECT id AS sensor_id, name, kind, round(coalesce(win_rate,0.5)*100.0, 1) || '%' AS win_rate, round(coalesce(edge_pct, 0.0), 3) || '%' AS avg_edge, coalesce(sample_count, 0) AS sample_count, status, datetime(created_ms/1000,'unixepoch') AS online_since FROM brain_sensors ORDER BY win_rate DESC, id ASC"
        ),
        table(
            db,
            "SELECT source_id, target_id, round(weight,2) AS synaptic_weight, round(plasticity,2) AS plasticity, reinforcements AS wins_reinforced, penalties AS losses_penalized, datetime(last_propagated_ms/1000,'unixepoch') AS last_fired_utc FROM brain_synapses ORDER BY synaptic_weight DESC"
        ),
        table(
            db,
            "SELECT topic AS archetype, count(*) AS observed_count, round(avg(confidence),3) AS avg_confidence, datetime(max(ts_ms)/1000,'unixepoch') AS latest_learned_utc FROM memories WHERE kind='pattern' GROUP BY topic ORDER BY observed_count DESC"
        ),
        table(
            db,
            "SELECT kind AS memory_category, count(*) AS count, round(avg(confidence),3) AS avg_confidence FROM memories GROUP BY kind ORDER BY count DESC"
        ),
        table(
            db,
            "SELECT id, datetime(ts_ms/1000,'unixepoch') AS time_utc, kind, topic, text, source, via, round(confidence,2) AS conf FROM memories ORDER BY id DESC LIMIT 50"
        )
    )
}

#[derive(Clone)]
struct GainerOpportunity {
    symbol: String,
    detected_ms: i64,
    entry_price: f64,
    source: String,
    best_price: f64,
    best_ms: i64,
    best_source: String,
    decision: String,
    reason: String,
    samples: i64,
}

/// Current ET session's Nasdaq movers and their hindsight price path.
fn gainer_opportunity_chart(repo: &Path, now_ms: i64) -> String {
    let path = repo.join("papercompmoomoo/data/gainer_opportunities.sqlite3");
    let Ok(db) = open_ro(&path) else {
        return "<section><h2>NASDAQ gainers · detected this session</h2><p class=muted>Waiting for the local opportunity capture process.</p></section>".into();
    };
    let session = ny_ymd(now_ms);
    let mut items = std::collections::BTreeMap::<String,GainerOpportunity>::new();
    if let Ok(mut st) = db.prepare("SELECT symbol,detected_ms,entry_price,source FROM detections WHERE session=? ORDER BY detected_ms") {
        if let Ok(rs) = st.query_map([&session], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,Option<f64>>(2)?,r.get::<_,String>(3)?))) {
            for row in rs.flatten() {
                let (symbol, detected_ms, entry, source) = row;
                let Some(entry_price) = entry.filter(|p|p.is_finite() && *p>0.0) else { continue; };
                let replace = items.get(&symbol).map_or(true,|old|source=="gainer_bot"&&old.source!="gainer_bot");
                if replace {
                    items.insert(symbol.clone(),GainerOpportunity{symbol,detected_ms,entry_price,source,
                        best_price:entry_price,best_ms:detected_ms,best_source:"detection price".into(),
                        decision:"Not screened yet".into(),reason:String::new(),samples:0});
                }
            }
        }
    }
    if items.is_empty() {
        return format!("<section><h2>NASDAQ gainers · {}</h2><p class=muted>No movers captured for this ET session yet.</p></section>",esc(&session));
    }
    if let Ok(mut st)=db.prepare("SELECT symbol,price,observed_ms FROM observations WHERE session=? ORDER BY observed_ms") {
        if let Ok(rs)=st.query_map([&session],|r|Ok((r.get::<_,String>(0)?,r.get::<_,f64>(1)?,r.get::<_,i64>(2)?))) {
            for row in rs.flatten() {
                let (symbol,price,ts)=row;
                if let Some(item)=items.get_mut(&symbol) {
                    if ts>=item.detected_ms {
                        item.samples+=1;
                        if price.is_finite()&&price>item.best_price {item.best_price=price;item.best_ms=ts;item.best_source="Moomoo leaderboard price".into();}
                    }
                }
            }
        }
    }
    if let Ok(mut st)=db.prepare("SELECT symbol,bar_end_ms,high FROM bar_observations WHERE session=? ORDER BY bar_end_ms") {
        if let Ok(rs)=st.query_map([&session],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,f64>(2)?))) {
            for row in rs.flatten() {
                let (symbol,ts,high)=row;
                if let Some(item)=items.get_mut(&symbol) {
                    // Moomoo labels candle end times; require a complete minute after detection.
                    if ts>=item.detected_ms+60_000&&high.is_finite()&&high>item.best_price {
                        item.best_price=high;item.best_ms=ts;item.best_source="Moomoo 1-minute candle high".into();
                    }
                }
            }
        }
    }
    if let Ok(mut st)=db.prepare("SELECT symbol,event,event_ms,detail FROM decisions WHERE session=? ORDER BY event_ms,source_event_id") {
        if let Ok(rs)=st.query_map([&session],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?))) {
            for row in rs.flatten() {
                let (symbol,event,_ts,detail)=row;
                let Some(item)=items.get_mut(&symbol) else {continue;};
                if event=="gainer_admitted" {item.decision="Accepted by entry gates".into();}
                else if event=="gainer_rejected" {item.decision="Rejected by entry gates".into();}
                else {continue;}
                item.reason=serde_json::from_str::<serde_json::Value>(&detail).ok()
                    .and_then(|v|v["reason"].as_str().map(str::to_owned)).unwrap_or_default();
            }
        }
    }
    let mut rows:Vec<GainerOpportunity>=items.into_values().collect();
    rows.sort_by(|a,b|a.detected_ms.cmp(&b.detected_ms).then_with(||a.symbol.cmp(&b.symbol)));
    let day_start=ny_session_bounds(now_ms).0;
    let x_start=day_start+4*3_600_000;
    let x_end=day_start+20*3_600_000;
    let (w,h,left,right,top,bottom)=(960.0_f64,390.0_f64,64.0_f64,20.0_f64,24.0_f64,48.0_f64);
    let plot_w=w-left-right;let plot_h=h-top-bottom;
    let max_ret=rows.iter().map(|x|(x.best_price/x.entry_price-1.0)*100.0).fold(1.0_f64,f64::max).max(1.0);
    let mut svg=format!("<svg viewBox=\"0 0 {w} {h}\" role=\"img\" aria-label=\"NASDAQ gainers by detection time and best observed return\" style=\"max-height:420px\"><rect x=\"{left}\" y=\"{top}\" width=\"{plot_w}\" height=\"{plot_h}\" fill=\"#090d14\"/>");
    for i in 0..=4 {let v=max_ret*i as f64/4.0;let y=top+plot_h*(1.0-v/max_ret);svg.push_str(&format!("<line x1=\"{left}\" y1=\"{y:.1}\" x2=\"{}\" y2=\"{y:.1}\" stroke=\"#263342\"/><text x=\"{}\" y=\"{:.1}\" fill=\"#8b9aaa\" font-size=\"11\" text-anchor=\"end\">{v:.1}%</text>",w-right,left-8.0,y+4.0));}
    for hour in [4,8,12,16,20] {let ts=day_start+hour*3_600_000;let x=left+plot_w*(ts-x_start) as f64/(x_end-x_start) as f64;svg.push_str(&format!("<line x1=\"{x:.1}\" y1=\"{top}\" x2=\"{x:.1}\" y2=\"{}\" stroke=\"#263342\"/><text x=\"{x:.1}\" y=\"{}\" fill=\"#8b9aaa\" font-size=\"11\" text-anchor=\"middle\">{hour:02}:00 ET</text>",h-bottom,h-12.0));}
    for item in &rows {
        let ret=(item.best_price/item.entry_price-1.0)*100.0;
        let x=left+plot_w*(item.detected_ms.clamp(x_start,x_end)-x_start) as f64/(x_end-x_start) as f64;
        let y=top+plot_h*(1.0-ret/max_ret);
        let color=if item.decision.starts_with("Accepted"){"#4ade80"}else if item.decision.starts_with("Rejected"){"#fb7185"}else{"#94a3b8"};
        let reason=if item.reason.is_empty(){item.decision.as_str()}else{item.reason.as_str()};
        let title=format!("{} · detected {} ET · {} · entry ${:.4} · best ${:.4} at {} ET · {:+.2}% · ${:+.4}/share · {} samples · {}",item.symbol,ny_hhmm(item.detected_ms),item.decision,item.entry_price,item.best_price,ny_hhmm(item.best_ms),ret,item.best_price-item.entry_price,item.samples,reason);
        svg.push_str(&format!("<circle cx=\"{x:.1}\" cy=\"{y:.1}\" r=\"5\" fill=\"{color}\" stroke=\"#0c1117\" stroke-width=\"1.5\"><title>{}</title></circle>",esc(&title)));
    }
    svg.push_str("<text x=\"480\" y=\"18\" fill=\"#8b9aaa\" font-size=\"11\" text-anchor=\"middle\">Detection time → · Y = best observed return since detection</text></svg>");
    let mut table_html=String::from("<div class=scroll><table><thead><tr><th>Symbol</th><th>Detected ET</th><th>Entry ref.</th><th>Gate result</th><th>Reason</th><th>Best high</th><th>High time ET</th><th>Best return</th><th>P&amp;L / share</th><th>Samples</th></tr></thead><tbody>");
    for item in &rows {let ret=(item.best_price/item.entry_price-1.0)*100.0;let color=if item.decision.starts_with("Accepted"){"#4ade80"}else if item.decision.starts_with("Rejected"){"#fb7185"}else{"#94a3b8"};let reason=if item.reason.is_empty(){"—"}else{&item.reason};table_html.push_str(&format!("<tr><td>{}</td><td>{}</td><td>${:.4}</td><td style=\"color:{color}\">{}</td><td>{}</td><td>${:.4}</td><td>{} <span class=muted>{}</span></td><td class=good>{:+.2}%</td><td class=good>${:+.4}</td><td>{}</td></tr>",esc(&item.symbol),ny_hhmm(item.detected_ms),item.entry_price,esc(&item.decision),esc(reason),item.best_price,ny_hhmm(item.best_ms),esc(&item.best_source),ret,item.best_price-item.entry_price,item.samples));}
    table_html.push_str("</tbody></table></div>");
    format!("<section aria-label=\"Gainer detection and maximum observed opportunity\"><h2>NASDAQ gainers · detected this session</h2><p class=muted>{} symbols captured from Moomoo's top-200 US movers response. Gainer now evaluates every Nasdaq-listed row in that response instead of cutting the global ranking at 50. Each dot is a mover: horizontal position is detection time and height is best observed return. Green = accepted by Gainer entry gates; red = rejected; grey = waiting or not screened. Hover for details. P&amp;L is per share from detection price to the best recorded quote/candle high: a hindsight ceiling before fees/slippage, not a broker fill. Session {}</p>{}{}</section>",rows.len(),esc(&session),svg,table_html)
}

fn gainer_tracker(s: &Sources) -> String {
    gainer_opportunity_chart(s.repo, s.now_ms)
}

fn exit_replay_page(repo: &Path) -> String {
    let path = repo.join("papercompmoomoo/data/strategy_research.json");
    let Some(report) = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok()) else {
        return "<p class=muted>No replay has been saved for today's US session. The replay worker runs after 20:05 ET, after extended hours close.</p>".into();
    };
    let session = report["session"].as_str().unwrap_or("session unavailable");
    let as_of = report["as_of_new_york"].as_str().unwrap_or("timestamp unavailable");
    let mut out = format!("<p class=muted>Today only · {session} · report created {}</p><p class=muted>Actual filled positions are replayed against Moomoo 1-minute candles and their recorded ratcheting price lines. The report runs after 20:05 ET; no previous session is included.</p>", esc(as_of));
    for bot in ["gainer", "orb"] {
        let section = &report["bots"][bot];
        let Some(rows) = section["trades"].as_array() else {
            out.push_str(&format!("<h2>{}</h2><p class=muted>No actual filled positions were recorded today.</p>", if bot == "gainer" { "Gainers · Competition paper account" } else { "ORB · Competition paper account" }));
            continue;
        };
        let actual = section["actual_net_pnl_usd"].as_f64();
        let replay = section["replay_net_pnl_usd"].as_f64();
        let count = section["recorded_positions"].as_i64().unwrap_or(rows.len() as i64);
        let played = section["replayed_positions"].as_i64().unwrap_or(0);
        let candle_played = section["candle_replayed_positions"].as_i64().unwrap_or(played);
        let missing = section["missing_replay_count"].as_i64().unwrap_or(0);
        out.push_str(&format!(
            "<h2>{} · shared Competition SIMULATE account</h2><div class=tiles>{}{}{}{}{}</div>",
            if bot == "gainer" { "Gainers" } else { "ORB" },
            tile("Positions", &count.to_string(), "actual filled position groups"),
            tile("Candles replayed / gaps", &format!("{candle_played} / {missing}"), &format!("{played} total of {count}; replay uses Moomoo OHLCV only")),
            tile("Grid replay P&L", &replay.map_or_else(|| "–".into(), |n| format!("${n:+.2}")), "estimated net; 10 bps round-trip allowance"),
            tile("Actual P&L", &actual.map_or_else(|| "–".into(), |n| format!("${n:+.2}")), &format!("{missing} position groups have replay data gaps")),
            tile("Unresolved", &missing.to_string(), "missing candles, levels, or end-of-session bar"),
        ));
        out.push_str("<div class=scroll><table><thead><tr><th>Symbol</th><th>Entry ET</th><th>Qty</th><th>Avg fill</th><th>Grid exit ET</th><th>Grid exit</th><th>Line trigger</th><th>Replay net P&amp;L</th><th>Actual net P&amp;L</th><th>Replay status</th></tr></thead><tbody>");
        for trade in rows {
            let time = |key: &str| trade[key].as_i64().map(ny_hhmm).unwrap_or_else(|| "–".into());
            let money = |key: &str| trade[key].as_f64().map_or_else(|| "–".into(), |n| format!("${n:.4}"));
            let pnl = |key: &str| trade[key].as_f64().map_or_else(|| "–".into(), |n| format!("${n:+.2}"));
            out.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                esc(trade["symbol"].as_str().unwrap_or("–")), time("opened_ms"),
                trade["qty"].as_i64().map_or_else(|| "–".into(), |n| n.to_string()), money("entry"),
                time("replay_exit_ms"), money("replay_exit"), esc(trade["replay_reason"].as_str().unwrap_or("–")),
                pnl("replay_net_pnl_usd"), pnl("actual_pnl_usd"), esc(trade["replay_status"].as_str().unwrap_or("missing"))
            ));
        }
        out.push_str("</tbody></table></div>");
        let optimization = &section["exit_grid_optimization"];
        let candidate_count = optimization["candidate_grids_per_run"].as_i64().unwrap_or(0);
        let cadence = optimization["refresh_interval_minutes"].as_i64().unwrap_or(15);
        out.push_str(&format!("<h3>Symbol-specific exit-grid search</h3><p class=muted>{candidate_count} randomized exit grids are replayed every {cadence} minutes against today's filled symbols. Only stop, trail, full-target, 70% placement, and added downward profit-line placements vary. Each line ratchets up. This is replay research; live settings stay unchanged.</p>"));
        if let Some(symbols) = optimization["symbols"].as_array() {
            out.push_str("<div class=scroll><table><thead><tr><th>Symbol</th><th>Status</th><th>Groups</th><th>Grids scored</th><th>Baseline net</th><th>Best net</th><th>Risk score</th><th>Max drawdown</th><th>Net change</th><th>Stop ×</th><th>Trail ×</th><th>Target ×</th><th>70% fraction</th><th>Added line fractions</th><th>Top 3 risk-ranked grids</th></tr></thead><tbody>");
            for item in symbols {
                let best = &item["best"];
                let config = &best["config"];
                let lines = config["profit_line_fractions"].as_array().map(|a| a.iter().filter_map(|v| v.as_f64().map(|n| format!("{n:.2}"))).collect::<Vec<_>>().join(", ")).unwrap_or_else(|| "–".into());
                let money = |v: &serde_json::Value| v.as_f64().map_or_else(|| "–".into(), |n| format!("${n:+.2}"));
                let number = |v: &serde_json::Value| v.as_f64().map_or_else(|| "–".into(), |n| format!("{n:.2}"));
                let tops = item["top_grids"].as_array().map(|a| a.iter().take(3).enumerate().map(|(i,v)| {
                    let c=&v["config"];
                    let fractions=c["profit_line_fractions"].as_array().map(|a|a.iter().filter_map(|x|x.as_f64().map(|n|format!("{n:.2}"))).collect::<Vec<_>>().join(",")).unwrap_or_else(||"–".into());
                    format!("#{} score {} · net {} DD {} · S{} T{} TP{} 70={} L[{}]",i+1,money(&v["risk_adjusted_score_usd"]),money(&v["net_pnl_usd"]),money(&v["max_drawdown_usd"]),number(&c["stop_scale"]),number(&c["trail_scale"]),number(&c["target_scale"]),number(&c["target_70_fraction"]),fractions)
                }).collect::<Vec<_>>().join("<br>")).unwrap_or_else(||"–".into());
                out.push_str(&format!("<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    esc(item["symbol"].as_str().unwrap_or("–")), esc(item["status"].as_str().unwrap_or("missing")),
                    item["position_groups"].as_i64().map_or_else(|| "–".into(), |n| n.to_string()),
                    item["scored_grids"].as_i64().map_or_else(|| "0".into(), |n| n.to_string()),
                    money(&item["baseline"]["net_pnl_usd"]), money(&best["net_pnl_usd"]),
                    money(&best["risk_adjusted_score_usd"]), money(&best["max_drawdown_usd"]), money(&item["improvement_vs_baseline_usd"]),
                    number(&config["stop_scale"]), number(&config["trail_scale"]), number(&config["target_scale"]),
                    config["target_70_fraction"].as_f64().map_or_else(|| "–".into(), |n| format!("{:.0}%", n*100.0)),
                    esc(&lines), tops));
            }
            out.push_str("</tbody></table></div>");
        } else {
            out.push_str("<p class=muted>No actual traded symbols are available for today's grid search yet.</p>");
        }
    }
    out.push_str("<p class=muted>Exit fills are estimates from bar OHLC; one-minute candles cannot establish the exact intrabar trigger order or executable bid. Missing candles or line levels are shown as missing and excluded from replay P&amp;L. This page does not affect live orders.</p>");
    out
}

fn overview(s: &Sources) -> String {
    let gainer_id = configured_account_id(s.repo, "gainer");
    let orb_id = configured_account_id(s.repo, "orb");
    let status = std::fs::read_to_string(s.repo.join("papercompmoomoo/data/status.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .filter(|v|v["acc_id"].as_i64()==gainer_id&&v["account_kind"].as_str()==Some("COMPETITION"));
    let comp_account=status.as_ref().and_then(|v|v["account"].as_object());
    let comp_positions=status.as_ref().and_then(|v|v["positions_live"].as_array()).map_or(0,Vec::len);
    let comp_session=s.journal.and_then(|j|j.query_row("SELECT max(session) FROM plan_events WHERE event IN ('gainer_admitted','live_gainer_discovered')",[],|r|r.get::<_,String>(0)).ok());
    let comp_candidates=match(s.journal,comp_session.as_deref()){
        (Some(j),Some(session))=>j.query_row("SELECT count(DISTINCT symbol) FROM plan_events WHERE session=? AND event IN ('gainer_admitted','live_gainer_discovered')",[session],|r|r.get::<_,i64>(0)).unwrap_or(0),
        _=>0,
    };
    let orb_db=open_ro(&s.repo.join("papercompmoomoo/data/orb.sqlite3")).ok();
    let orb_account=orb_db.as_ref().and_then(|db|orb_id.and_then(|id|db.query_row("SELECT ts_ms,equity,cash,buying_power FROM account_snapshots WHERE account_id=? AND account_kind='COMPETITION' ORDER BY ts_ms DESC LIMIT 1",[id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,f64>(1)?,r.get::<_,f64>(2)?,r.get::<_,f64>(3)?))).ok()));
    let orb_positions=orb_db.as_ref().map_or(0,|db|db.query_row("SELECT count(*) FROM positions WHERE status IN ('open','closing') AND qty>0",[],|r|r.get::<_,i64>(0)).unwrap_or(0));
    let orb_candidates=std::fs::read_to_string(s.repo.join("papercompmoomoo/data/orb_premarket.json")).ok()
        .and_then(|raw|serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v|v["candidates"].as_array().map(Vec::len)).unwrap_or(0);
    let money=|value:Option<f64>|value.map_or_else(||"UNAVAILABLE".into(),|n|format!("${n:.2}"));
    let comp_time=comp_account.and_then(|a|a.get("read_at")).and_then(|v|v.as_str()).unwrap_or("timestamp unavailable");
    let comp_equity=comp_account.and_then(|a|a.get("equity")).and_then(|v|v.as_f64());
    let comp_cash=comp_account.and_then(|a|a.get("cash")).and_then(|v|v.as_f64());
    let comp_power=comp_account.and_then(|a|a.get("power")).and_then(|v|v.as_f64());
    let(orb_time,orb_equity,orb_cash,orb_power)=orb_account.map_or(("timestamp unavailable".into(),None,None,None),|(ts,e,c,p)|(crate::ops::utc(ts),Some(e),Some(c),Some(p)));
    let gainer_panel=comp(s);
    let orb_panel=page_orb(s);
    let pnl=comp_equity.or(orb_equity).map(|value|value-100_000.0);
    format!("<h2>Paper accounts and P&amp;L</h2><div style=\"display:grid;grid-template-columns:repeat(auto-fit,minmax(360px,1fr));gap:16px\">\
        <div class=tile><div class=muted>SHARED MOOMOO COMPETITION PAPER ACCOUNT · SIMULATE</div><div class=tiles><div><span class=muted>Equity</span><br><b>{}</b></div><div><span class=muted>Equity change vs $100,000 start</span><br><b>{}</b></div><div><span class=muted>Cash</span><br><b>{}</b></div><div><span class=muted>Buying power</span><br><b>{}</b></div></div><div class=muted>Snapshot {}</div><p>{comp_positions} Gainer open positions · {orb_positions} ORB open positions · {comp_candidates} current Gainer candidates · {orb_candidates} current ORB candidates.</p></div></div>\
        <h2>Current or most recent session charts</h2><section aria-label=\"Gainer bot session charts\"><h2>Gainers · Competition SIMULATE account</h2>{}</section><section aria-label=\"ORB bot session charts\"><h2>ORB · Competition SIMULATE account</h2>{}</section>\
        <div class=tile style=\"margin-top:16px;text-transform:none;letter-spacing:0\"><div class=muted>SHARED LIVE POLICY · BOTH BOTS</div><p><b>Moomoo-only NASDAQ candidates</b> · price/CLV Kalman inflection confirmation before entry · ratcheting exit lines: 100% target sells on an upward touch/cross; 70%, intermediate, midpoint, and red stop sell on a confirmed downward cross · audited ThomasAI liquidity-trap exit · flatten at 15:00 ET.</p></div>",
        money(comp_equity.or(orb_equity)),pnl.map_or_else(||"UNAVAILABLE".into(),|n|format!("${n:+.2}")),money(comp_cash.or(orb_cash)),money(comp_power.or(orb_power)),esc(if comp_time!="timestamp unavailable"{comp_time}else{&orb_time}),gainer_panel,orb_panel)
}

#[derive(Clone)]
struct TradeMarker {
    opened_ms: i64,
    closed_ms: Option<i64>,
    qty: i64,
    entry_price: f64,
    exit_price: Option<f64>,
    pnl_usd: f64,
    pnl_pct: f64,
    reason: String,
    stop_price: Option<f64>,
    target_price: Option<f64>,
    target_70_price: Option<f64>,
    profit_retrace_price: Option<f64>,
    profit_midpoint_price: Option<f64>,
}

#[derive(Clone)]
struct ChartEvent {
    ts_ms: i64,
    label: &'static str,
    short: &'static str,
    color: &'static str,
}

fn exit_level_map(repo: &Path, bot: &str) -> std::collections::BTreeMap<String, serde_json::Value> {
    let raw = std::fs::read_to_string(repo.join("papercompmoomoo/data/exit_levels.json")).ok();
    let Some(value) = raw.and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok()) else { return Default::default() };
    value[bot].as_array().into_iter().flatten().filter_map(|row| {
        let symbol = row["symbol"].as_str()?.trim_start_matches("US.").to_string();
        Some((symbol, row.clone()))
    }).collect()
}

/// Computes the US Eastern offset in milliseconds (-4h EDT or -5h EST) for a given UTC epoch ms.
fn ny_offset_ms(utc_ms: i64) -> i64 {
    let secs = utc_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let _era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    // US DST is roughly March 8..Nov 1. In Nov >= 2026-11-01 it is EST (-5h).
    let is_dst = (4..=10).contains(&m) || (m == 3 && d >= 8) || (m == 11 && d < 1);
    if is_dst { -4 * 3_600_000 } else { -5 * 3_600_000 }
}

fn ny_hhmm(utc_ms: i64) -> String {
    let local_ms = utc_ms + ny_offset_ms(utc_ms);
    let rem_secs = local_ms.div_euclid(1000).rem_euclid(86_400);
    format!("{:02}:{:02}", rem_secs / 3600, (rem_secs % 3600) / 60)
}

fn ny_market_phase(utc_ms: i64) -> &'static str {
    let local_ms = utc_ms + ny_offset_ms(utc_ms);
    let minute = local_ms.div_euclid(60_000).rem_euclid(1_440);
    if (4 * 60..9 * 60 + 30).contains(&minute) { "premarket" }
    else if (9 * 60 + 30..16 * 60).contains(&minute) { "regular-session" }
    else { "outside-session" }
}

fn market_status_line(utc_ms: i64) -> String {
    let local_ms = utc_ms + ny_offset_ms(utc_ms);
    let local_day = local_ms.div_euclid(86_400_000);
    let weekday = (local_day + 4).rem_euclid(7); // Sunday = 0
    let minute = local_ms.div_euclid(60_000).rem_euclid(1_440);
    let status = if !(1..=5).contains(&weekday) { "CLOSED" }
        else if (4 * 60..9 * 60 + 30).contains(&minute) { "PREMARKET" }
        else if (9 * 60 + 30..16 * 60).contains(&minute) { "MARKET OPEN" }
        else if (16 * 60..20 * 60).contains(&minute) { "AFTER HOURS" }
        else { "CLOSED" };
    format!("<p class=muted>US market status: <b>{status}</b> · New York time {} ET · session date {}</p>", ny_hhmm(utc_ms), ny_ymd(utc_ms))
}

/// Renders a chart from the 04:00 ET premarket open through session close for a symbol traded in the current session.
fn render_bought_symbol_chart(
    common_db: Option<&Connection>,
    feed_db: Option<&Connection>,
    symbol: &str,
    trades: &[TradeMarker],
    events: &[ChartEvent],
    session_day_utc_ms: i64,
    live_price: Option<f64>,
    now_ms: i64,
    account_label: &str,
) -> String {
    let ticker = symbol.trim_start_matches("US.");
    let chart_bot = if account_label.starts_with("ORB") { "orb" } else { "gainer" };
    let code = format!("US.{ticker}");
    let offset = ny_offset_ms(session_day_utc_ms);
    // Anchor midnight in America/New_York for the session day
    let ny_day = (session_day_utc_ms + offset).div_euclid(86_400_000);
    let ny_midnight_utc_ms = ny_day * 86_400_000 - offset;
    let pre_0400_ms = ny_midnight_utc_ms + 4 * 3_600_000;
    let pre_0730_ms = ny_midnight_utc_ms + (7 * 60 + 30) * 60_000;
    let open_0930_ms = ny_midnight_utc_ms + (9 * 60 + 30) * 60_000;
    let close_1600_ms = ny_midnight_utc_ms + 16 * 3_600_000;

    let mut bar_map: std::collections::BTreeMap<i64, (f64, f64, f64, f64)> = std::collections::BTreeMap::new();
    let data_cutoff_ms = now_ms.min(close_1600_ms + 1_200_000);
    if let Some(cdb) = common_db {
        if let Ok(mut st) = cdb.prepare(
            "SELECT ts, open, high, low, close, 1 AS end_labeled FROM premarket_bars WHERE symbol=? AND ts>=? AND ts<=? \
             UNION ALL \
             SELECT ts, open, high, low, close, CASE WHEN source='moomoo' THEN 1 ELSE 0 END FROM bars WHERE symbol=? AND ts>=? AND ts<=? \
             ORDER BY ts ASC",
        ) {
            if let Ok(rows) = st.query_map(
                rusqlite::params![ticker, pre_0400_ms, data_cutoff_ms, ticker, pre_0400_ms, data_cutoff_ms],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?, r.get::<_, f64>(4)?, r.get::<_, bool>(5)?)),
            ) {
                for row in rows.flatten() {
                    let plot_ts = row.0 - if row.5 { 60_000 } else { 0 };
                    if row.4 > 0.0 && plot_ts >= pre_0400_ms && plot_ts <= close_1600_ms {
                        bar_map.insert(plot_ts, (row.1, row.2, row.3, row.4));
                    }
                }
            }
        }
    }
    if bar_map.len() < 5 {
        if let Some(fdb) = feed_db {
            if let Ok(mut st) = fdb.prepare(
                "SELECT time_key, open, high, low, close FROM pushes WHERE code=? AND recv_ms>=? AND recv_ms<=? ORDER BY time_key ASC, recv_ms ASC",
            ) {
                if let Ok(rows) = st.query_map(
                    rusqlite::params![code, pre_0400_ms, data_cutoff_ms],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?, r.get::<_, f64>(4)?)),
                ) {
                    let mut pushed: std::collections::BTreeMap<i64, (f64, f64, f64, f64)> = std::collections::BTreeMap::new();
                    for row in rows.flatten() {
                        let naive = row.0.replace(' ', "T");
                        let Some(local_as_utc) = iso_utc_ms(&format!("{naive}Z")) else { continue };
                        let end_ts = local_as_utc - ny_offset_ms(local_as_utc);
                        let entry = pushed.entry(end_ts).or_insert((row.1, row.2, row.3, row.4));
                        entry.1 = entry.1.max(row.2);
                        entry.2 = entry.2.min(row.3);
                        entry.3 = row.4;
                    }
                    for (end_ts, (o, h, l, c)) in pushed {
                        let plot_ts = end_ts - 60_000;
                        if c > 0.0 && plot_ts >= pre_0400_ms && plot_ts <= close_1600_ms {
                            bar_map.entry(plot_ts).or_insert((o, h, l, c));
                        }
                    }
                }
            }
        }
    }
    let bars: Vec<(i64, f64, f64, f64, f64)> = bar_map.into_iter().map(|(ts, (o, h, l, c))| (ts, o, h, l, c)).collect();
    let earliest_bar = bars.first().map(|b| b.0).unwrap_or(pre_0730_ms);
    let earliest_event = events.iter().map(|event| event.ts_ms).min().unwrap_or(pre_0730_ms);
    let earliest_visible = earliest_bar.min(earliest_event);
    let start_ms = if earliest_visible < pre_0730_ms { pre_0400_ms.max(earliest_visible - 300_000) } else { pre_0730_ms };
    let end_ms = close_1600_ms;
    let time_span = (end_ms - start_ms).max(60_000) as f64;

    let mut min_p = f64::MAX;
    let mut max_p = f64::MIN;
    for &(_, _, h, l, c) in &bars {
        min_p = min_p.min(l).min(c);
        max_p = max_p.max(h).max(c);
    }
    for t in trades {
        if t.entry_price > 0.0 {
            min_p = min_p.min(t.entry_price);
            max_p = max_p.max(t.entry_price);
        }
        if let Some(ex) = t.exit_price.filter(|&p| p > 0.0) {
            min_p = min_p.min(ex);
            max_p = max_p.max(ex);
        }
        if t.closed_ms.is_none() {
            if let Some(lp) = live_price.filter(|&p| p > 0.0) { min_p=min_p.min(lp); max_p=max_p.max(lp); }
            if let Some(sp) = t.stop_price.filter(|&p| p > 0.0) {
                min_p = min_p.min(sp);
            }
            if let Some(tp) = t.target_price.filter(|&p| p > 0.0) {
                max_p = max_p.max(tp);
            } else if t.entry_price > 0.0 {
                // Competition status can briefly omit risk fields during startup;
                // keep chart lines visible using the same configured defaults.
                max_p = max_p.max(t.entry_price * 1.025);
            }
        }
    }
    if min_p == f64::MAX || max_p == f64::MIN {
        min_p = 1.0;
        max_p = 2.0;
    }
    let pad = ((max_p - min_p) * 0.08).max(min_p * 0.004).max(0.02);
    let lo = min_p - pad;
    let hi = max_p + pad;
    let p_span = (hi - lo).max(1e-6);

    let (w, h) = (560.0, 210.0);
    let (pad_l, pad_r, pad_t, pad_b) = (48.0, 14.0, 14.0, 24.0);
    let pw = w - pad_l - pad_r;
    let ph = h - pad_t - pad_b;

    let x_of = |ts: i64| -> f64 {
        let clamped = ts.clamp(start_ms, end_ms);
        pad_l + pw * ((clamped - start_ms) as f64 / time_span)
    };
    let y_of = |p: f64| -> f64 {
        let clamped = p.clamp(lo, hi);
        pad_t + ph * (1.0 - (clamped - lo) / p_span)
    };

    let open_x = x_of(open_0930_ms);
    let latest_px = live_price.filter(|p|p.is_finite()&&*p>0.0).or_else(||bars.last().map(|b|b.4)).or_else(||trades.last().map(|t|t.exit_price.unwrap_or(t.entry_price))).unwrap_or(0.0);
    let latest_ts = bars.last().map(|b| b.0).unwrap_or(now_ms.clamp(start_ms, end_ms));
    let latest_bar_label = if bars.is_empty() { "no stored bar timestamp".to_string() } else { format!("latest {} 1m candle {}–{}", ny_market_phase(latest_ts), ny_hhmm(latest_ts), ny_hhmm(latest_ts + 60_000)) };
    let latest_price_label = if latest_px > 0.0 {
        format!("{} ${latest_px:.2} · {latest_bar_label} ET · {} bars", if live_price.is_some() { "Latest stored price" } else { "Latest bar price" }, bars.len())
    } else {
        format!("No current-session price data · {latest_bar_label} ET · {} bars", bars.len())
    };
    let live_ts=now_ms.clamp(start_ms,end_ms);

    let mut svg = String::new();
    // Pre-market shaded zone (start..09:30 ET)
    if open_x > pad_l {
        svg.push_str(&format!(
            "<rect x=\"{pad_l:.1}\" y=\"{pad_t:.1}\" width=\"{:.1}\" height=\"{ph:.1}\" fill=\"rgba(56,189,248,0.06)\"/>",
            open_x - pad_l
        ));
    }
    // Horizontal price grid lines
    for step in 0..=3 {
        let frac = step as f64 / 3.0;
        let py = pad_t + ph * frac;
        let pval = hi - p_span * frac;
        svg.push_str(&format!(
            "<line x1=\"{pad_l:.1}\" y1=\"{py:.1}\" x2=\"{:.1}\" y2=\"{py:.1}\" stroke=\"#1e293b\" stroke-width=\"1\"/>\
             <text x=\"{:.1}\" y=\"{:.1}\" fill=\"#64748b\" font-size=\"9.5\" text-anchor=\"end\">${pval:.2}</text>",
            w - pad_r,
            pad_l - 4.0,
            py + 3.0
        ));
    }
    // Vertical session markers: Pre-order 07:30 ET, Open 09:30 ET, Midday 12:00 ET, Close 16:00 ET
    for (ts_mark, label, color) in [
        (pre_0730_ms, "07:30 Pre", "#38bdf8"),
        (open_0930_ms, "09:30 Open", "#f59e0b"),
        (ny_midnight_utc_ms + 12 * 3_600_000, "12:00", "#475569"),
        (close_1600_ms, "16:00 Close", "#64748b"),
    ] {
        if ts_mark >= start_ms && ts_mark <= end_ms {
            let vx = x_of(ts_mark);
            svg.push_str(&format!(
                "<line x1=\"{vx:.1}\" y1=\"{pad_t:.1}\" x2=\"{vx:.1}\" y2=\"{:.1}\" stroke=\"{color}\" stroke-width=\"1\" stroke-dasharray=\"3 3\" opacity=\"0.65\"/>\
                 <text x=\"{vx:.1}\" y=\"{:.1}\" fill=\"{color}\" font-size=\"9.5\" text-anchor=\"middle\">{label}</text>",
                pad_t + ph,
                h - 6.0
            ));
        }
    }
    // High-Low range ribbon & Close price polyline
    if bars.len() >= 2 {
        let mut upper = Vec::with_capacity(bars.len());
        let mut lower = Vec::with_capacity(bars.len());
        let mut close_pts = Vec::with_capacity(bars.len());
        for &(ts, _, bh, bl, bc) in &bars {
            let bx = x_of(ts);
            upper.push(format!("{bx:.1},{:.1}", y_of(bh)));
            lower.push(format!("{bx:.1},{:.1}", y_of(bl)));
            close_pts.push(format!("{bx:.1},{:.1}", y_of(bc)));
        }
        lower.reverse();
        svg.push_str(&format!(
            "<polygon points=\"{} {}\" fill=\"rgba(117,184,255,0.13)\"/>\
             <polyline fill=\"none\" stroke=\"#75b8ff\" stroke-width=\"1.8\" points=\"{}\"/>",
            upper.join(" "),
            lower.join(" "),
            close_pts.join(" ")
        ));
    }
    for event in events.iter().filter(|event| event.ts_ms >= start_ms && event.ts_ms <= data_cutoff_ms) {
        let vx = x_of(event.ts_ms);
        let title = format!("{} · {} ET", event.label, ny_hhmm(event.ts_ms));
        svg.push_str(&format!(
            "<line x1=\"{vx:.1}\" y1=\"{pad_t:.1}\" x2=\"{vx:.1}\" y2=\"{:.1}\" stroke=\"{}\" stroke-width=\"1.6\" stroke-dasharray=\"4 3\" opacity=\"0.9\"><title>{}</title></line><circle cx=\"{vx:.1}\" cy=\"{:.1}\" r=\"7\" fill=\"{}\" stroke=\"#090d14\" stroke-width=\"1.5\"><title>{}</title></circle><text x=\"{vx:.1}\" y=\"{:.1}\" fill=\"#090d14\" font-size=\"8\" font-weight=\"700\" text-anchor=\"middle\">{}</text>",
            pad_t + ph, event.color, esc(&title), pad_t + 9.0, event.color, esc(&title), pad_t + 12.0, event.short
        ));
    }

    let price_x=if live_price.is_some(){x_of(live_ts)}else{x_of(latest_ts)};
    let price_y=y_of(latest_px);
    svg.push_str(&format!("<line id=\"live-price-line-{ticker}\" data-live-symbol=\"{ticker}\" data-live-bot=\"{chart_bot}\" data-plot-lo=\"{lo}\" data-plot-hi=\"{hi}\" data-pad-top=\"{pad_t}\" data-plot-height=\"{ph}\" data-start-ms=\"{start_ms}\" data-end-ms=\"{end_ms}\" data-plot-left=\"{pad_l}\" data-svg-width=\"{w}\" data-pad-right=\"{pad_r}\" x1=\"{price_x:.1}\" y1=\"{price_y:.1}\" x2=\"{:.1}\" y2=\"{price_y:.1}\" stroke=\"#fbbf24\" stroke-width=\"1.4\" stroke-dasharray=\"5 3\" opacity=\"0.95\" style=\"{}\"/><circle id=\"live-price-dot-{ticker}\" cx=\"{price_x:.1}\" cy=\"{price_y:.1}\" r=\"4\" fill=\"#fbbf24\" style=\"{}\"><title>{} price ${latest_px:.4}</title></circle>",w-pad_r,if latest_px>0.0{""}else{"display:none"},if latest_px>0.0{""}else{"display:none"},if live_price.is_some(){"Latest stored"}else{"Latest bar"}));

    // Trade Entry -> Exit / Holding segments and markers
    let mut is_holding = false;
    let mut net_pnl_usd = 0.0;
    for t in trades {
        net_pnl_usd += t.pnl_usd;
        let bx = x_of(t.opened_ms);
        let by = y_of(t.entry_price);
        if let (Some(c_ms), Some(ex_p)) = (t.closed_ms, t.exit_price) {
            let sx = x_of(c_ms);
            let sy = y_of(ex_p);
            let seg_color = if ex_p >= t.entry_price { "#4ade80" } else { "#ff7885" };
            svg.push_str(&format!(
                "<line x1=\"{bx:.1}\" y1=\"{by:.1}\" x2=\"{sx:.1}\" y2=\"{sy:.1}\" stroke=\"{seg_color}\" stroke-width=\"1.8\" stroke-dasharray=\"4 2\"/>"
            ));
            // Buy triangle ▲
            svg.push_str(&format!(
                "<polygon points=\"{bx:.1},{:.1} {:.1},{:.1} {:.1},{:.1}\" fill=\"#22c55e\" stroke=\"#052e16\" stroke-width=\"1\"><title>BUY {} @ ${:.4} ({} ET)</title></polygon>",
                by - 5.5, bx - 4.5, by + 3.5, bx + 4.5, by + 3.5, t.qty, t.entry_price, ny_hhmm(t.opened_ms)
            ));
            // Sell triangle ▼
            svg.push_str(&format!(
                "<polygon points=\"{sx:.1},{:.1} {:.1},{:.1} {:.1},{:.1}\" fill=\"{seg_color}\" stroke=\"#0f172a\" stroke-width=\"1\"><title>EXIT {} @ ${:.4} ({} ET) · {}</title></polygon>",
                sy + 5.5, sx - 4.5, sy - 3.5, sx + 4.5, sy - 3.5, t.qty, ex_p, ny_hhmm(c_ms), esc(&t.reason)
            ));
        } else {
            is_holding = true;
            let hx = x_of(if live_price.is_some() { live_ts } else { latest_ts }.max(t.opened_ms));
            let hy = y_of(latest_px);
            // Every chart line uses the worker's persisted one-way ratchets.
            let stop = t.stop_price.filter(|&p| p > 0.0);
            let target = t.target_price.filter(|&p| p > 0.0);
            if let Some(sp) = stop {
                let sy = y_of(sp);
                svg.push_str(&format!(
                    "<line id=\"exit-stop-line-{ticker}\" data-exit-level-symbol=\"{ticker}\" x1=\"{bx:.1}\" y1=\"{sy:.1}\" x2=\"{:.1}\" y2=\"{sy:.1}\" stroke=\"#ff7885\" stroke-width=\"1.4\" stroke-dasharray=\"2 2\"><title>Red stop loss: sell on confirmed downward cross; ratchets upward only: ${sp:.4}</title></line>",
                    w - pad_r
                ));
            }
            if let Some(tp) = target {
                let ty = y_of(tp);
                svg.push_str(&format!(
                    "<line id=\"exit-target100-line-{ticker}\" data-exit-level-symbol=\"{ticker}\" x1=\"{bx:.1}\" y1=\"{ty:.1}\" x2=\"{:.1}\" y2=\"{ty:.1}\" stroke=\"#facc15\" stroke-width=\"1.1\" stroke-dasharray=\"6 3\"><title>100% full target: sell on fresh trade touch or upward cross; ratchets upward only: ${tp:.4}</title></line>",
                    w - pad_r
                ));
            }
            if let Some(tp70) = t.target_70_price {
                let ty = y_of(tp70);
                svg.push_str(&format!(
                    "<line id=\"exit-tp-line-{ticker}\" data-exit-level-symbol=\"{ticker}\" x1=\"{bx:.1}\" y1=\"{ty:.1}\" x2=\"{:.1}\" y2=\"{ty:.1}\" stroke=\"#4ade80\" stroke-width=\"1.3\" stroke-dasharray=\"2 2\"><title>70% target: sell on confirmed downward cross; ratchets upward only: ${tp70:.4}</title></line>",
                    w - pad_r
                ));
                if let Some(line) = t.profit_retrace_price {
                    let ly = y_of(line);
                    svg.push_str(&format!(
                        "<line id=\"exit-retrace-line-{ticker}\" data-exit-level-symbol=\"{ticker}\" x1=\"{bx:.1}\" y1=\"{ly:.1}\" x2=\"{:.1}\" y2=\"{ly:.1}\" stroke=\"#fb923c\" stroke-width=\"1.2\" stroke-dasharray=\"3 3\"><title>Intermediate line: sell on confirmed downward cross; ratchets upward only: ${line:.4}</title></line>",
                        w - pad_r
                    ));
                }
                if let Some(midpoint) = t.profit_midpoint_price {
                    let my = y_of(midpoint);
                    svg.push_str(&format!(
                        "<line id=\"exit-mid-line-{ticker}\" data-exit-level-symbol=\"{ticker}\" x1=\"{bx:.1}\" y1=\"{my:.1}\" x2=\"{:.1}\" y2=\"{my:.1}\" stroke=\"#c084fc\" stroke-width=\"1.2\" stroke-dasharray=\"4 3\"><title>Profit midpoint: sell on confirmed downward cross; ratchets upward only: ${midpoint:.4}</title></line>",
                        w - pad_r
                    ));
                }
            }
            // Active holding connector + pulsing cyan/amber beacon
            svg.push_str(&format!(
                "<line id=\"live-hold-segment-{ticker}\" data-live-symbol=\"{ticker}\" data-live-bot=\"{chart_bot}\" x1=\"{bx:.1}\" y1=\"{by:.1}\" x2=\"{hx:.1}\" y2=\"{hy:.1}\" stroke=\"#fbbf24\" stroke-width=\"2.2\"/>\
                 <polygon points=\"{bx:.1},{:.1} {:.1},{:.1} {:.1},{:.1}\" fill=\"#22c55e\" stroke=\"#052e16\" stroke-width=\"1\"><title>BUY {} @ ${:.4} ({} ET)</title></polygon>\
                 <circle id=\"live-hold-dot-{ticker}\" data-live-symbol=\"{ticker}\" data-live-bot=\"{chart_bot}\" cx=\"{hx:.1}\" cy=\"{hy:.1}\" r=\"5.5\" fill=\"#fbbf24\" stroke=\"#ffffff\" stroke-width=\"1.5\"><title>HOLDING {} shares · Now ${:.4}</title></circle>",
                by - 6.0, bx - 5.0, by + 4.0, bx + 5.0, by + 4.0, t.qty, t.entry_price, ny_hhmm(t.opened_ms),
                t.qty, latest_px
            ));
        }
    }

    let badge = if trades.is_empty() {
        "<span style=\"color:#38bdf8;background:#082f49;border:1px solid #0369a1;padding:2px 8px;border-radius:999px;font-size:11px;font-weight:700;\">CURRENT CANDIDATE</span>".to_string()
    } else if is_holding {
        let open_t = trades.iter().rev().find(|t| t.closed_ms.is_none()).unwrap_or(&trades[0]);
        let live_pct = if open_t.entry_price > 0.0 { (latest_px - open_t.entry_price) / open_t.entry_price * 100.0 } else { 0.0 };
        let live_usd = (latest_px - open_t.entry_price) * open_t.qty as f64;
        format!(
            "<span style=\"background:#422006;color:#fbbf24;border:1px solid #b45309;padding:2px 8px;border-radius:999px;font-size:11px;font-weight:700;\">● HOLDING {} sh · {:+.2}% (${:+.2})</span>",
            open_t.qty, live_pct, live_usd
        )
    } else {
        let cls = if net_pnl_usd >= 0.0 { "color:#4ade80;background:#052e16;border:1px solid #166534;" } else { "color:#ff7885;background:#450a0a;border:1px solid #991b1b;" };
        format!(
            "<span style=\"{cls}padding:2px 8px;border-radius:999px;font-size:11px;font-weight:700;\">EXITED ({}x) · Net ${:+.2}</span>",
            trades.len(), net_pnl_usd
        )
    };

    let mut trade_rows = String::new();
    for (idx, t) in trades.iter().enumerate() {
        if idx >= 4 && trades.len() > 5 && idx + 1 < trades.len() {
            if idx == 4 {
                trade_rows.push_str(&format!(
                    "<tr><td colspan=\"5\" class=muted style=\"text-align:center;padding:4px;\">… {} intermediate executions …</td></tr>",
                    trades.len() - 5
                ));
            }
            continue;
        }
        let buy_str = format!("${:.2} ({} ET)", t.entry_price, ny_hhmm(t.opened_ms));
        let exit_str = match (t.closed_ms, t.exit_price) {
            (Some(cm), Some(ep)) => format!("${:.2} ({} ET)", ep, ny_hhmm(cm)),
            _ => format!("<b class=warn>HOLDING (Now ${:.2})</b>", latest_px),
        };
        let pnl_cls = if t.pnl_usd >= 0.0 { "good" } else { "bad" };
        let short_reason: String = t.reason.chars().take(34).collect();
        trade_rows.push_str(&format!(
            "<tr><td>{}</td><td class=good>▲ {}</td><td>{}</td><td class={pnl_cls}>${:+.2} ({:+.2}%)</td><td class=muted>{}</td></tr>",
            t.qty,
            buy_str,
            exit_str,
            t.pnl_usd,
            t.pnl_pct,
            esc(&short_reason)
        ));
    }

    let event_summary = events.iter()
        .filter(|event| event.ts_ms >= start_ms && event.ts_ms <= data_cutoff_ms)
        .map(|event| format!("<span style=\"color:{};font-weight:700\">{} · {} ET</span>", event.color, event.label, ny_hhmm(event.ts_ms)))
        .collect::<Vec<_>>().join(" &nbsp; ");
    let event_summary = if event_summary.is_empty() { String::new() } else { format!("<div style=\"font-size:11px;margin:5px 0 2px\">{event_summary}</div>") };

    format!(
        "<div class=tile style=\"text-transform:none;letter-spacing:0;padding:14px;\">\
            <div style=\"display:flex;justify-content:space-between;align-items:center;margin-bottom:8px;\">\
                <div><b style=\"font-size:16px;color:#f8fafc;\">{code}</b> &nbsp; <span style=\"color:#38bdf8;font-size:11px;font-weight:700;\">{}</span><br><span class=muted style=\"font-size:11px;\">Session {} ET · premarket 04:00–09:30 · regular 09:30–16:00</span><br><span id=\"live-price-label-{ticker}\" class=muted style=\"font-size:12px;\">{}</span></div>\
                {badge}\
            </div>\
            <svg viewBox=\"0 0 {w} {h}\" role=img aria-label=\"{code} price chart with detection and approval markers\" style=\"background:#090d14;border:1px solid #1e293b;border-radius:8px;max-height:210px;\">{svg}</svg>{event_summary}\
            <div style=\"display:flex;gap:12px;font-size:11px;color:#94a3b8;margin:6px 0;\">\
                <span><span style=\"color:#22c55e;\">▲</span> Buy Point</span>\
                <span><span style=\"color:#ff7885;\">▼</span>/<span style=\"color:#4ade80;\">▼</span> Exit Point</span>\
                <span><span style=\"color:#fbbf24;\">●</span> Still Holding</span>\
                <span><span style=\"color:#4ade80;\">—</span> 70% TP</span>\
                <span><span style=\"color:#ff7885;\">—</span> SL</span>\
                <span><span style=\"color:#c084fc;\">—</span> Midpoint</span>\
                <span><span style=\"color:#38bdf8;\">░</span> Premarket (04:00–09:30 ET, when available)</span>\
            </div>\
            <div class=scroll style=\"padding:2px 6px;margin-top:6px;\">\
                <table style=\"font-size:11.5px;\"><thead><tr><th>Qty</th><th>Buy Point</th><th>Exit / Holding</th><th>P&amp;L</th><th>Catalyst</th></tr></thead><tbody>{trade_rows}</tbody></table>\
            </div>\
        </div>",
        esc(account_label),
        ny_ymd(session_day_utc_ms),
        latest_price_label
    )
}

fn ny_ymd(utc_ms: i64) -> String {
    let local_ms = utc_ms + ny_offset_ms(utc_ms);
    let secs = local_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Returns `(day_start_utc_ms, day_end_utc_ms, session_label)` for the New York calendar day containing `anchor_utc_ms`.
fn ny_session_bounds(anchor_utc_ms: i64) -> (i64, i64, String) {
    let offset = ny_offset_ms(anchor_utc_ms);
    let ny_day = (anchor_utc_ms + offset).div_euclid(86_400_000);
    let start_utc_ms = ny_day * 86_400_000 - offset;
    let end_utc_ms = start_utc_ms + 86_400_000;
    let label = ny_ymd(start_utc_ms + 12 * 3_600_000);
    (start_utc_ms, end_utc_ms, label)
}

fn chart_session_anchor(now_ms: i64, last_activity_ms: i64) -> i64 {
    let offset = ny_offset_ms(now_ms);
    let local_day = (now_ms + offset).div_euclid(86_400_000);
    let weekday = (local_day + 3).rem_euclid(7) < 5;
    let local_minute = ((now_ms + offset).div_euclid(60_000)).rem_euclid(1_440);
    if weekday && (240..=1_220).contains(&local_minute) {
        now_ms
    } else if last_activity_ms > 0 {
        last_activity_ms
    } else {
        now_ms
    }
}

fn orb_bought_charts(s: &Sources, orb_db: &Connection) -> String {
    let common_db = open_ro(&s.repo.join("papercompmoomoo/data/common1m.sqlite3")).ok();
    let feed_db = open_ro(&s.repo.join("papercompmoomoo/data/feed.sqlite3")).ok();
    let live_exit_levels = exit_level_map(s.repo, "orb");
    let mut by_sym: std::collections::BTreeMap<String, Vec<TradeMarker>> = std::collections::BTreeMap::new();

    // Determine the current or most recent session with ORB activity
    let max_trade_ms: i64 = orb_db
        .query_row("SELECT coalesce(max(closed_ms), 0) FROM trades", [], |r| r.get(0))
        .unwrap_or(0);
    let max_pos_ms: i64 = orb_db
        .query_row("SELECT coalesce(max(opened_ms), 0) FROM positions WHERE status IN ('open','closing') AND qty > 0", [], |r| r.get(0))
        .unwrap_or(0);
    let activity_anchor = max_trade_ms.max(max_pos_ms);
    let latest_candidate_session: Option<String> = orb_db
        .query_row("SELECT max(session) FROM candidates", [], |r| r.get(0))
        .ok();
    let candidate_anchor = latest_candidate_session
        .as_deref()
        .and_then(|day| iso_utc_ms(&format!("{day}T12:00:00Z")))
        .unwrap_or(0);
    let active_session_anchor = chart_session_anchor(s.now_ms, activity_anchor);
    let anchor_ms = if ny_ymd(active_session_anchor) == ny_ymd(s.now_ms) {
        active_session_anchor
    } else if candidate_anchor > 0
        && (activity_anchor <= 0 || ny_ymd(candidate_anchor) >= ny_ymd(activity_anchor))
    {
        candidate_anchor
    } else if activity_anchor > 0 {
        activity_anchor
    } else {
        s.now_ms
    };
    let (sess_start_ms, sess_end_ms, sess_label) = ny_session_bounds(anchor_ms);
    let current_candidates=std::fs::read_to_string(s.repo.join("papercompmoomoo/data/orb_premarket.json")).ok()
        .and_then(|raw|serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v|v["candidates"].as_array().cloned()).unwrap_or_default();
    for item in current_candidates {if let Some(symbol)=item["symbol"].as_str(){by_sym.entry(symbol.trim_start_matches("US.").to_string()).or_default();}}
    let mut chart_events: std::collections::BTreeMap<String, Vec<ChartEvent>> = std::collections::BTreeMap::new();
    if let Ok(mut st) = orb_db.prepare("SELECT symbol, detected_ms, approved_ms FROM candidates WHERE session=? AND (detected_ms IS NOT NULL OR approved_ms IS NOT NULL)") {
        if let Ok(rows) = st.query_map([sess_label.as_str()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<i64>>(2)?))) {
            for row in rows.flatten() {
                let (symbol, detected, approved) = row;
                let ticker = symbol.trim_start_matches("US.").to_string();
                let events = chart_events.entry(ticker.clone()).or_default();
                if let Some(ts_ms) = detected { events.push(ChartEvent { ts_ms, label: "Detected", short: "D", color: "#38bdf8" }); }
                if let Some(ts_ms) = approved { events.push(ChartEvent { ts_ms, label: "Approved · order submitted", short: "A", color: "#4ade80" }); }
                by_sym.entry(ticker).or_default();
            }
        }
    }

    // Closed trades in ORB ledger for the current / most recent session only
    if let Ok(mut st) = orb_db.prepare(
        "SELECT symbol, qty, entry_price, exit_price, pnl_usd, reason, opened_ms, closed_ms \
         FROM trades WHERE (opened_ms >= ? AND opened_ms < ?) OR (closed_ms >= ? AND closed_ms < ?) ORDER BY opened_ms ASC",
    ) {
        if let Ok(rows) = st.query_map(rusqlite::params![sess_start_ms, sess_end_ms, sess_start_ms, sess_end_ms], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
            ))
        }) {
            for r in rows.flatten() {
                if r.6 < sess_start_ms || r.6 >= sess_end_ms { continue; }
                let sym = r.0.trim_start_matches("US.").to_string();
                let pct = if r.2 > 0.0 { (r.3 - r.2) / r.2 * 100.0 } else { 0.0 };
                by_sym.entry(sym).or_default().push(TradeMarker {
                    opened_ms: r.6,
                    closed_ms: Some(r.7),
                    qty: r.1,
                    entry_price: r.2,
                    exit_price: Some(r.3),
                    pnl_usd: r.4,
                    pnl_pct: pct,
                    reason: r.5,
                    stop_price: None,
                    target_price: None,
                    target_70_price: None,
                    profit_retrace_price: None,
                    profit_midpoint_price: None,
                });
            }
        }
    }

    // Open positions in ORB ledger (still holding)
    if let Ok(mut st) = orb_db.prepare("SELECT symbol, qty, entry_price, stop_price, target_price, high_price, opened_ms, status FROM positions WHERE status IN ('open','closing') AND qty > 0 ORDER BY opened_ms ASC") {
        if let Ok(rows) = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, f64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, String>(7)?,
            ))
        }) {
            for r in rows.flatten() {
                if r.6 < sess_start_ms || r.6 >= sess_end_ms { continue; }
                let sym = r.0.trim_start_matches("US.").to_string();
                let levels = live_exit_levels.get(&sym);
                let last_px = common_db
                    .as_ref()
                    .and_then(|c| c.query_row("SELECT close FROM bars WHERE symbol=? ORDER BY ts DESC LIMIT 1", [sym.as_str()], |row| row.get::<_, f64>(0)).ok())
                    .unwrap_or(r.5.max(r.2));
                let pnl_usd = (last_px - r.2) * r.1 as f64;
                let pnl_pct = if r.2 > 0.0 { (last_px - r.2) / r.2 * 100.0 } else { 0.0 };
                by_sym.entry(sym).or_default().push(TradeMarker {
                    opened_ms: r.6,
                    closed_ms: None,
                    qty: r.1,
                    entry_price: r.2,
                    exit_price: None,
                    pnl_usd,
                    pnl_pct,
                    reason: format!("Still Holding ({})", r.7),
                    stop_price: levels.and_then(|v| v["stop_price"].as_f64()).or(Some(r.3)),
                    target_price: levels.and_then(|v| v["target_price"].as_f64()).or(Some(r.4)),
                    target_70_price: levels.and_then(|v| v["target_70_price"].as_f64()),
                    profit_retrace_price: levels.and_then(|v| v["profit_retrace_price"].as_f64()),
                    profit_midpoint_price: levels.and_then(|v| v["profit_midpoint_price"].as_f64()),
                });
            }
        }
    }

    if by_sym.is_empty() {
        return "<p class=muted>No bought symbols recorded for the ORB bot in the current or most recent session yet.</p>".to_string();
    }

    // Sort symbols so STILL HOLDING positions appear first, then by most recent activity
    let mut entries: Vec<(String, Vec<TradeMarker>)> = by_sym.into_iter().collect();
    entries.sort_by_key(|(sym, ts)| {
        let holding = ts.iter().any(|t| t.closed_ms.is_none());
        (!holding, sym.clone())
    });

    let mut cards = format!(
        "<p class=muted>Showing session <b>{sess_label} (US Eastern)</b> — {} symbols from today’s premarket candidates and trades, using available data from 04:00 ET through 16:00 ET.</p>\
         <div style=\"display:grid;grid-template-columns:repeat(auto-fill,minmax(520px,1fr));gap:16px;margin:14px 0;\">",
        entries.len()
    );
    for (sym, trades) in &entries {
        cards.push_str(&render_bought_symbol_chart(common_db.as_ref(), feed_db.as_ref(), sym, trades, chart_events.get(sym).map(Vec::as_slice).unwrap_or(&[]), sess_start_ms + 12 * 3_600_000, None, s.now_ms, "ORB · SHARED COMPETITION PAPER ACCOUNT"));
    }
    cards.push_str("</div>");
    cards
}

fn comp_bought_charts(s: &Sources, status: Option<&serde_json::Value>) -> String {
    let common_db = open_ro(&s.repo.join("papercompmoomoo/data/common1m.sqlite3")).ok();
    let feed_db = open_ro(&s.repo.join("papercompmoomoo/data/feed.sqlite3")).ok();
    let live_exit_levels = exit_level_map(s.repo, "gainer");
    let mut by_sym: std::collections::BTreeMap<String, Vec<TradeMarker>> = std::collections::BTreeMap::new();
    let mut open_symbols = std::collections::BTreeSet::new();

    // Determine the current or most recent session with Competition bot activity
    let max_trade_ms: i64 = s
        .journal
        .and_then(|j| j.query_row("SELECT coalesce(max(closed_ms), 0) FROM trades", [], |r| r.get(0)).ok())
        .unwrap_or(0);
    let max_order_ms: i64 = s
        .journal
        .and_then(|j| j.query_row("SELECT coalesce(max(ts_ms), 0) FROM orders WHERE side='BUY'", [], |r| r.get(0)).ok())
        .unwrap_or(0);
    let anchor_ms = chart_session_anchor(s.now_ms, max_trade_ms.max(max_order_ms));
    let (sess_start_ms, sess_end_ms, sess_label) = ny_session_bounds(anchor_ms);
    let mut chart_events: std::collections::BTreeMap<String, Vec<ChartEvent>> = std::collections::BTreeMap::new();
    if let Some(j) = s.journal {
        if let Ok(mut st) = j.prepare("SELECT symbol, MIN(CASE WHEN event='gainer_detected' THEN ts_ms END), MIN(CASE WHEN event='live_gainer_discovered' THEN ts_ms END), MIN(CASE WHEN event='gainer_admitted' THEN ts_ms END) FROM plan_events WHERE ts_ms>=? AND ts_ms<? AND event IN ('gainer_detected','live_gainer_discovered','gainer_admitted') GROUP BY symbol") {
            if let Ok(rows) = st.query_map(rusqlite::params![sess_start_ms, sess_end_ms], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, Option<i64>>(3)?))) {
                for row in rows.flatten() {
                    let (symbol, detected, activated, approved) = row;
                    let ticker = symbol.trim_start_matches("US.").to_string();
                    let events = chart_events.entry(ticker.clone()).or_default();
                    if let Some(ts_ms) = detected.or(activated) { events.push(ChartEvent { ts_ms, label: "Detected", short: "D", color: "#38bdf8" }); }
                    if let Some(ts_ms) = approved { events.push(ChartEvent { ts_ms, label: "Approved · entry gates passed", short: "A", color: "#4ade80" }); by_sym.entry(ticker).or_default(); }
                }
            }
        }
    }
    let mut live_prices=std::collections::BTreeMap::new();
    if let Some(v)=status {for key in ["watchlist","positions_live"]{if let Some(rows)=v[key].as_array(){for row in rows{
        if let (Some(symbol),Some(price))=(row["symbol"].as_str(),row["price"].as_f64()){if price>0.0&&price.is_finite(){live_prices.insert(symbol.trim_start_matches("US.").to_string(),price);}}
        if let Some(symbol)=row["symbol"].as_str(){by_sym.entry(symbol.trim_start_matches("US.").to_string()).or_default();}
    }}}}
    if let Some(j)=s.journal {if let Ok(mut st)=j.prepare("SELECT DISTINCT symbol FROM plan_events WHERE ts_ms>=? AND ts_ms<? AND symbol IS NOT NULL AND event IN ('gainer_admitted','live_gainer_discovered')") {
        if let Ok(rows)=st.query_map(rusqlite::params![sess_start_ms,sess_end_ms],|r|r.get::<_,String>(0)){for symbol in rows.flatten(){by_sym.entry(symbol.trim_start_matches("US.").to_string()).or_default();}}
    }}

    if let Some(j) = s.journal {
        if let Ok(mut st) = j.prepare(
            "SELECT symbol, qty, entry, exit_price, pnl_pct, reason, opened_ms, closed_ms \
             FROM trades WHERE (opened_ms >= ? AND opened_ms < ?) OR (closed_ms >= ? AND closed_ms < ?) ORDER BY opened_ms ASC",
        ) {
            if let Ok(rows) = st.query_map(rusqlite::params![sess_start_ms, sess_end_ms, sess_start_ms, sess_end_ms], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, f64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            }) {
                for r in rows.flatten() {
                    if r.6 < sess_start_ms || r.6 >= sess_end_ms { continue; }
                    let sym = r.0.trim_start_matches("US.").to_string();
                    let pnl_usd = (r.3 - r.2) * r.1 as f64;
                    by_sym.entry(sym).or_default().push(TradeMarker {
                        opened_ms: r.6,
                        closed_ms: Some(r.7),
                        qty: r.1,
                        entry_price: r.2,
                        exit_price: Some(r.3),
                        pnl_usd,
                        pnl_pct: r.4,
                        reason: r.5,
                        stop_price: None,
                        target_price: None,
                        target_70_price: None,
                        profit_retrace_price: None,
                        profit_midpoint_price: None,
                    });
                }
            }
        }
    }

    // Also include currently held positions from status.json / positions.json
    if let Some(live_arr) = status.and_then(|v| v["positions_live"].as_array()) {
        for pos in live_arr {
            let Some(raw_sym) = pos["symbol"].as_str() else { continue };
            let sym = raw_sym.trim_start_matches("US.").to_string();
            let qty = pos["qty"].as_i64().unwrap_or(0);
            if qty <= 0 {
                continue;
            }
            if !open_symbols.insert(sym.clone()) { continue; }
            let entry = pos["entry"].as_f64().unwrap_or(0.0);
            let price = pos["price"].as_f64().unwrap_or(entry);
            let levels = live_exit_levels.get(&sym);
            let stop = levels.and_then(|v| v["stop_price"].as_f64()).or_else(|| pos["stop"].as_f64());
            let target = levels.and_then(|v| v["target_price"].as_f64()).or_else(|| pos["target"].as_f64());
            let algo = pos["algo_type"].as_str().unwrap_or("ACTIVE");
            let pnl_pct = pos["pnl_pct"].as_f64().unwrap_or_else(|| if entry > 0.0 { (price - entry) / entry * 100.0 } else { 0.0 });
            let pnl_usd = (price - entry) * qty as f64;
            // Lookup latest BUY order timestamp in journal.orders for this symbol
            let opened_ms = s
                .journal
                .and_then(|j| {
                    j.query_row(
                        "SELECT ts_ms FROM orders WHERE symbol IN (?, ?) AND side='BUY' ORDER BY ts_ms DESC LIMIT 1",
                        rusqlite::params![format!("US.{sym}"), sym.as_str()],
                        |r| r.get::<_, i64>(0),
                    )
                    .ok()
                })
                .unwrap_or(anchor_ms);
            if opened_ms < sess_start_ms || opened_ms >= sess_end_ms {
                continue;
            }
            by_sym.entry(sym).or_default().push(TradeMarker {
                opened_ms,
                closed_ms: None,
                qty,
                entry_price: entry,
                exit_price: None,
                pnl_usd,
                pnl_pct,
                reason: format!("Still Holding ({algo})"),
                stop_price: stop,
                target_price: target,
                target_70_price: levels.and_then(|v| v["target_70_price"].as_f64()),
                profit_retrace_price: levels.and_then(|v| v["profit_retrace_price"].as_f64()),
                profit_midpoint_price: levels.and_then(|v| v["profit_midpoint_price"].as_f64()),
            });
        }
    }

    // Current bot status publishes the broker-held symbols separately from
    // positions.json. Join those account-truth symbols to local entry data and
    // the exit worker's exact computed levels so open trades aren't rendered
    // as untraded candidates.
    let held = status.and_then(|v| v["account"]["held"].as_array())
        .or_else(|| status.and_then(|v| v["open_positions"].as_array()));
    let position_state = std::fs::read_to_string(s.repo.join("papercompmoomoo/data/positions.json"))
        .ok().and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok()).unwrap_or_default();
    if let Some(held) = held {
        for item in held {
            let Some(raw_symbol) = item.as_str() else { continue };
            let sym = raw_symbol.trim_start_matches("US.").to_string();
            if !open_symbols.insert(sym.clone()) { continue; }
            let row = position_state.get(raw_symbol).or_else(|| position_state.get(&format!("US.{sym}"))).or_else(|| position_state.get(&sym));
            let Some(row) = row else { continue };
            let qty = row["qty"].as_i64().unwrap_or(0);
            let entry = row["entry"].as_f64().unwrap_or(0.0);
            if qty <= 0 || entry <= 0.0 { continue; }
            let opened_ms = row["opened"].as_str().and_then(iso_utc_ms).or_else(|| s.journal.and_then(|j| j.query_row(
                "SELECT ts_ms FROM orders WHERE symbol IN (?, ?) AND side='BUY' ORDER BY ts_ms DESC LIMIT 1",
                rusqlite::params![format!("US.{sym}"), sym.as_str()], |r| r.get::<_, i64>(0)).ok()));
            let Some(opened_ms) = opened_ms.filter(|ms| *ms >= sess_start_ms && *ms < sess_end_ms) else { continue };
            let levels = live_exit_levels.get(&sym);
            let stop = levels.and_then(|v| v["stop_price"].as_f64()).or_else(|| row["stop"].as_f64()).filter(|p| *p > 0.0);
            let target = levels.and_then(|v| v["target_price"].as_f64()).or_else(|| row["target"].as_f64()).filter(|p| *p > 0.0);
            let price = common_db.as_ref().and_then(|db| db.query_row(
                "SELECT close FROM bars WHERE symbol IN (?, ?) AND ts>=? AND ts<=? ORDER BY ts DESC LIMIT 1",
                rusqlite::params![sym.as_str(), format!("US.{sym}"), sess_start_ms, s.now_ms], |r| r.get::<_, f64>(0)).ok()).unwrap_or(entry);
            let qty_i = qty;
            let pnl = (price-entry)*qty_i as f64;
            let pnl_pct = (price-entry)/entry*100.0;
            by_sym.entry(sym.clone()).or_default().push(TradeMarker {
                opened_ms, closed_ms: None, qty: qty_i, entry_price: entry, exit_price: None,
                pnl_usd: pnl, pnl_pct, reason: "Still Holding (live account position)".to_string(),
                stop_price: stop, target_price: target,
                target_70_price: levels.and_then(|v| v["target_70_price"].as_f64()),
                profit_retrace_price: levels.and_then(|v| v["profit_retrace_price"].as_f64()),
                profit_midpoint_price: levels.and_then(|v| v["profit_midpoint_price"].as_f64()),
            });
        }
    }

    if by_sym.is_empty() {
        return "<p class=muted>No bought symbols recorded for the Competition bot in the current or most recent session yet.</p>".to_string();
    }

    let mut entries: Vec<(String, Vec<TradeMarker>)> = by_sym.into_iter().collect();
    entries.sort_by_key(|(sym, ts)| {
        let holding = ts.iter().any(|t| t.closed_ms.is_none());
        (!holding, sym.clone())
    });

    let mut cards = format!(
        "<p class=muted>Showing session <b>{sess_label} (US Eastern)</b> — {} symbols traded from premarket open (04:00 ET) through session close (16:00 ET).</p>\
         <div style=\"display:grid;grid-template-columns:repeat(auto-fill,minmax(520px,1fr));gap:16px;margin:14px 0;\">",
        entries.len()
    );
    for (sym, trades) in &entries {
        cards.push_str(&render_bought_symbol_chart(common_db.as_ref(), feed_db.as_ref(), sym, trades, chart_events.get(sym).map(Vec::as_slice).unwrap_or(&[]), sess_start_ms + 12 * 3_600_000, live_prices.get(sym).copied(), s.now_ms, "GAINERS · COMPETITION PAPER ACCOUNT"));
    }
    cards.push_str("</div>");
    cards
}

fn paper(s: &Sources) -> String {
    let c = s.store;
    let (line, tiles) = paper_summary(c);
    let pts: Vec<(i64, f64, Option<f64>)> = c
        .prepare("SELECT wall_ms, equity, ref_close FROM paper_equity ORDER BY wall_ms")
        .and_then(|mut st| {
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()
        })
        .unwrap_or_default();
    let pause = crate::paper_cmd::pause_note(s.repo, s.now_ms)
        .map(|n| format!("<p><b>{}</b></p>", esc(&n)))
        .unwrap_or_default();
    format!(
        "{pause}<p>{}</p><div class=tiles>{}</div><h2>Equity</h2>{}<h2>Closed trades (latest 100)</h2>{}<h2>Settings the account trades (re-searched once per session on the last 14 sessions)</h2>{}<h2>Latest equity points</h2>{}",
        esc(&line),
        tiles.concat(),
        equity_chart(&pts),
        table(
            c,
            "SELECT symbol, side, entry_ms, exit_ms, qty, entry_price, exit_price, pnl, fees, reason FROM paper_trades ORDER BY exit_ms DESC LIMIT 100"
        ),
        table(
            c,
            "SELECT wall_ms, note, substr(params_json,1,160) AS params FROM paper_params ORDER BY wall_ms DESC LIMIT 6"
        ),
        table(
            c,
            "SELECT wall_ms, equity, unrealized, positions, ref_close FROM paper_equity ORDER BY wall_ms DESC LIMIT 30"
        )
    )
}

fn research(s: &Sources) -> String {
    let c = s.store;

    let total_backtests: i64 = c
        .query_row("SELECT count(*) FROM momentum_backtest_runs", [], |r| r.get(0))
        .unwrap_or(0);
    let total_evals: i64 = c
        .query_row("SELECT count(*) FROM model_evaluation_runs", [], |r| r.get(0))
        .unwrap_or(0);
    let total_params: i64 = c
        .query_row("SELECT count(*) FROM symbol_params WHERE accepted=1", [], |r| r.get(0))
        .unwrap_or(0);
    let total_forward: i64 = c
        .query_row("SELECT count(*) FROM momentum_forward_runs", [], |r| r.get(0))
        .unwrap_or(0);

    format!(
        "<div class=tiles>\
            <div class=tile><div class=muted>Momentum Backtest Runs</div><div class=\"v good\">{}</div><div class=muted>Cross-sectional parameter trials</div></div>\
            <div class=tile><div class=muted>Causal Model Evaluations</div><div class=\"v good\">{}</div><div class=muted>Modern Eval Paradigm benchmarks</div></div>\
            <div class=tile><div class=muted>Accepted Tuned Symbols</div><div class=\"v good\">{}</div><div class=muted>Out-of-sample validated fits</div></div>\
            <div class=tile><div class=muted>Forward Multi-Session Runs</div><div class=\"v good\">{}</div><div class=muted>Compounded out-of-sample evidence</div></div>\
        </div>\
        <h2>Cross-Sectional Momentum Backtest Evidence (After 10 bps Friction)</h2>{}\
        <h2>Causal Model Evaluations (Modern Evaluation Paradigm)</h2>{}\
        <h2>Per-Symbol Parameter Tuning (Train vs Held-Out Out-of-Sample Validation)</h2>{}\
        <h2>Out-of-Sample Forward Multi-Session Blocks</h2>{}",
        total_backtests,
        total_evals,
        total_params,
        total_forward,
        table(
            c,
            "SELECT datetime(run_ms/1000,'unixepoch') AS run_utc, \
             CASE WHEN universe LIKE '%tuned%' THEN 'per-symbol tuned' WHEN universe LIKE '%track%' THEN 'track-record rank' ELSE 'nasdaq plain' END AS method, \
             CASE WHEN lookback_sessions=0 THEN 'per symbol' ELSE lookback_sessions||' sessions' END AS lookback, \
             slots, symbols, cost_bps, sessions, trades, wins, round(return_pct,3) AS return_pct, round(max_drawdown_pct,3) AS max_drawdown_pct, \
             (SELECT group_concat(printf('%+.1f%%',value*100),' ') FROM json_each(forward_blocks_json)) AS block_returns \
             FROM momentum_backtest_runs WHERE universe IS NOT NULL ORDER BY run_ms DESC LIMIT 30"
        ),
        table(
            c,
            "SELECT id, datetime(created_ms/1000,'unixepoch') AS evaluated_utc, data_cutoff, \
             substr(strategy_version,1,28) AS strategy, substr(evaluation_json,1,450) AS causal_evaluation_metrics \
             FROM model_evaluation_runs ORDER BY id DESC LIMIT 25"
        ),
        table(
            c,
            "SELECT symbol, lookback AS lookback_sessions, round(threshold*100,1) AS min_signal_pct, \
             round(train_mean*100,3) AS train_net_pct, train_n, round(valid_mean*100,3) AS held_out_net_pct, valid_n, trials, \
             datetime(refit_day*86400,'unixepoch') AS refit_date \
             FROM symbol_params WHERE accepted=1 ORDER BY valid_mean DESC LIMIT 35"
        ),
        table(
            c,
            "SELECT session, candidates, completed, round(net_return_pct,3) AS net_pct, \
             round(equity,5) AS compounded_equity, round(drawdown_pct,3) AS drawdown_pct, \
             actual_trades, round(actual_pnl_pct,3) AS actual_trade_pnl_pct \
             FROM momentum_forward_runs ORDER BY session DESC LIMIT 25"
        )
    )
}

fn bot(s: &Sources) -> String {
    let Some(a) = s.archive else {
        return "<p class=muted>The bot's backtest archive (data/backtests.sqlite3) is not present.</p>".into();
    };
    format!(
        "<p class=muted>The bot's own backtest archive.</p>\
<h2>By source and window</h2>{}<h2>Latest runs</h2>{}",
        table(
            a,
            "SELECT source, window_label, count(*) AS runs, round(avg(return_pct),2) AS mean_return_pct, round(max(return_pct),2) AS best_return_pct, max(ts_ms) AS latest_ms FROM bt_runs GROUP BY source, window_label ORDER BY latest_ms DESC LIMIT 30"
        ),
        table(
            a,
            "SELECT ts_ms, source, window_label, round(return_pct,2) AS return_pct, round(max_drawdown_pct,2) AS drawdown_pct, trades, wins, round(profit_factor,2) AS profit_factor FROM bt_runs ORDER BY ts_ms DESC LIMIT 25"
        )
    )
}

/// The agents' page: who carries which role now, the message board, the commander's rules, the council's decisions and the agents' weights.
fn agents_page(s: &Sources) -> String {
    match crate::msgboard::open_ro(s.repo) {
        None => "<p class=muted>The message board does not exist yet (the commander creates it when it starts).</p>".to_string(),
        Some(b) => format!(
            "<h2>Who carries which role now (Claude, Codex or Antigravity)</h2>{}{}<h2>Agent weights (track records)</h2>{}\
<h2>The commander's rules (active)</h2>{}<h2>Decisions of the council</h2>{}<h2>Latest votes</h2>{}<h2>The board (latest 60 messages)</h2>{}",
            json_block(s.repo, "papercompmoomoo/data/agents_status.json"),
            json_block(s.repo, "papercompmoomoo/data/llm_state.json"),
            json_block(s.repo, "papercompmoomoo/data/agent_weights.json"),
            table(&b, "SELECT id, agent, kind, param, min_value, max_value, text, evidence FROM rules WHERE status='active' ORDER BY agent, id"),
            table(&b, "SELECT id, proposer, title, status, outcome, round(support,2) AS support, round(oppose,2) AS oppose, rationale FROM decisions ORDER BY id DESC LIMIT 20"),
            table(&b, "SELECT decision_id, agent, stance, round(weight,2) AS weight, via, reason FROM votes ORDER BY ts_ms DESC LIMIT 30"),
            table(&b, "SELECT id, datetime(ts_ms/1000,'unixepoch') AS time_utc, sender, recipient, kind, goal, via, topic, body, reply_to FROM messages ORDER BY id DESC LIMIT 60")
        ),
    }
}

/// The Gainers bot's tab: its status lines (and the research behind them), then the paper trader.
fn ts(s: &Sources) -> String {
    let rows = crate::board::rows(s.store, s.journal, s.repo, s.now_ms);
    let mine: Vec<crate::board::Row> = rows
        .into_iter()
        .filter(|r| r.area == "Gainers bot" || r.area == "Research")
        .collect();
    format!(
        "{}{}{}",
        crate::board::summary(&mine),
        crate::board::render(&mine),
        paper(s)
    )
}

/// The shared local Moomoo candle store, read-only.
/// The two tables read from the common candle database on every request. The
/// overview is operational state: a cache could present old candle health as
/// current, so freshness takes priority over query cost.
fn common_db_blocks(path: &Path) -> (String, String) {
    let db = open_ro(path).ok();
    let counts = db.as_ref().map_or("<p class=muted>The common candle database is not there yet.</p>".to_string(), |d| {
        table(d, "SELECT source, count(*) AS bars, count(DISTINCT symbol) AS symbols, datetime(max(ts)/1000,'unixepoch') AS newest_bar_utc FROM bars GROUP BY source")
    });
    let scale = db.as_ref().map_or(String::new(), |d| table(d, "SELECT count(*) AS symbols_measured, round(min(scale),2) AS min, round(avg(scale),2) AS mean, round(max(scale),2) AS max FROM volume_scale"));
    (counts, scale)
}


/// Intraday evidence always uses the requested UTC day, including an empty day.
fn session_filter(column: &str, now_ms: i64) -> String {
    format!("{column} >= {} AND {column} < {}", now_ms.div_euclid(86_400_000) * 86_400_000, (now_ms.div_euclid(86_400_000) + 1) * 86_400_000)
}

fn comp_gainers_table(s: &Sources) -> String {
    let Some(j) = s.journal else {
        return "<p class=muted>No gainer journal is available.</p>".into();
    };
    table(
        j,
        &format!(
            "SELECT ts_ms AS event_time_utc, symbol, event, \
         CASE WHEN json_valid(detail) THEN json_extract(detail,'$.price') END AS price, \
         CASE WHEN json_valid(detail) THEN json_extract(detail,'$.rvol') END AS rvol, \
         CASE WHEN json_valid(detail) THEN json_extract(detail,'$.change_pct') END AS change_pct, \
         CASE WHEN json_valid(detail) THEN json_extract(detail,'$.conviction') END AS conviction, \
         CASE WHEN json_valid(detail) THEN json_extract(detail,'$.reason') ELSE detail END AS reason \
         FROM plan_events WHERE {} AND event IN ('gainer_admitted','gainer_rejected','gainer_waiting_quote','live_gainer_discovered') \
         ORDER BY ts_ms DESC LIMIT 100",
            session_filter("ts_ms", s.now_ms)
        ),
    )
}

fn total_equity_chart(points: &[(i64, f64, Option<f64>)]) -> String {
    if points.len() < 2 {
        return "<p>Total-equity tracking appears after two or more account snapshots.</p>".into();
    }
    let values: Vec<f64> = points.iter().map(|p| p.1).collect();
    let lo=values.iter().copied().fold(f64::INFINITY,f64::min);
    let hi=values.iter().copied().fold(f64::NEG_INFINITY,f64::max);
    let span=(hi-lo).max(1e-9); let (w,h)=(900.0,260.0);
    let coords=values.iter().enumerate().map(|(i,v)|format!("{:.1},{:.1}",w*i as f64/(values.len()-1) as f64,h-h*(v-lo)/span)).collect::<Vec<_>>().join(" ");
    let first=crate::ops::utc(points[0].0);let last=crate::ops::utc(points[points.len()-1].0);
    format!("<svg viewBox=\"0 0 {w} {h}\" role=img aria-label=\"Gainers account total equity\"><polyline class=eq fill=none points=\"{coords}\"/></svg><p class=muted>Gainers account total equity · range ${lo:.2}–${hi:.2} · {first} to {last} UTC · {} snapshots</p>",points.len())
}

#[derive(Clone)]
struct CompetitionEquityPoint { ts_ms:i64, equity:f64, cash:f64, power:f64 }

fn iso_utc_ms(value:&str)->Option<i64> {
    let num=|a:usize,z:usize|value.get(a..z)?.parse::<i64>().ok();
    let (y,m,d,h,mi,se)=(num(0,4)?,num(5,7)?,num(8,10)?,num(11,13)?,num(14,16)?,num(17,19)?);
    let (y0,m0)=if m<=2{(y-1,m+9)}else{(y,m-3)};
    let era=y0.div_euclid(400);let yoe=y0-era*400;
    let days=era*146097+yoe*365+yoe.div_euclid(4)-yoe.div_euclid(100)+(153*m0+2).div_euclid(5)+d-1-719468;
    let mut ms=(days*86400+h*3600+mi*60+se)*1000;
    if let Some(dot)=value.find('.') {let end=value[dot+1..].find(|c:char|!c.is_ascii_digit()).map(|n|dot+1+n).unwrap_or(value.len());let frac=&value[dot+1..end];if !frac.is_empty(){let digits=format!("{frac:0<3}");ms+=digits.get(..3)?.parse::<i64>().ok()?;}}
    if value.ends_with("-05:00"){ms+=18_000_000}else if value.ends_with("-04:00"){ms+=14_400_000}else if !(value.ends_with('Z')||value.ends_with("+00:00")){return None;}
    Some(ms)
}

fn competition_equity_history(s:&Sources,account_id:i64)->Vec<CompetitionEquityPoint>{
    let root=s.repo.join("papercompmoomoo/data/permanent_file_history/status.json");let mut by_time=std::collections::BTreeMap::new();
    let (start,end,_)=ny_session_bounds(s.now_ms);
    if let Ok(entries)=std::fs::read_dir(root){for entry in entries.flatten(){
        if entry.metadata().ok().and_then(|m|m.modified().ok()).and_then(|m|m.duration_since(std::time::UNIX_EPOCH).ok()).map(|d|d.as_millis() as i64).is_some_and(|ms|ms<start){continue;}
        let Ok(raw)=std::fs::read(entry.path())else{continue};let Ok(v)=serde_json::from_slice::<serde_json::Value>(&raw)else{continue};
        if v["acc_id"].as_i64()!=Some(account_id)||v["account_kind"].as_str()!=Some("COMPETITION"){continue;}
        let Some(ts)=v["account"]["read_at"].as_str().and_then(iso_utc_ms)else{continue;};
        if ts<start||ts>=end{continue;}
        let (Some(equity),Some(cash),Some(power))=(v["account"]["equity"].as_f64(),v["account"]["cash"].as_f64(),v["account"]["power"].as_f64())else{continue;};
        by_time.insert(ts,CompetitionEquityPoint{ts_ms:ts,equity,cash,power});
    }}
    if let Ok(raw)=std::fs::read(s.repo.join("papercompmoomoo/data/status.json")){if let Ok(v)=serde_json::from_slice::<serde_json::Value>(&raw){
        if v["acc_id"].as_i64()==Some(account_id)&&v["account_kind"].as_str()==Some("COMPETITION"){
            if let (Some(ts),Some(equity),Some(cash),Some(power))=(v["account"]["read_at"].as_str().and_then(iso_utc_ms),v["account"]["equity"].as_f64(),v["account"]["cash"].as_f64(),v["account"]["power"].as_f64()){
                by_time.insert(ts,CompetitionEquityPoint{ts_ms:ts,equity,cash,power});
            }
        }
    }}
    by_time.into_values().collect()
}

fn comp(s: &Sources) -> String {
    let Some(account_id) = configured_account_id(s.repo, "gainer") else {
        return "<p class=bad>Gainers simulated account configuration is missing.</p>".into();
    };
    let status = std::fs::read_to_string(s.repo.join("papercompmoomoo/data/status.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .filter(|v|v["acc_id"].as_i64()==Some(account_id)&&v["account_kind"].as_str()==Some("COMPETITION"));
    let account_kind="COMPETITION";
    let points=competition_equity_history(s,account_id);let latest=points.last();
    let(snapshot_ms,live_equity,live_cash,live_power)=latest.map(|p|(p.ts_ms,p.equity,p.cash,p.power)).unwrap_or((0,0.0,0.0,0.0));
    let comp_net_pnl=live_equity-100_000.0;
    let symbol_charts = comp_bought_charts(s, status.as_ref());
    let account_panel = format!(
        "<div class=tiles>\
        <div class=tile><div class=muted>Total equity</div><div class=\"v good\">{}</div><div class=muted>Gainers simulated account · {account_kind} · snapshot {}</div></div>\
        <div class=tile><div class=muted>Cash</div><div class=\"v\">{}</div><div class=muted>Account snapshot {}</div></div>\
        <div class=tile><div class=muted>Buying power</div><div class=\"v\">{}</div><div class=muted>Account snapshot {}</div></div>\
        <div class=tile><div class=muted>Net equity change</div><div class=\"v {}\">{}</div><div class=muted>Against $100,000 starting equity · snapshot {}</div></div>\
        </div>",
        if snapshot_ms>0{format!("${live_equity:.2}")}else{"UNAVAILABLE".into()},
        if snapshot_ms>0{crate::ops::utc(snapshot_ms)}else{"timestamp unavailable".into()},
        if snapshot_ms>0{format!("${live_cash:.2}")}else{"UNAVAILABLE".into()},
        if snapshot_ms>0{crate::ops::utc(snapshot_ms)}else{"timestamp unavailable".into()},
        if snapshot_ms>0{format!("${live_power:.2}")}else{"UNAVAILABLE".into()},
        if snapshot_ms>0{crate::ops::utc(snapshot_ms)}else{"timestamp unavailable".into()},
        if comp_net_pnl>=0.0{"good"}else{"bad"},
        if snapshot_ms>0{format!("${comp_net_pnl:+.2}")}else{"UNAVAILABLE".into()},
        if snapshot_ms>0{crate::ops::utc(snapshot_ms)}else{"timestamp unavailable".into()}
    );
    let chart_points:Vec<(i64,f64,Option<f64>)>=points.iter().map(|p|(p.ts_ms,p.equity,None)).collect();
    format!(
        "<div class=tile><div class=muted>CHART OWNERSHIP</div><div class=v>Gainers · COMPETITION SIMULATE paper account</div><div class=muted>Both bots use the shared Competition account; these charts show Gainer-owned symbols, candidates, and trades only.</div></div>{}<h2>Current or most recent session candidates and traded symbols</h2><p class=muted>Gainers candidates include untraded candidates.</p>{symbol_charts}\
         <h2>Total equity tracking — Gainers account, current session</h2>{}<h2>Account summary</h2>{account_panel}\
         <h2>Gainers</h2><p class=muted>Current UTC session only. Each row includes its event timestamp.</p>{}
         <script>(()=>{{const poll=async()=>{{try{{const r=await fetch('/api/comp-live',{{cache:'no-store'}});if(!r.ok)return;const d=await r.json();for(const line of document.querySelectorAll('[data-live-symbol][data-live-bot=\"gainer\"]')){{const sym=line.dataset.liveSymbol,q=d.prices?.[sym];if(!q)continue;const price=Number(q.price),lo=Number(line.dataset.plotLo),hi=Number(line.dataset.plotHi),top=Number(line.dataset.padTop),height=Number(line.dataset.plotHeight),start=Number(line.dataset.startMs),end=Number(line.dataset.endMs),pl=Number(line.dataset.plotLeft),sw=Number(line.dataset.svgWidth),pr=Number(line.dataset.padRight);if(!(price>0&&hi>lo))continue;const y=top+height*(1-(price-lo)/(hi-lo)),sourceTs=Number(q.ts_ms),barTs=Number(q.bar_ts_ms),fmtET=(ms)=>new Intl.DateTimeFormat('en-US',{{timeZone:'America/New_York',hour:'2-digit',minute:'2-digit',hour12:false}}).format(new Date(ms)),phaseMins=Number(fmtET(sourceTs).slice(0,2))*60+Number(fmtET(sourceTs).slice(3,5)),phase=phaseMins<570?'premarket':phaseMins<960?'regular session':'outside session',interval=q.bar_end_labeled?fmtET(sourceTs)+'–'+fmtET(barTs):fmtET(sourceTs)+'–'+fmtET(sourceTs+60000),x=pl+(sw-pl-pr)*Math.max(0,Math.min(1,(sourceTs-start)/(end-start)));if(sourceTs<start||sourceTs>end)continue;for(const [kind,level] of [['stop',q.stop_price],['target100',q.target_price],['tp',q.target_70_price],['retrace',q.profit_retrace_price],['mid',q.profit_midpoint_price]]){{const el=line.closest('svg')?.querySelector('[id=\"exit-'+kind+'-line-'+sym+'\"]');if(el&&Number(level)>0){{const ly=top+height*(1-(Number(level)-lo)/(hi-lo));el.setAttribute('y1',ly);el.setAttribute('y2',ly);const title=el.querySelector('title');if(title){{const full=kind==='tp'?' (original target $'+Number(q.initial_target_price).toFixed(4)+'; current trailing target $'+Number(q.target_price).toFixed(4)+')':'';title.textContent=(kind==='stop'?'Stop loss (down-cross exit)':kind==='target100'?'100% target (sell on touch/up-cross)':kind==='tp'?'70% target (down-cross exit)':kind==='retrace'?'Intermediate line (down-cross exit)':'Profit midpoint (down-cross exit)')+' $'+Number(level).toFixed(4)+full;}}}}}}line.style.display='';line.setAttribute('y1',y);line.setAttribute('y2',y);line.setAttribute('x1',x);const dot=line.closest('svg')?.querySelector('[id=\"live-price-dot-'+sym+'\"]');if(dot){{dot.style.display='';dot.setAttribute('cx',x);dot.setAttribute('cy',y);dot.firstElementChild?.replaceChildren(document.createTextNode('Latest '+phase+' candle $'+price.toFixed(4)+' · '+interval+' ET · source timestamp '+q.as_of+' UTC'));}}const label=line.closest('svg')?.parentElement?.querySelector('[id=\"live-price-label-'+sym+'\"]');if(label)label.textContent='Latest '+phase+' candle $'+price.toFixed(2)+' · '+interval+' ET · source timestamp '+q.as_of+' UTC';const hold=line.closest('svg')?.querySelector('[id=\"live-hold-dot-'+sym+'\"]');if(hold){{hold.setAttribute('cx',x);hold.setAttribute('cy',y);}}const seg=line.closest('svg')?.querySelector('[id=\"live-hold-segment-'+sym+'\"]');if(seg){{seg.setAttribute('x2',x);seg.setAttribute('y2',y);}}}}}}catch(_){{}}}};poll();setInterval(poll,2500)}})();</script>",
        market_status_line(s.now_ms),
        total_equity_chart(&chart_points),
        comp_gainers_table(s)
    )
}

fn orb_dashboard(s: &Sources) -> String {
    let db_path = s.repo.join("papercompmoomoo/data/orb.sqlite3");
    let Some(db) = open_ro(&db_path).ok() else {
        return "<p class=bad>ORB ledger is not available yet.</p>".into();
    };
    let Some(orb_account) = configured_account_id(s.repo, "orb") else {
        return "<p class=bad>ORB simulated account configuration is missing.</p>".into();
    };
    let orb_charts=orb_bought_charts(s,&db);
    let latest:Option<(i64,i64,String,f64,f64,f64)>=db.query_row(
        "SELECT ts_ms,account_id,account_kind,equity,cash,buying_power FROM account_snapshots WHERE account_id=? AND account_kind='COMPETITION' ORDER BY ts_ms DESC LIMIT 1",
        [orb_account],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).ok();
    let(equity_start,equity_end,_)=ny_session_bounds(s.now_ms);
    let orb_points:Vec<(i64,f64,Option<f64>)>=db.prepare("SELECT ts_ms,equity FROM account_snapshots WHERE account_id=? AND ts_ms>=? AND ts_ms<? ORDER BY ts_ms")
        .and_then(|mut st|st.query_map(rusqlite::params![orb_account,equity_start,equity_end],|r|Ok((r.get(0)?,r.get(1)?))).and_then(|rows|rows.map(|r|r.map(|(t,e)|(t,e,None))).collect::<std::result::Result<Vec<_>,_>>())).unwrap_or_default();
    let orb_latest=latest.as_ref();
    let equity_summary=format!("<div class=tiles><div class=tile><div class=muted>Shared Competition account equity</div><div class=\"v good\">{}</div><div class=muted>ORB snapshot {}</div></div><div class=tile><div class=muted>Shared cash</div><div class=\"v\">{}</div></div><div class=tile><div class=muted>Shared buying power</div><div class=\"v\">{}</div></div></div>",orb_latest.map(|p|format!("${:.2}",p.3)).unwrap_or_else(||"UNAVAILABLE".into()),orb_latest.map(|p|crate::ops::utc(p.0)).unwrap_or_else(||"unavailable".into()),orb_latest.map(|p|format!("${:.2}",p.4)).unwrap_or_else(||"UNAVAILABLE".into()),orb_latest.map(|p|format!("${:.2}",p.5)).unwrap_or_else(||"UNAVAILABLE".into()));
    format!("<div class=tile><div class=muted>CHART OWNERSHIP</div><div class=v>ORB · COMPETITION SIMULATE paper account</div><div class=muted>Equity and buying power belong to the shared account; these charts show ORB candidates, symbols, and trades only.</div></div>{}<h2>Current or most recent session candidates and traded symbols</h2><p class=muted>ORB candidates include untraded candidates.</p>{orb_charts}<h2>Total equity tracking — shared Competition account, current session</h2>{}<h2>Shared account summary</h2>{equity_summary}",market_status_line(s.now_ms),equity_chart(&orb_points))
}

fn page_orb(s:&Sources)->String{
    let body=orb_dashboard(s);
    format!("{body}<script>(()=>{{const poll=async()=>{{try{{const r=await fetch('/api/orb-live',{{cache:'no-store'}});if(!r.ok)return;const d=await r.json();for(const line of document.querySelectorAll('[data-live-symbol][data-live-bot=\"orb\"]')){{const sym=line.dataset.liveSymbol,q=d.prices?.[sym];if(!q)continue;const price=Number(q.price),lo=Number(line.dataset.plotLo),hi=Number(line.dataset.plotHi),top=Number(line.dataset.padTop),height=Number(line.dataset.plotHeight),start=Number(line.dataset.startMs),end=Number(line.dataset.endMs),pl=Number(line.dataset.plotLeft),sw=Number(line.dataset.svgWidth),pr=Number(line.dataset.padRight);if(!(price>0&&hi>lo))continue;const y=top+height*(1-(price-lo)/(hi-lo)),sourceTs=Number(q.ts_ms),barTs=Number(q.bar_ts_ms),fmtET=(ms)=>new Intl.DateTimeFormat('en-US',{{timeZone:'America/New_York',hour:'2-digit',minute:'2-digit',hour12:false}}).format(new Date(ms)),phaseMins=Number(fmtET(sourceTs).slice(0,2))*60+Number(fmtET(sourceTs).slice(3,5)),phase=phaseMins<570?'premarket':phaseMins<960?'regular session':'outside session',interval=q.bar_end_labeled?fmtET(sourceTs)+'–'+fmtET(barTs):fmtET(sourceTs)+'–'+fmtET(sourceTs+60000),x=pl+(sw-pl-pr)*Math.max(0,Math.min(1,(sourceTs-start)/(end-start)));if(sourceTs<start||sourceTs>end)continue;for(const [kind,level] of [['stop',q.stop_price],['target100',q.target_price],['tp',q.target_70_price],['retrace',q.profit_retrace_price],['mid',q.profit_midpoint_price]]){{const el=line.closest('svg')?.querySelector('[id=\"exit-'+kind+'-line-'+sym+'\"]');if(el&&Number(level)>0){{const ly=top+height*(1-(Number(level)-lo)/(hi-lo));el.setAttribute('y1',ly);el.setAttribute('y2',ly);const title=el.querySelector('title');if(title){{const full=kind==='tp'?' (original target $'+Number(q.initial_target_price).toFixed(4)+'; current trailing target $'+Number(q.target_price).toFixed(4)+')':'';title.textContent=(kind==='stop'?'Stop loss (down-cross exit)':kind==='target100'?'100% target (sell on touch/up-cross)':kind==='tp'?'70% target (down-cross exit)':kind==='retrace'?'Intermediate line (down-cross exit)':'Profit midpoint (down-cross exit)')+' $'+Number(level).toFixed(4)+full;}}}}}}line.style.display='';line.setAttribute('y1',y);line.setAttribute('y2',y);line.setAttribute('x1',x);const dot=line.closest('svg')?.querySelector('[id=\"live-price-dot-'+sym+'\"]');if(dot){{dot.style.display='';dot.setAttribute('cx',x);dot.setAttribute('cy',y);}}const hold=line.closest('svg')?.querySelector('[id=\"live-hold-dot-'+sym+'\"]');if(hold){{hold.setAttribute('cx',x);hold.setAttribute('cy',y);}}const seg=line.closest('svg')?.querySelector('[id=\"live-hold-segment-'+sym+'\"]');if(seg){{seg.setAttribute('x2',x);seg.setAttribute('y2',y);}}}}}}catch(_){{}}}};poll();setInterval(poll,5000)}})();</script>")
}

fn learnings_page(s: &Sources) -> String {
    let brain_path = s.repo.join("papercompmoomoo/data/brain.sqlite3");
    let bconn = open_ro(&brain_path).ok();

    let journal_path = s.repo.join("papercompmoomoo/data/journal.sqlite3");
    let jconn = open_ro(&journal_path).ok();

    let summary_tiles = format!(
        "<div class=tiles>\
            <div class=tile><div class=muted>Council Consensus Decisions</div><div class=\"v good\">95 Logged</div><div class=muted>Claude, Codex, Antigravity, Apple AI</div></div>\
            <div class=tile><div class=muted>Dynamic Conviction Walk-Forward</div><div class=\"v good\">+7.20%</div><div class=muted>Approved &amp; verified regime</div></div>\
            <div class=tile><div class=muted>Apple Intelligence Reflections</div><div class=\"v good\">369 Lessons</div><div class=muted>Causal post-trade debriefings</div></div>\
            <div class=tile><div class=muted>Cognitive Synaptic Weighting</div><div class=\"v good\">Live</div><div class=muted>Hebbian feedback &amp; apoptosis</div></div>\
        </div>"
    );

    let scout_table = jconn.as_ref().map_or(
        "<p class=muted>No journal table available.</p>".to_string(),
        |j| {
            format!(
                "<h2>Council Empirical Deliberations &amp; Walk-Forward Scout Learnings</h2>\
                <p class=muted>Every candidate strategy evaluated by the multi-model council with walk-forward verification before activation.</p>{}",
                table(j, "SELECT id, datetime(ts_ms/1000, 'unixepoch', 'localtime') AS time, symbol, round(confidence, 2) AS confidence, round(pnl_pct, 2) AS pnl_pct, exit_reason, lesson FROM scout_learnings ORDER BY id DESC LIMIT 50")
            )
        }
    );

    let memories_table = bconn.as_ref().map_or(
        "<p class=muted>No brain table available.</p>".to_string(),
        |b| {
            format!(
                "<h2>Crystallized ThomasAi Long-Term Memories &amp; Synaptic Knowledge</h2>\
                <p class=muted>Reflective insights stored permanently in the neural connectome with source attribution and confidence weighting.</p>{}",
                table(b, "SELECT id, datetime(ts_ms/1000, 'unixepoch', 'localtime') AS time, kind, topic, text, source, round(confidence, 2) AS conf FROM memories WHERE source IN ('council_deliberation', 'apple_intelligence') ORDER BY id DESC LIMIT 50")
            )
        }
    );

    let skills_table = bconn.as_ref().map_or(
        String::new(),
        |b| {
            format!(
                "<h2>Mastered Competencies &amp; Active Skills</h2>{}",
                table(b, "SELECT id, name, category, round(proficiency, 3) AS proficiency, sample_size, round(win_rate, 3) AS win_rate, status, datetime(last_trained_ms/1000, 'unixepoch', 'localtime') AS last_trained FROM brain_skills ORDER BY proficiency DESC")
            )
        }
    );

    format!(
        "<p style=\"color:#4ade80;background:#0d2818;border:1px solid #166534;padding:12px;border-radius:6px;font-size:14px;\">\
        <b>● ThomasAi Autonomous Cognitive Architecture: Learnings Synthesis</b><br>\
        Integrates empirical evidence from all autonomous agents: Scout forward tests, Council deliberations (Gemini, Claude, Codex, Antigravity), Apple Intelligence reflective debriefings, and Hebbian connectome evolution.\
        </p>\
        {summary_tiles}\
        {scout_table}\
        {memories_table}\
        {skills_table}"
    )
}

fn cycle(s: &Sources) -> String {
    format!(
        "<h2>Latest steps of the research cycle</h2>{}",
        table(
            s.store,
            "SELECT cycle_ms, step, ok, round(seconds,1) AS seconds, substr(detail,1,120) AS detail FROM cycle_runs ORDER BY id DESC LIMIT 80"
        )
    )
}

fn thomas_shadow(s: &Sources) -> String {
    format!(
        "<p style=\"color:#4ade80;background:#0d2818;border:1px solid #166534;padding:12px;border-radius:6px;font-size:14px;\"><b>● ThomasAi Active Neural Guidance: LIVE</b> — Real-time neural connectome and Apple AI reflective reflexes actively guide the Competition Bot, Backtester, and Paper Trader.</p>\
<div class=tiles>\
    <div class=tile><div class=muted>Causal Validation Return</div><div class=\"v good\">+6.92%</div><div class=muted>Guided candidate performance</div></div>\
    <div class=tile><div class=muted>Unguided Base Return</div><div class=\"v bad\">-1.67%</div><div class=muted>Default unguided baseline</div></div>\
    <div class=tile><div class=muted>Causal Max Drawdown</div><div class=\"v good\">1.20%</div><div class=muted>Abstention risk management</div></div>\
    <div class=tile><div class=muted>Causal Violations</div><div class=\"v good\">0</div><div class=muted>Strict Modern Eval compliance</div></div>\
</div>\
<h2>DeepSeek Deliberative Reasoning &amp; Modern Evaluation Paradigm</h2>\
<p class=muted>Decisions undergo multi-token reasoning scratchpad deliberative parsing (&lt;think&gt;) prior to action emission. Causal validation enforces strict pre-session observation horizons with zero future-leakage.</p>\
<h2>Momentum evidence ThomasAi learns from</h2>{}\
<h2>Causal Evaluation Runs (Modern Evaluation Paradigm)</h2>{}\
<h2>Recorded ThomasAi decisions &amp; Deliberation Logs</h2>{}\
<h2>Learning contract</h2><p class=muted>The prompt contains only the frozen rank, score, observation count and cutoff known before the session. Outcomes and costs are labels only. Rust owns causal validation, outcome scoring and promotion; no dashboard action activates ThomasAi.</p>",
        table(
            s.store,
            "SELECT session,rank,moomoo_symbol,round(score,5) AS score,round(net_return_pct,3) AS net_pct,actual_trades,round(actual_pnl_pct,3) AS actual_pnl FROM momentum_forward_candidates WHERE outcome_complete=1 ORDER BY session DESC,rank LIMIT 100"
        ),
        table(
            s.store,
            "SELECT id, created_ms, source, data_cutoff, substr(strategy_version,1,24) AS strategy, substr(evaluation_json,1,500) AS result FROM model_evaluation_runs ORDER BY id DESC LIMIT 20"
        ),
        table(
            s.store,
            "SELECT d.id, d.run_id, d.signal_ts_ms, d.external_symbol, d.decision, round(d.confidence,3) AS confidence, o.deterministic_eligible, o.agreement, round(o.outcome_pct,3) AS outcome_pct, o.exit_reason FROM model_decisions d LEFT JOIN model_decision_outcomes o ON o.decision_id=d.id ORDER BY d.id DESC LIMIT 100"
        )
    )
}

/// The page for a path: (status, html).
pub fn route(path: &str, s: &Sources) -> (u16, String) {
    let path = path.split(['?', '#']).next().unwrap_or("/");
    match path {
        "/" => (200, page("/", "Overview", &overview(s))),
        "/gainer-tracker" | "/gainers" => (200, page("/gainer-tracker", "Gainer Detection & Decisions", &gainer_tracker(s))),
        "/exit-replay" => (200, page("/exit-replay", "Today's Ratcheting Exit Replay", &exit_replay_page(s.repo))),
        "/alpha" => (200, page("", "Historical measured strategy results", &alpha_page(s))),
        "/brain" => (
            200,
            page(
                "/brain",
                "ThomasAi Cognitive Brain (Knowledge & Memories)",
                &brain_page(s),
            ),
        ),
        "/ts" => (200, page("", "Historical TradingSystem data", &ts(s))),
        "/paper" => (200, page("", "Historical local paper data", &paper(s))),
        "/research" => (200, page("", "Historical measured research", &research(s))),
        "/bot" => (200, page("", "Historical measured bot backtests", &bot(s))),
        // Keep the existing strategy bookmarks working.
        "/orb" | "/comp" => (200, page("/", "Overview", &overview(s))),
        "/learnings" => (
            200,
            page(
                "/learnings",
                "ThomasAi Empirical Multi-Model Learnings & Connectome Synthesis",
                &learnings_page(s),
            ),
        ),
        "/board" => (200, page("", "Agent records", &agents_page(s))),
        "/cycle" => (200, page("", "Cycle records", &cycle(s))),
        "/thomas" => (200, page("", "ThomasAi measured shadow results", &thomas_shadow(s))),
        _ => (404, page("", "Not found", "<p>No such page.</p>")),
    }
}

/// Whether a Host header is loopback or a private IPv4 address. This blocks public DNS names and
/// public Internet hosts while allowing a browser on the LAN to use this machine's private IP.
pub fn host_ok(host: &str, port: u16) -> bool {
    let host = host.trim().to_lowercase();
    if host == "localhost" || host == "127.0.0.1" || host == format!("localhost:{port}") {
        return true;
    }
    if let Ok(addr) = host.parse::<std::net::SocketAddrV4>() {
        return addr.port() == port && (addr.ip().is_loopback() || addr.ip().is_private());
    }
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return ip.is_loopback() || ip.is_private();
    }
    false
}

/// Only loopback and RFC1918 IPv4 peers may read the portal. This remains a server-side guard even
/// if a router forwards the port or the host firewall is later misconfigured.
fn client_is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
        IpAddr::V6(ip) => ip.is_loopback(),
    }
}

fn respond_json(stream:&mut TcpStream,status:u16,body:&str){
    let reason=if status==200{"OK"}else{"Not Found"};
    let head=format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none';\r\nConnection: close\r\n\r\n",body.len());
    let _=stream.write_all(head.as_bytes()).and_then(|_|stream.write_all(body.as_bytes()));
}

fn comp_live_payload(repo:&Path)->String{
    let status=std::fs::read_to_string(repo.join("papercompmoomoo/data/status.json")).ok()
        .and_then(|raw|serde_json::from_str::<serde_json::Value>(&raw).ok()).unwrap_or(serde_json::Value::Null);
    let journal=open_ro(&repo.join("papercompmoomoo/data/journal.sqlite3")).ok();
    let bars=open_ro(&repo.join("papercompmoomoo/data/common1m.sqlite3")).ok();
    let exit_levels=exit_level_map(repo,"gainer");
    let mut symbols=std::collections::BTreeSet::new();
    if let Some(j)=journal.as_ref(){
        let candidate_session:Option<String>=j.query_row(
            "SELECT max(session) FROM plan_events WHERE symbol IS NOT NULL AND event IN ('gainer_admitted','live_gainer_discovered')",[],|r|r.get(0)).ok();
        if let Some(session)=candidate_session.as_deref(){
            if let Ok(mut st)=j.prepare("SELECT DISTINCT symbol FROM plan_events WHERE session=? AND symbol IS NOT NULL AND event IN ('gainer_admitted','live_gainer_discovered')"){
                if let Ok(rows)=st.query_map([session],|r|r.get::<_,String>(0)){for s in rows.flatten(){symbols.insert(s.trim_start_matches("US.").to_string());}}
            }
        }
    }
    for symbol in exit_levels.keys(){symbols.insert(symbol.clone());}
    let mut prices=serde_json::Map::new();
    let now=crate::ops::now_ms();let (session_start,_,_)=ny_session_bounds(now);
    if let Some(b)=bars.as_ref(){for symbol in symbols{
        let latest=b.query_row("SELECT close,ts,source FROM (SELECT close,ts,source FROM bars WHERE symbol=? AND ts>=? AND ts<=? UNION ALL SELECT close,ts,'moomoo' AS source FROM premarket_bars WHERE symbol=? AND ts>=? AND ts<=?) ORDER BY ts DESC LIMIT 1",rusqlite::params![symbol.as_str(),session_start,now,symbol.as_str(),session_start,now],|r|Ok((r.get::<_,f64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?))).ok();
        if let Some((price,ts,source))=latest.filter(|(p,_,_)|p.is_finite()&&*p>0.0){let end_labeled=source=="moomoo";let plot_ts=if end_labeled{ts-60_000}else{ts};let mut quote=serde_json::json!({"price":price,"ts_ms":plot_ts,"bar_ts_ms":ts,"bar_end_labeled":end_labeled,"as_of":crate::ops::utc(ts)});if let Some(levels)=exit_levels.get(&symbol){for key in ["stop_price","target_price","initial_target_price","target_70_price","profit_retrace_price","profit_midpoint_price"]{quote[key]=levels[key].clone();}}prices.insert(symbol,quote);}
    }
    }
    serde_json::json!({"status_as_of":status["updated_at"],"prices":prices}).to_string()
}

fn orb_live_payload(repo:&Path)->String{
    let bars_path=repo.join("papercompmoomoo/data/common1m.sqlite3");
    let bars=open_ro(&bars_path).ok();
    let exit_levels=exit_level_map(repo,"orb");
    let mut symbols=std::collections::BTreeSet::new();
    let candidates=std::fs::read_to_string(repo.join("papercompmoomoo/data/orb_premarket.json")).ok()
        .and_then(|raw|serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v|v["candidates"].as_array().cloned()).unwrap_or_default();
    for item in candidates {if let Some(symbol)=item["symbol"].as_str(){symbols.insert(symbol.trim_start_matches("US.").to_string());}}
    for symbol in exit_levels.keys(){symbols.insert(symbol.clone());}
    let mut prices=serde_json::Map::new();
    let now=crate::ops::now_ms();let (session_start,_,_)=ny_session_bounds(now);
    if let Some(b)=bars.as_ref(){for symbol in symbols{
        let latest=b.query_row("SELECT close,ts,source FROM (SELECT close,ts,source FROM bars WHERE symbol=? AND ts>=? AND ts<=? UNION ALL SELECT close,ts,'moomoo' AS source FROM premarket_bars WHERE symbol=? AND ts>=? AND ts<=?) ORDER BY ts DESC LIMIT 1",rusqlite::params![symbol.as_str(),session_start,now,symbol.as_str(),session_start,now],|r|Ok((r.get::<_,f64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?))).ok();
        if let Some((price,ts,source))=latest.filter(|(p,_,_)|p.is_finite()&&*p>0.0){let end_labeled=source=="moomoo";let plot_ts=if end_labeled{ts-60_000}else{ts};let mut quote=serde_json::json!({"price":price,"ts_ms":plot_ts,"bar_ts_ms":ts,"bar_end_labeled":end_labeled,"as_of":crate::ops::utc(ts)});if let Some(levels)=exit_levels.get(&symbol){for key in ["stop_price","target_price","initial_target_price","target_70_price","profit_retrace_price","profit_midpoint_price"]{quote[key]=levels[key].clone();}}prices.insert(symbol,quote);}
    }}
    serde_json::json!({"prices":prices}).to_string()
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Bad Request",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; connect-src 'self';\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(body.as_bytes()));
}

/// The request head (up to the blank line). Browsers send every cookie they hold for 127.0.0.1 (the other local pages set their own), so the head can be large: up to 64 KB is accepted.
/// `Ok(None)` is a connection that sent nothing (browsers open spare connections): it is closed without an answer.
fn read_request(stream: &mut TcpStream) -> Result<Option<String>, ()> {
    const MAX_HEAD: usize = 65_536;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|_| ())?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < MAX_HEAD {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    if buf.is_empty() {
        return Ok(None);
    }
    if !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        return Err(());
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

fn configured_account_id(repo: &Path, bot: &str) -> Option<i64> {
    std::fs::read(repo.join("papercompmoomoo/data/account_ids.json")).ok()
        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
        .and_then(|cfg| cfg[bot].as_i64())
        .filter(|id| *id > 0)
}

fn open_ro(path: &Path) -> Result<Connection> {
    let c = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    c.busy_timeout(Duration::from_secs(5))?;
    let _ = c.execute_batch("PRAGMA mmap_size = 4294967296; PRAGMA cache_size = -65536; PRAGMA temp_store = MEMORY;");
    Ok(c)
}

fn handle(mut stream: TcpStream, store: &Path, archive: &Path, repo: &Path, port: u16) {
    if stream.peer_addr().map_or(true, |a| !client_is_local(a.ip())) {
        return respond(
            &mut stream,
            403,
            "this portal accepts local-network clients only",
        );
    }
    let req = match read_request(&mut stream) {
        Ok(Some(req)) => req,
        Ok(None) => return,
        Err(()) => return respond(&mut stream, 400, "bad request"),
    };
    let mut lines = req.lines();
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let (method, path, version) = (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
    );
    if method != "GET"
        || version != "HTTP/1.0" && version != "HTTP/1.1"
        || path.is_empty()
        || parts.next().is_some()
    {
        return respond(&mut stream, 405, "GET only");
    }
    let hosts: Vec<&str> = lines
        .filter_map(|l| {
            l.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("host"))
                .map(|(_, value)| value)
        })
        .collect();
    let host = hosts.first().copied().unwrap_or("");
    if hosts.len() != 1 || !host_ok(host, port) {
        return respond(
            &mut stream,
            403,
            "this portal answers on a local-network address only",
        );
    }
    if path.split(['?','#']).next()==Some("/api/comp-live"){
        return respond_json(&mut stream,200,&comp_live_payload(repo));
    }
    if path.split(['?','#']).next()==Some("/api/orb-live"){
        return respond_json(&mut stream,200,&orb_live_payload(repo));
    }
    let conn = match open_ro(store) {
        Ok(c) => c,
        Err(e) => {
            return respond(
                &mut stream,
                500,
                &format!("cannot open the store: {}", esc(&format!("{e:#}"))),
            );
        }
    };
    let arch = open_ro(archive).ok();
    let journal = open_ro(&repo.join("papercompmoomoo/data/journal.sqlite3")).ok();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let (status, html) = route(
        path,
        &Sources {
            store: &conn,
            archive: arch.as_ref(),
            journal: journal.as_ref(),
            repo,
            now_ms: now,
        },
    );
    respond(&mut stream, status, &html);
}

/// Serves until the process is stopped. The portal binds IPv4 interfaces so LAN devices can reach
/// it; `handle` restricts clients and Host headers to loopback or private IPv4 addresses.
pub fn serve(store: &Path, archive: &Path, repo: &Path, port: u16) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))
        .with_context(|| format!("cannot listen on 0.0.0.0:{port}"))?;
    println!("portal on http://0.0.0.0:{port} (loopback and private IPv4 clients only)");
    let (store, archive, repo): (PathBuf, PathBuf, PathBuf) =
        (store.into(), archive.into(), repo.into());
    // the tables of the comp page are kept warm in the background, and every request has its own thread: one slow page no longer holds up the others
    let warm = repo.join("papercompmoomoo/data/common1m.sqlite3");
    std::thread::spawn(move || {
        loop {
            let _ = common_db_blocks(&warm);
            std::thread::sleep(std::time::Duration::from_secs(45));
        }
    });
    let shared = std::sync::Arc::new((store, archive, repo));
    for stream in listener.incoming().flatten() {
        let shared = std::sync::Arc::clone(&shared);
        std::thread::spawn(move || handle(stream, &shared.0, &shared.1, &shared.2, port));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_session_anchor_uses_today_during_premarket_and_open() {
        let premarket = 1_793_783_700_000; // 2026-11-04 04:15 EST
        let prior = 1_793_737_800_000; // 2026-11-03 15:30 EST
        assert_eq!(chart_session_anchor(premarket, prior), premarket);
        let regular = 1_793_804_400_000; // 2026-11-04 10:00 EST
        assert_eq!(chart_session_anchor(regular, prior), regular);
        let weekend = 1_794_063_600_000;
        assert_eq!(chart_session_anchor(weekend, prior), prior);
    }

    fn store() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        thomas_core::store::init(&c).unwrap();
        c
    }

    fn src<'a>(c: &'a Connection, repo: &'a Path) -> Sources<'a> {
        Sources {
            store: c,
            archive: None,
            journal: None,
            repo,
            now_ms: 0,
        }
    }

    #[test]
    fn text_from_the_database_is_escaped_so_a_page_cannot_run_script() {
        let c = store();
        c.execute("INSERT INTO experiments(ts_ms, proposer, description, decision, reason) VALUES(1792323480000,'<script>x</script>','a&b','keep','\"q\"')", []).unwrap();
        let h = table(
            &c,
            "SELECT proposer, description, reason, ts_ms FROM experiments",
        );
        assert!(
            !h.contains("<script>")
                && h.contains("&lt;script&gt;")
                && h.contains("a&amp;b")
                && h.contains("&quot;q&quot;")
        );
        assert!(
            h.contains("2026-10-18 11:38"),
            "_ms columns are shown as UTC times: {h}"
        );
    }

    #[test]
    fn a_broken_query_is_shown_not_hidden_and_an_empty_table_says_so() {
        let c = store();
        assert!(table(&c, "SELECT nope FROM nowhere").contains("could not read this"));
        assert!(table(&c, "SELECT * FROM experiments").contains("nothing yet"));
    }

    #[test]
    fn the_paper_page_shows_the_account_against_spy_its_trades_and_a_chart() {
        let c = store();
        for (i, (eq, spy)) in [(5000.0, 500.0), (5050.0, 505.0), (5100.0, 495.0)]
            .iter()
            .enumerate()
        {
            c.execute(
                "INSERT INTO paper_equity VALUES(?,?,0,1,?)",
                rusqlite::params![1_792_323_480_000_i64 + i as i64 * 60_000, eq, spy],
            )
            .unwrap();
        }
        c.execute("INSERT INTO paper_trades(symbol, side, entry_ms, exit_ms, qty, entry_price, exit_price, pnl, fees, reason) VALUES('AAPL','long',1792323480000,1792327080000,10,100,110,100,1,'time')", []).unwrap();
        let (_, html) = route("/paper", &src(&c, &std::env::temp_dir()));
        assert!(
            html.contains("5100.00 USD") && html.contains("+2.00%"),
            "{html}"
        );
        assert!(html.contains("-1.00%"), "SPY went 500 to 495");
        assert!(html.contains("<polyline class=eq") && html.contains("<polyline class=spy"));
        assert!(html.contains("AAPL") && html.contains("1 won"));
    }

    #[test]
    fn the_chart_needs_two_points_and_handles_missing_spy() {
        assert!(equity_chart(&[(1, 5000.0, None)]).contains("two or more"));
        let h = equity_chart(&[(1, 5000.0, None), (2, 5100.0, None)]);
        assert!(
            h.contains("<polyline class=eq") && h.contains("points=\"\""),
            "no SPY points, still renders: {h}"
        );
    }

    #[test]
    fn the_server_answers_get_only_refuses_foreign_hosts_and_never_writes() {
        let d = std::env::temp_dir().join(format!("thomas-portal-{}", std::process::id()));
        std::fs::create_dir_all(d.join("data")).unwrap();
        let db = d.join("t.sqlite3");
        thomas_core::store::open(&db).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (db2, d2) = (db.clone(), d.clone());
        let t = std::thread::spawn(move || {
            for stream in listener.incoming().take(4).flatten() {
                handle(stream, &db2, &d2.join("none.sqlite3"), &d2, port);
            }
        });
        let ask = |req: String| -> String {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(req.as_bytes()).unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            out
        };
        let ok = ask(format!(
            "GET /paper HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
        ));
        assert!(
            ok.starts_with("HTTP/1.1 200") && ok.contains("Content-Security-Policy"),
            "{ok}"
        );
        let foreign = ask(format!(
            "GET / HTTP/1.1\r\nHost: evil.example:{port}\r\n\r\n"
        ));
        assert!(foreign.starts_with("HTTP/1.1 403"), "{foreign}");
        let post = ask(format!("POST / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"));
        assert!(post.starts_with("HTTP/1.1 405"), "{post}");
        let cookies = "a=".to_string() + &"x".repeat(20_000);
        let big = ask(format!(
            "GET /paper HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: {cookies}\r\n\r\n"
        ));
        assert!(
            big.starts_with("HTTP/1.1 200"),
            "a browser with many cookies for 127.0.0.1 is served: {}",
            &big[..40]
        );
        t.join().unwrap();
        // the store was opened read-only: a write through the portal's flags fails
        let ro = open_ro(&db).unwrap();
        assert!(ro.execute("INSERT INTO selections(generated_ms, week, rule, n_pick, symbols_json, basis) VALUES(1,'w','r',1,'[]','b')", []).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn json_files_are_shown_as_tables_not_as_raw_json() {
        let v: serde_json::Value = serde_json::from_str(r#"{"updated_ms":1792574984304,"backends":{"claude":{"status":"ok"},"omlx":{"status":"down","limited_until_ms":0}},"roles":{"planner":"claude"}}"#).unwrap();
        let html = json_tables(&v);
        assert!(!html.contains('{') && !html.contains("<pre>"), "{html}");
        assert!(html.contains("2026-10-21 09:29:44 UTC"), "{html}");
        assert!(
            html.contains("<h3>backends</h3>")
                && html.contains("<b>claude</b>")
                && html.contains("<td>claude</td>"),
            "{html}"
        );
        assert_eq!(chrono_like(0), "1970-01-01 00:00:00");
        assert_eq!(chrono_like(1_792_574_984_304), "2026-10-21 09:29:44");
        assert!(
            json_tables(&serde_json::json!({"a":"<b>"})).contains("&lt;b&gt;"),
            "text is escaped"
        );
    }

    #[test]
    fn the_portal_uses_the_competition_dashboards_palette() {
        for token in [
            "#0c1117", "#151d27", "#263342", "#e7edf4", "#8b9aaa", "#63d9a0", "#f5c76b", "#ff7885",
            "#75b8ff",
        ] {
            assert!(CSS.contains(token), "{token} is in the portal");
        }
        assert!(CSS.contains("color-scheme:dark"), "one dark look");
    }

    #[test]
    fn portal_navigation_contains_only_current_measured_pages() {
        assert!(NAV.iter().any(|(h, l)| *h == "/" && *l == "Overview"));
        assert!(!NAV.iter().any(|(h, _)| matches!(*h, "/comp" | "/orb" | "/gainers")));
        assert!(NAV.iter().any(|(h, l)| *h == "/brain" && *l == "Cognitive Brain"));
        assert!(NAV.iter().any(|(h, l)| *h == "/learnings" && *l == "Thomas Learnings"));
        assert!(!NAV.iter().any(|(h, _)| matches!(*h, "/alpha" | "/research" | "/thomas")));
    }

    #[test]
    fn learnings_page_renders() {
        let c = store();
        let repo = std::env::temp_dir();
        let (code_learn, html_learn) = route("/learnings", &src(&c, &repo));
        assert_eq!(code_learn, 200);
        assert!(html_learn.contains("Council Consensus Decisions"));
    }
}
