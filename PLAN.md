# TradingSystem project plan

Reviewed: 2026-11-08. This plan describes the running paper-trading project. It is not the retired ThomasAi/legacy provider roadmap preserved in Git history.

## Current operating system

TradingSystem runs two Moomoo US simulated strategies against one configured `SIMULATE` `COMPETITION` account. Gainers owns the US hottest-movers scan, filters the Nasdaq list, and publishes a short-lived local snapshot. ORB consumes that snapshot; it does not run a duplicate broad mover scan. An atomic allocator shares available buying power and prevents both strategies from claiming the same symbol. Strategy candidates, orders, positions, trades, and evidence remain separate.

- **Gainers:** discovers movers during premarket and regular hours; fetches data for identified movers and open positions; enters only after its Nasdaq, liquidity, price, volume-pressure, CLV/Kalman, account, and fresh-quote checks pass. Premarket orders use the verified ETH session; regular-session orders use RTH.
- **ORB:** prepares candidates from the shared mover snapshot before the open; enters only in its configured RTH opening window after opening-range, VWAP, volume-pressure, and confirmation checks pass.
- **Exits:** the exit worker applies the deterministic ratcheting stop/target lines, confirmed price crossings, the audited ThomasAI liquidity-trap condition, and the 15:00 ET flatten. It does not use discretionary AI exit review for these two bots. ETH exits require fresh provider-time executable quotes; stale data cannot be made fresh by local receipt time.
- **Market data:** Moomoo is the active bots' source. Candidate-specific native one-minute candles are written directly to the local SQLite store. The old legacy provider/converter feed is not part of live bot operation. Runtime databases and immutable evidence are local operational state and must be preserved.
- **Dashboard:** the Rust portal is read-only. Overview displays the shared account summary followed by current or most-recent-session charts for each strategy. The current symbol-chart renderer stops at 16:00 ET, although exit monitoring can continue during extended hours.

See [`README.md`](README.md) and [`STATE.md`](STATE.md) for operating details. These do not replace live checks of Moomoo, service state, or broker fills.

## Priorities

### 1. Session readiness and safe execution

- Reconcile broker positions, working orders, fills, account identity, and strategy ownership before permitting entries after startup or reconnect.
- Confirm premarket mover discovery starts at 04:00 ET and ORB consumes the same-session snapshot before RTH.
- Keep quote, book, candle, and order-list traffic within Moomoo quotas. Share discovery/data locally where possible; retain candidate-scoped subscriptions and the shared `order_list_query` limiter.
- For stale mover recovery, refresh the identified mover and accept it only with provider-timestamped fresh data. A fresh two-sided Moomoo book can recover a stale last-sale field by using its midpoint; if both quote and book are stale, request one candidate-scoped batched market-snapshot fallback. Stale pre-market price is never reused. If the fallback is also stale or lacks a valid two-sided quote, the candidate remains waiting.
- Reconcile terminal broker-confirmed fills idempotently by broker order id; preserve partial dealt quantity and average price. Acceptance or a working order is not a fill. Continue checking order and position frequency errors from telemetry.

### 2. Make exits match the stated line contract

- Keep every exit deterministic: upward-only ratchets, provider-time quote validation, confirmed downward crossings, THOMAS liquidity-trap trigger, and the one-hour-before-close flatten. No position-age exit or discretionary AI exit decision.
- Every line, including 70%, ratchets upward only. The worker persists each level per bot, symbol, and opened position; 70% is recalculated from the current 100% target and maxed against its previous value. The fixed-original-target interpretation is not the active rule.
- Compare the displayed 100%, 70%, intermediate, midpoint, and stop values with the worker snapshot for each open position. A held position without valid published protection lines is a fault.

### 3. Improve evidence and chart acceptance

- Preserve timestamped detection, approval, decisions, market-data source/provider times, order lifecycle, cancellations, and broker-confirmed fills for later review.
- Keep candidate and trade data strategy-specific while account equity and buying power are labeled as shared.
- Confirm charts use current New York session data, correct ET intervals, source/provider timestamps, account labels, and live price lines without mixing unrelated sessions.
- Continue the shared research replay only after the full Moomoo US extended session closes at 20:00 ET (or on weekends). It replays candidate-scoped synthetic-GEX breakout entries plus GEX and deterministic price-line exits, while also replaying actual filled positions with recorded entry levels. The replay code refuses weekday 04:00–20:00 ET, including premarket and regular hours. Use Moomoo bars; report missing tick/book evidence and uncertain fills instead of fabricating them. Do not use the replay as a profitability claim.

### 4. Qwen learning is shelved

- Qwen learning, adapter review, and Qwen corpus refresh are disabled indefinitely. Trainer, MCP, review, and corpus-refresh code has been removed; model files, datasets, adapters, and immutable records are retained. The local inference engine is separate from learning and remains installed.
- No agent/MCP/Antigravity Qwen training workflow is active or authorized by the current operating plan. Any future restart requires an explicit change to this plan.
- The recorded-trade replay is the research evidence path. It uses each bot's own traded sessions, actual entries/levels, New York session dates, and Moomoo minute bars; same-minute OHLC ambiguity is treated conservatively. Tick/book fills remain unavailable and are never claimed as exact.

## Completion criteria

The project is ready for a session only when both strategy processes and the exit worker are current, the shared Competition paper account and market session are verified, positions/orders are reconciled, required line snapshots and data feeds are current, and no known API frequency errors are recurring. Session results are complete only after broker deal reconciliation and immutable evidence export. Research or training is complete only when source data, session splits, artifacts, and independent evaluations are recorded.
