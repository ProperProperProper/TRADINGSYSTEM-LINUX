# Paper-bot session monitor

This prompt is for read-only monitoring of the Gainers and ORB paper bots. It is not an order-management or repair authorization. The two strategies use one configured Moomoo `SIMULATE` `COMPETITION` account with separate strategy ledgers and a shared buying-power pool. Never put the numeric account ID in reports or Git.

## Source of truth

- Current strategy behavior comes from the active source under `papercompmoomoo/src/` and `papercompmoomoo/tools/`, plus `papercompmoomoo/STRATEGY.md` and `docs/SYSTEM.md`.
- `papercompmoomoo/data/BRAIN.md`, journals, session reports, and audits are historical evidence or unverified learning. Do not use them to set live rules, infer current account state, or override source/configuration.
- Gainers owns the Moomoo US hottest-gainers poll and publishes the fresh Nasdaq-filtered snapshot. ORB consumes that snapshot; do not issue a duplicate leaderboard request.
- The Overview dashboard is `/` at `http://127.0.0.1:8790/`. `/comp` is a legacy alias, not the operator entry point.

## Read-only checks

1. Check the installed user services and timers with `systemctl --user`; distinguish active executors from dashboards, Qwen, and standby services. Do not kill processes or start duplicate workers.
2. Inspect recent service status/logs and each bot's heartbeat. Treat local receive time as separate from provider quote/candle time; stale provider data is stale even if just received locally.
3. Compare each strategy's local status and position records with Moomoo `SIMULATE` broker positions and orders, using the configured account mapping. Do not print numeric account IDs or credentials.
4. Confirm Gainers' latest movers snapshot and ORB's consumption timestamp. If the snapshot is missing or stale, report it; do not make a second leaderboard request.
5. Confirm exit-worker heartbeat and its fresh-quote state. ETH exits require a current executable quote; stale book data must use the implemented Moomoo snapshot fallback, and must not be described as fresh solely because it was recently received. Confirm order fills from broker deal records; accepted orders are not fills.
6. Open the Overview and check current/most-recent session scope and displayed account labels. The symbol charts currently plot through 16:00 ET; do not claim they show 16:00–20:00 extended-hours candles unless the renderer has been verified to do so.

## If a fault is found

Report the evidence, timestamp, affected bot/account scope, and exact service or data fault. Do not submit, cancel, replace, or modify orders; edit strategy/risk settings; restart an executor; change owner rules; write to the learning brain; run tests or builds; commit; or push. Escalate any action requiring those changes to the owner. Preserve runtime databases, order records, and decision evidence.

## Report

Give a concise status for Gainers and ORB separately: service/heartbeat freshness, mover snapshot age and ORB consumption, broker/local position agreement, exit-worker/quote freshness, and dashboard state. Clearly label anything not verified.
