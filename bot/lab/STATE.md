# Archived qwen-lab STATE

> **Archive notice (2026-11-06):** The state below is a historical snapshot of the retired Rust research lab. It is not current service, model, data, or trading status. The active bots are documented in the root `STATE.md`.
Updated 2026-10-13. Rust lab for the SPY bot. ALL AI work is done by the lab's own small model, the BRAIN (src/brain.rs, brain_roles.rs,
mltrade.rs; docs/BRAIN.md): Rust only, CPU only, ~17 MB. Hermes, Honcho, Ollama and Qwen/oMLX are retired and UNINSTALLED (owner, 2026-10-13).
Claude Code supervises (supervisor every 6 h, log doctor, watchdog fixer). Code tests and judges everything. Repo qwen-lab is mirrored into
the bot repo's lab/ (ops/sync_lab.sh). Rule: everything is Rust only (shell only as installers/stubs).

## Jobs (launchd; binary qwen-lab-stocks/bin/qwen-lab; config ~/Projects/qwen-lab-stocks/lab.toml)
brain (always on: `brain serve`, one role of twelve every 5 min) | improve (24/7 test loop) | tick (hourly :07: imports, winrate, promotion, number scrub) | dash :8790 | monitor (15 min) | supervisor (6 h, Claude) | resourcewatch (5 min, `qwen-ops watch`) | watchdog (10 min, `qwen-ops watchdog`).
Rust helpers in ~/.local/bin: qwen-tools, qwen-ops, qwen-notify (Discord), qwen-guard (webhook never reaches git); build+sign: ops/install_tools.sh.
After code changes: cargo build --release; `qwen-lab --config <lab.toml> schedule install`, then `brain install`; a RE-SIGNED binary needs every job
unloaded and loaded again (kickstart alone leaves launchd refusing it: exit 78) -- the monitor and qwen-ops heal do this. touch MAINTENANCE while editing by hand.

## The brain (docs/BRAIN.md)
Roles through the same lab tools as before, authors brain-<role>: strategist (policies heuristic/ucb/additive/random take turns, plan), tuner (tune: bands),
researcher (guarded paper search, topic words must appear), auditor (fragile / too-few-trades notes), statistician (quant verdict, consistency, DAILY
pre-registered ML test on the real trades), execution (paper-decision blockers). `qwen-lab brain report` compares the policies on real outcomes; `brain ml` runs the ML test; `brain lessons <words>` searches the 117 lessons + 5 skills imported from the retired Hermes archive.
Knowledge: brain_facts, brain_proposals (SQLite). Reports: brain/reports/*-brain-<role>.md, logs/brain.log.

## Deep learning 24x7 (docs/BRAIN.md): own autodiff, 34 in-house indicators, residual net + Transformer on candle/indicator sequences, evolutionary architecture search in the always-on job com.qwenlab.trainer (`qwen-lab trainer status`); a genome becomes champion only if it beats the ridge at the adjusted level, is re-tested hourly, demoted when it stops winning. No champion yet (first Transformer: no gain).
## The model + the 12 roles that run 24x7, one every 5 min (docs/BRAIN.md): data, steward (ML_GATE_OFF on significantly negative recent IC; last 15 sessions IC -0.106, se 0.068, not yet significant), drift, risk, promotion, experimenter. `qwen-lab model ...` = versioned artifact + registry + live top-20 ranking with calibrated expected bps and reasons + rollback; every live call is stored and scored against the real outcome (`model scoreboard`)
## Trading intelligence (docs/BRAIN.md): risk model (SPY volatility forecast beats naive by 41.5%, proven; direction: no skill), per-signal take/skip with costs and tail-based slot size (`model decide`), model as a filter on the bot's entries (`model trades`: lean, not significant). Entry rule: 2 positions, ANY symbol whose signal fires first takes a slot -- no top-N portfolio picking.
## Candle ML (all in docs/BRAIN.md; research only, nothing live)
Data: `candles import` -> data/candles.sqlite3 (SPY holdings only, splits dropped like the bot: 80 symbols, ~472k 15-min bars); `candles ml` (fees + 14-session
walk-forward verdict), `candles improve` (the round that runs on every new bar: recency schemes, 7 extra features, meta-labelling; trial ledger ml_trials; paired, adjusted intervals),
`candles scores` (-> moomoo-bot/target/oos-cache/ml_scores.json, refreshed daily). Model: ridge + SPY-regime interactions on the bot's own score components, top-20.
Results: trees/ensembles/extra features/meta-labelling/recency schemes: nothing significant, nothing adopted. With real fees the standalone verdict is "does not
qualify" (+20 bps/day [-42,+80]). In the BOT's backtester (`variant_study 5h mlgate`, the percentile searched per block): cut A +3.44%/block vs baseline +2.84%
(worst -7.6% vs -9.8%); cut B +3.92% vs +1.92% but later half +0.93 vs +2.06 and worst -10.4%: a lead, not validated (lab loop judges `mlgate`). Brain joined:
strategist `ml-lead` queues mlgate, execution reports the model's view of rejected names, statistician notes verdict changes.

## Bot (repo SPY-ETF-UNIVERSE-HOLDINGS-TRADING-BOT, ~/Projects/moomoo-bot)
5h candles, SPY trend filter, SPY holdings with candles, 5,000 USD, 2 positions, EXITS FROZEN, paper only. Console 8789.

## Rules (code-enforced)
2026+ data; walk-forward 14-session blocks; cut A + cut B + noisy tape; max 2 options (families, tune:margin 0-10, tune:dca 0-40); no signal/direction/
exit change; winner run = 60%+ trades profitable. The brain sees the TRAINING half only. Validated = cut A and cut B. Settings come from the search.

## Real-data findings (docs/BRAIN.md)
As analysed on 2026-10-13: 194 tested 5h variants, 24 passed the holdout (the lab keeps testing; the dashboard has the live counts). No model beats chance at picking holdout passers (top quarter 6-13% vs 15-20%). The old Hermes strategist did
(8 of 28 = 29% vs 12%). ML on 40,076 real trades: raw win/lose AUC 0.48 (no skill); within a block the return-trees give +0.11 pts/trade [-0.06,+0.31],
13/23 blocks was memorisation: with early stopping (chosen on training data only) the trees pick 0 rounds -> no gate. Learning curves (BRAIN.md): only the additive average learns; the GP did not (dropped).

## Switches (plain files in qwen-lab-stocks; delete to undo): STOP (kill switch), MAINTENANCE (pause repair sessions), brain/OFF (stop the brain), ML_GATE_OFF (empty score file = no ML gating; the steward sets it).

## Persistence (nothing important only in RAM)
Lab SQLite (memory/, data/), brain_facts/brain_proposals, reports, logs, backups (lab memory every 12 rounds), bot store + records + paper + console state.

## Gotchas
- Dashboard ensures the schema at start. json! literal needs recursion_limit 256. launchctl bootstrap can fail right after bootout (error 5): retry.
- Process matches (pgrep -f) must be anchored: a shell merely MENTIONING a name matches an unanchored pattern.
- serde_json needs `float_roundtrip` or 0.9199999999999999 parses as 0.92. fancy-regex hits its backtracking limit on big input: keep lookarounds off
  large texts (fix_report::find_secret is linear) and raise the limit where they must stay.
- The doc generator refuses source containing credential-shaped literals (build such test strings at run time); qwen-guard refuses webhook-shaped ones.
- Tests must never touch live data (they use temp folders via Config::for_root). Rebuild (cargo build) before `dash install`.
- Several Claude Code sessions can touch this repo at once (supervisor, watchdog fixer, log doctor): check `git status`/`git log` before editing shared files.
- Versions pinned (ops/versions.lock): drift = owner notice only. Repair sessions never touch models or uncommitted work.
