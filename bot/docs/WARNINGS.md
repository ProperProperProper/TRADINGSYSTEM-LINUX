# Warnings and errors from the logs

Updated 2026-10-18 09:59 +11:00 by `qwen-tools warnings` (code). Every log of the bot and the lab is scanned hourly; one entry per distinct problem. **Fixed** = a real commit (checked) fixed it; if it is seen again after the fix it is **reopened** automatically. **Expected** = a known harmless condition, with the reason. The hourly Claude Code log doctor (`ops/log_doctor.sh`) works on the open entries. Account-like numbers and secrets are redacted.

**0 reopened · 29 open · 1 fixed · 2 expected**

## Open

| # | severity | seen | first / last | log | message | resolution |
|---|---|---|---|---|---|---|
| 17 | error | 20x | 2026-10-18 00:55 / 2026-10-18 09:59 | `bot console (http://127.0.0.1:8789)` | bot console did not answer http://127.0.0.1:8789/api/status: <urlopen error curl exit 7> |  |
| 10 | error | 626x | before the ledger (time unknown) / 2026-10-18 09:59 | `spy-etf-universe-holdings-trading-bot/ops/logs/lab_export.log` | Error: OpenD is not reachable at 127.0.0.1:11111 (is moomoo OpenD running and logged in?) |  |
| 8 | error | 16x | 2026-10-17 14:55 / 2026-10-18 07:55 | `.local/share/ClaudeMoomooBot/logs/log_doctor.log` | 2026-10-17T20:55:05Z repair session exit 1 -> session-20261017T2055.md |  |
| 30 | error | 1x | 2026-10-18 07:44 / 2026-10-18 07:44 | `.local/share/ClaudeMoomooBot/logs/fix_report.log` | 2026-10-17T20:44:05Z claude exit 1 |  |
| 24 | error | 15x | 2026-10-17 13:35 / 2026-10-18 07:10 | `.local/share/qwen-lab-stocks/logs/supervisor.log` | 2026-10-17T20:10:07Z session exit 1 -> session-20261017T2010.md |  |
| 4 | error | 28x | 2026-10-17 14:55 / 2026-10-18 05:55 | `Projects/qwen-lab-stocks/logs/improve.log` | log not written for ever (expected at least every 60 min): its job may have stopped |  |
| 5 | error | 28x | 2026-10-17 14:55 / 2026-10-18 05:55 | `Projects/qwen-lab-stocks/logs/brain.log` | log not written for ever (expected at least every 30 min): its job may have stopped |  |
| 7 | error | 28x | 2026-10-17 14:55 / 2026-10-18 05:55 | `Projects/qwen-lab-stocks/logs/monitor.log` | log not written for ever (expected at least every 60 min): its job may have stopped |  |
| 26 | error | 14x | 2026-10-17 14:33 / 2026-10-18 05:10 | `.local/share/qwen-lab-stocks/logs/supervisor.log` | 2026-10-17T18:10:11Z session exit 1 -> codex-20261017T1810.md |  |
| 27 | error | 7x | 2026-10-17 14:33 / 2026-10-18 05:10 | `.local/share/qwen-lab-stocks/logs/supervisor.log` | 2026-10-17T18:10:11Z FAULT: supervisor session exited 1 |  |
| 16 | error | 2x | 2026-10-17 20:55 / 2026-10-17 20:55 | `lab dashboard (http://127.0.0.1:8790)` | lab dashboard shows at .inputs.ai.roles.line: 2026-10-17 09:26 UTC strategist ok in 0s: Qwen unavailable; deterministic strategist continued: http://127.0.0.1:8080/v1/chat/completions: Connection Failed: Connect error: C |  |
| 6 | error | 4x | 2026-10-17 14:55 / 2026-10-17 15:55 | `Projects/moomoo-bot/ops/logs/monitor.log` | log not written for ever (expected at least every 60 min): its job may have stopped |  |
| 28 | error | 1x | 2026-10-17 14:38 / 2026-10-17 14:38 | `.local/share/qwen-lab-stocks/logs/supervisor.log` | 2026-10-17T03:38:29Z recovered agent exit 1: verified changes committed and pushed |  |
| 25 | error | 1x | 2026-10-17 13:57 / 2026-10-17 13:57 | `.local/share/qwen-lab-stocks/logs/supervisor.log` | 2026-10-17T02:57:58Z FAULT: Claude session exited 1 |  |
| 13 | error | 1x | 2026-10-14 23:58 / 2026-10-14 23:58 | `spy-etf-universe-holdings-trading-bot/ops/logs/push_guard.log` | 2026-10-14T12:58:17Z spy-etf-universe-holdings-trading-bot: PUSH FAILED (2 commit(s) not on GitHub); retried by the next monitor run |  |
| 12 | error | 1x | 2026-10-14 23:57 / 2026-10-14 23:57 | `spy-etf-universe-holdings-trading-bot/ops/logs/monitor.log` | 2026-10-14T12:57:50Z 2026-10-14T12:57Z improve session finished (exit 1): /home/local/spy-etf-universe-holdings-trading-bot/ops/logs/improve-2026-10-14T1257Z.md |  |
| 18 | error | 9x | before the ledger (time unknown) / before the ledger (time unknown) | `.local/share/qwen-lab-stocks/logs/brain.err.log` | Error: run `qwen-lab init` first |  |
| 23 | error | 1x | before the ledger (time unknown) / before the ledger (time unknown) | `.local/share/qwen-lab-stocks/logs/improve.log` | announce scan failed: no such table: training_runs: Error code 1: SQL error or missing database |  |
| 11 | warning | 626x | before the ledger (time unknown) / 2026-10-18 09:59 | `spy-etf-universe-holdings-trading-bot/ops/logs/lab_export.log` | Connection refused (os error 111) |  |
| 14 | warning | 11x | 2026-10-16 22:04 / 2026-10-18 09:53 | `spy-etf-universe-holdings-trading-bot/ops/logs/universe_fetch.log` | 2026-10-17T22:53:36Z universe import failed; will retry on the next scheduled run |  |
| 32 | warning | 1x | 2026-10-18 08:30 / 2026-10-18 08:30 | `spy-etf-universe-holdings-trading-bot/ops/logs/spy_holdings.log` | 2026-10-17T21:30:28Z moomoo check failed; keeping the current file |  |
| 31 | warning | 1x | 2026-10-18 07:44 / 2026-10-18 07:44 | `.local/share/ClaudeMoomooBot/logs/fix_report.log` | 2026-10-17T20:44:05Z write-up refused or missing: no file |  |
| 22 | warning | 3x | 2026-10-17 09:08 / 2026-10-17 10:26 | `.local/share/qwen-lab-stocks/logs/brain.log` | 2026-10-17 10:26 UTC strategist ok in 0s: Qwen unavailable; deterministic strategist continued: http://127.0.0.1:8080/v1/chat/completions: Connection Failed: Connect error: Connection refused (os error 111): Connection r |  |
| 21 | warning | 1x | 2026-10-17 08:49 / 2026-10-17 08:49 | `.local/share/qwen-lab-stocks/logs/brain.log` | 2026-10-17 08:49 UTC strategist ok in 9s: Qwen proposal cmf+rollstop was safely refused: you already have 4 open requests: wait for their results (lab_experiments) before asking for more \| waiting: 4 open requests alrea |  |
| 20 | warning | 15x | 2026-10-16 12:22 / 2026-10-17 02:34 | `.local/share/qwen-lab-stocks/logs/brain.log` | 2026-10-17 02:34 UTC tuner FAILED in 0s: FAILED: too few runs for tuning facts |  |
| 2 | warning | 1x | 2026-10-14 23:57 / 2026-10-14 23:57 | `.local/share/ClaudeMoomooBot/logs/fix_report.log` | 2026-10-14T12:57:39Z REFUSED to publish: the report looks like it contains a secret |  |
| 3 | warning | 1x | 2026-10-14 23:57 / 2026-10-14 23:57 | `.local/share/ClaudeMoomooBot/logs/log_doctor.log` | qwen-tools is not installed (qwen-ops install-tools in the lab repo)2026-10-14T12:57:40Z scan failed |  |
| 9 | warning | 1x | before the ledger (time unknown) / before the ledger (time unknown) | `spy-etf-universe-holdings-trading-bot/ops/logs/lab_export.log` | BRK.B: FAILED OpenD closed the connection: early eof |  |
| 19 | warning | 9x | before the ledger (time unknown) / before the ledger (time unknown) | `.local/share/qwen-lab-stocks/logs/brain.err.log` | 1: No such file or directory (os error 2) |  |

## Fixed

| # | severity | seen | first / last | log | message | resolution |
|---|---|---|---|---|---|---|
| 29 | error | 32x | 2026-10-17 13:45 / 2026-10-18 07:33 | `.local/share/qwen-lab-stocks/logs/watchdog.log` | 2026-10-18 07:33:07 fix session finished (exit 1): logs/watchdog-fix-[number]-0733.md | `86bcaff` Watchdog started a Claude fixer every 30 min during an OpenD outage no session can fix; now alerts once per 6 h (lab/src/ops.rs). Takes effect once the lab is redeployed. (marked 2026-10-18 08:59) |

## Expected (harmless, with reason)

| # | severity | seen | first / last | log | message | resolution |
|---|---|---|---|---|---|---|
| 15 | warning | 12x | 2026-10-17 16:55 / 2026-10-17 21:55 | `~/.local/share/ClaudeMoomooBot/bin/bot` | the installed bot binary (2026-10-17 14:23) is older than the newest bot code commit (2026-10-17 15:21): the running program and the dashboards do not have the latest code (the bot redeploys only while the US market is c | Redeployed at market-closed: the built program is byte-identical to the installed one (deploy reports 'bot program unchanged'); the post-14:23 commits did not change the binary. (marked 2026-10-18 09:59) |
| 1 | warning | 6919x | 2026-10-16 22:38 / 2026-10-17 14:24 | `.local/share/ClaudeMoomooBot/logs/bot.err` | [2026-10-17T03:24:21Z WARN  bot::service] EXCLUDED ZTS: candles unavailable: historical-candle quota: 96 used, 4 left (reserve 6); ZTS not pulled in the last 7 days | Known harmless: moomoo's 100-stock candle quota leaves symbols out; bot logs EXCLUDED and keeps running. (marked 2026-10-18 09:59) |

