# TradingSystem

TradingSystem contains two Moomoo **simulated paper-trading** bots, Rust research tools, and a read-only dashboard. The active strategies are the NASDAQ Gainers bot and the ORB bot. Both bots fetch candidate and open-position data directly from Moomoo. The old Yahoo/converter pipeline is retired and cannot be started through the session controls. The older Rust bot under `bot/` is archived and is not either bot's live execution service.

## Active paper accounts

| Bot | Moomoo simulated account kind | Dashboard | Data scope |
| --- | --- | --- | --- |
| Gainers and ORB | One shared `COMPETITION` paper account | [Overview](http://127.0.0.1:8790/) · [Gainer Tracker](http://127.0.0.1:8790/gainer-tracker) | Account and strategy charts stay on Overview; live gainer detections and entry decisions have their own tracker tab |

Both strategies use the same simulated Competition account. Moomoo's live buying power is the common pool; each strategy sizes from its own risk rules and atomically reserves only the amount it needs. Gainers owns the single Moomoo hottest-movers request and publishes a short-lived local snapshot for ORB, avoiding a duplicate leaderboard request. Strategy ledgers and charts remain distinct.

The account is simulated. Orders must remain restricted to `SIMULATE` and the locally configured Competition account ID.

## Market data and chart times

Moomoo is the source for Gainer discovery, quotes, and the native one-minute candles used for identified candidates and open positions. Gainers polls Moomoo's US hottest-gainers leaderboard and publishes the fresh Nasdaq-filtered list for ORB candidate preparation; discovery is not restricted to an ETF or portfolio holdings list. Runtime candle and decision stores are under `papercompmoomoo/data/` and are intentionally excluded from Git. The per-install account-ID map is also local-only at `papercompmoomoo/data/account_ids.json`.

Overview shows shared Competition account equity/P&amp;L and the two strategies' current or most recent session charts. The separate Gainer Tracker tab shows each captured Nasdaq row from Moomoo's top-200 US movers response, plus the Gainer entry approval or rejection when evaluated. Live Gainer discovery now screens every Nasdaq row in that response; it does not truncate the global US ranking at 50. The tracker also shows price observations and locally available Moomoo candle highs after detection. Hypothetical P&amp;L is per share from detection price to best observed price; it is a hindsight ceiling, not a fill or sized-position simulation. A separate low-priority process reads existing local files only and makes no Moomoo or broker requests. Symbol charts plot premarket (04:00–09:30 ET) and regular-session data through 16:00 ET; they do not currently plot 16:00–20:00 extended-hours candles. Moomoo one-minute candle timestamps label the interval end; charts place the candle over its actual one-minute interval and show that interval in ET, with the source timestamp in UTC on live updates.

Current-session candidate charts mark first detection (`D`) and approval (`A`) with vertical lines and exact ET timestamps. For Gainers, approval means its entry gates passed; for ORB, approval means Moomoo accepted the paper buy order.

Open-position charts show the exit worker's current ratcheting 100% target, green 70% target, intermediate line, purple midpoint, and red stop. Their levels update with the worker snapshot while the chart is open; charts do not invent levels when a valid position level is missing.

## Exit decisions and learning

Both bots use upward-only ratcheting 100%, 70%, intermediate, midpoint, and red stop lines. The 100% target sells on fresh-price touch/up-cross; the 70%, intermediate, midpoint, and red stop sell on confirmed down-cross. The 70% line follows 70% of the current full-target line and is itself maxed against its persisted value. Regular-hours exits submit market sells; during ETH the worker uses a limit at the current executable bid and reprices from fresh data. Stale provider timestamps cannot be made fresh by local receipt time. The worker also applies the audited ThomasAI liquidity-trap exit and flattens all positions at 15:00 ET, one hour before close. No position-age exit is active. The `codex_exit_worker.py` service name is historical; it skips discretionary model review for these two bots. Qwen learning is shelved indefinitely and its automation has been removed; the local inference service and cognitive brain/ThomasAI decision components are retained. See [the exit worker notes](docs/EXIT_WORKER.md) and [the Qwen learning status](docs/QWEN_SESSION_LEARNING.md).

## Repository map

| Path | Purpose |
| --- | --- |
| `crates/core`, `crates/engine`, `crates/cli` | Rust data, research/evaluation, operations, and portal |
| `papercompmoomoo/` | Gainers and ORB paper bots, candidate-scoped Moomoo feed, account and decision records |
| `docs/` | Current system notes, data contracts, and dated audit/handover records |
| `bot/` | Archived Rust strategy code; not an active paper-trading service |
| `data/`, `papercompmoomoo/data/` | Local runtime state and market evidence; not source-controlled |

Start with [system map](docs/SYSTEM.md), [current host state](STATE.md), and [open follow-ups](TASKS.md). The Gainers implementation audit is dated historical evidence in [GAINER_GATING_AUDIT.md](docs/GAINER_GATING_AUDIT.md).

## Build and operate

Build the Rust CLI with:

```sh
cargo build --release -p thomas
```

The read-only portal listens on port `8790`:

```sh
target/release/thomas portal
```

The paper-bot Python package and pinned Moomoo SDK are in `papercompmoomoo/`. Use the host's existing service manager for normal operation; do not start a second executor alongside the installed one. See [papercompmoomoo/README.md](papercompmoomoo/README.md) and [new-machine notes](docs/NEW_MACHINE.md) before setting up another host.

Do not commit credentials, runtime databases, account snapshots, logs, or generated build output. Preserve local paper-trading evidence during repository cleanup.
