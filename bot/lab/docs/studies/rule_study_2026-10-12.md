# Rule study: absolute -8% block limit vs a limit relative to the current strategy (2026-10-12)

**Question (owner):** no entry gate validates, and 43 of 65 failed gate variants fail on the rule "no block may lose more than 8%".
The current strategy itself breaks that rule on cuts A, B and C (worst blocks -9.86, -9.47, -10.22). Would a relative rule
("worst block no worse than the current strategy's on the same cut") let genuinely better variants through? Tested, not assumed
(owner: "you need to test and validate not just change").

**Method:** `qwen-lab rule-study --run` (src/strategy.rs `rule_study`, `verdict_rule`). Every 5h variant at 5,000 USD that passes
cut A under the relative rule (32 of 180 screened) was backtested on all five block cuts (A = offsets 0,5,10; B = 2,7,12;
C = 1,6,11; D = 3,8,13; E = 4,9): 9 + 18 + 18 + 18 variant-cuts were new backtests (11 study runs, 96 min). Each cut was judged
under both rules; every other criterion unchanged (positive in both halves, beats the current strategy on the unseen half,
40% of blocks positive, 2+ trades per block, win rate not lower, profitable on the noisy tape). Full validation = at least 4 of
5 cuts.

**Result:**

| | absolute -8% rule | relative rule |
|---|---|---|
| variants passing 4 or 5 of 5 cuts | **0** | **0** |
| most cuts passed by any variant | 3 | 3 |
| cuts passed (A / B / C / D / E) | 22 / 7 / 7 / 5 / 4 | 32 / 15 / 15 / 5 / 2 |

Gain over the current strategy on the unseen half was positive for 32 of 32 candidates on cut A (where they were selected),
26 on B, 22 on C, 13 on D and 12 on E: the edge shrinks on the cuts the selection never saw and is below a coin flip on D and E.
Best by cuts passed (relative rule, 3 of 5): gate:ret+vwap (mean gain +1.80 points per block over 5 cuts), gate:ret+grade:riskdd
(+1.13), grade:risk+obv (+0.96).

**Conclusion:** the relative rule does not create a validated winner: on cuts D and E the current strategy's worst blocks are mild
(-7.41, -6.32), so there the relative rule is stricter than -8%, and most candidates also lose to the current strategy on those
cuts. The block rule is not what stands between the lab and a better strategy; no tested variant beats the current strategy
robustly across block starts. **Recommendation: keep the -8% rule unchanged.** The next gate needs a new mechanism aimed at the
drawdown blocks themselves (the entry-gating verdict's "WHAT BLOCKS GATES"), judged by the same full validation.

The new cut results are stored like any other run (statuses and full-validation figures on the dashboard now include them).
