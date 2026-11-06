# System map

Last reviewed: 2026-11-06. Dated audits describe their own implementation snapshot and are not proof that a host service is currently healthy.

## Components

| Component | Location | Responsibility |
| --- | --- | --- |
| Gainers paper bot | `papercompmoomoo/src/papercompmoomoo/` | Owns the Moomoo US hottest-gainers leaderboard poll and publishes a fresh local snapshot; trades the shared Moomoo `SIMULATE` `COMPETITION` account. |
| ORB paper bot | `papercompmoomoo/tools/orb_premarket.py`, `orb_paper.py` | Reads Gainers' fresh Moomoo movers snapshot and fetches candles only for selected candidates premarket; entries are RTH-only after the opening-range conditions pass. |
| Candidate data | `papercompmoomoo/src/papercompmoomoo/bot.py`, `tools/orb_premarket.py`, `tools/orb_paper.py`, `moomoo_feed.py` | Fetches candidate and open-position data directly from Moomoo and writes native bars to the shared SQLite store. The former converter/Yahoo live-feed path is retired. |
| Exit worker | `papercompmoomoo/src/papercompmoomoo/codex_exit_worker.py` | Computes position levels and handles deterministic line, liquidity-trap, and session-flatten conditions for both strategies in the shared account. Its module name is historical; these bots do not receive discretionary model decisions from it. |
| Session exit replay | `papercompmoomoo/src/papercompmoomoo/research_replay_worker.py`, `papercompmoomoo/tools/research_replay.py` | Every 15 minutes, tests 10,000 randomized exit grids against today's actual fills and stored Moomoo OHLCV. After 20:05 ET it quota-checks and backfills only traded symbols with candle gaps, then stores the final run and publishes the latest report to `/exit-replay`. It cannot change live settings. |
| Rust research and portal | `crates/core`, `crates/engine`, `crates/cli` | Data validation, research/evaluation, operational commands, and the read-only dashboard on port 8790. |
| Archived strategy | `bot/` | Historical Rust strategy/library, not an active paper executor. |

## Data flow

1. Gainers queries Moomoo's US hottest-gainers leaderboard, filters to Nasdaq-listed symbols, and publishes a fresh local snapshot. ORB consumes that snapshot. Candidate-specific data is fetched only after symbols are identified; discovery is not restricted to SPY or other holdings.
2. Moomoo supplies quotes and native one-minute candles for identified candidates and open positions. The bots write shared candles to `papercompmoomoo/data/common1m.sqlite3`; account journals and decision/fill evidence remain separate under `papercompmoomoo/data/`.
3. The exit worker publishes computed levels to ignored local `data/exit_levels.json` for dashboard display. The portal reads local stores/status and does not submit orders.
4. The replay worker waits until the US extended session closes, then uses the day's actual filled positions only. It checks historical-candle quota before requesting gap recovery, stores the replay and coverage status locally, and does not place orders.
5. Gainers and ORB have separate strategy candidates, positions, trades, and ledgers, but use the same configured Competition paper account and shared account equity/buying-power pool. The Overview labels each strategy's charts and the shared account.

Runtime data is ignored by Git because it contains market history and trading evidence. It is not disposable build output.

## Sessions, charts, and exits

Market sessions use `America/New_York` (ET): premarket 04:00–09:30, regular session 09:30–16:00, extended session 16:00–20:00. The Overview reports current New York market status and time, shared Competition account equity/P&amp;L, then current or most recent session charts for both strategies. Candidate charts mark first recorded detection (`D`) and approval (`A`) with ET timestamps. Open-position charts show the ratcheting 100%, 70%, intermediate, midpoint, and red stop lines from the exit worker's current snapshot. Every line, including 70%, ratchets upward only; 70% tracks 70% of the current full-target distance and cannot fall below its persisted value.

Moomoo's one-minute `time_key` is an interval-end timestamp. Symbol charts place each candle in its actual one-minute interval, show the interval in ET, and retain the provider timestamp in UTC. The current renderer plots session data from 04:00 through 16:00 ET and does not plot extended-hours candles after 16:00, although exit monitoring may continue during extended hours. Pushed-bar fallback groups by `time_key`; live overlays cannot cross into another session. Equity charts label snapshot time ranges in UTC.

Gainers entries may use ETH during confirmed premarket and RTH during the regular session; ORB entries are RTH-only. Both use candidate-scoped Moomoo data and keep legacy non-Moomoo rows out of live decisions. The ratcheting 100% target exits when a fresh Moomoo trade touches or crosses upward through it; the 70%, intermediate, midpoint, and red stop exit on confirmed downward crosses. The ThomasAI liquidity-trap condition and 15:00 ET session flatten also exit. ETH exits use a live-bid limit; RTH exits use a market order. Stale mover recovery refreshes only identified movers; it can use a fresh book midpoint when the trade quote is stale, then a provider-timestamped batched market snapshot if both streams are stale. If no fresh two-sided data is returned, the candidate waits. Exit submissions likewise require a fresh executable price. At the RTH open, a remaining ETH exit is cancelled and replaced with an RTH market order. The worker checks account identity and Moomoo's current US market state before submission. Qwen learning is shelved and its automation has been removed; Qwen has no live exit authority. See [`MOOMOO.md`](MOOMOO.md), [`MOOMOO_DATA.md`](MOOMOO_DATA.md), and [`EXIT_WORKER.md`](EXIT_WORKER.md).

## Current references

- [Root README](../README.md): project orientation and operator entry points.
- [Gainers gating audit](GAINER_GATING_AUDIT.md): dated implementation audit, not a live health report.
- [Moomoo candidate-data contract](MOOMOO_DATA.md): active candidate-data scope and timestamp rules.
- [Causal data rules](NO_BIAS.md): live and retrospective evidence constraints.
- [New host setup](NEW_MACHINE.md): source-only setup and local runtime-data boundaries.
