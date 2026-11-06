//! Phase 4, unattended operation: `thomas cycle` (one pass of refresh -> verify -> research -> report, every step logged in `cycle_runs`),
//! `thomas report` (the daily honest report), `thomas doctor` (what is wrong), `thomas schedule` (a launchd agent that runs the cycle every hour at
//! low priority). Everything is on disk: SQLite rows, report files, a lock file. Nothing here pauses or waits for the machine.
use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const LABEL: &str = "com.thomasai.cycle";
pub const PORTAL_LABEL: &str = "com.thomasai.portal";

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// "2026-10-18 11:38" (UTC) from epoch milliseconds.
pub fn utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60
    )
}

/// One step of the cycle: a `thomas` subcommand. A fatal step that fails stops the cycle (research on broken data would mislead).
pub struct Step {
    pub name: &'static str,
    pub args: Vec<String>,
    pub fatal: bool,
    /// A step that runs longer than this is stopped (and reported as failed), so one slow step cannot hold the cycle and everything behind it (the daily judge).
    pub limit_secs: Option<u64>,
}

/// The longest any research step may run in one cycle (the longest normal one, the search, takes a few minutes).
pub const STEP_LIMIT_SECS: u64 = 40 * 60;

/// True on a weekday between 13:25 and 21:05 UTC: the US regular session (09:30-16:00 Eastern) falls inside it in both summer and winter time, with a few minutes
/// either side so the last candle is picked up.
pub fn in_trading_window(ms: i64) -> bool {
    let minute = ms.rem_euclid(86_400_000) / 60_000;
    !is_weekend(ms) && (13 * 60 + 25..=21 * 60 + 5).contains(&minute)
}

/// Saturday or Sunday (UTC).
pub fn is_weekend(ms: i64) -> bool {
    matches!((ms.div_euclid(86_400_000) + 3).rem_euclid(7), 5 | 6)
}

/// What the cycle should run now. The Yahoo fetch only when `FETCH_EVERY_MS` has passed since the last good one; the sole strategy's selection refreshes when the newest merged day changes.
pub fn plan(c: &Connection, now: i64, refresh: bool, rounds: usize) -> Vec<Step> {
    plan_with(
        c,
        now,
        refresh,
        rounds,
        thomas_core::config::YAHOO_DOWNLOADS,
    )
}

/// `plan` with the Yahoo downloads switched explicitly (`yahoo`): when off, the refresh only merges what is already stored locally.
pub fn plan_with(
    _c: &Connection,
    _now: i64,
    _refresh: bool,
    _rounds: usize,
    _yahoo: bool,
) -> Vec<Step> {
    let s = |name, args: &[&str], fatal| Step {
        name,
        args: args.iter().map(|a| a.to_string()).collect(),
        fatal,
        limit_secs: Some(STEP_LIMIT_SECS),
    };
    vec![
        s("doctor", &["doctor"], true),
        s("neural-step", &["neural-step"], false),
    ]
}

/// Remembers the newest bar used by the momentum selector so an unchanged day is not selected again.
pub fn mark_selection(c: &Connection, cycle_ms: i64) -> Result<()> {
    if let Some(newest) = scalar::<i64>(c, "SELECT max(wall_ms) FROM candles_merged") {
        c.execute("INSERT INTO cycle_runs(cycle_ms, step, ok, seconds, detail) VALUES(?, 'mark', 1, 0, ?)", rusqlite::params![cycle_ms, newest.to_string()])?;
    }
    Ok(())
}

/// Runs `cmd`, returning (success, seconds, the last 4,000 characters of its output); a command still running after `limit_secs` is killed and reported as failed.
pub fn run_command_limited(mut cmd: Command, limit_secs: Option<u64>) -> (bool, f64, String) {
    use std::process::Stdio;
    let t = Instant::now();
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                false,
                t.elapsed().as_secs_f64(),
                format!("could not start: {e}"),
            );
        }
    };
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let (output, stopped) = match limit_secs {
        Some(limit) => match rx.recv_timeout(Duration::from_secs(limit)) {
            Ok(o) => (o, false),
            Err(_) => {
                let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
                (
                    rx.recv()
                        .unwrap_or_else(|_| Err(std::io::Error::other("lost the child"))),
                    true,
                )
            }
        },
        None => (
            rx.recv()
                .unwrap_or_else(|_| Err(std::io::Error::other("lost the child"))),
            false,
        ),
    };
    match output {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&o.stderr));
            let mut tail: String = text
                .chars()
                .rev()
                .take(4000)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            if stopped {
                tail.push_str(&format!(
                    "\nstopped after {} s: too slow for one cycle\n",
                    limit_secs.unwrap_or(0)
                ));
            }
            (
                o.status.success() && !stopped,
                t.elapsed().as_secs_f64(),
                tail,
            )
        }
        Err(e) => (
            false,
            t.elapsed().as_secs_f64(),
            format!("could not run: {e}"),
        ),
    }
}

/// A lock file holding our pid; a stale one (its process is gone) is replaced.
#[derive(Debug)]
pub struct Lock(PathBuf);

impl Lock {
    pub fn take(path: &Path) -> Result<Lock> {
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(mut f) => {
                    use std::io::Write;
                    f.write_all(std::process::id().to_string().as_bytes())?;
                    return Ok(Lock(path.to_path_buf()));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if let Ok(text) = std::fs::read_to_string(path)
                        && let Ok(pid) = text.trim().parse::<u32>()
                        && Command::new("kill")
                            .args(["-0", &pid.to_string()])
                            .output()
                            .is_ok_and(|o| o.status.success())
                    {
                        bail!("another cycle is running (pid {pid})");
                    }
                    std::fs::remove_file(path)?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        bail!("could not acquire lock {}", path.display())
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// One pass. `exe` is this binary; every step is a subprocess so its memory is returned when it ends. Returns the ids of failed steps.
pub fn cycle(conn: &Connection, exe: &Path, plan: &[Step]) -> Result<Vec<&'static str>> {
    let cycle_ms = now_ms();
    let mut failed = Vec::new();
    for st in plan {
        let mut cmd = Command::new(exe);
        cmd.args(&st.args);
        let (ok, secs, tail) = run_command_limited(cmd, st.limit_secs);
        conn.execute(
            "INSERT INTO cycle_runs(cycle_ms, step, ok, seconds, detail) VALUES(?,?,?,?,?)",
            rusqlite::params![cycle_ms, st.name, ok as i64, secs, tail],
        )?;
        eprintln!(
            "[{}] {}: {} in {:.0} s",
            utc(now_ms()),
            st.name,
            if ok { "ok" } else { "FAILED" },
            secs
        );
        if ok && st.name == "momentum-select" {
            mark_selection(conn, cycle_ms)?;
        }
        if !ok {
            failed.push(st.name);
            if st.fatal {
                break;
            }
        }
    }
    Ok(failed)
}

fn scalar<T: rusqlite::types::FromSql>(c: &Connection, sql: &str) -> Option<T> {
    c.query_row(sql, [], |r| r.get(0)).ok()
}

/// Last output of a cycle step, lines containing any of `keys`.
fn last_lines(c: &Connection, step: &str, keys: &[&str]) -> Vec<String> {
    let text: Option<String> = c
        .query_row(
            "SELECT detail FROM cycle_runs WHERE step=? ORDER BY id DESC LIMIT 1",
            [step],
            |r| r.get(0),
        )
        .ok();
    text.unwrap_or_default()
        .lines()
        .filter(|l| keys.iter().any(|k| l.contains(k)))
        .map(|l| l.trim().chars().take(240).collect())
        .collect()
}

/// The daily honest report, markdown, built only from what is in the store.
pub fn report(c: &Connection) -> String {
    let mut o = vec![
        format!("# ThomasAi report {}", utc(now_ms())),
        String::new(),
    ];
    o.push("## Data".into());
    let last: Option<i64> = scalar(c, "SELECT max(wall_ms) FROM candles_merged");
    let n_sym: Option<i64> = scalar(
        c,
        "SELECT count(DISTINCT moomoo_symbol) FROM candles_merged",
    );
    let sessions: Option<i64> = scalar(c, "SELECT count(*) FROM sessions_calendar");
    o.push(format!(
        "- newest 1-minute bar: {}; {} symbols with candles; {} sessions in the calendar",
        last.map_or("none".into(), utc),
        n_sym.unwrap_or(0),
        sessions.unwrap_or(0)
    ));
    let mix: Vec<String> = c
        .prepare("SELECT source, count(*) FROM candles_merged GROUP BY source ORDER BY source")
        .and_then(|mut s| {
            s.query_map([], |r| {
                Ok(format!(
                    "{} {}",
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(0)?
                ))
            })
            .map(|r| r.filter_map(Result::ok).collect())
        })
        .unwrap_or_default();
    o.push(format!("- bars by source: {}", mix.join(", ")));
    o.push(String::new());
    o.push("## Active strategy: 20-session cross-sectional momentum".into());
    let forward: Option<(i64, i64, Option<f64>, Option<f64>)> = c
        .query_row(
            "SELECT count(*),coalesce(sum(candidates=completed AND candidates>0),0),100*(coalesce((SELECT equity FROM momentum_forward_runs WHERE equity IS NOT NULL ORDER BY session DESC LIMIT 1),1)-1),max(drawdown_pct) FROM momentum_forward_runs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .ok();
    match forward {
        Some((sessions, mature, total, drawdown)) => o.push(format!(
            "- {mature}/{sessions} forward session blocks complete; compounded return after costs {:+.2}%; maximum drawdown {:.2}%",
            total.unwrap_or(0.0),
            drawdown.unwrap_or(0.0)
        )),
        None => o.push("- forward ledger not initialized; run `thomas momentum-forward`".into()),
    }
    o.push("- selections use exactly 20 prior completed sessions; outcomes use the next real session and 10 bps round-trip cost; no active strategy mutation is automatic".into());
    o.push(String::new());
    o.push("## Archived strategy research (inactive; retained as historical evidence)".into());
    let periods: i64 = scalar(
        c,
        "SELECT count(DISTINCT period_start_ms) FROM search_periods",
    )
    .unwrap_or(0);
    let winners: i64 = scalar(c, "SELECT count(*) FROM winning_settings").unwrap_or(0);
    o.push(format!(
        "- {periods} overlapping 14-session periods searched; {winners} winning settings kept"
    ));
    for l in last_lines(
        c,
        "search",
        &["PREFERRED", "VERDICT", "FORWARD", "REGISTRY", "RESULT:"],
    ) {
        o.push(format!("- {l}"));
    }
    let models: Vec<String> = c
        .prepare("SELECT DISTINCT model FROM learner_periods ORDER BY model")
        .and_then(|mut st| {
            st.query_map([], |r| r.get(0))
                .map(|r| r.filter_map(Result::ok).collect())
        })
        .unwrap_or_default();
    if models.is_empty() {
        o.push("- learner: not run".into());
    }
    for m in models {
        let ic: Vec<(f64, i64)> = c
            .prepare("SELECT ic_mean, has_signal FROM learner_periods WHERE model=? ORDER BY period_start_ms")
            .and_then(|mut st| st.query_map([&m], |r| Ok((r.get(0)?, r.get(1)?))).map(|r| r.filter_map(Result::ok).collect()))
            .unwrap_or_default();
        o.push(format!(
            "- learner {m}: {} of {} periods show a signal (IC per period: {})",
            ic.iter().filter(|x| x.1 == 1).count(),
            ic.len(),
            ic.iter()
                .map(|x| format!("{:+.3}", x.0))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    o.push(String::new());
    o.push("## Archived walk-forward evidence (inactive)".into());
    let latest: Option<i64> = scalar(c, "SELECT max(run_ms) FROM walkforward_runs");
    match latest {
        None => o.push("- not run yet".into()),
        Some(run) => {
            let rows: Vec<(String, String, f64, Option<f64>, i64, i64)> = c
                .prepare("SELECT layout, rule, return_pct, ref_return_pct, trades, wins FROM walkforward_runs WHERE run_ms=? ORDER BY layout, rule, fold")
                .and_then(|mut st| st.query_map([run], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))).map(|r| r.filter_map(Result::ok).collect()))
                .unwrap_or_default();
            let mut keys: Vec<(String, String)> =
                rows.iter().map(|r| (r.0.clone(), r.1.clone())).collect();
            keys.dedup();
            for (layout, rule) in keys {
                let mine: Vec<_> = rows
                    .iter()
                    .filter(|r| r.0 == layout && r.1 == rule)
                    .collect();
                let stitch = |it: Vec<f64>| {
                    (it.into_iter().fold(1.0, |a, r| a * (1.0 + r / 100.0)) - 1.0) * 100.0
                };
                let oos = stitch(mine.iter().map(|r| r.2).collect());
                let spy = stitch(mine.iter().filter_map(|r| r.3).collect());
                let positive = mine.iter().filter(|r| r.2 > 0.0).count();
                o.push(format!(
                    "- {layout} / {rule}: stitched {oos:+.2}% vs SPY {spy:+.2}% over the same blocks; {positive} of {} blocks positive; {} trades, {} wins",
                    mine.len(),
                    mine.iter().map(|r| r.4).sum::<i64>(),
                    mine.iter().map(|r| r.5).sum::<i64>()
                ));
            }
        }
    }
    o.push(String::new());
    o.push("## Archived local paper account (inactive; no order is sent anywhere)".into());
    match scalar::<String>(c, "SELECT json FROM paper_state WHERE id=1")
        .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
    {
        None => o.push("- not started".into()),
        Some(a) => {
            let first: Option<(i64, Option<f64>)> = c
                .query_row(
                    "SELECT wall_ms, ref_close FROM paper_equity ORDER BY wall_ms LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .ok();
            let last: Option<(i64, f64, f64, i64, Option<f64>)> = c.query_row("SELECT wall_ms, equity, unrealized, positions, ref_close FROM paper_equity ORDER BY wall_ms DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).ok();
            let start_equity = a["start_equity"].as_f64().unwrap_or(0.0);
            o.push(format!(
                "- opened {} with {start_equity:.0} USD; setting {}",
                utc(a["start_ms"].as_i64().unwrap_or(0)),
                if a["params"].is_null() {
                    "none usable yet (flat)"
                } else {
                    "in force"
                }
            ));
            match (first, last) {
                (Some((_, f_ref)), Some((w, eq, unreal, pos, l_ref))) => {
                    let spy = match (f_ref, l_ref) {
                        (Some(a), Some(b)) if a > 0.0 => format!("{:+.2}%", 100.0 * (b / a - 1.0)),
                        _ => "n/a".into(),
                    };
                    o.push(format!("- {}: equity {eq:.2} USD ({:+.2}% since the start, SPY {spy} over the same candles), {pos} open position(s), unrealized {unreal:+.2}", utc(w), 100.0 * (eq / start_equity - 1.0)));
                }
                _ => o.push("- no candle stepped yet".into()),
            }
            let (n, wins, fees): (i64, i64, f64) = c.query_row("SELECT count(*), coalesce(sum(pnl>0),0), coalesce(sum(fees),0) FROM paper_trades", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap_or((0, 0, 0.0));
            o.push(format!(
                "- {n} closed trade(s), {wins} winning, {fees:.2} USD of fees"
            ));
        }
    }
    o.push(String::new());
    o.push("## Archived registry (not used by the momentum bot)".into());
    let reg: Vec<String> = c
        .prepare("SELECT name, status FROM strategies ORDER BY id")
        .and_then(|mut s| {
            s.query_map([], |r| {
                Ok(format!(
                    "{} [{}]",
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?
                ))
            })
            .map(|r| r.filter_map(Result::ok).collect())
        })
        .unwrap_or_default();
    o.extend(reg.iter().map(|r| format!("- {r}")));
    let (kept, discarded): (i64, i64) = (
        scalar(c, "SELECT count(*) FROM experiments WHERE proposer='agent:improve' AND decision='kept'").unwrap_or(0),
        scalar(c, "SELECT count(*) FROM experiments WHERE proposer='agent:improve' AND decision='discarded'").unwrap_or(0),
    );
    o.push(format!(
        "- improvement loop: {kept} kept, {discarded} discarded experiments so far"
    ));
    o.push(String::new());
    o.push("## Conclusion".into());
    o.push("- NO PROVEN EDGE: the active momentum strategy and every ThomasAi candidate remain under forward observation. Historical registry rows do not promote or activate either one.".into());
    o.push(String::new());
    o.push("## Operation".into());
    let cyc: Option<i64> = scalar(c, "SELECT max(cycle_ms) FROM cycle_runs");
    match cyc {
        None => o.push("- no cycle has run yet".into()),
        Some(ms) => {
            let rows: Vec<String> = c
                .prepare("SELECT step, ok, seconds FROM cycle_runs WHERE cycle_ms=? AND step != 'mark' ORDER BY id")
                .and_then(|mut s| {
                    s.query_map([ms], |r| {
                        Ok(format!(
                            "{} {} ({:.0} s)",
                            r.get::<_, String>(0)?,
                            if r.get::<_, i64>(1)? == 1 {
                                "ok"
                            } else {
                                "FAILED"
                            },
                            r.get::<_, f64>(2)?
                        ))
                    })
                    .map(|r| r.filter_map(Result::ok).collect())
                })
                .unwrap_or_default();
            o.push(format!("- last cycle {}: {}", utc(ms), rows.join(", ")));
        }
    }
    o.join("\n") + "\n"
}

#[derive(Debug, PartialEq)]
pub struct Problem {
    pub error: bool,
    pub text: String,
}

/// What is wrong, from the store and the Mac. `uncommitted` is passed in so this stays testable.
pub fn doctor(c: &Connection, now: i64, uncommitted: usize) -> Vec<Problem> {
    let mut p = Vec::new();
    let mut add = |error, text: String| p.push(Problem { error, text });
    match scalar::<i64>(c, "SELECT max(cycle_ms) FROM cycle_runs") {
        None => add(false, "no cycle has run yet".into()),
        Some(ms) => {
            let age_h = (now - ms) as f64 / 3_600_000.0;
            if age_h > 26.0 {
                add(
                    true,
                    format!(
                        "the last cycle was {age_h:.0} hours ago ({}); is the schedule installed?",
                        utc(ms)
                    ),
                );
            }
            let failed: Vec<String> = c
                // `doctor` is itself the first cycle step. Never report its
                // own previous failure as the reason the next doctor fails;
                // that creates a permanent self-referential red state.
                .prepare("SELECT step FROM cycle_runs WHERE cycle_ms=? AND ok=0 AND step != 'doctor'")
                .and_then(|mut s| {
                    s.query_map([ms], |r| r.get(0))
                        .map(|r| r.filter_map(Result::ok).collect())
                })
                .unwrap_or_default();
            if !failed.is_empty() {
                add(
                    true,
                    format!("failed steps in the last cycle: {}", failed.join(", ")),
                );
            }
        }
    }
    if let Some(last) = scalar::<i64>(c, "SELECT max(wall_ms) FROM candles_merged") {
        let age_d = (now - last) as f64 / 86_400_000.0;
        if age_d > 5.0 {
            add(
                true,
                format!("the newest candle is {age_d:.0} days old ({})", utc(last)),
            );
        }
    } else {
        add(true, "there are no merged candles".into());
    }
    let forward_table: i64 = c
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='momentum_forward_runs'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if forward_table == 0 {
        add(
            true,
            "the momentum forward ledger is not initialized".into(),
        );
    } else if scalar::<i64>(c, "SELECT count(*) FROM momentum_forward_runs").unwrap_or(0) == 0 {
        add(false, "no versioned momentum plan is stored yet".into());
    }
    if uncommitted > 0 {
        add(
            false,
            format!(
                "{uncommitted} uncommitted file(s) in the repo (rule: commit every step locally)"
            ),
        );
    }
    p
}

/// A leading `~/` in a path given as a default or on the command line becomes the home folder.
pub fn expand_tilde(p: &Path) -> PathBuf {
    match (p.strip_prefix("~"), std::env::var_os("HOME")) {
        (Ok(rest), Some(h)) => PathBuf::from(h).join(rest),
        _ => p.to_path_buf(),
    }
}

/// When a launchd job fires: every so many seconds after the last run, or at fixed minutes of every hour.
#[derive(Clone, Copy)]
pub enum When<'a> {
    Every(u32),
    Minutes(&'a [u32]),
}

/// A launchd agent that runs `exe <args>` every `interval_s` seconds at the lowest priority from the repo root, logging to `data/<log>.log`.
fn job_plist(
    label: &str,
    args: &[&str],
    exe: &Path,
    repo: &Path,
    when: When<'_>,
    log: &str,
) -> String {
    let schedule = match when {
        When::Every(s) => format!("<key>StartInterval</key><integer>{s}</integer>"),
        When::Minutes(m) => format!(
            "<key>StartCalendarInterval</key><array>{}</array>",
            m.iter()
                .map(|x| format!("<dict><key>Minute</key><integer>{x}</integer></dict>"))
                .collect::<String>()
        ),
    };
    let args: String = std::iter::once(exe.display().to_string())
        .chain(args.iter().map(|a| a.to_string()))
        .map(|a| format!("<string>{}</string>", xml_escape(&a)))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{label}</string>
    <key>ProgramArguments</key><array>{args}</array>
    <key>WorkingDirectory</key><string>{repo}</string>
    {schedule}
    <key>RunAtLoad</key><false/>
    <key>Nice</key><integer>19</integer>
    <key>ProcessType</key><string>Background</string>
    <key>LowPriorityIO</key><true/>
    <key>EnvironmentVariables</key><dict><key>PATH</key><string>/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin</string></dict>
    <key>StandardOutPath</key><string>{repo}/data/{log}.log</string>
    <key>StandardErrorPath</key><string>{repo}/data/{log}.log</string>
</dict>
</plist>
"#,
        label = xml_escape(label),
        repo = xml_escape(&repo.display().to_string()),
        log = xml_escape(log)
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The research cycle: `exe cycle`, one hour after the previous run ends (the interval counts from the end of a run).
pub fn plist(exe: &Path, repo: &Path, interval_s: u32) -> String {
    job_plist(
        LABEL,
        &["cycle"],
        exe,
        repo,
        When::Every(interval_s),
        "cycle",
    )
}

/// The web portal: `exe portal`, started at login and restarted if it stops. It only reads, so it runs at the lowest priority.
pub fn portal_plist(exe: &Path, repo: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{PORTAL_LABEL}</string>
    <key>ProgramArguments</key><array><string>{exe}</string><string>portal</string></array>
    <key>WorkingDirectory</key><string>{repo}</string>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>ThrottleInterval</key><integer>30</integer>
    <key>Nice</key><integer>19</integer>
    <key>ProcessType</key><string>Background</string>
    <key>StandardOutPath</key><string>{repo}/data/portal.log</string>
    <key>StandardErrorPath</key><string>{repo}/data/portal.log</string>
</dict>
</plist>
"#,
        exe = xml_escape(&exe.display().to_string()),
        repo = xml_escape(&repo.display().to_string())
    )
}

fn uid() -> String {
    String::from_utf8_lossy(
        &Command::new("id")
            .arg("-u")
            .output()
            .map(|o| o.stdout)
            .unwrap_or_default(),
    )
    .trim()
    .to_string()
}

fn agent_file(label: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(std::env::var("HOME").context("HOME")?)
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist")))
}

/// Writes the agent file and (re)loads the agent.
fn install_agent(label: &str, text: &str) -> Result<()> {
    let path = agent_file(label)?;
    std::fs::create_dir_all(path.parent().context("no parent")?)?;
    let _ = Command::new("launchctl")
        .args(["bootout", &format!("gui/{}/{label}", uid())])
        .output();
    std::fs::write(&path, text)?;
    let out = Command::new("launchctl")
        .args([
            "bootstrap",
            &format!("gui/{}", uid()),
            &path.to_string_lossy(),
        ])
        .output()?;
    if !out.status.success() {
        bail!(
            "launchctl bootstrap failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

fn remove_agent(label: &str) -> Result<()> {
    let _ = Command::new("launchctl")
        .args(["bootout", &format!("gui/{}/{label}", uid())])
        .output();
    let path = agent_file(label)?;
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

pub fn agent_installed(label: &str) -> bool {
    if cfg!(target_os = "linux") {
        return systemd::installed(label);
    }
    Command::new("launchctl")
        .args(["print", &format!("gui/{}/{label}", uid())])
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn install(exe: &Path, repo: &Path, interval_s: u32) -> Result<()> {
    if cfg!(target_os = "linux") {
        return systemd::install(exe, repo, interval_s);
    }
    install_agent(LABEL, &plist(exe, repo, interval_s))?;
    install_agent(PORTAL_LABEL, &portal_plist(exe, repo))
}

pub fn remove() -> Result<()> {
    if cfg!(target_os = "linux") {
        return systemd::remove();
    }
    remove_agent(LABEL)?;
    remove_agent(PORTAL_LABEL)
}

/// Restarts a job onto the current binary (launchd `kickstart -k`, systemd `restart`); the tool's output, or its error text.
pub fn restart_job(label: &str) -> std::result::Result<String, String> {
    let out = if cfg!(target_os = "linux") {
        Command::new("systemctl")
            .args(["--user", "restart", &format!("{label}.service")])
            .output()
    } else {
        Command::new("launchctl")
            .args(["kickstart", "-k", &format!("gui/{}/{label}", uid())])
            .output()
    };
    match out {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// The pid the service manager currently runs for `label`, if any (launchd `list`, systemd `MainPID`).
pub fn job_pid(label: &str) -> Option<u32> {
    if cfg!(target_os = "linux") {
        let o = Command::new("systemctl")
            .args([
                "--user",
                "show",
                "-p",
                "MainPID",
                "--value",
                &format!("{label}.service"),
            ])
            .output()
            .ok()?;
        return String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse()
            .ok()
            .filter(|&p| p != 0);
    }
    let o = Command::new("launchctl")
        .args(["list", label])
        .output()
        .ok()?;
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .find_map(|l| l.trim().strip_prefix("\"PID\" = "))
        .and_then(|v| v.trim_end_matches(';').trim().parse().ok())
}

/// Linux: the same four jobs as systemd user units in `~/.config/systemd/user` (a `.service`, plus a `.timer` for the timed jobs), at the
/// lowest CPU and IO priority from the repo root, logging to `data/<log>.log`. `loginctl enable-linger` keeps them running with nobody logged in.
pub mod systemd {
    use super::{LABEL, PORTAL_LABEL, When};
    use anyhow::{Context, Result, bail};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// How a unit runs: started by a timer, or kept alive (started at login, restarted 30 s after it stops).
    #[derive(Clone, Copy)]
    pub enum Run<'a> {
        Timer(When<'a>),
        KeepAlive,
    }

    /// The retained ThomasAI brain cycle and read-only portal jobs.
    pub fn jobs(
        interval_s: u32,
    ) -> [(
        &'static str,
        &'static [&'static str],
        Run<'static>,
        &'static str,
    ); 2] {
        [
            (
                LABEL,
                &["cycle"],
                Run::Timer(When::Every(interval_s)),
                "cycle",
            ),
            (PORTAL_LABEL, &["portal"], Run::KeepAlive, "portal"),
        ]
    }

    /// A value systemd reads literally: `%` doubled (specifiers), the whole quoted with `\` and `"` escaped.
    fn quote(s: &str) -> String {
        format!(
            "\"{}\"",
            s.replace('%', "%%")
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        )
    }

    pub fn service(
        label: &str,
        args: &[&str],
        exe: &Path,
        repo: &Path,
        run: Run<'_>,
        log: &str,
    ) -> String {
        let exec: Vec<String> = std::iter::once(exe.display().to_string())
            .chain(args.iter().map(|a| a.to_string()))
            .map(|a| quote(&a))
            .collect();
        let repo = repo.display().to_string().replace('%', "%%");
        let (kind, keep) = match run {
            Run::Timer(_) => ("oneshot", ""),
            Run::KeepAlive => ("simple", "Restart=always\nRestartSec=30\n"),
        };
        let install = match run {
            Run::Timer(_) => "",
            Run::KeepAlive => "\n[Install]\nWantedBy=default.target\n",
        };
        format!(
            "[Unit]\nDescription=TradingSystem {label}\n\n[Service]\nType={kind}\nWorkingDirectory={repo}\nExecStart={exec}\n{keep}Nice=19\n\
             IOSchedulingClass=idle\nEnvironment=PATH=%h/.local/bin:/usr/local/bin:/usr/bin:/bin\n\
             StandardOutput=append:{repo}/data/{log}.log\nStandardError=append:{repo}/data/{log}.log\n{install}",
            exec = exec.join(" ")
        )
    }

    /// The timer of a timed job: `Every(s)` first fires `s` seconds after it is enabled, then `s` seconds after each run ends (as the
    /// launchd job, the interval counts from the end of a run); `Minutes(m)` fires at those minutes of every hour.
    pub fn timer(label: &str, when: When<'_>) -> String {
        let schedule = match when {
            When::Every(s) => format!("OnActiveSec={s}s\nOnUnitInactiveSec={s}s"),
            When::Minutes(m) => format!(
                "OnCalendar=*-*-* *:{}:00",
                m.iter()
                    .map(|x| format!("{x:02}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        };
        format!(
            "[Unit]\nDescription=TradingSystem {label} timer\n\n[Timer]\n{schedule}\nAccuracySec=1s\n\n[Install]\nWantedBy=timers.target\n"
        )
    }

    fn unit_dir() -> Result<PathBuf> {
        Ok(PathBuf::from(std::env::var("HOME").context("HOME")?).join(".config/systemd/user"))
    }

    fn systemctl(args: &[&str]) -> Result<()> {
        let out = Command::new("systemctl")
            .arg("--user")
            .args(args)
            .output()?;
        if !out.status.success() {
            bail!(
                "systemctl --user {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(())
    }

    /// The unit that starts the job: its timer, or the kept-alive service itself.
    fn start_unit(label: &str, run: Run<'_>) -> String {
        match run {
            Run::Timer(_) => format!("{label}.timer"),
            Run::KeepAlive => format!("{label}.service"),
        }
    }

    /// Writes every unit, reloads systemd and (re)starts each job; a kept-alive job is restarted so it runs the current binary.
    pub fn install(exe: &Path, repo: &Path, interval_s: u32) -> Result<()> {
        let dir = unit_dir()?;
        std::fs::create_dir_all(&dir)?;
        std::fs::create_dir_all(repo.join("data"))?;
        for (label, args, run, log) in jobs(interval_s) {
            std::fs::write(
                dir.join(format!("{label}.service")),
                service(label, args, exe, repo, run, log),
            )?;
            if let Run::Timer(when) = run {
                std::fs::write(dir.join(format!("{label}.timer")), timer(label, when))?;
            }
        }
        systemctl(&["daemon-reload"])?;
        for (label, _, run, _) in jobs(interval_s) {
            let unit = start_unit(label, run);
            systemctl(&["enable", "--now", &unit])?;
            if let Run::KeepAlive = run {
                systemctl(&["restart", &unit])?;
            }
        }
        Ok(())
    }

    pub fn remove() -> Result<()> {
        let dir = unit_dir()?;
        for (label, _, _, _) in jobs(0) {
            for ext in ["timer", "service"] {
                let _ = systemctl(&["disable", "--now", &format!("{label}.{ext}")]);
            }
            for ext in ["service", "timer"] {
                let path = dir.join(format!("{label}.{ext}"));
                if path.exists() {
                    std::fs::remove_file(path)?;
                }
            }
        }
        systemctl(&["daemon-reload"])
    }

    /// Whether the job's timer or kept-alive service is enabled.
    pub fn installed(label: &str) -> bool {
        ["timer", "service"].iter().any(|ext| {
            Command::new("systemctl")
                .args(["--user", "is-enabled", "--quiet", &format!("{label}.{ext}")])
                .status()
                .is_ok_and(|s| s.success())
        })
    }
}

pub fn installed() -> bool {
    agent_installed(LABEL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_systemd_jobs_match_the_launchd_jobs() {
        let jobs = systemd::jobs(3600);
        let labels: Vec<&str> = jobs.iter().map(|j| j.0).collect();
        assert_eq!(labels, [LABEL, PAPER_LABEL, PORTAL_LABEL, WATCHDOG_LABEL]);
        let (exe, repo) = (Path::new("/r/target/release/thomas"), Path::new("/r"));
        for (label, args, run, log) in jobs {
            let s = systemd::service(label, args, exe, repo, run, log);
            assert!(
                s.contains(&format!(
                    "ExecStart=\"/r/target/release/thomas\" \"{}\"",
                    args[0]
                )),
                "{s}"
            );
            assert!(
                s.contains("WorkingDirectory=/r\n")
                    && s.contains("Nice=19\n")
                    && s.contains("IOSchedulingClass=idle\n"),
                "{s}"
            );
            assert!(
                s.contains(&format!("StandardOutput=append:/r/data/{log}.log\n")),
                "{s}"
            );
        }
    }

    #[test]
    fn the_systemd_cycle_waits_its_interval_after_each_run() {
        let t = systemd::timer(LABEL, When::Every(3600));
        assert!(
            t.contains("OnActiveSec=3600s\nOnUnitInactiveSec=3600s\n"),
            "{t}"
        );
        let s = systemd::service(
            LABEL,
            &["cycle"],
            Path::new("/r/thomas"),
            Path::new("/r"),
            systemd::Run::Timer(When::Every(3600)),
            "cycle",
        );
        assert!(
            s.contains("Type=oneshot\n") && !s.contains("Restart=") && !s.contains("[Install]"),
            "{s}"
        );
    }

    #[test]
    fn the_systemd_paper_trader_runs_at_its_minutes_and_the_portal_stays_alive() {
        let t = systemd::timer(PAPER_LABEL, When::Minutes(&PAPER_MINUTES));
        assert!(t.contains("OnCalendar=*-*-* *:04,19,34,49:00\n"), "{t}");
        let s = systemd::service(
            PORTAL_LABEL,
            &["portal"],
            Path::new("/r/thomas"),
            Path::new("/r"),
            systemd::Run::KeepAlive,
            "portal",
        );
        assert!(
            s.contains("Type=simple\nWorkingDirectory=")
                && s.contains("Restart=always\nRestartSec=30\n"),
            "{s}"
        );
        assert!(s.contains("[Install]\nWantedBy=default.target\n"), "{s}");
    }

    #[test]
    fn systemd_values_are_quoted_and_percent_escaped() {
        let s = systemd::service(
            "x",
            &["a b"],
            Path::new("/my dir/100%/thomas"),
            Path::new("/r"),
            systemd::Run::KeepAlive,
            "x",
        );
        assert!(
            s.contains("ExecStart=\"/my dir/100%%/thomas\" \"a b\"\n"),
            "{s}"
        );
    }

    fn store() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        thomas_core::store::init(&c).unwrap();
        c
    }

    #[test]
    fn utc_formats_known_instants() {
        assert_eq!(utc(0), "1970-01-01 00:00");
        assert_eq!(utc(1_792_323_480_000), "2026-10-18 11:38");
        assert_eq!(utc(951_782_400_000), "2000-02-29 00:00");
    }

    fn names(p: &[Step]) -> Vec<&'static str> {
        p.iter().map(|s| s.name).collect()
    }

    #[test]
    fn the_cycle_checks_live_health_then_ingests_current_gainer_evidence() {
        let c = store();
        c.execute(
            "INSERT INTO candles_merged VALUES('A','A',1000,1,1,1,1,1,1,'yahoo',0,1)",
            [],
        )
        .unwrap();
        let p = plan_with(&c, 10_000_000_000, true, 2, true);
        assert_eq!(names(&p), ["doctor", "neural-step"]);
        assert!(
            p.iter()
                .filter(|s| s.fatal)
                .map(|s| s.name)
                .eq(["doctor"])
        );
        assert_eq!(
            names(&plan_with(&c, 10_000_000_000, false, 0, true)),
            ["doctor", "neural-step"]
        );
    }

    #[test]
    fn the_cycle_never_runs_the_retired_fifteen_minute_pipeline() {
        let c = store();
        let now = 10_000_000_000_i64;
        c.execute(
            "INSERT INTO candles_merged VALUES('A','A',1000,1,1,1,1,1,1,'yahoo',0,1)",
            [],
        )
        .unwrap();
        let names = names(&plan_with(&c, now, true, 2, true));
        assert_eq!(names, ["doctor", "neural-step"]);
        for retired in ["fetch-sessions", "fetch-extra", "import-bars", "merge", "verify", "momentum-forward", "deliberate", "momentum-select"] {
            assert!(!names.contains(&retired), "retired step {retired} returned");
        }
    }

    #[test]
    fn a_step_that_runs_too_long_is_stopped_and_reported_as_failed_and_a_quick_one_is_not() {
        let mut slow = Command::new("sleep");
        slow.arg("30");
        let (ok, secs, tail) = run_command_limited(slow, Some(1));
        assert!(
            !ok && secs < 10.0 && tail.contains("stopped after 1 s"),
            "{ok} {secs} {tail}"
        );
        let mut quick = Command::new("echo");
        quick.arg("hello");
        let (ok, _, tail) = run_command_limited(quick, Some(5));
        assert!(ok && tail.contains("hello"));
        assert!(
            plan_with(&store(), 0, true, 1, false)
                .iter()
                .all(|s| s.limit_secs == Some(STEP_LIMIT_SECS)),
            "every cycle step has the general limit"
        );
    }

    #[test]
    fn every_step_is_logged_and_a_fatal_failure_stops_the_cycle() {
        let c = store();
        let plan = vec![
            Step {
                name: "one",
                args: vec!["--version".into()],
                fatal: true,
                limit_secs: None,
            },
            Step {
                name: "two",
                args: vec![],
                fatal: false,
                limit_secs: None,
            },
        ];
        // `true` / `false` stand in for the binary
        let ok = cycle(&c, Path::new("/usr/bin/true"), &plan).unwrap();
        assert!(ok.is_empty());
        let n: i64 = c
            .query_row("SELECT count(*) FROM cycle_runs WHERE ok=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 2);
        let bad = cycle(&c, Path::new("/usr/bin/false"), &plan).unwrap();
        assert_eq!(
            bad,
            ["one"],
            "the fatal step failed, so the second step never ran"
        );
        let n: i64 = c
            .query_row("SELECT count(*) FROM cycle_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn a_lock_refuses_a_live_owner_and_replaces_a_stale_one() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("cycle.lock");
        let l = Lock::take(&path).unwrap();
        assert!(
            Lock::take(&path)
                .unwrap_err()
                .to_string()
                .contains("another cycle is running")
        );
        drop(l);
        assert!(!path.exists(), "released on drop");
        std::fs::write(&path, "999999").unwrap(); // no such process
        let _l = Lock::take(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            std::process::id().to_string()
        );
    }

    #[test]
    fn the_report_states_what_is_in_the_store_and_says_no_edge_without_a_champion() {
        let c = store();
        c.execute("INSERT INTO strategies(name,version,status,created_ms) VALUES('xs-setting abc',1,'challenger',0)", []).unwrap();
        c.execute("INSERT INTO experiments(ts_ms,proposer,description,decision,reason) VALUES(0,'agent:improve','hold=long @3p','discarded','x')", []).unwrap();
        c.execute("INSERT INTO cycle_runs(cycle_ms,step,ok,seconds,detail) VALUES(1792323480000,'search',1,12.0,'VERDICT: UNPROVEN. no forward test\nFORWARD (out-of-sample): none yet')", []).unwrap();
        c.execute(
            "INSERT INTO learner_periods VALUES(1,10,10,-0.02,-1.5,-0.3,'m',0.01,0.0,0.01,0,0,'gbdt')",
            [],
        )
        .unwrap();
        let r = report(&c);
        assert!(
            r.contains("xs-setting abc [challenger]") && r.contains("0 kept, 1 discarded"),
            "{r}"
        );
        assert!(
            r.contains("VERDICT: UNPROVEN") && r.contains("FORWARD (out-of-sample): none yet"),
            "{r}"
        );
        assert!(
            r.contains("0 of 1 periods show a signal") && r.contains("NO PROVEN EDGE"),
            "{r}"
        );
        assert!(
            r.contains("last cycle 2026-10-18 11:38: search ok (12 s)"),
            "{r}"
        );
        c.execute("UPDATE strategies SET status='champion'", [])
            .unwrap();
        assert!(report(&c).contains("Historical registry rows do not promote"));
    }

    #[test]
    fn the_doctor_flags_stale_cycles_failed_steps_old_candles_and_uncommitted_work() {
        let c = store();
        let now = 1_792_323_480_000_i64;
        let t = |p: &[Problem]| {
            p.iter()
                .map(|x| x.text.clone())
                .collect::<Vec<_>>()
                .join(" | ")
        };
        let p = doctor(&c, now, 0);
        assert!(
            t(&p).contains("no cycle has run yet")
                && t(&p).contains("no merged candles")
                && t(&p).contains("momentum forward ledger is not initialized")
        );
        c.execute("INSERT INTO cycle_runs(cycle_ms,step,ok,seconds,detail) VALUES(?,'search',0,1.0,'boom')", [now - 30 * 3_600_000]).unwrap();
        c.execute(
            "INSERT INTO candles_merged VALUES('A','A',?,1,1,1,1,1,1,'yahoo',0,1)",
            [now - 9 * 86_400_000],
        )
        .unwrap();
        c.execute_batch("CREATE TABLE momentum_forward_runs(session TEXT PRIMARY KEY);")
            .unwrap();
        let p = doctor(&c, now, 2);
        let text = t(&p);
        assert!(
            text.contains("30 hours ago")
                && text.contains("failed steps in the last cycle: search")
                && text.contains("9 days old"),
            "{text}"
        );
        assert!(text.contains("2 uncommitted"), "{text}");
        assert!(p.iter().filter(|x| x.error).count() >= 3 && p.iter().any(|x| !x.error));
        // a healthy store has nothing to report
        c.execute(
            "INSERT INTO cycle_runs(cycle_ms,step,ok,seconds,detail) VALUES(?,'verify',1,1.0,'')",
            [now - 3_600_000],
        )
        .unwrap();
        c.execute("UPDATE candles_merged SET wall_ms=?", [now - 86_400_000])
            .unwrap();
        c.execute("INSERT INTO momentum_forward_runs VALUES('2026-10-23')", [])
            .unwrap();
        assert!(doctor(&c, now, 0).is_empty());
    }

    #[test]
    fn the_launchd_plist_runs_the_cycle_hourly_at_the_lowest_priority_from_the_repo() {
        let x = plist(Path::new("/r/target/release/thomas"), Path::new("/r"), 3600);
        for want in [
            "<string>com.thomasai.cycle</string>",
            "<string>/r/target/release/thomas</string><string>cycle</string>",
            "<key>WorkingDirectory</key><string>/r</string>",
            "<integer>3600</integer>",
            "<key>Nice</key><integer>19</integer>",
            "Background",
            "/r/data/cycle.log",
        ] {
            assert!(x.contains(want), "missing {want}");
        }
        assert_eq!(x.matches("<dict>").count(), x.matches("</dict>").count());
    }

    #[test]
    fn the_report_shows_the_latest_walk_forward_stitched_against_spy() {
        let c = store();
        let ins = |run: i64, fold: i64, ret: f64, spy: f64, trades: i64, wins: i64| {
            c.execute("INSERT INTO walkforward_runs(run_ms,layout,rule,fold,train_from_ms,train_to_ms,test_from_ms,test_to_ms,return_pct,max_drawdown_pct,trades,wins,ref_return_pct,params_json,min_trades,samples,seed) VALUES(?,'5h live 14x3','robust',?,0,0,0,0,?,0,?,?,?,NULL,2,1,1)", rusqlite::params![run, fold, ret, trades, wins, spy]).unwrap();
        };
        ins(1, 0, 50.0, 0.0, 9, 9); // an older run is not shown
        ins(2, 0, 10.0, -1.0, 1, 1);
        ins(2, 1, -10.0, -1.0, 1, 0);
        let r = report(&c);
        assert!(
            r.contains("5h live 14x3 / robust: stitched -1.00% vs SPY -1.99%"),
            "{r}"
        );
        assert!(
            r.contains("1 of 2 blocks positive; 2 trades, 1 wins"),
            "{r}"
        );
        assert!(!r.contains("+50"), "{r}");
        assert!(report(&store()).contains("not run yet"));
    }

    #[test]
    fn weekends_are_recognised_and_a_same_day_bar_does_not_rerun_momentum() {
        let day = |y, m, d| crate::select::week_start_for_tests(y, m, d);
        assert!(
            !is_weekend(day(2026, 10, 19)) && !is_weekend(day(2026, 10, 23)),
            "Monday and Friday"
        );
        assert!(
            is_weekend(day(2026, 10, 24)) && is_weekend(day(2026, 10, 25)),
            "Saturday and Sunday"
        );
        let c = store();
        c.execute(
            "INSERT INTO candles_merged VALUES('A','A',1000,1,1,1,1,1,1,'yahoo',0,1)",
            [],
        )
        .unwrap();
        mark_selection(&c, 1).unwrap();
        c.execute(
            "INSERT INTO candles_merged VALUES('A','A',5000,1,1,1,1,1,1,'yahoo',0,1)",
            [],
        )
        .unwrap();
        assert!(
            !names(&plan_with(&c, 10_000_000_000, false, 0, true)).contains(&"momentum-select"),
            "a newer bar on the same day"
        );
    }

    #[test]
    fn the_report_shows_the_local_paper_account_against_spy() {
        let c = store();
        assert!(report(&c).contains("not started"));
        let acc = serde_json::json!({"start_equity": 5000.0, "start_ms": 1792323480000_i64, "params": null});
        c.execute("INSERT INTO paper_state VALUES(1, ?, 0)", [acc.to_string()])
            .unwrap();
        c.execute(
            "INSERT INTO paper_equity VALUES(1000, 5000.0, 0.0, 0, 100.0)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO paper_equity VALUES(2000, 5100.0, 10.0, 1, 98.0)",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO paper_trades(symbol, side, entry_ms, exit_ms, qty, entry_price, exit_price, pnl, fees, reason) VALUES('X','Long',1,2,1,1,2,90.0,1.5,'r')", []).unwrap();
        let r = report(&c);
        assert!(r.contains("equity 5100.00 USD (+2.00% since the start, SPY -2.00% over the same candles), 1 open position(s)"), "{r}");
        assert!(
            r.contains("1 closed trade(s), 1 winning, 1.50 USD of fees")
                && r.contains("none usable yet (flat)"),
            "{r}"
        );
    }

    #[test]
    fn the_trading_window_covers_the_us_session_on_weekdays_only() {
        let at = |d, h: i64, m: i64| {
            crate::select::week_start_for_tests(2026, 10, d) + (h * 60 + m) * 60_000
        };
        assert!(
            in_trading_window(at(19, 13, 30))
                && in_trading_window(at(19, 20, 0))
                && in_trading_window(at(23, 21, 0))
        );
        assert!(
            !in_trading_window(at(19, 13, 0))
                && !in_trading_window(at(19, 21, 30))
                && !in_trading_window(at(19, 3, 0))
        );
        assert!(
            !in_trading_window(at(24, 15, 0)) && !in_trading_window(at(25, 15, 0)),
            "Saturday and Sunday"
        );
    }

    #[test]
    fn the_scheduled_plan_is_live_gainer_only() {
        let c = store();
        const {
            assert!(
                !thomas_core::config::YAHOO_DOWNLOADS,
                "the owner switched Yahoo downloads off on 2026-10-19"
            )
        };
        let p = plan(&c, 10_000_000_000, true, 2);
        let n = names(&p);
        assert_eq!(n, ["doctor", "neural-step"]);
    }

    #[test]
    fn a_leading_tilde_becomes_the_home_folder_and_other_paths_stay() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(expand_tilde(Path::new("~/a/b")), home.join("a/b"));
        assert_eq!(expand_tilde(Path::new("data/x")), PathBuf::from("data/x"));
        assert_eq!(
            expand_tilde(Path::new("/abs/~/x")),
            PathBuf::from("/abs/~/x")
        );
    }

    #[test]
    fn the_watchdog_agent_runs_every_ten_minutes_from_the_repo() {
        let x = watchdog_plist(Path::new("/r/target/release/thomas"), Path::new("/r"));
        for want in [
            "<string>com.thomasai.watchdog</string>",
            "<string>/r/target/release/thomas</string><string>watchdog</string>",
            "<key>StartInterval</key><integer>600</integer>",
            "/r/data/watchdog.log",
        ] {
            assert!(x.contains(want), "missing {want}");
        }
    }

    #[test]
    fn the_portal_agent_starts_at_login_restarts_and_runs_the_portal_command() {
        let x = portal_plist(Path::new("/r/target/release/thomas"), Path::new("/r"));
        for needle in [
            "<string>com.thomasai.portal</string>",
            "<string>/r/target/release/thomas</string><string>portal</string>",
            "<key>KeepAlive</key><true/>",
            "<key>RunAtLoad</key><true/>",
            "<integer>19</integer>",
        ] {
            assert!(x.contains(needle), "{needle}");
        }
    }

    #[test]
    fn the_paper_agent_runs_a_light_job_of_its_own_three_minutes_after_each_candle_feed() {
        let x = paper_plist(Path::new("/r/target/release/thomas"), Path::new("/r"));
        for want in [
            "<string>com.thomasai.paper</string>",
            "<string>/r/target/release/thomas</string><string>paper-run</string>",
            "<key>StartCalendarInterval</key>",
            "<dict><key>Minute</key><integer>4</integer></dict>",
            "<dict><key>Minute</key><integer>49</integer></dict>",
            "<key>Nice</key><integer>19</integer>",
            "/r/data/paper.log",
        ] {
            assert!(x.contains(want), "missing {want}");
        }
        assert!(
            !x.contains("StartInterval"),
            "a fixed minute of the hour, not an interval from the last run"
        );
        let c = plist(Path::new("/r/target/release/thomas"), Path::new("/r"), 3600);
        assert!(
            c.contains("<key>StartInterval</key><integer>3600</integer>")
                && !c.contains("StartCalendarInterval")
        );
        assert!(!x.contains("<string>cycle</string>"));
        assert_eq!(x.matches("<dict>").count(), x.matches("</dict>").count());
    }
}
