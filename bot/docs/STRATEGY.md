# The live strategy: what it is, and how it changes

## What trades now

| Part | Live setting | Where in code |
|---|---|---|
| Market | SPY's holdings with candles (moomoo quota: about 95), SPY as the trend reference | `data/spy_holdings.csv`, `REFERENCE` |
| Candles | All supported intervals built from real 15-minute bars | `Interval::LIVE` |
| Account | 5,000 USD inside the moomoo SIMULATE (paper) account; long only, cash only, whole shares | `START_CAPITAL` |
| Basket | Exactly 1 position, sized from available cash; no trade below 50% of its slot; excess legacy positions close only at break-even/profit | `LIVE_TOP`, `MIN_SIZE_FRACTION` |
| Entries | Stocks ranked by a signal; buy the best-ranked while SPY is above its trend average (margin searched) | `engine/xs.rs`, `engine/walkforward.rs` |
| **Promoted entry options** | **none yet** (the plain strategy) | **`engine/live_options.rs` `LIVE`** |
| Settings | Every search tests 200,000 random combinations on the latest 14 sessions; highest safe P&L wins, ties prefer shorter holding periods; nothing hand-fixed | `walkforward::search_random`, `choose_live` |
| Exits | Frozen by the owner: searched take-profit (70% used), stops, 25% drawdown exit | never changed by research |

## How it changes (automatic, owner's rule 2026-10-10)

1. **Testing:** the root deterministic testers (`examples/variant_study.rs`,
   `examples/interval_study.rs` and the service's interval search) and
   walk-forward engine) test variants of the CURRENT strategy: at most 2 added entry options, on 2026 data, in
   14-session walk-forward blocks (each block's settings searched on the sessions before it), judged on the half of the blocks
   no agent saw. The live service compares all interval candidates; `examples/interval_study.rs`
   remains available for independent research runs.
2. **Qualifies to go live** only when ALL hold (the validation code in this repository):
   - considerably better on block cuts A and B: unseen-block return at least +1.0 point per 14-session block above the current
     strategy on both, trade win rate not lower, positive on the noisy tape (jittered prices, costs x1.5);
   - **full validation:** the same test on all 14 possible block offsets (cuts A to E); at least 4 of the 5 cuts pass. This
     protects against a result that only exists because of where the block boundaries fall (2026-10-10: `obv` was +1.1 and
     +1.9 points better on cuts A and B but failed cuts C, D and E (2 of 5), so it did NOT go live);
   - on the selected live interval, with entry options only (never signals, direction, exits, universe).
   It does not matter who found it (code or an agent): the owner chose not to approve each one.
3. **Applied deliberately, never mid-session:** a reviewed code change is tested and
   deployed only from a clean repository.
   The session changes `LIVE` in `src/engine/live_options.rs` with a comment (what differs, the numbers, why), adds an entry
   below, runs the tests and clippy, deploys (`./deploy.sh` restarts the bot while the market is closed: paper state, records
   and open positions persist; exits are untouched), commits and pushes. The job then verifies the commit and reports to the lab.
4. **Everything follows the new strategy:** the live search and therefore the paper account, the console's stability and
   walk-forward tests, and the backtester (`examples/variant_study`, whose "baseline" is the live strategy) all build their search
   space with `live_options::space`. Settings recorded before a promotion that lack the new option are no longer re-tested. The
   lab tags results with the live strategy they were measured under and re-measures the new current strategy.
5. **Undo:** revert the promotion commit (`git revert <commit>`) and deploy while the market is closed; the lab marks the
   promotion `reverted` with `qwen-lab promotion-status <variant> reverted <why>`.

## Change log (newest first; every change of `LIVE` adds an entry)

| Date (UTC) | Options before -> after | Variant (found by) | Full validation (unseen %/block vs previous strategy) | Why |
|---|---|---|---|---|
| 2026-10-10 | none -> none | - | - | Historical validation report; nothing qualified. The `obv` and `dca:on+grade:rank` candidates failed full validation and were not promoted. |
