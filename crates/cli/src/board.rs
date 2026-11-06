//! The single status board (the portal's Overview page): everything that can and should be watched about BOTH bots on one screen, each line green, amber or red
//! with the reason. Read-only: it reads the stores, the heartbeat files the other programs already write, the deployment check and the free resources; it starts
//! nothing. Freshness (is it alive?) is judged from the heartbeats, so no process is probed.
use rusqlite::Connection;
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Ok,
    Warn,
    Bad,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub area: &'static str,
    pub name: &'static str,
    pub state: State,
    pub detail: String,
}

fn row(area: &'static str, name: &'static str, state: State, detail: impl Into<String>) -> Row {
    Row {
        area,
        name,
        state,
        detail: detail.into(),
    }
}

fn json(repo: &Path, rel: &str) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(repo.join(rel)).ok()?).ok()
}

/// "2026-10-20T11:23:41+00:00" (or with fractions) as UTC milliseconds.
pub fn parse_utc_ms(text: &str) -> Option<i64> {
    let (date, time) = text.get(..19)?.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':').map(|x| x.parse::<i64>().ok());
    let (hh, mm, ss) = (t.next()??, t.next()??, t.next()??);
    let (yy, mp) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(((days * 24 + hh) * 60 + mm) * 60_000 + ss * 1000)
}

fn age(now: i64, then: i64) -> String {
    let s = ((now - then) / 1000).max(0);
    match s {
        0..=119 => format!("{s} s ago"),
        120..=7199 => format!("{} min ago", s / 60),
        7200..=172_799 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

/// Green when the heartbeat is younger than `ok_s`, amber under `warn_s`, else red.
fn fresh(now: i64, then: Option<i64>, ok_s: i64, warn_s: i64) -> State {
    match then {
        Some(t) if now - t <= ok_s * 1000 => State::Ok,
        Some(t) if now - t <= warn_s * 1000 => State::Warn,
        _ => State::Bad,
    }
}

fn one<T: rusqlite::types::FromSql>(c: &Connection, sql: &str) -> Option<T> {
    c.query_row(sql, [], |r| r.get(0)).ok()
}

/// Health is evaluated on every dashboard request. The page itself refreshes
/// every minute, so health messages never come from a stale in-process cache.
fn slow_rows(repo: &Path, _now: i64) -> Vec<Row> {
    let mut rows = Vec::new();
    let problems = crate::deploy::problems(repo);
    rows.push(if problems.is_empty() {
        row("Health", "Deployment", State::Ok, "local changes are committed; thomas, the portal and the competition bot run the code on disk")
    } else {
        let worst = if problems.iter().any(|p| p.error) { State::Bad } else { State::Warn };
        row("Health", "Deployment", worst, problems.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(" | "))
    });
    rows.push(match crate::headroom::measure() {
        Ok(h) => {
            let low = h
                .ram_free_percent
                .min(h.cpu_idle_percent)
                .min(h.disk_free_percent);
            let state = if low < crate::headroom::MIN_FREE_PERCENT {
                State::Bad
            } else if low < crate::headroom::MIN_FREE_PERCENT + 9.0 {
                State::Warn
            } else {
                State::Ok
            };
            row(
                "Health",
                "Resources",
                state,
                format!(
                    "free: RAM {:.0}%, CPU {:.0}%, disk {:.0}% (the rule keeps at least {}%)",
                    h.ram_free_percent,
                    h.cpu_idle_percent,
                    h.disk_free_percent,
                    crate::headroom::MIN_FREE_PERCENT
                ),
            )
        }
        Err(e) => row(
            "Health",
            "Resources",
            State::Warn,
            format!("could not be measured: {e}"),
        ),
    });
    rows
}

/// Every line of the board.
pub fn rows(_store: &Connection, journal: Option<&Connection>, repo: &Path, now: i64) -> Vec<Row> {
    let mut v = Vec::new();
    v.extend(slow_rows(repo, now));
    // watchdog
    let wd = json(repo, "data/watchdog.json");
    let wd_ms = wd.as_ref().and_then(|w| w["last_run_ms"].as_i64());
    v.push(row(
        "Health",
        "Watchdog",
        fresh(now, wd_ms, 1800, 7200),
        wd_ms.map_or("has not run".into(), |t| {
            format!(
                "last pass {}; {} action(s)",
                age(now, t),
                wd.as_ref()
                    .and_then(|w| w["actions"].as_array())
                    .map_or(0, Vec::len)
            )
        }),
    ));
    let agents = json(repo, "papercompmoomoo/data/agents_status.json");
    let agents_ms = agents.as_ref().and_then(|a| a["updated_ms"].as_i64());
    let local_ok = agents
        .as_ref()
        .and_then(|a| a["backends"]["local"]["status"].as_str())
        == Some("ok");
    v.push(row(
        "Health",
        "Local Qwen",
        if local_ok && fresh(now, agents_ms, 120, 600) == State::Ok {
            State::Ok
        } else {
            State::Bad
        },
        format!(
            "llama.cpp backend {}; coordinator heartbeat {}",
            if local_ok { "reachable" } else { "not reachable" },
            agents_ms.map_or("never".into(), |t| age(now, t))
        ),
    ));

    // ---- the NASDAQ Gainers bot ----
    let status = json(repo, "papercompmoomoo/data/status.json");
    let upd = status
        .as_ref()
        .and_then(|s| s["updated_at"].as_str())
        .and_then(parse_utc_ms);
    match &status {
        Some(s) => {
            let positions = s["open_positions"].as_array().map_or(0, Vec::len);
            v.push(row(
                "Gainers bot",
                "Bot",
                fresh(now, upd, 120, 900),
                format!("{}; updated {}; {} universe symbols, {} Nasdaq; orders today {}; {} open position(s)", s["state"].as_str().unwrap_or("?"), upd.map_or("never".into(), |t| age(now, t)), s["universe"], s["nasdaq_symbols"].as_i64().unwrap_or(0), s["orders_today"], positions),
            ));
            let a = &s["account"];
            let kind_ok =
                s["account_kind"].as_str() == Some("COMPETITION") && s["acc_id"].is_number();
            let status_fresh = fresh(now, upd, 120, 900) == State::Ok;
            v.push(row(
                "Gainers bot",
                "Account",
                if kind_ok && status_fresh { State::Ok } else { State::Bad },
                format!(
                    "{} {}: equity {} · cash {} · buying power {} · {} working order(s)",
                    s["account_kind"].as_str().unwrap_or("?"),
                    s["acc_id"],
                    a["equity"],
                    a["cash"],
                    a["power"],
                    a["working_orders"].as_array().map_or(0, Vec::len)
                ),
            ));
        }
        None => v.push(row(
            "Gainers bot",
            "Bot",
            State::Bad,
            "no status file: the bot has never started here",
        )),
    }
    let audit = json(repo, "papercompmoomoo/data/audit.json");
    let audit_ms = audit
        .as_ref()
        .and_then(|a| a["at"].as_str())
        .and_then(parse_utc_ms);
    v.push(match &audit {
        Some(a) => {
            let (errs, warns) = (
                a["errors"].as_i64().unwrap_or(0),
                a["warnings"].as_i64().unwrap_or(0),
            );
            let fixed = a["fixed"].as_i64().unwrap_or(0);
            let active_warns = (warns - fixed).max(0);
            let state = if errs > 0 || fresh(now, audit_ms, 900, 3600) == State::Bad {
                State::Bad
            } else if active_warns > 0 || fresh(now, audit_ms, 900, 3600) == State::Warn {
                State::Warn
            } else {
                State::Ok
            };
            row(
                "Gainers bot",
                "Self-audit",
                state,
                format!(
                    "{} error(s), {} warning(s), {} repaired; {}",
                    errs,
                    warns,
                    a["fixed"],
                    audit_ms.map_or("never".into(), |t| age(now, t))
                ),
            )
        }
        None => row("Gainers bot", "Self-audit", State::Bad, "no audit yet"),
    });
    let plan = json(repo, "papercompmoomoo/data/plan.json");
    let selection = plan.as_ref().and_then(|p| p.get("selection"));
    let current_gainer = selection
        .and_then(|s| s["strategy"].as_str())
        == Some("nasdaq_hottest_gainers_live");
    v.push(row(
        "Gainers bot",
        "Strategy in force",
        if current_gainer {
            State::Ok
        } else {
            State::Bad
        },
        selection.map_or("no current-session gainer selection is recorded".into(), |s| {
            let strategy = s["strategy"].as_str().unwrap_or("invalid");
            format!(
                "{strategy}; native one-minute OpenD evidence; session {}",
                s["data_cutoff"].as_str().unwrap_or("missing")
            )
        }),
    ));
    if let Some(j) = journal {
        let n = |sql: &str| one::<i64>(j, sql).unwrap_or(0);
        let (ord, tr) = (
            n("SELECT count(*) FROM orders"),
            n("SELECT count(*) FROM trades"),
        );
        let pnl: f64 = one(j, "SELECT coalesce(sum(pnl_pct),0) FROM trades").unwrap_or(0.0);
        v.push(row("Gainers bot", "Live evidence", State::Ok, format!("journal: {ord} simulated orders, {tr} closed trades (sum of trade returns {pnl:+.2}%)")));
    }

    v
}

pub fn render(rows: &[Row]) -> String {
    let mut out = String::from(
        "<div class=scroll><table><tr><th></th><th>Area</th><th>What</th><th>State</th></tr>",
    );
    for r in rows {
        let (dot, class) = match r.state {
            State::Ok => ("●", "good"),
            State::Warn => ("●", "warn"),
            State::Bad => ("●", "bad"),
        };
        out.push_str(&format!("<tr><td class={class}>{dot}</td><td>{}</td><td><b>{}</b></td><td style=\"white-space:normal\">{}</td></tr>", crate::portal::esc(r.area), crate::portal::esc(r.name), crate::portal::esc(&r.detail)));
    }
    out.push_str("</table></div>");
    out
}

/// A line that summarises the board for the top of the page.
pub fn summary(rows: &[Row]) -> String {
    let bad = rows.iter().filter(|r| r.state == State::Bad).count();
    let warn = rows.iter().filter(|r| r.state == State::Warn).count();
    match (bad, warn) {
        (0, 0) => "<p class=good><b>Everything is green.</b></p>".to_string(),
        (0, w) => format!("<p class=warn><b>{w} item(s) need attention (amber).</b></p>"),
        (b, w) => format!("<p class=bad><b>{b} item(s) are red, {w} amber.</b></p>"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        thomas_core::store::init(&c).unwrap();
        c
    }

    fn folder(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("thomas-board-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("papercompmoomoo/data")).unwrap();
        std::fs::create_dir_all(d.join("data")).unwrap();
        d
    }

    #[test]
    fn utc_times_from_the_heartbeat_files_parse() {
        assert_eq!(parse_utc_ms("1970-01-01T00:00:00+00:00"), Some(0));
        assert_eq!(
            parse_utc_ms("2026-10-20T11:23:41.123456+00:00"),
            Some(1_792_495_421_000)
        );
        assert_eq!(parse_utc_ms("garbage"), None);
    }

    #[test]
    fn freshness_is_green_amber_then_red() {
        let now = 1_000_000_000;
        assert_eq!(fresh(now, Some(now - 10_000), 60, 600), State::Ok);
        assert_eq!(fresh(now, Some(now - 300_000), 60, 600), State::Warn);
        assert_eq!(fresh(now, Some(now - 700_000), 60, 600), State::Bad);
        assert_eq!(fresh(now, None, 60, 600), State::Bad);
    }

    #[test]
    fn a_folder_with_nothing_running_shows_red_for_what_should_be_running_and_a_folder_with_fresh_heartbeats_shows_green()
     {
        let now = 1_792_495_421_000;
        let c = store();
        let d = folder("a");
        let find = |rows: &[Row], name: &str| rows.iter().find(|r| r.name == name).map(|r| r.state);
        let empty = rows(&c, None, &d, now);
        assert_eq!(find(&empty, "Bot"), Some(State::Bad));
        assert_eq!(find(&empty, "Self-audit"), Some(State::Bad));
        assert_eq!(find(&empty, "Watchdog"), Some(State::Bad));
        assert_eq!(find(&empty, "Strategy in force"), Some(State::Bad));
        // fresh heartbeats
        let iso = |_ms: i64| "2026-10-20T11:23:41+00:00".to_string();
        std::fs::write(d.join("papercompmoomoo/data/status.json"), format!(r#"{{"state":"scanning","updated_at":"{}","account_kind":"COMPETITION","acc_id":111111,"account":{{"equity":100000.0,"cash":100000.0,"power":200000.0,"working_orders":[]}},"open_positions":[],"orders_today":0,"universe":8018,"baseline_count":7836}}"#, iso(now))).unwrap();
        std::fs::write(
            d.join("papercompmoomoo/data/audit.json"),
            format!(
                r#"{{"at":"{}","errors":0,"warnings":0,"fixed":0}}"#,
                iso(now)
            ),
        )
        .unwrap();
        std::fs::write(
            d.join("data/watchdog.json"),
            format!(r#"{{"last_run_ms":{now},"actions":[]}}"#),
        )
        .unwrap();
        std::fs::write(
            d.join("papercompmoomoo/data/agents_status.json"),
            format!(r#"{{"updated_ms":{now},"backends":{{"local":{{"status":"ok"}}}}}}"#),
        )
        .unwrap();
        let live = rows(&c, None, &d, now + 5_000);
        assert_eq!(find(&live, "Bot"), Some(State::Ok), "{live:?}");
        assert_eq!(find(&live, "Account"), Some(State::Ok));
        assert_eq!(find(&live, "Self-audit"), Some(State::Ok));
        assert_eq!(find(&live, "Watchdog"), Some(State::Ok));
        assert_eq!(find(&live, "Local Qwen"), Some(State::Ok));
        // an account that is not the competition one is red whatever else is fine
        std::fs::write(d.join("papercompmoomoo/data/status.json"), format!(r#"{{"state":"scanning","updated_at":"{}","account_kind":"OTHER","acc_id":1,"account":{{}},"open_positions":[]}}"#, iso(now))).unwrap();
        assert_eq!(
            find(&rows(&c, None, &d, now + 5_000), "Account"),
            Some(State::Bad)
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_summary_and_the_table_say_what_is_wrong() {
        let rs = vec![
            row("A", "x", State::Ok, "fine"),
            row("A", "y", State::Bad, "broken <b>"),
            row("B", "z", State::Warn, "late"),
        ];
        assert!(summary(&rs).contains("1 item(s) are red, 1 amber"));
        assert!(summary(&rs[..1]).contains("Everything is green"));
        let html = render(&rs);
        assert!(
            html.contains("broken &lt;b&gt;")
                && html.contains("class=bad")
                && html.contains("class=warn")
                && html.contains("class=good")
        );
    }
}
