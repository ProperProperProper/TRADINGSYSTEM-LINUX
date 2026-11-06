# Moomoo full-universe collection: operational rollout and remaining limits

Date: 12 October 2026 (Australia/Sydney, AEDT)

## 1. Executive status

**The quote-only collection and historical-backfill schedules are installed and enabled. The paper bot has NOT been switched to the new store.**

This report deliberately distinguishes a working data producer from completed trader/research integration. The implementation grew beyond the smallest operational change. Following the user's scope correction, further consumer refactoring was parked; the delivery priority became enabling the tested collector, verifying the managed run, and documenting the exact remaining constraints.

Verified operational results:

- Every one of the **504 valid capture codes** returned current M15 data in the full-universe trial.
- That trial returned **503,182 native candles** in **397.8416 seconds**, across six owned subscription batches, without changing historical-quota usage.
- A separate integrated four-bar update populated the persistent store for all 504 codes in **397.9113 seconds**, with no rejected rows or missing latest bars.
- The earlier full raw capture was reused locally rather than downloaded again.
- A current-year import from the independent historical archive added **396,312** previously absent bars. The shared store then held **899,494 bars across 504 symbols**, with SQLite integrity check `ok`.
- Historical backfill's first managed run completed successfully, adding MSFT, GOOGL and GOOG. The separate long-span archive now holds **5,021,886 bars across 98 fetched-available histories**.
- Historical quota was independently read back as **98 used / 2 remaining**, preserving the configured reserve.
- The original paper executable, paper service, nightly collector and nightly service hashes remain unchanged. Paper remained active with the original process.

**Not completed:** full historical coverage for the intended universe, a full market-session shadow test, paper-source cutover, production research-source cutover, or a claim of improved strategy profitability.

## 2. Root cause and the practical fix

The existing paper service obtains new closed candles through Moomoo's historical-candle interface. The nightly research collector also repeats historical acquisition. Repeated requests for already-counted symbols need not consume another distinct slot, but they can refresh that symbol's rolling historical-quota timestamp. Therefore, retaining the same historical clients prevents an assumption that all slots will automatically become free each week.

Moomoo's **historical-symbol quota** and **concurrent subscription quota** are separate resources. The operational collector uses the documented subscription/current-candle route for routine updates. Historical requests remain a finite, quota-aware bootstrap operation.

No account upgrade, paid data subscription, funding operation or broker test order was performed.

## 3. Inventory and data contract

The official SPY holdings artifact is dated **8 October 2026**:

- 504 source holding rows, plus the SPY reference: **505 inventory entries**.
- **504 valid provider codes** after retaining one unresolved source identifier as an explicit exception.
- **503 strategy-expected codes** under the audited NYSE/Nasdaq-plus-SPY policy.
- `2602335D` remains unresolved. It was neither repaired into a guessed ticker nor silently removed from the source inventory.
- `US.CBOE` is captured, but remains an explicit exception to the existing exchange-eligibility policy. Capturing its prices does not change trading eligibility.

The stored contract is unadjusted M15, regular trading hours, with UTC **bar-open** timestamps. Actual current-candle time keys were verified as New York **bar-end** labels; normalization subtracts 900,000 milliseconds. Raw provider time keys and observation evidence remain retained.

OHLC values must be positive and finite; volume and turnover may legitimately be zero but must be finite and nonnegative. Off-grid, malformed and unsupported timestamp precision are not silently rounded onto a valid candle.

The calendar is the exact union of retained and freshly retrieved provider sessions: 2,235 sessions, with 194 overlapping sessions checked for agreement. It currently extends through 20 November 2026. It is not a generated weekday calendar.

### The new-listing case

`US.VYLR` returned 182 bars rather than 1,000. Moomoo's instrument metadata reports listing date **1 October 2026**, which agrees with the first returned bar at 09:30 New York on that date. Those bars have no internal gaps or normalization rejections and reach the common latest closed cutoff.

Verified pre-listing time is not missing market data. However, VYLR remains in the inventory and does not receive invented earlier bars. Its available data represents 14 H5 candles, below the unchanged 69-H5 score warm-up. The staged consumer work handles this distinction; it has not been enabled in production. Unknown or `1970-01-01` placeholder dates must never excuse truncated old-stock history.

## 4. Installed schedules

### Current-candle collector

Installed user units:

- `spy-etf-current-capture.service`
- `spy-etf-current-capture.timer`

Timer: `*-*-* *:00,15,30,45:10`, with one-second accuracy and persistent scheduling.

The runner uses the authoritative calendar. Already-current runs skip without opening an SDK context; ordinary off-hours runs skip, except the final-bar collection window immediately after the session closes. A successful skip is not represented as a new data receipt.

Routine requests start at four recent bars. Calendar-based catch-up sizing adds overlap/forming-bar allowance and is capped at Moomoo's 1,000-bar limit. Larger or unknown history gaps remain visible; they are not filled or claimed repaired.

Service interpreter and entry point:

```text
/home/nyx/projects/spy-etf-runtime/moomoo-sdk/.venv/bin/python
/home/nyx/projects/moomoo-market-data-refactor/scheduled_capture.py
--config /home/nyx/projects/moomoo-market-data-refactor/current-config.json
```

The persistent destination is:

```text
/home/nyx/projects/moomoo-market-data-refactor/data/candles.sqlite3
```

The first ordinary managed invocation returned `skipped_up_to_date` successfully. A controlled forced four-bar run under the same sandbox then completed with exit 0: **504 codes / 2,016 rows in 398.3246 seconds**, zero price changes on the already-current store, verified cleanup, and a durable successful status pointer. Historical usage stayed at **98 used before and after**, with no member or renewal-timestamp changes. Independent post-run readback confirmed **0 used / 100 available subscriptions** and **98 used / 2 remaining historical slots**.

At the final 17:14 AEDT readback, both timers were enabled and active. The next current-collector trigger was 17:15:10 AEDT; the runner will skip while the closed-market dataset is already current.

### Historical bootstrap

Installed user units:

- `spy-etf-universe-backfill.service`
- `spy-etf-universe-backfill.timer`

Timer: daily **17:00 America/New_York**, outside regular market hours. The next trigger was verified as **13 October 2026, 08:00 AEDT**. Missed-run catch-up is disabled so a reboot does not launch a long history job during the next market session.

Bounds:

- At least two historical-quota slots reserved.
- At most 12 new symbols allocated per invocation.
- A 1,200-second worker budget, plus explicitly bounded service supervision.
- Completed available histories are not downloaded again merely because the timer fired.
- Every new allocation is preceded by an actual quota check; another independent client can still change shared-account state between calls.

The first managed run finished with `Result=success`, exit 0, and application status `waiting_for_quota`: **98 complete available histories / 407 pending / 505 inventory**, with `finished=false`. Pending includes the unresolved source row; these counts do not claim 505 valid tradable securities.

Newly archived bars:

- MSFT: 52,108.
- GOOGL: 52,108.
- GOOG: 52,107.

All 3,000 recent overlapping rows for those three symbols exactly matched the current-feed store across OHLC, volume and turnover. Earlier SPY, AAPL and NVDA checks also produced exact 1,000-row overlaps per symbol.

### Coordination and sandbox

All cooperating OpenD collection jobs use:

```text
/home/nyx/projects/moomoo-market-data-refactor/opend-capture.lock
```

Future manual current captures must use the same project `trusted_root` as the scheduled runner, so the per-database lock namespace agrees. Early isolated trials used the parent projects directory; those invocations are finished and should not be restarted against the shared store with the old root.

The collector reserves observed subscription headroom, holds each subscription batch for at least 65 seconds after the subscribe response, and unsubscribes only resources confirmed as its own. SDK quota inspection and subscription are not atomic across independent clients.

Both managed jobs run as the existing user with restricted filesystem access, no new privileges, resource limits and a private umask. The SDK's log directory is explicitly writable. The live paper environment file is not loaded and is inaccessible from the service sandbox. Abnormal termination can still prevent Python cleanup; after a kill/OOM/stuck SDK, ownership must be checked rather than assuming broker-side release.

## 5. Persistence, import and verification

Operational Python components live under `moomoo-market-data-refactor/`:

- `probe.py`, `bar_contract.py`: SDK adapter and native-bar normalization.
- `rotation.py`: bounded owned-subscription rotation.
- `capture.py`: raw evidence, pinned cutoff and canonical-store ingestion.
- `store.py`: durable accepted bars, revisions and evidence.
- `snapshots.py`: explicit coverage evaluation and stable selected-input identity, separate from evaluation-time metadata.
- `scheduled_capture.py`: calendar gating, overlap sizing and status pointers.
- `import_history.py`: offline import from a verified SQLite snapshot and original completion receipts.

The historical worker remains in the independent `moomoo-universe-backfill-20261012/` project.

The shared store uses WAL and full synchronous persistence. It does not silently adopt a foreign database schema. Invalid accepted batches do not advance accepted bars; rejection evidence is retained. Reads are bounded to explicit symbols and requested windows rather than scanning the whole archive.

The historical import executed for **2 January–9 October 2026**, not the entire multi-year archive. It processed **494,312 rows in 588 bounded batches**, adding 396,312 previously absent values and reporting zero stale conflicts. Original completion receipts provide explicitly labeled conservative observation-time bounds; import time is not substituted for provider freshness. The separate complete available multi-year archive remains retained.

That import is a completed one-shot hydration. It is not an installed automatic bridge from every future historical-backfill run into the shared store.

Final operational Python test run:

```text
163 passed, 47 subtests passed
```

These are inert automated tests, not simulated market evidence. They cover normalization, quota/ownership cleanup, store integrity, bounded reads, explicit universes, raw-persistence failure, scheduling, path safety and importer recovery. Real API and database evidence is listed separately below.

## 6. What remains deliberately unchanged or parked

The live paper service continues using its original source. Its deployed binary and unit hashes were verified unchanged; the same is true of the old nightly collector and data-pull service. The canonical source checkout was not modified by this rollout.

An isolated worktree contains an opt-in Rust cache reader and service integration. A staged local-only research preparer also exists. These changes are **not production deployments** and are parked following the scope correction. Test success for those files is not a claim that paper/research now consumes the shared store.

Consequently:

- The old paper/nightly clients still make their existing historical requests.
- No claim is made that routine historical quota pressure has already been eliminated from all consumers.
- The new collector itself was verified to avoid historical-symbol charges.
- No strategy parameters, fees, execution timing rules or protective-order policies were deliberately changed in the running bot.
- No trader restart or source cutover was performed.

## 7. Remaining readiness constraints

The full recent capture provides **77 complete H5 candles for mature names**, while the live optimization contract requires **111**: 69 warm-up plus 42 test candles. The recent endpoint is therefore not an immediate substitute for deeper native history.

Using the supplied real session calendar, uninterrupted accumulation alone would reach the required window after **3 November 2026 at 16:00 New York**, equivalent to **4 November at 08:00 AEDT**, before collection/publication delay. This is conditional on gap-free future collection and no earlier full-universe historical completion; it is **not** a cutover promise.

Other explicit limits:

- Full market-session shadow validation remains outstanding. Closed-market retrieval does not prove live-cycle latency, corrections or reconnect behavior.
- Full intended-universe history, common cutoff and consumer-specific readiness must pass before the paper source changes.
- Historical completion currently means fetched available history plus final-page/action evidence, not automatic gap-free/backtest readiness.
- Membership and calendar inputs are currently explicit versioned artifacts, not a new automatic membership-refresh service. They require operator refresh; a stale calendar fails closed.
- Raw corporate-action metadata is retained, but is not automatically equivalent to complete entitlement normalization.
- Current holdings do not recreate historical point-in-time SPY membership or eliminate survivorship bias.
- A better data feed does not establish optimizer profitability or solve the previously observed fee-sensitivity problem.

## 8. Operational checks and rollback

```sh
systemctl --user list-timers 'spy-etf-*' --no-pager
systemctl --user show spy-etf-current-capture.service -p Result -p ExecMainStatus
systemctl --user show spy-etf-universe-backfill.service -p Result -p ExecMainStatus
journalctl --user -u spy-etf-current-capture.service -n 30 --no-pager
journalctl --user -u spy-etf-universe-backfill.service -n 30 --no-pager
```

Inspect `current-state/last_attempt.json` separately from `last_success.json`. Failure, busy and skipped runs do not become fresh successful captures. For history progress, inspect the backfill project's `status.json` and receipts; its quota snapshot can be the last pre-request observation, so obtain a fresh provider readback when an exact current balance matters.

To stop future automatic acquisition without changing the paper service:

```sh
systemctl --user disable --now spy-etf-current-capture.timer
systemctl --user disable --now spy-etf-universe-backfill.timer
```

Disabling timers does not by itself stop a currently active oneshot. Allow owned-subscription cleanup to finish normally when possible; after forced termination, verify ownership and release before retrying. Do not delete the data store, source archive or raw receipts as part of schedule rollback. The prior one-symbol history configuration is retained as `worker-config.pre-schedule.json`.

## 9. Local evidence index

Relative to `/home/nyx/projects/moomoo-market-data-refactor/`:

- `full-current-raw-001/report.json`, `verified-summary.json`, `cleanup-readback.json`: complete raw collection, normalization, counts and resource readback.
- `verification/capture-integrated-001-verified.json`, `capture-integrated-001-cleanup.json`: real incremental capture into persistent storage.
- `verification/hydrate-current-raw-001/report.json`: reuse of the full raw capture, exact counts and integrity.
- `verification/import-native-2026-001/receipt.json`: current-year historical hydration and immutable source provenance.
- `verification/backfill-managed-001-verified.json`: managed history acquisition, archive counts, parity and quota reserve.
- `verification/warmup-accumulation-bound.json`: conditional calendar-derived warm-up bound.
- `verification/current-input-copy-receipt.json`: exact hashes of the approved scheduler input copies.
- `verification/final-operational-readback.json`: successful sandboxed current capture, independent quota/cleanup readback, enabled timers and exact operational source/unit hashes.
- `operational-python-final.log`: final operational Python test output.
- `current-config.json`, `units/`, `current-state/`, `current-runs/`: deployed configuration, unit sources and runtime evidence.

Raw licensed market data, broker credentials, account identifiers and private environment contents are not uploaded with this report. Existing daily reports are not overwritten.

Selected operational SHA-256 identities:

```text
capture.py                  b9fcf68ca6f2aad63eef0548877d72f3e3ffce81e40bd1edda8b3ccbbf8a846a
scheduled_capture.py        dc2dad84e31926a12b925f2a15317d2d929fc51404c75c534f58e06c99a810d9
store.py                    eb155dde10c173433dca995bae7eb7612b059ce3ccd386c187ebf3afd513f491
current capture service     fd6fe9f489a0141f9ad022a1a0a6e1b7b024126d4bb2f4bb589eefbccf85e643
current capture timer       386b651ec97fd7b4be4efb2bcd686d3725a8b20924bc0888e11927fcd93e347f
historical backfill service  2588f4f6a6ee2593659e20fffa53441dccee32482b4df7aaa6f98ad351792703
historical backfill timer    f0f4cd06321df07fcf4e806e5a9546bcc95d19b09c89ea6f630d065b0a297cc1
```

Installed unit contents were hashed against their staged sources after installation. These standalone operational files are identified by hash; this report does not imply that the parked Rust worktree has been merged or deployed.

## 10. Provider references

- [Moomoo quota and authority rules](https://openapi.moomoo.com/moomoo-api-doc/en/intro/authority.html)
- [Current candlesticks: subscribed symbols, nearest 1,000 limit](https://openapi.moomoo.com/moomoo-api-doc/en/quote/get-kl.html)
- [Subscription and cancellation requirements](https://openapi.moomoo.com/moomoo-api-doc/en/quote/sub.html)
- [Historical quota inspection](https://openapi.moomoo.com/moomoo-api-doc/en/quote/get-history-kl-quota.html)
- [Historical candle requests](https://openapi.moomoo.com/moomoo-api-doc/en/quote/request-history-kline.html)
- [Official SPY holdings workbook](https://www.ssga.com/us/en/intermediary/library-content/products/fund-data/etfs/us/holdings-daily-us-en-spy.xlsx)

These sources establish API contracts. The account-specific coverage, elapsed times and cleanup claims above come from actual retained execution evidence, not documentation alone.
