# Paper exit worker and price-line rules

Updated 2026-11-06. This describes the active Gainers and ORB exit path. The service/module name `codex_exit_worker.py` is historical: the worker skips discretionary model review for these two bots and applies deterministic price-line checks. Confirm runtime health on the host; this note does not prove a service is running.

## Active exits

**Hard rule for both bots: every exit line only ratchets upward and never moves down.** This applies independently to the red stop, midpoint, intermediate line, green 70% line, and 100% target. The worker persists them per bot, symbol, and position-open time, and each update takes the maximum of the newly calculated line and its saved value. The 70% line is 70% of the current full-target distance from entry, with its own upward-only ratchet. The 100% target exits on a fresh Moomoo trade touching or crossing upward through it. The 70%, intermediate, midpoint, and red stop exit on confirmed downward crosses. During regular hours the worker submits a simulated market sell; during extended hours it uses a limit at the current executable bid and reprices using fresh quotes. If no fresh executable quote is available, the exit remains latched and no stale-priced limit is submitted. At the regular-session open, a still-pending extended-hours exit is cancelled and replaced with a simulated market order. The 15:00 ET session flatten is one hour before the regular close. The ThomasAI liquidity-trap condition is an additional deterministic exit condition. Position age is not an exit condition.

- The red stop uses the position's configured stop/high-water rules.
- The 70% line moves upward with the current full-target line but is independently prevented from decreasing by its persisted ratchet.
- The purple midpoint is between that green line and the red stop.
- ORB keeps its levels in `data/orb.sqlite3`; Gainers derives them from the position, candidate terms, and Moomoo high-water candles.

The dashboard reads the levels computed by the worker from the local ignored `data/exit_levels.json` snapshot. That file is presentation data; it is not used to make exit decisions. Charts display price lines only for open positions with valid levels, and update their lines from the worker snapshot while the page is open.

## Code paths

- `papercompmoomoo/src/papercompmoomoo/codex_exit_worker.py`: reconciles both simulated accounts, computes risk levels, detects line/flatten/liquidity-trap conditions, and coordinates paper-only execution.
- `papercompmoomoo/src/papercompmoomoo/paper_exit_controller.py`: rechecks fresh quotes, current positions, pending sells, and broker fill records before and after submission.
- `papercompmoomoo/src/papercompmoomoo/exit_ownership.py`: enforces the configured simulated-account allowlist.
- `papercompmoomoo/tools/orb_paper.py`: updates ORB's high-water stop and target in its own ledger.

A submitted order is not a fill. Confirm exits from Moomoo deal records. Runtime status and logs are local and excluded from Git; check them on the host before relying on an active session.

## Learning boundary

Qwen is used only for retrospective learning/review from recorded candidate, decision, order, and fill data. Its output is not a live HOLD/EXIT decision and cannot submit orders. No model-training claim should be made without validated training artifacts and held-out session evidence.
