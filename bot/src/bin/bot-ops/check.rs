//! `bot-ops check [minutes]`: the compact health check of the bot (was check.sh): OpenD, the service, the console status and winners, and the warnings and
//! errors of the last `minutes` of the bot's log.

use crate::common;
use crate::platform;
use crate::service;

/// Counted, deduplicated WARN/ERROR lines (newest `minutes`) of bot.err in the format of `uniq -c | sort -rn | head -8`.
pub fn log_summary(log_text: &str, since: &str) -> Vec<String> {
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for l in log_text.lines() {
        // [2026-10-13T20:22:29Z WARN  bot::service] message
        if l.len() < 17 || !l.starts_with('[') || &l[1..17] < since {
            continue;
        }
        let Some((head, msg)) = l.split_once("] ") else {
            continue;
        };
        let level = if head.contains(" WARN ") {
            "WARN"
        } else if head.contains(" ERROR ") {
            "ERROR"
        } else {
            continue;
        };
        let text: String = format!("{level} {msg}").chars().take(200).collect();
        *counts.entry(text).or_default() += 1;
    }
    let mut v: Vec<(usize, String)> = counts.into_iter().map(|(t, c)| (c, t)).collect();
    v.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    v.into_iter()
        .take(8)
        .map(|(c, t)| format!("{c:4} {t}"))
        .collect()
}

/// The last `bar 20xx-..` line of the log, cut to 90 characters.
pub fn last_bar(log_text: &str) -> Option<String> {
    log_text
        .lines()
        .rev()
        .find(|l| {
            l.contains("bar 20")
                && l.split("bar 20")
                    .nth(1)
                    .is_some_and(|r| r.chars().take(3).all(|c| c.is_ascii_digit() || c == '-'))
        })
        .map(|l| l.chars().take(90).collect())
}

pub fn run(minutes: i64) -> i32 {
    print!("{}", report(minutes));
    0
}

/// The text of the check (so the monitor and the lab can reuse it).
pub fn report(minutes: i64) -> String {
    let mut out = String::new();
    out += if common::opend_up() {
        "OpenD :11111 up"
    } else {
        "OpenD :11111 DOWN"
    };
    let st = service::state(common::BOT_LABEL);
    let pid = if !st.loaded {
        "NOT LOADED".to_string()
    } else {
        st.pid.map_or("-".to_string(), |p| p.to_string())
    };
    out += &format!(
        " | service pid {pid} | now {} UTC\n",
        chrono::Utc::now().format("%H:%M")
    );
    let status = common::http_get(8789, "/api/status");
    // qwen-tools belongs to the archived lab and is optional for the bot-only
    // checkout. A missing lab helper must not make the bot health check noisy.
    if std::process::Command::new("qwen-tools")
        .arg("--help")
        .output()
        .is_ok_and(|x| x.status.success())
    {
        out += &common::tool("qwen-tools", &["bot-status"], Some(&status)).1;
        let winners = common::http_get(8789, "/api/winners");
        out += &common::tool("qwen-tools", &["bot-winners"], Some(&winners)).1;
    }
    let log = platform::bot_logs_dir().join("bot.err");
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let since = (chrono::Utc::now() - chrono::Duration::minutes(minutes))
        .format("%Y-%m-%dT%H:%M")
        .to_string();
    for l in log_summary(&text, &since) {
        out += &l;
        out += "\n";
    }
    if let Some(l) = last_bar(&text) {
        out += &l;
        out += "\n";
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: warnings and errors are counted and the prefix removed, newest window only.
    #[test]
    fn the_log_summary_counts_and_cuts() {
        let log = "[2026-10-13T20:00:00Z WARN  bot::service] old thing\n[2026-10-13T20:22:29Z WARN  bot::service] EXCLUDED X: quota\n[2026-10-13T20:22:30Z WARN  bot::service] EXCLUDED X: quota\n[2026-10-13T20:23:00Z ERROR bot::service] boom\n[2026-10-13T20:24:00Z INFO  bot::service] fine\n";
        let s = log_summary(log, "2026-10-13T20:10");
        assert_eq!(
            s,
            vec![
                "   2 WARN EXCLUDED X: quota".to_string(),
                "   1 ERROR boom".to_string()
            ]
        );
        assert!(log_summary(log, "2026-10-14T00:00").is_empty());
    }

    /// Test: the last bar line is found.
    #[test]
    fn the_last_bar_line_is_found() {
        let log = "[x INFO] bar 2026-10-13 15:45 ET | 80 symbols\n[x INFO] other\n[x INFO] bar 2026-10-13 16:00 ET | 80 symbols\n";
        assert!(last_bar(log).unwrap().contains("16:00 ET"));
        assert!(last_bar("nothing").is_none());
    }
}
