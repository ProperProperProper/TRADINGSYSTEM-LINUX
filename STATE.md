# Current project state

Reviewed: 2026-11-08.

## Active system

- Two Moomoo simulated paper bots use the same configured `COMPETITION` account. A shared SQLite allocator atomically reserves live buying power for risk-sized orders and prevents both strategies claiming the same symbol; candidates, positions, trades, and strategy ledgers remain separate. ORB reads Gainers' fresh local Moomoo leaderboard snapshot instead of polling the leaderboard again.
- Gainers owns the Moomoo US hottest-gainers leaderboard query, filters for Nasdaq-listed symbols, and publishes a fresh local snapshot. ORB consumes that snapshot; it does not make a duplicate leaderboard request. Both fetch candidate-specific native one-minute data and do not use a SPY-holdings restriction.
- The exit worker applies deterministic upward-only stop, high-water, 100%, 70%, intermediate, midpoint, ThomasAI liquidity-trap, and one-hour-before-close rules. Every line, including 70%, ratchets up; the 70% line follows 70% of the current full-target line. Its legacy `codex_exit_worker.py` name does not mean it makes discretionary model decisions for these bots.
- Qwen learning is shelved indefinitely. Learning timers, trainer/MCP code, and review automation have been removed. Historical datasets and the local inference service remain; the cognitive brain and ThomasAI decision components remain available.
- Research replay is offline-only and guarded against weekday 04:00–20:00 ET; it replays candidate-scoped GEX entries plus GEX/price-line exits and runs only after extended trading closes at 20:00 ET or on weekends.
- The Rust portal is read-only on port 8790. Overview shows shared Competition account equity/P&L and Gainer/ORB current or most recent session charts. The separate Gainer Tracker tab shows current-session Nasdaq detections and Gainer entry decisions. Chart time uses New York session dates and ET intervals; provider/source timestamps use UTC.
- `bot/` is the shared Rust protocol and strategy library; live execution is handled by the Python services.

## Evidence boundaries

Source code and a running service do not prove that a broker order filled. Confirm fills from Moomoo deal records. Runtime stores under `papercompmoomoo/data/` are ignored by Git and must be preserved; the configured account-ID file is local-only.

## Primary references

- [`README.md`](README.md): project and operator entry points.
- [`TASKS.md`](TASKS.md): runtime acceptance work.
