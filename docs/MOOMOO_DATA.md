# Moomoo candidate-data contract

Reviewed: 2026-11-06. This document describes the active Gainers and ORB bots. Older Yahoo/common-universe research notes are not an active-feed requirement.

## Source and scope

- Moomoo is the source for gainer discovery, quotes, and the native one-minute candles used by active bots.
- Query Moomoo's US hottest-gainers leaderboard, filter for Nasdaq-listed symbols, then fetch candidate-specific quotes/candles and keep data current for open positions. Do not constrain candidates to SPY holdings.
- Do not bulk-fetch candles for other sessions. The Overview shows the current or most recent session, using locally retained data where available; it does not trigger a market-data request for a prior session. The current symbol-chart renderer plots bars only through 16:00 ET, even though exit monitoring can continue during extended hours.
- Candidate-specific Moomoo candles are written directly to the shared local SQLite store `papercompmoomoo/data/common1m.sqlite3`; the retired Yahoo/converter pipeline does not own or feed it. Each bot's journal and trade/account evidence remains in its own records.
- After 20:05 ET, the replay worker may request historical 1-minute candles only for symbols actually traded in that same session and only where local coverage has gaps. It checks Moomoo's seven-day historical-candle quota first, spaces requests below the documented frequency ceiling, and stores the resulting coverage and replay output locally. This is an after-hours recovery path, not a live-session poll.
- Replay input is scoped to actual filled positions for that session, and replay exits use only the current ratcheting price lines plus the existing 15:00 ET flatten. Missing candles or invalid entry levels remain visible as gaps and do not get filled from another provider.

## Time and candle interpretation

All market sessions use `America/New_York` (ET). Premarket is 04:00–09:30 ET; regular trading is 09:30–16:00 ET; extended trading is 16:00–20:00 ET. The current symbol charts plot through 16:00 ET; extended-hours exit monitoring does not mean extended-hours candles are currently plotted. Store timestamps as Unix milliseconds with their source convention intact. Moomoo's native one-minute `time_key` labels the candle end. A chart maps that observation to the one-minute interval ending there, shows the interval in ET, and retains the provider timestamp in UTC. Pushed bars must be grouped by `time_key`, not by network receive time.

Trading signals must use the strategy's required closed-bar evidence and reject stale, missing, malformed, or future data. Entry approval additionally requires a current provider-timestamped quote and executable book. Candle capture for identified movers may continue to preserve candidate evidence while a quote is waiting; such candle data does not make a stale quote executable. A chart may display a forming/current observation only when it is timestamped as such; it must not make a prior-session observation look current.

## Ownership and preservation

Gainers and ORB share one Moomoo `SIMULATE` `COMPETITION` account and the live buying-power pool. They share the Moomoo movers snapshot and market storage, but not candidates, strategy orders, positions, or trade outcomes. Account equity is common; charts and trade reporting remain strategy-specific. Keep account identity attached to all reporting.

Runtime databases, raw feed captures, and immutable decision/fill evidence are ignored by Git and must be preserved during cleanup. Never treat them as disposable build caches.
