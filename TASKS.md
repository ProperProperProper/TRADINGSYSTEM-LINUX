# Current operational checks

Reviewed: 2026-11-06. Implementation work reflected here is checked against the working tree. Runtime-dependent checks remain conditional on Moomoo returning fresh provider data and the US session being open.

## Completed implementation

- [x] Every exit line, including the 70% line, is persisted per open position and ratchets upward only. The 70% line follows 70% of the current full-target line and cannot move below its previous value.
- [x] Stale mover recovery actively refreshes identified movers and can use a provider-timestamped two-sided Moomoo book midpoint when the last-sale quote is stale. It does not substitute local receipt time or stale pre-market price. If Moomoo returns stale quote and book data, the bot waits.
- [x] Shared research replay uses New York session dates, actual traded symbols/sessions and each bot's recorded entry levels; it avoids using the same minute's high to ratchet a line before evaluating that minute's low.
- [x] Research replay is blocked during weekday premarket, regular, and extended sessions; it can run only after 20:00 ET or on weekends and includes today's session only after the extended close.
- [x] Live Gainer and ORB data ingestion is Moomoo-only. Removed the dormant Yahoo fetch/normalization/history-import path and the duplicate ThomasAI live mover scan; the brain consumes the bots' shared local candidate snapshots.
- [x] Qwen learning/review/corpus jobs and trainer/MCP entrypoints are removed. Existing model/data/evidence files and the separate local inference service are retained.
- [x] Moomoo `order_list_query` calls are serialized through the shared rolling budget limiter, which reconciles unbudgeted recent calls from append-only telemetry. Telemetry records waits, frequency errors, and method calls.
- [x] A dedicated Gainer Tracker tab shows all Nasdaq rows captured from each Moomoo top-200 US movers response, detection time, Gainer approval/rejection when evaluated, reason, and best observed post-detection return. Live Gainer screens every Nasdaq row in the returned 200 instead of truncating the global ranking at 50. Its low-priority capture process reads existing local snapshots, the Gainer journal, and native candle store; it makes no Moomoo/broker calls and never writes bot state. Outcomes remain a hindsight ceiling, not assumed fills or position sizing.

## Runtime evidence still needed

- [ ] During the next live US session, verify mover recovery end to end when Moomoo supplies a fresh book but stale last-sale quote; confirm source/provider timestamps and admission evidence in the immutable log.
- [ ] Review terminal broker order records against the immutable fill journal for every new exit, including canceled-partial orders. The source records actual dealt quantity and average price once per terminal order.
- [ ] During active load, confirm `order_list_query` remains inside the 8-call rolling 30-second budget and no broker frequency-limit errors occur. Position-query calls are cached by the exit worker; ORB and Gainer account reads still need telemetry review.
- [ ] For each open position, compare the chart's 100%, 70%, intermediate, midpoint, and stop with the persisted worker snapshot. This requires a live position to be open.
- [ ] Re-run and review the recorded-session replay after each strategy change. OHLC replay is an estimate; historical executable bid/ask sequences are not retained, so exact fills and the liquidity-trap overlay cannot be reconstructed.
- [ ] After this US session closes, confirm the opportunity capture includes final available Moomoo candle highs and review symbols whose price path has only leaderboard snapshots. This hindsight ceiling can miss intrainterval highs when no candidate-specific Moomoo candle was already stored.

Qwen learning remains indefinitely disabled. Re-enabling it requires an explicit plan change.
