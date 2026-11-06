# Setup: build the whole system from scratch

How to rebuild the research lab on Linux, macOS or Windows. Everything is Rust and runs on the CPU; the only other AI is
the **Claude Code CLI** (the supervisors, on a Claude subscription; no API keys anywhere). No language model, Ollama, Hermes, Honcho, Postgres or Python is
needed (all retired and uninstalled, 2026-10-13; the git history has their old setup). Nothing leaves the Mac except paper searches, git pushes, Discord posts and
the Claude CLI's own traffic. The installers are idempotent: running them again repairs the setup and never wipes the lab's memory. Once everything works,
versions are pinned and never upgraded (`ops/versions.lock`).

## 0. Prerequisites
```sh
# openSUSE / Linux
sudo zypper install rust gcc make protobuf-compiler pkg-config
```
A firewall such as LuLu asks once per new program (the lab binaries, git, curl, claude): allow them. The binaries are signed with fixed identifiers
(`com.qwenlab.lab`, `.tools`, `.ops`, `.notify`, `.guard`) so the rules keep matching after rebuilds. The bot side (moomoo OpenD, the bot repo) is in the bot repo's README.

## 1. The lab
```sh
cd /home/local/spy-etf-universe-holdings-trading-bot/lab
cargo build --release && cargo test && cargo clippy --all-targets -- -D warnings
target/release/qwen-lab --config ./lab.toml init                                      # first time only
target/release/qwen-lab --config ./lab.toml schedule install                          # tick, improve, monitor, supervisor
target/release/qwen-lab --config ./lab.toml brain install                             # the always-on brain
target/release/qwen-lab --config ./lab.toml dash install                              # AFTER schedule install
bash ops/install_tools.sh                                                               # qwen-tools, qwen-ops, qwen-notify, qwen-guard
bash ops/resource_safety/install.sh                                                     # resource watch, watchdog, git hooks
```
Type them literally. Config `lab.toml` contains candle sizes, 5,000 USD and 4 variants per round.
The lab's state (`memory/` = persistent learning, never wiped; `data/` = imports and the candle store `candles.sqlite3`; logs)
lives in the configured lab data directory. A rebuilt binary is reinstalled and reloaded by the platform service manager; `ops/lab_monitor.sh` and `qwen-ops` do it.

## 2. The candle store and the ML model
```sh
qwen-lab --config ... candles import      # bot's candles + SPY holdings + measured fees/spreads -> data/candles.sqlite3
qwen-lab --config ... candles scores      # the model's scores -> ~/Projects/moomoo-bot/target/oos-cache/ml_scores.json (refreshed by the steward)
qwen-lab --config ... candles ml          # the fee-inclusive walk-forward verdict;  candles improve  = the daily improvement round
```
The bot's backtester reads the score file for `Signal::Ml` and the `mlgate` entry gate (research only; bot repo `examples/variant_study.rs`).

## 3. Notifications (Discord)
`docs/NOTIFICATIONS.md`: the webhook URL is stored outside every repo (`~/.config/qwen-lab/discord_webhook`, mode 600) and `qwen-guard` blocks it from git.

## 4. The Claude Code supervisors (mindful of session usage: each runs only when there is work)

| Job | Script | When | Does |
|---|---|---|---|
| `com.qwenlab.supervisor` | `supervisor/quant_supervisor.sh` | every 6 h | reviews the brain and the ML logs, automation health, quant analysis; builds improvements in code; commits and pushes |
| `com.claudemoomoobot.logdoctor` | bot `ops/log_doctor.sh` | :55, only with open warnings | every log and both dashboards into the warnings ledger; fixes root causes |
| `com.claudemoomoobot.fixreport` | bot `ops/fix_report.sh` | daily 00:50 | the past day's fixes: problem / solution / action taken |
| `com.claudemoomoobot.promote` | bot `ops/promote.sh` | :35, only with a request, market closed | applies a fully validated variant to the live strategy |
| monitors | `ops/lab_monitor.sh`, bot `ops/monitor.sh` | every 15 min | restart jobs, rebuild after commits, push guarantee; Claude only on a fault |

Every Claude job loads the MCP servers registered for Claude Code (`claude mcp add --scope user`): `qwen-lab` (`qwen-lab --config lab.toml mcp`: read-only
lab tools, `lab_quant`) and `moomoo-bot` (read-only bot checks). The lab dashboard (127.0.0.1:8790) has a **Live logs** panel (whitelisted names only).

## 5. Rules (what the brain follows)
`supervisor/AGENTS.md`: the hard no-invention rule, the bot, the backtest, what may change, the ML model. Change a rule: edit it and commit.

## 6. The bot side, git hooks and versions
* `cd ~/Projects/moomoo-bot && ./deploy.sh`: tests, lint, the bot service (restarted only when the program changed), the bot's jobs, and the post-commit hooks of both
  repos (`ops/push_guard.sh`: every commit pushed; work left idle for an hour is committed; secrets refused).
* `ops/versions.lock`: Rust, protoc and the Qwen weights kept on disk (unused). Drift is a notice for the owner, never an automatic change. A deliberate change:
  `touch ~/Projects/qwen-lab-stocks/MAINTENANCE` (repair sessions pause 6 h), change, update `ops/versions.lock` in the same commit.
* Kill switch: `touch ~/Projects/qwen-lab-stocks/STOP` stops learning and the brain. `brain/OFF` stops only the brain. `ML_GATE_OFF` makes the score file empty (no ML gating).

## 7. Check that it works
```sh
curl -s http://127.0.0.1:8790/api/snapshot | head -c 200     # lab dashboard data (never an error)
curl -s http://127.0.0.1:8789/api/status | head -c 200       # bot console
tail -5 ~/Projects/qwen-lab-stocks/logs/brain.log            # "<role> ok in Ns" every 5 minutes
qwen-ops health                                              # services, brain runs, ML verdicts, alerts
qwen-lab --config ~/Projects/qwen-lab-stocks/lab.toml quant  # the quant review
```
The hourly log doctor checks all of this continuously (`docs/WARNINGS.md` in the bot repo).
