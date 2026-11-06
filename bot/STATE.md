# moomoo-bot STATE (keep <60 lines; update at milestones; newest facts win)
Updated 2026-10-17. Read this instead of re-exploring the repo. This checkout runs on
openSUSE Linux; service and path examples are Linux-first unless marked otherwise.
## What it is
SPY ETF Universe Holdings Trading Bot (GitHub ProperProperProper/SPY-ETF-UNIVERSE-HOLDINGS-TRADING-BOT, private, collaborator
rubixxfreeze). Rust paper bot over moomoo OpenD :11111; orders only to the SIMULATE account. Long only, cash, whole shares.
Service is managed by `bot-ops` (systemd user units on Linux); console 127.0.0.1:8789;
deploy ONLY with ./deploy.sh; health ./check.sh [min].
Commit with thorough messages (CLAUDE.md), push, redeploy after every change. lab/ = mirror of ~/Projects/qwen-lab (edit there).
## Live setup
- 15m source bars are aggregated into every live candidate interval; the paper account follows the best completed interval winner, with the SPY trend filter (`REFERENCE`). Universe: every SPY holding (502 of 503 eligible,
  data/spy_holdings.csv, daily refresh ops/spy_holdings_refresh.sh -> examples/holdings_check.rs; removed: data/spy_removed.csv).
- Candle quota 100 stocks / rolling 7 days (account NOT upgraded): ~95 heaviest holdings have candles; rest excluded and logged.
- 5,000 USD account (`START_CAPITAL`, min(cash, 5000)); exactly 1 position (`LIVE_TOP`); no trade below 50% of slot (`MIN_SIZE_FRACTION`).
- If an older paper state has excess positions, retain the strongest marked-P&L position and close excess only at break-even or profit; never force a loss solely for the cap.
- EXITS FROZEN. Research changes entries only (`entry_*` / `grade_*` / `entry_gate` incl. GateFeature::RefSlope = gate:slope).
- Liquidity model live only after ops/fill_probe.sh passes (file liquidity_model.on); research tools default it ON.
- Live service CPU throttling disabled; interval searches may use all available cores.
- PAPER ENTRIES (fixed 2026-10-13): engine bar = one 5h CANDLE (09:30-14:30, 14:30-16:00 ET; 15m bars only feed it). A decision at a candle close fills
  at the open of the candle AFTER NEXT (decision latency) = 09:30 or 14:30 ET of the next session, in-session. It keeps its frozen settings; a new search
  winner or a restart no longer cancels it (before, a settings change cleared it: the paper account never traded). examples/replay_decision replays it.
## Layout
src/engine: xs.rs engine | walkforward.rs search/space | rules.rs order rules + liquidity | moomoo.rs gateway (sessions_from for
research calendars) | store.rs | btarchive.rs | governor.rs. src/bin/bot/service.rs service + console page.
examples: variant_study (strategy variants, blocks in parallel, win rate per half), lab_export (lab archive), universe_fetch (deepens
history, free re-pulls only), holdings_check, history_depth, fill_probe. ops: lab_export.sh (daily), sync_lab.sh, monitor.sh.
Store: the configured application-data directory (on this machine under `~/.local/share/`);
use `./check.sh` to print the active path.
## Research
- 2026+ ONLY (user rule): `examples/common::RESEARCH_FROM` = 2026-01-02 is a floor; load_data drops older cached candles (the cache
  history.json still holds 2024-2025 from a free deep pull; unused). Pre-2026 archive runs were removed 2026-10-09 (backup
  ClaudeMoomooBotArchive/backup-before-2026-only-*). Walk-forward: each 14-session block's params optimised on the prior sessions.
- 14-session look-forward blocks; cut A offsets 0,5,10, cut B 2,7,12; noisy tape (jitter, costs x1.5).
- Live/research candidates (all from 15m bars): 15m 30m 45m 1h 90m 2h 150m 3h 4h 5h 6h 1d. Stale data removed 2026-10-09:
  archive keeps only 5,000 USD runs from 2026; research cache trimmed to 2026. No crypto on this Mac (Bybit bot disabled).
- variant_study tokens: baseline, keep, entrywide, entry0, flatenter, top1/top2/top12/top13, univN, gate:ret/dist/vol/atr/any/slope,
  kind:*, grade:*, sig:*, dca:off/on/tight/wide, tune:margin=lo-hi, tune:dca=lo-hi, liq/noliq (the lab offers only complementary ones, max 2). Env STUDY_EQUITY, STUDY_THREADS, STUDY_ORDER_CAP, RESEARCH_FROM. Columns end with chained,
  win 1st, win 2nd.

## Current operations (2026-10-17)
- Local Qwen was retired and removed on 2026-10-17. The hourly strategist uses two agreeing, load-balanced OpenCode free online models; Claude validates when available, otherwise `opencode/deepseek-v4.1-flash-free` decides. Unattended automation never launches Codex. See `lab/docs/ONLINE_AI.md`.
- Prompts, choices, refusals and test outcomes persist in `online_turns`/`online_memory`; old `qwen_*` tables are migrated once and retained only as an archive. Every model gets the same shared memory (verified outcomes, skills, recent actions, earlier model answers). Only stored-candle backtests and walk-forward verdicts label winners or mistakes; live results do not.
- The persistent online brain maintains auto-updating `online_skills` and `online_brain`. Each turn retrieves those stores, recent actions/refusals, and inherited `brain_facts`; only code-verified stored-candle walk-forward outcomes can update trusted skills.
- OpenCode 2.0.32 is installed. Free-model calls use the stock `plan` agent from an empty runtime directory because OpenCode misclassifies permission-denied custom agents as external traffic. Researcher/validator rules travel in the prompt, direct provider IDs are rejected, and the provenance error stops the cycle immediately pending `opencode auth login opencode`.
- Source and lab are deployed as of commit c98bee5: `brain.log` shows the live ensemble ("Online brain: ... chose to wait"); bot jobs and lab jobs verified, and the lab `tick` job's broken `./lab.toml` path is fixed (redeploy re-registers it with the absolute config).
- The active lab runs its brain, strategy loop, trainer, tick and monitor. Research still requires walk-forward evidence on both block cuts.
- The live strategy is the plain `src/engine/live_options.rs` `LIVE`. `rollstop+top13` passes 3/5 full-validation cuts (4 needed); promotion stays blocked.
- Bot `other_jobs()` now registers promotion hourly :35, log doctor hourly :55, fix report daily, bot monitor every 15 minutes, daily lab export, universe fetch and fill probe; all installed and scheduled.
- Console persistence: console_state.json (research, walk-forward, search summaries, paper choice) and
  search_candidates.json (candidates with forward tracks) in the runtime dir, loaded at start.

## Gotchas
- Engine times are a synthetic TRADING CLOCK (CLOCK_EPOCH 2026-01-02 for live); convert with clock_wall/wall_trade before export.
- OpenD must be running and logged in (GUI); the bot crash-loops without it.
- zsh: never put a command+flags in one variable (silently fails). macOS sed differs from GNU sed: for multi-line edits use the Edit tool or a small Rust change (everything is Rust only; no Python).
- OpenCode project discovery follows inherited `PWD`; teacher_engine deliberately sets both cwd and `PWD` to its empty runtime directory so the stock agent cannot discover either repository.
- Live-draw fingerprint test (walkforward.rs) must be re-recorded for LIVE_TOP=(1,1); this is a deliberate live change.
- QQQ-era results removed 2026-10-09 (backup ClaudeMoomooBotArchive/backup-before-qqq-removal-*); reference SPY only. Never touch
  the REAL account. No fixed params. No crypto anywhere.
