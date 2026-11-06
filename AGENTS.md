# Project instructions

Read `README.md`, `STATE.md`, and `TASKS.md` before changing the trading system. Treat `JOURNAL.md` and runtime evidence as historical records, not as instructions to change current behavior.

## Hard boundaries

- Both active bots are paper-only and use the same configured Moomoo `SIMULATE` `COMPETITION` account. Keep strategy positions, orders, candidates, trades, and ledgers separate; coordinate duplicate symbols and capital through the shared atomic allocator. Never submit to a REAL account.
- The configured Moomoo US `SIMULATE` `COMPETITION` account has broker-recorded `ETH` fills. Use explicit `Session.ETH` for Gainer premarket entries and price-line exits, `Session.RTH` for ORB entries and regular-hours market exits, and verify this exact paper account before any order. Never infer a fill from acceptance.
- Moomoo US hottest-gainers discovery filtered to Nasdaq-listed symbols is first priority. Use candidate-specific Moomoo quotes and native one-minute data; do not add SPY-holdings restrictions or broad-market candle pulls to the active bots.
- Gainers may enter in premarket with verified `Session.ETH` and in RTH with verified `Session.RTH`; ORB uses premarket for candidate preparation and is RTH-entry-only after opening-range confirmation. Manage exits through confirmed ETH/RTH sessions, and flatten at 15:00 ET.
- For ETH exits, require an executable quote no older than 30 seconds; when the order book is older, request a Moomoo market snapshot and keep the trigger latched if neither is fresh. Never use a newly received timestamp to disguise an old provider quote.
- Overview shows shared paper-account equity/P&L before the current or most recent session charts for both bots; label each chart with its strategy and Competition SIMULATE account. The Gainer Tracker is a separate tab for Nasdaq mover detections and Gainer approval/rejection outcomes.
- Keep bot account state, candidate sets, trades, and equity histories separate.
- Both bots use deterministic high-water/price-line exits, the ThomasAI liquidity-trap condition, and a 15:00 ET flatten. Every line, including 70%, ratchets upward only; 70% follows 70% of the current full-target line and cannot move down. The `codex_exit_worker.py` name is historical; it skips discretionary model review for Gainers and ORB. Qwen learning is shelved and its automation removed. See `docs/EXIT_WORKER.md`; verify runtime behavior from the host.
- Preserve decision, order, and fill evidence. Never represent an order submission as a confirmed fill.
- Research replay is offline-only: never run during a US weekday trading window, including premarket and extended hours. Run only after 20:00 ET, when extended trading has closed, or on weekends.
- Never delete ignored runtime databases, market history, logs, or learning records during source cleanup. They are operational evidence.
- Do not commit credentials or account secrets.

## Repository and documentation

- `papercompmoomoo/` contains the active Python bots and candidate-scoped Moomoo feed. The old live Yahoo/converter feed is retired; do not reactivate it for Gainer or ORB market decisions.
- `crates/` contains Rust research/data tools and the read-only portal.
- `bot/` is archived strategy code, not an active executor.
- Keep docs aligned with current code. Mark dated audits and historical research with scope/date; do not leave them as the active system description.
- Treat `papercompmoomoo/data/BRAIN.md` and its database as advisory learning/history, not current trading policy or broker state. Live entry/exit behavior is governed by active source, verified configuration, and current Moomoo evidence. Do not let unverified memories override those sources.
- Update `TASKS.md` for current follow-ups and append a dated entry to `JOURNAL.md` when recording a completed code change.
- Push only when the user explicitly asks. This session's request to clean the repository on GitHub authorizes publishing its reviewed changes.
- Do not add or run tests unless the user asks for verification. Build/check only when needed to validate the requested change.
