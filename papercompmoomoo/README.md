# Moomoo paper bots

This package contains two isolated paper-trading bots and the shared candidate-market-data services. Both use Moomoo and must remain restricted to `TrdEnv.SIMULATE` and their assigned account.

| Bot | Account ID | Account kind | Dashboard |
| --- | ---: | --- | --- |
| Gainers and ORB | one shared configured `COMPETITION` account | `COMPETITION` | Overview (`/`); Gainer Tracker (`/gainer-tracker`); Exit Replay (`/exit-replay`) |

The numeric account IDs are stored only in the ignored local file `data/account_ids.json` (`gainer` and `orb` keys); they are intentionally absent from source and docs. Both IDs must resolve to the same Competition account. Buying power is shared and dynamically reserved across both strategies; positions, trades, and candidate sets remain strategy-specific. ORB consumes the local Moomoo movers snapshot published by Gainers to avoid a duplicate leaderboard query.

The low-priority `research-replay` service evaluates 10,000 randomized exit-grid configurations every 15 minutes against only that day's actual fills and stored Moomoo OHLCV candles. After 20:05 ET, it backfills session candles only for traded symbols with gaps, checks Moomoo's seven-day historical-candle quota first, and stores the best per-symbol grids locally. The Exit Replay tab shows the latest run. The optimizer is in-sample research and cannot submit or alter live trades or settings.

## Candidate data

Gainers owns Moomoo's US hottest-gainers leaderboard query, filters all Nasdaq-listed symbols in its top-200 response, and publishes a fresh local snapshot. It does not cut the global US ranking to 50 before filtering Nasdaq. The Gainer Tracker tab shows detection and entry approval/rejection. ORB consumes the shared snapshot without issuing a duplicate leaderboard query. Both fetch candidate-specific quotes and native one-minute candles through Moomoo; open positions remain monitored. The active candidate workflow does not use a SPY-holdings filter. The bots write native Moomoo candles directly to `data/common1m.sqlite3`; journals, account state, and exit evidence are stored separately under `data/`. Qwen learning automation and trainer code have been removed; historical datasets and evidence remain preserved. The old Yahoo/converter entrypoints are retired and excluded from service controls.

Moomoo's one-minute `time_key` labels the end of the candle interval. Chart code must preserve that meaning: place the candle over its actual one-minute interval, label it in ET, and expose its source timestamp in UTC. Premarket is 04:00–09:30 ET; regular trading is 09:30–16:00 ET; extended trading is 16:00–20:00 ET. Gainers can enter during confirmed premarket and regular sessions; ORB discovers and prepares candidates premarket but enters only after its regular-session opening range is formed. Candidate and account data are scoped to the New York session date.

## Live decision boundary

Both bots use deterministic high-water/price-line exits, the ThomasAI liquidity-trap condition, and a 15:00 ET flatten. Every line, including 70%, ratchets upward only; 70% follows 70% of the current full-target line. The `codex_exit_worker.py` name is historical; the worker skips discretionary model review for Gainers and ORB. It validates fresh position/market evidence and submits only to the shared simulated account. Qwen learning is shelved indefinitely; its automation has been removed while its historical data and the local inference service remain. See [`docs/EXIT_WORKER.md`](../docs/EXIT_WORKER.md) and [`docs/QWEN_SESSION_LEARNING.md`](../docs/QWEN_SESSION_LEARNING.md).

All decisions, actions, orders, and factual fills used for later analysis must retain their source timestamps and immutable evidence. An accepted order is not evidence of a fill. Check the host services and broker deal records before claiming a live paper cycle completed.

## Running and checking

Use the existing host service manager for normal operation. Do not start a second supervisor or executor while the installed service is running. For a new machine, follow [`docs/NEW_MACHINE.md`](../docs/NEW_MACHINE.md) and the pinned dependencies in `versions.lock`.

Read-only diagnostics and service controls are in `cli.py`, `check.py`, `auditor.py`, and the root `ops/` scripts. Never put Moomoo credentials in source or Git. The package's tests are under `tests/`; run them when explicitly requested or as part of an approved change workflow.

## Runtime data

`data/` is intentionally ignored by Git. It contains the shared candles, per-bot journals, snapshots, audit records, feed captures, and learning artifacts. **Do not remove it as part of repository cleanup.** It is operational evidence, not cache output.
