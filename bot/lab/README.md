# Archived qwen-lab research system

> **Archive notice (2026-11-06):** This lab belongs to the retired SPY-holdings Rust bot under `bot/`. Its role schedules, model descriptions, service names, local paths, and runtime claims are historical and do not describe the active Gainers/ORB system or current host. Do not install or start these jobs for live paper trading.

The research side of the **SPY ETF Universe Holdings Trading Bot** (repo `SPY-ETF-UNIVERSE-HOLDINGS-TRADING-BOT`, where this code is mirrored under `lab/`).
It started as a lab for a local Qwen language model, hence the name. Local model runtimes are retired and uninstalled. Deterministic testing remains **Rust** and CPU-local; an optional checked OpenCode ensemble proposes experiments from the shared SQLite brain. Nothing here changes live trading without a reviewed code change in the bot
repo, and nothing touches an account, an order or an exit.

What it does:
* **Tests strategy variants** on the bot's own backtester (walk-forward 14-session blocks, 2026 data only, cut A and cut B, noisy tape, holdout) and decides by code.
* **The brain** (`docs/BRAIN.md`): the lab's own small models. Twelve roles take turns, one every 5 minutes, 24x7: strategist, tuner, researcher, auditor,
  statistician, execution (they read and report) and **data, steward, drift, risk, promotion, experimenter (they act, within checked limits)**.
* **The machine-learning model**: ridge regression with market-regime interactions on the bot's own score components, refitted every 14 sessions on older data only,
  scored for the bot's backtester (`Signal::Ml`, the searched entry gate `mlgate`). A daily improvement round tests training schemes, extra features and meta-labelling
  against a trial ledger with paired, multiple-testing-adjusted intervals; a model steward switches the model off by itself if its recent out-of-sample IC is
  significantly negative.
* **Code checks every claim** (`src/facts.rs`) and refuses invented numbers. Outside text (papers, notes) passes the security guard (`src/guard.rs`) first.

Read first: [`STATE.md`](STATE.md) (current state, 1 page), [`docs/BRAIN.md`](docs/BRAIN.md) (the brain, the ML model, every result and its limits),
[`docs/ML_RESEARCH.md`](docs/ML_RESEARCH.md) (models and leakage controls), [`CLAUDE.md`](CLAUDE.md) (rules), [`docs/SETUP.md`](docs/SETUP.md), [`docs/NOTIFICATIONS.md`](docs/NOTIFICATIONS.md), [`docs/RESOURCE_SAFETY.md`](docs/RESOURCE_SAFETY.md).

## What runs (local machine; platform-specific service manager)

The service manager is selected automatically: systemd user units on Linux, launchd on
macOS, and Task Scheduler on Windows. The lab is local; external traffic is limited to
the configured paper-data searches, Git operations and notifications.

| Job | What it does |
|---|---|
| `com.qwenlab.brain` | The always-on brain (`brain serve`): a role every 5 minutes, low priority. Reports in `brain/reports/*-brain-<role>.md`, `logs/brain.log`. |
| `com.qwenlab.improve` | The strategy loop, 24/7: queued requests, re-tests of winners, interval sweep, planner. |
| `com.qwenlab.tick` | Hourly: imports the bot's backtest and paper archives, win/lose analysis, promotion requests, scrub of unverified numbers in stored brain text. |
| `com.qwenlab.dash` | Read-only dashboard at http://127.0.0.1:8790. |
| `com.qwenlab.monitor` | Every 15 min (`ops/lab_monitor.sh`): restarts stopped jobs, checks results and disk, writes `STATUS.md`; a fault it cannot fix starts one headless Claude Code session. |
| `com.qwenlab.supervisor` | Hourly at minute 10 (`qwen-ops supervisor`): a Claude Code session that keeps the jobs running and improves how the lab tests, learns and applies. If Claude hits its weekly limit, the unattended run stops; Codex is reserved for interactive use. |
| `com.qwenlab.resourcewatch` / `watchdog` | `qwen-ops watch` (5 min) and `qwen-ops watchdog` (10 min): memory, swap, OpenD, brain liveness; self-heal; Claude fixer at most every 30 min. |

The online ensemble is documented in [`docs/ONLINE_AI.md`](docs/ONLINE_AI.md). Its models receive the same stored-candle lessons and option-skill records. Two free models must agree and Claude—or an OpenCode free validator when Claude is unavailable—must approve before a proposal reaches the ordinary test queue. Unattended automation never launches Codex. Models never declare winners: only completed stored-candle backtests and walk-forward validation do. Paper/live P&L is never used as a trusted label.

Helpers in `~/.local/bin` (built and signed by `ops/install_tools.sh`): `qwen-tools`, `qwen-ops`, `qwen-notify` (Discord), `qwen-guard` (keeps the webhook out of git).

## The strategy loop: how variants are judged

Variants add at most **2 entry options** to the current strategy, from a fixed vocabulary (`VOCAB` in `src/strategy.rs`: entry trend band, basket size, trend-line
kind, data-driven entry gates, confirmations, universe width, graduated sizing, DCA, the machine-learning gate `mlgate`) or tuned entry search ranges
(`tune:margin=lo-hi`, `tune:dca=lo-hi`). The ranking signal and its direction are never changed and **exits are frozen**. Settings inside a variant always come from the
bot's random search. Each variant is backtested on 14-session look-forward blocks (each block's settings searched on the sessions before it, then held fixed) at the
bot's 5,000 USD account. A variant is a **winner** only if, on the half of the blocks never shown to the brain, it makes money in both halves, beats the baseline, keeps
40%+ of blocks positive, never loses more than 8% in a block, trades 2+ times per block, stays positive on a noisy tape (costs x1.5) and does not lower the trade win
rate. It must then pass cut B to be **validated**, and cuts C, D, E for **full validation** (`strategy::full_validation`).

**Promotion.** A variant goes live only when it is considerably better on cuts A and B (+1.0 point per block, win rate not lower, noisy tape positive) AND passes at
least 4 of the 5 cuts, on the live interval with entry options only (`src/promote.rs`). The lab writes a request; the bot repo's `ops/promote.sh` has Claude Code
change the live strategy in code, test, deploy and push, only while the US market is closed. Research switches such as `mlgate` cannot go live by themselves; the
promotion agent lists them as needing a code review.

## Why runs win or lose (`src/winrate.rs`)
A run is a **winner** when at least 60% of its closed trades made money. The lab compares winners and losers on settings, entry conditions and exit mix; a reason counts
only if the later third of the blocks agrees (block-resampled interval, false-discovery control). Run by hand: `qwen-lab winrate`.

## The machine-learning work (all in `docs/BRAIN.md`, with its negative results)
`src/candles.rs` (the lab's candle store, the bot's data hygiene), `mlcandle.rs`/`mlbot.rs`/`mlfeat.rs` (features), `mlscores.rs` (scores for the bot's backtester),
`mlwalk.rs` (fee-inclusive walk-forward), `mlimprove.rs`/`mlround.rs` (trial ledger, paired adjusted tests, the daily round), `mlmeta.rs` (meta-labelling),
`mltrade.rs` (logistic regression and gradient-boosted trees, self-written), `brain_agents.rs` (the six agents that act).

## Dashboard (http://127.0.0.1:8790)
Banner when a considerably better strategy is found; strategy experiment winners with the holdout next to the baseline for both cuts; research ideas from papers;
win/lose analysis; the bot's backtest archive and paper record; candle sizes; fine-tuning; the brain's scoreboard; security guard; status and blockers. It polls every
15 s and reloads itself after an update.

## Commands
Always pass the stock lab's config: `qwen-lab --config ~/Projects/qwen-lab-stocks/lab.toml <command>`.
```
import-bt | ingest | import-paper    read-only imports of the bot's backtest archive and paper record
improve [--rounds N | --forever]       the strategy loop
winrate | highlights | quant           why runs win or lose; 15%+ runs explained; the quant review
brain eval|ml|report|lessons|run <role>|all|serve [secs]|install|uninstall   the brain
candles import|ml|improve|scores        the candle store, the fee-inclusive verdict, the improvement round, the bot's score file
dash [serve|install|uninstall|status]   schedule [install|uninstall|status]   mcp
promote | promotion-status | note --to <role> <text> | announce
guard-scan <paths> | study-import | origins-backfill | status | report | backup | cull [--apply]
```
After a code change: `cargo build --release`, then `qwen-lab --config ... schedule install` and `brain install`; the jobs run an installed copy in
`qwen-lab-stocks/bin`. A rebuilt binary is reinstalled and reloaded by the platform service manager; the monitor does it.

## Rules
* Real data only (one mode); tests may use small made-up inputs to check code, never to report findings. Report negative results plainly.
* No invented or manipulated data or results: every number comes from code or a command that ran (`src/facts.rs`).
* Versions are pinned (`ops/versions.lock`): nothing is upgraded once it works. No account numbers, credentials or API keys anywhere; the Discord webhook never reaches git.
* Commit messages are read by others: what changed and why, live vs research impact, how it was verified, known limits (`CLAUDE.md`). Built and maintained with Claude Code only.

## Function reference and tests
Every function is described in [`docs/functions/`](docs/functions/README.md), generated by `qwen-tools gen-function-docs`. Run `cargo test` to obtain the current test count; the count is intentionally not hard-coded here.
refused, quant review, statistics against hand-computed values, ingest integrity, look-ahead refused, verdict rules, the ML pipeline's causality and
leakage tests, the agents' rules, the security guard). Tests never touch live data.
