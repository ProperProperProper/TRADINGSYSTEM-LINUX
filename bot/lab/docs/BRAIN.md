# The lab brain: Rust candle learning plus a checked online-model ensemble

Current architecture (2026-10-18): the hourly strategist load-balances across OpenCode free online models. Two independent models must agree; Claude validates when available, otherwise `opencode/deepseek-v4.1-flash-free` decides. Unattended automation never launches Codex. Every model receives the SAME persistent memory. Models receive only valid untested choices, training-side stored-candle evidence, and durable verified lessons. The tool guard validates requests, the strategy loop tests them, and deterministic walk-forward outcomes update SQLite. Models cannot place orders, promote themselves, alter exits, or use live results as winner labels.

The durable brain is split by purpose: `online_turns` is the complete decision/action audit trail, `online_memory` stores verified training-side experiment outcomes, `online_training_skills` learns per-option evidence counts, wins, losses, confidence, guidance and reuse, and `online_brain` stores the current objective and state summary; `online_model_responses` keeps every model answer. Every strategist turn retrieves these stores plus the existing `brain_facts` lessons and gives them to each model, so the ensemble's memory is shared, not per-model. Test completion updates memory and skills from paired clean and noisy training evidence only; candidate/validated status and other holdout-derived fields never enter this feedback. The old `online_skills` and pre-v2 model responses are preserved for audit but excluded from decisions. Model prose never promotes itself into trusted memory—only deterministic lab measurements do. Old `qwen_*` tables are retained as a recoverable archive.

Owner directive 2026-10-13: *a narrow model, specific to this bot, its trading and its learning; machine learning allowed; it must do everything Qwen,
Honcho and Hermes did here, so those can be removed.* Numerical learning and every validator remain Rust (`src/brain.rs`, `src/brain_roles.rs`, `src/mltrade.rs`, `qwen-lab brain ...`). The brain runs on the CPU as an always-on process (`qwen-lab brain serve`, service `com.qwenlab.brain`).

## What was retired (2026-10-13) and where it went
| Retired | Why | State |
|---|---|---|
| Honcho (shared memory) | already gone before this work | nothing left |
| Hermes (six tool-using language-model agents) | the brain does their jobs | **uninstalled**: ~/.hermes (7.5 GB), its launch agents, its review job. Profiles, memories and sessions archived in `qwen-lab-stocks/archive/hermes/hermes-final-20261013.tar.gz`; lessons and skills **imported** into the brain's knowledge store |
| Local Qwen models | replaced by the checked OpenCode free-model ensemble | service, weights, adapters and training environment removed 2026-10-17 |
| GRPO training of the language model | modest, mixed result for an hour of downtime per run (below) | **removed**: job, Python trainer and its environment |
| The old lesson pipeline (hypotheses, replicated lessons, tweaks, the Q&A corpus, the LoRA training gate, claim checks of the agents' memories, the Ollama agents) | it served the language models only; the brain, the ML model and the strategy loop do not use it | **removed from the code** (2026-10-13); its old tables stay in `memory.sqlite3` untouched, nothing reads them |
Reverse any of it only on the owner's decision: reinstall from git history and the archive.

## The six roles (what the Hermes agents did, now code plus the small models)
Each role works through the SAME lab tools (`lab_queue_test`, `lab_note`, `lab_new_results`, `lab_record_idea`, `lab_whiteboard_edit` ...) as author
`brain-<role>`, so validation, the honesty gate, the scoreboard and the dashboards treat it exactly like an agent. One role runs every 10 minutes in rotation.

| Role | What it does | Status |
|---|---|---|
| strategist | queues untested variants under weighted policies (`build` 3 of 7 turns, then `heuristic`, `ucb`, `additive`, `random`), acts on the ML lead (`ml-lead`: queues the entry gate `mlgate` and pairings with training-half multiplicity survivors once each), keeps the shared plan true, reads its inbox | live |
| tuner | queues `tune:margin` / `tune:dca` bands from where the most profitable training runs set them, only when they differ clearly from all runs | live (no band qualifies today) |
| researcher | rotates through 20 market-research topics, searches the literature through the guard, records an idea only when the paper uses the topic's own terms | live |
| auditor | notes fragile results (positive clean, negative noisy) and results with too few trades | live |
| statistician | passes on the quant verdict, flags validated/candidate results that are not consistent across blocks, runs the daily pre-registered ML tests (trades, candles with fees) and the daily improvement round, tells the strategist when the ML verdict changes class | live |
| execution | reads the bot's real paper decisions, reports the rejection reason the bot itself gave, adds the ML model's view of the rejected names (top/bottom half at the newest scored bar), queues one test when the regime filter blocks most entries | live |
| knowledge | every note, idea, plan and lesson is a row in `brain_facts` (repeats suppressed); 117 lessons + 5 agent-made skills imported from the Hermes archive; `brain lessons <words>` | live |

Not replicated on purpose: the agents' free-text memory and the rewording of highlight explanations (the lab's code-written ones stay).

## The agents that DO things, and learning 24x7 (`src/brain_agents.rs`, 2026-10-13)
Six more roles join the six reporting ones; twelve take turns in the one always-on process, **a role every 5 minutes, day and night, weekends included** (each role
hourly; the process runs at low priority). Every action is logged as a knowledge-store fact, goes through checked code, and is undone by deleting a plain file. None
touches the live strategy, an account, an order or an exit.

| Agent | What it does | Undo |
|---|---|---|
| data | imports the bot's candles and the SPY holdings list every pass; reports bars stored, holdings with candles (79 of 502; moomoo allows 100 symbols per 7 days and 96 are used, so the other 423 cannot be pulled and the lab never spends that quota), stale symbols, thin coverage, quota, how old the newest bar is; alerts Discord when symbols go stale or bars are over 3 days old | none needed (read/import only) |
| steward | owns the ML model: re-scores whenever a newer bar exists, writes the score file the bot's backtester reads, keeps the model card, and watches the last 15 sessions' out-of-sample IC: switches the model OFF (file `ML_GATE_OFF`; the score file is then empty, so no gating) when the IC is significantly below zero (mean + 1.96 se < 0, at least 10 sessions) and back ON when it is above zero again | delete `ML_GATE_OFF` |
| drift | compares the paper bot's real fills with the candle bar opens (observed cost against the cost table; alerts when it is over twice the model's) and how the model rated the entries the bot took | none (read only) |
| risk | watches the paper account against the owner's rules (drawdown cap 25% -> warns at 20%, worst 14-session window -> warns at -6%, positions 2) using only rows of the current capital | none (alerts) |
| promotion | writes an evidence packet (`brain/reports/promotion-*.md`, with the rollback) for every variant that passes the lab's full validation, lists the ones that need a code review (research switches such as `mlgate`) | the promotion job itself is unchanged |
| experimenter | finds where a lead loses to the baseline block by block (`mlgate`: 6 of 29 blocks by more than 2 points) and runs one standing ML experiment per turn (training windows 63-210 days, recency half-lives 21-168 days; each a ledger trial, paired and adjusted, the least recently run repeated as data grows) | none |

**Learning 24x7.** New bars (any hour the bot's store grows) trigger: the data agent's import, the steward's re-scoring and recent-IC check, and the improvement
round (recency schemes, extra features, meta-labelling); with no new bars the experimenter's standing experiments keep going. An adopted scheme (paired difference
above zero at the multiple-testing-adjusted level) is picked up by the scorer from the ledger (`mlscores::Scheme::from_ledger`) without a code change.
**First steward reading: the last 15 sessions' out-of-sample IC is -0.106 (standard error 0.068): negative, not yet significant, so the model stays on. This is the
same fading the first-half/second-half comparison of the gate showed.**

## The packaged model, constant testing, and the neural network (`src/aimodel.rs`, `src/nn.rs`, 2026-10-13)
**One versioned model** (`qwen-lab model train | maintain | versions | predict | explain <SYMBOL> | scoreboard | card`): a JSON artifact per version
(`<lab>/model/v<N>.json`: the fitted ridge, 44 feature names, training window and scheme, a decile CALIBRATION table measured out of sample on a held-out tail and made
monotone by isotonic regression, held-out IC), a registry with a current pointer, live inference on the newest unlabelled bars (per stock: score, percentile in the
bot's top 20, calibrated expected excess return and hit rate, the three features that drove it), and a lifecycle: a new version every 14 sessions of new labelled data,
every version's out-of-sample IC after its training end tracked per session (`model/monitor.json`), and automatic rollback (plus Discord) when a newer version is
significantly worse than its predecessor over the same sessions. First version: 98,218 samples through 2026-10-09, held-out IC +0.033; calibration 0-40th percentile
-5.8 bps, 90-100th +29.4 bps per session before costs.
**It tests itself constantly.** Every live ranking (all 20 calls) is stored in the prediction ledger (`ml_predictions`) when it is made; each call is scored against the
real outcome as soon as the next 26 bars exist, and the model's REAL record (rank IC with standard error, realised bps of its top 3 against its bottom 3, against
what it expected) is stored as a fact, shown by `model scoreboard` and in the daily digest. The steward does this every time new bars arrive, day and night; the
experimenter runs a standing experiment every turn; the improvement round runs on every new bar; the neural-network trial runs about daily.
**Neural network** (`nn.rs`, written from scratch: two tanh layers 24-12, Adam, L2 weight decay, early stopping on the last fifth of the training period, a 3-seed bag;
tests: it learns y = sign(x0 x1), which a linear model cannot, and invents nothing in pure noise). Against the ridge on the lab's data, rolling windows, paired and
adjusted for 22-23 trials: **MLP alone IC +0.044, paired vs the ridge -0.029 [-0.109,+0.041]; 50/50 blend IC +0.060, paired -0.013 [-0.070,+0.036]: no evidence it helps.**
It stays in the daily trial; if the ledger ever adopts the blend, the scorer and the artifact use it automatically (`Scheme::blend_mlp`).

## Trading intelligence, inspired by NVIDIA's "AI trading factory" page, in-house (`src/intel.rs`, `src/portfolio.rs`, 2026-10-13)
The page is a GPU/LLM marketing pitch (multimodal data into embeddings, learned alpha backtested on generated scenarios, adaptive execution, cuML/cuOpt/cuFOLIO/NeMo/NIM).
Most of it needs a data centre and data we do not have; what we recreated, all in Rust on the CPU, on real data:
* **Risk model** (volatility forecast, HAR-style ridge on log realised volatilities of SPY): out of sample it has **41.5% lower squared error than the naive forecast, interval
  above zero over 28 weeks (correlation 0.42 vs 0.25)** -- a real, proven result. **Direction model** (will SPY be higher after the next session): out-of-sample **AUC 0.447
  [0.391,0.504] = no skill**, and the report says direction is not predicted.
* **Mean-CVaR optimiser** (the cuFOLIO idea; `portfolio::mean_cvar`, scenario-based, projected subgradient, constraints, tested on a fat-tailed vs calm asset). **It does not apply
  as portfolio selection: the bot holds 2 positions and ANY symbol whose signal fires first takes a slot (owner), it does not pick "the best 2 of today's top N".** My first test
  assumed daily top-5 picking and is withdrawn (it showed +43.8 vs +8.8 bps/day, interval -16.6..+90.7: inconclusive anyway). What applies is `slot_scale`: a triggered stock's
  position shrinks (down to half) in proportion to how much wilder its own real tail (CVaR 90% of one-session returns) is than the median stock's.
* **Per-signal decision** (`intel::assess`, `qwen-lab model decide`): for every stock the model ranks, "if its signal fires, take it or skip it?": take = SPY above its 130-bar
  average, volatility not in the top fifth of history (else half-size slots), the model's recent record not significantly bad, and the calibrated expected return beats the stock's
  measured round-trip cost at the slot size (a 2,500 USD slot pays about 14 bps). Output: take list with sizes and net bps, skip list, stance, confidence (UNPROVEN / PROVEN LIVE
  / LOW from the model's own live scoreboard).
* **The model as a filter on the bot's real entries** (`model trades`, 28,148 backtest entries scored by walk-forward models, fees inside each trade's return): skipping the lowest
  20% / 35% / 50% by the model's percentile changes the mean return per trade by **+0.041 [-0.060,+0.125] / +0.066 [-0.092,+0.192] / +0.014 [-0.184,+0.169] points**, better in 36 /
  37 / 34 of 60 blocks, win rate 44.2% -> 44.6% / 45.1% / 44.5%. Consistent in direction (about 60% of blocks), **not significant** -- the same lean as `mlgate` in the bot's engine.
* Not recreated: multimodal/news embeddings (no such data here), GPU kernels (CPU only), generated synthetic scenarios
  (results must use real data; scenarios here are real joint returns).

## Deep learning that trains and improves itself 24x7 (`autodiff.rs`, `indicators.rs`, `deepnet.rs`, `automl.rs`, job `com.qwenlab.trainer`, 2026-10-13)
Owner: "a small network?? make it as advanced as possible", "train 24x7", "self improve 24x7", "historical data, backtest data, the candles themselves, indicators themselves", "no fake stuff".
* **An autodiff engine written from scratch** (reverse mode, f64): matmul, GELU, softmax, layer norm, attention pieces, dropout, a Pearson-correlation loss; every operation is checked
  against numerical gradients (a full attention block with layer norm included).
* **In-house indicators** (34 channels computed from the lab's own bars, causal and scale-free, a test rewrites the future): RSI, MACD line and histogram, Bollinger %b and
  bandwidth, ATR, stochastic K and D, CCI, ADX and DI spread, OBV slope, MFI, Chaikin money flow, session VWAP distance, Donchian position, ROC, Aroon, EMA ratios, Keltner
  position, candle body and wicks, engulfing, doji, inside bar, gap, volume z-score, range expansion, time of day, streak.
* **Two real architectures**: a residual network (pre-layer-norm blocks, GELU, dropout) on the static features, and a **Transformer that reads the stock's last 8-48 bars of candle
  and indicator channels** (learned positions, multi-head self-attention, residuals) together with the static features. Trained to maximise the CROSS-SECTIONAL rank correlation
  inside each moment (1 - Pearson, plus a small squared-error anchor), AdamW with warm-up and a cosine schedule, gradient clipping, dropout, data-parallel threads, early
  stopping on the latest fifth. Unit tests (made-up inputs, to check the code only): the residual network learns a nonlinear rule a linear model cannot, the Transformer finds a
  pattern in time, results are deterministic and thread-count-independent.
* **Self-improvement, around the clock**: the trainer is its own always-on job (lowest priority, 3 threads, paused only by the switch files STOP / MAINTENANCE / `trainer/OFF` and
  a Mac with more than 1.5 load per core; it is niced to the lowest priority and uses two threads instead of three while the US market is open). It never waits for new data: it works on what is stored and picks up newer bars when they come. It runs an EVOLUTIONARY SEARCH over architectures and training recipes (the genetic-algorithm idea of *The Nature of Code*): a population in SQLite, the first
  eight genomes random, then tournament selection among the best five, mutation (1-3 genes) or crossover, never the same genome twice. Each child is trained and tested on the last
  three real 28-day out-of-sample windows, compared with the ridge on the same windows (paired, week-resampled, widened for the number of trials in the ledger). A genome is a
  CHAMPION only if its paired gain is above zero at that level; the champion is a BAG of 7 models of the same genome trained with different seeds, one more seed per trainer turn on the EXISTING data (it improves without waiting for new bars),
  rebuilt seed by seed whenever a new session of bars arrives, **re-tested on fresh out-of-sample windows every hour, and DEMOTED (Discord told, bag removed) the moment it
  stops beating the ridge**. The bag is blended into the live ranking (every member and the ridge standardised within the moment, averaged). Between genomes the worker also runs one
  cheap ridge experiment (a new training window or recency half-life, generated without end and never repeated) so the existing data is questioned at the trainer's pace. Everything is on disk (population table, trial ledger, champion file); a restart loses nothing.
* **Review fixes (2026-10-13)**: the champion bag is replaced by building the next bag beside the live one and swapping it in whole (the live ranking never loses its champion while it is rebuilt); bag files are written atomically with stable indices and a damaged file is skipped; the live blend uses only members of one genome; generated experiments are capped at the available history; the trainer and the experimenter turn no longer run the same experiment at once. The seed fix (seeds 0 and 1 had collided) changes every stream: results recorded before it were produced with the old streams (no champion existed, so nothing validated is affected).
* **Real-data check**: the test suite runs the evaluation on the lab's real candle store (read-only) with labels shuffled within each moment; it must find nothing (|IC| < 0.06,
  no gain over the ridge). It does.
* **First real genome** (a 2-layer Transformer, d32, 32 bars, 18 epochs, 184 s on 3 real out-of-sample windows): IC -0.007 [-0.100,+0.083], paired vs the ridge -0.080
  [-0.241,+0.048] at 34 trials: **no evidence**. The search keeps going; nothing is blended in until a genome earns it. The earlier 24-12 MLP (`nn.rs`) is superseded.
* Learning cadences are continuous, not daily: the model version advances every new session; the steward re-scores and re-briefs whenever a new bar arrives; the improvement round
  runs on every new bar; the experimenter runs a standing experiment every turn; the trainer never stops. Only the Discord digest is daily.

## Real-data findings (no synthetic data in any result; `examples/brain_eval.rs`, `examples/mltrade_eval.rs`)
**Choosing variants.** As analysed on 2026-10-13: 194 tested 5h variants at cut A, 24 passed the holdout (12.4%); the lab keeps testing and the dashboard shows the live counts. Fits use the TRAINING half only; the holdout only evaluates.
* Models predict training-half gain weakly (rank correlation 0.2-0.4); the plain average of each option's past results beat a Gaussian process and ridge.
* Ranking by predicted training edge does **not** find holdout passers: the top quarter passes 6-13% against 15-20% for all later variants, for every target tried.
* The old Hermes strategist **was** better than chance: 8 of 28 requests passed (29%) against 12% overall. The brain does not claim to match it; the four
  policies (heuristic, ucb, additive, random control), are compared prospectively with `qwen-lab brain report` as results arrive.

* A replay once suggested building on options from earlier candidate/validated variants, but those labels include holdout evidence and therefore cannot select later tests without leaking the evaluation set. Policy **`build`** now uses only options from variants that clear the quant report's paired, multiple-testing-adjusted TRAINING-half screen, ranks combinations by past training gain, and gets three turns in seven; the four other arms remain, including the random control. `brain report` shows whether it holds up prospectively.
**Machine learning on trades.** 40,076 long trades of the unselected backtest runs, 18 entry features each, 68 blocks; train on the earlier two thirds of
the blocks, judge the later third once; whole blocks are resampled for the intervals (runs replay the same blocks, so trades are not independent); a
shuffled-label control must score 0.5 (it averages 0.51). Models are self-written: logistic regression and gradient-boosted trees (win/lose and return).
* Raw win/lose: AUC 0.48, no skill (blocks win and lose with the market).
* Relative to the block: win/lose trees AUC 0.56; return-trees gain **+0.11 points per trade, interval [-0.06, +0.31], better in 13 of 23 blocks, AUC 0.53 vs
  best shuffled 0.54**. Not significant; about ten model/view combinations were tried, so the best-looking one is expected to look good by chance alone.
* Verdict: **no ML entry gate is supported.** Instead the test is now pre-registered (one fixed model and rule, `mltrade::watch`) and re-run daily by the
  statistician as blocks accumulate. It qualifies only if the gain interval is above zero, the gate is better in at least 60% of test blocks and its AUC
  beats the best shuffled model; Discord is told the first time and whenever the verdict flips. Nothing touches the live strategy.

**The old way (GRPO of the language model).** 30 steps on 30 held-out prompts: mean reward 0.674 -> 0.771, valid JSON 93% -> 100%, valid under the rules
57% -> 67%, new and valid 17% -> 23%, but honesty (no invented numbers) 90% -> 77%, for about an hour of model-server downtime per run.

## Does it machine-learn? Learning curves on the real data (`examples/brain_learning.rs`, 2026-10-13)
A model learns if, trained on more history, it does better on the SAME fixed later data it never saw.
* **Choosing variants (195 tested; test = the latest 30).** The **additive average** learns: its error falls from 1.02 (10 variants) to 0.72 (165) and its
  rank correlation with the real gains rises from -0.25 to +0.13; at 165 it beats "always guess the average" (0.721 vs 0.738). The **Gaussian process did
  not learn** (error 0.84-1.01, always worse than guessing the average; correlation negative), so it was **dropped from the live rotation** and replaced by
  the additive policy. The optimistic-bonus rule inherits the additive mean and is weak (correlation about +0.09 at the end).
* **Trades (45 training blocks, tested on the same 23 later blocks).** The gradient-boosted trees learn the TRAINING data mechanically (log-loss 0.693 ->
  0.482 over 300 rounds) but the loss on unseen blocks RISES (0.693 -> 0.730): memorisation, not knowledge. The results at 3, 6, 12, 24, 36, 45 training
  blocks are erratic (AUC 0.50-0.56, gate gain flipping between +0.21 and -0.19): no stable learning curve.
* **Fix: early stopping**, chosen on the last quarter of the TRAINING period only (never the test blocks). It picks **0 rounds**: validation finds nothing
  in the entry features that generalises, so the honest model is the prior and **no entry gate is proposed** (version 2 of the pre-registered test;
  version 1 used 150 rounds and showed +0.11 points per trade [-0.06, +0.31], which was memorisation noise). With 40,076 trades but only 68 independent
  blocks there is simply little to learn from; the daily test will start choosing rounds as soon as validation finds real signal.

## Machine learning on ALL the candles (2026-10-13, `src/candles.rs`, `src/mlcandle.rs`, `qwen-lab candles import|ml`)
The lab keeps its own copy of every candle it can find (`data/candles.sqlite3`): 95 symbols (the SPY-holdings universe), 471,952 fifteen-minute bars
from 2026-01-02 to 2026-10-13 (research cache merged with the bot's live store; 0 invalid). No other candle source exists on disk.
Test, fixed in advance: hourly moments (105,492 symbol-moments), label = log return over the next 26 bars minus the average of all symbols at that moment
(market direction cancels), 23 features using only past bars (a test rewrites the future and checks nothing moves), train on the earliest 203 days, a 3-day
purge, test on the latest 66 days; scores = rank IC per moment and top-minus-bottom-fifth spread, intervals from resampling whole weeks.
* Trees (early-stopped on the training period, 150 rounds chosen): IC -0.003 [-0.079,+0.072], spread -13 bps [-84,+57]. Logistic: IC +0.025 [-0.05,+0.09].
* Simple signals, no learning (7 looks, so expect one to look good by chance): reversal over 26 bars IC +0.045 [-0.005,+0.105]; momentum over 130 bars
  IC -0.089 [-0.14,-0.04] (stocks that rose over 2 days did worse in this window; one regime of 66 days).
* Controls: trees trained on within-moment shuffled labels pick 0 rounds and score exactly 0. Learning curve: with 53, 102, 152, 203 training days the
  test IC stays at 0, -0.03, -0.02, -0.003: **no learning curve, no skill found. Verdict: does not qualify.**
* **Libraries (owner: "use libraries if it makes it better"), `examples/candle_libs.rs`, smartcore 0.4.0 as a dev-dependency only.** Random forests: IC -0.004 and
  -0.023 (no better than our trees). **Linear models (ridge, elastic net) did find something** on the single split: IC +0.074 [-0.006,+0.155], spread +68 bps
  [+1.5,+135] -- chosen after trying 8 configurations, so flattered. Our own 30-line ridge solver matches the library (IC +0.0738 vs +0.0740), so the lab
  needs no new runtime dependency. Rolling-origin test (first 100 days, then 4-week windows, every prediction out of sample, 26 weeks): **ridge IC +0.061
  [+0.015,+0.104], spread +63 bps [+18,+107] per session; control ridge on within-moment shuffled labels IC -0.006 [-0.014,+0.004]; our trees +0.002.**
  (Superseded by the fee-inclusive walk-forward below: the verdict was LEAD before fees and is now "does not qualify".) Earlier verdict: **LEAD, not qualified**: it becomes QUALIFIED only when the model frozen on 2026-10-13 also has intervals above zero on the
  first 20 trading days of candles AFTER that date (the only data not used to pick it). A spread of 63 bps per session is a gross figure between the best and
  worst fifth of 95 stocks, before costs, and says nothing yet about the long-only 5h strategy.
* **Version 2, "as advanced as possible" (`src/mlcandle2.rs`, `examples/candle_v2.rs`): it did NOT beat the simple model.** 20 extra features (cross-sectional
  ranks, market-relative and volatility-scaled returns, return since the open), rank labels, ridge strength tuned on training data, trees on the rank label, an
  ensemble; 5 variants, all reported, rolling windows, 26 weeks. Rank IC: v1 ridge +0.061 [+0.016,+0.104] (best), v2 features +0.058, rank-label ridge +0.050,
  trees +0.005, ensemble +0.034; the shuffled control +0.0005. More features, a smarter label and trees add nothing here: the signal is small and linear, and
  the extra capacity only fits noise. So the frozen v1 ridge stays THE pre-registered model. **Long-only portfolio test** (each day at 10:00 New York hold the top
  names for one session, 5 bps per side, excess over the equal-weight average of the 95 names, 121 days): v1 ridge top 19 NET +39.8 bps/day [+4.8,+73.7], Sharpe
  2.9; top 5 +30.5 [-17.3,+79.1]. Gross figures before real-world frictions (spread, slippage), over one bull-leaning 6-month window, 121 days: a lead, not proof.
* **Fees and per-block parameters (`src/mlwalk.rs`, owner: "it needs to include fees; test new parameters for every 14 day chunk in walk forward") -- this REPLACES the
  earlier flat 5 bps assumption and is now the verdict.** Fees are the bot's own measured ones: commission 0.995 USD per order (its real paper orders) over the
  position size (5,000 USD / positions) plus each symbol's measured half-spread (median 3.0 bps; 517 symbols in its order books), on every buy and sell; names kept
  to the next day cost nothing. For each block of 14 sessions the parameters (ridge strength, 2/3/5/10 positions, entry 10:00/11:00/12:00, minimum edge 0/10/20 bps)
  are re-chosen from the three blocks BEFORE it (each predicted by a model trained only on older data), the model is refitted on older data only, and the block is
  traded; results are pooled (10 blocks, 133 days). Real stock returns, cash earns 0. **Result: NET +40.8 bps/day [-15.8,+100.6], gross +52.4, fees 11.5,
  Sharpe 1.4, up days 44%; buying and holding all 95 names earned +18.0. Fixed parameters (2 positions, 10:00, ridge 100) did better, NET +52.2 [-38.4,+139.1], so
  re-choosing every block adds noise, not edge (the chosen settings jump from block to block). Control with shuffled labels: -16.2 [-48.9,+15.4].** The interval
  includes zero: **verdict "does not qualify" (the earlier LEAD is withdrawn).** Fees take about a fifth of the gross edge; a flat 1 USD commission makes many small
  positions impossible (10 positions of 500 USD pay 40 bps per round trip). Caveats: the 95 names are today's SPY holdings (survivorship), one bull-leaning window.
  The forward test is the same procedure on blocks starting on or after 2026-10-13 (28 sessions needed).
* **Does it work like the bot's backtester? (owner: "it should") -- the honest answer was NO, so the ML score now runs INSIDE it.** `mlwalk.rs` is a simplified
  separate simulation (all names, fixed hold, no trend filter, no exits). The bot's engine ranks only its top-20 liquid SPY holdings, gates entries on SPY's
  trend, has holds, stops and exits, uses its measured fee/spread book and a 100,000-combination parameter search per 14-session block. Bot repo change (research
  only, off by default, the live bot never loads it): `Signal::Ml` + variant token `sig:Ml`; the score file is `ml_scores.json` written by `qwen-lab candles scores`
  (model refitted every 14 sessions on older data only, ridge strength chosen on the training period, 2-session purge; test: garbling later labels cannot
  change an earlier score). **Data fixes taken from the bot:** only SPY holdings (14 of my 95 symbols, e.g. ASML, TSM, MSTR, were not) and its split rule (history
  before a jump dropped: CRWD, KLAC, MRNA); 80 symbols remain. **The model also takes the bot's own components** (`mlbot.rs`: multi-window volatility, EMA price
  action, volume z-score, RSI, percentiles in its top-20 universe, SPY trend distances), trained and judged on the top-20 universe only.
  * On the cleaned data the earlier numbers were inflated: ridge rolling rank IC **+0.050 [-0.007,+0.100]** (was +0.061), fee-walk-forward NET **+20.1 bps/day
    [-42.2,+79.7]** (was +40.8): the non-SPY high-flyers and split artifacts helped. Verdict unchanged: does not qualify.
  * Inside the top-20 (`examples/candle_bot.rs`): ridge on my features IC +0.071 [+0.001,+0.139]; on the bot's score features only +0.018 [-0.056,+0.094]; both
    +0.073; the bot's own composite ranking (rank_value) +0.019 [-0.021,+0.061]. So the bot's edge is not in its ranking; it is in the trend filter, holds and exits.
  * **The bot's own backtester (`variant_study 5h ...`, 29 blocks of 14 sessions, 5,000 USD, its fees, noisy-tape check):** baseline (live strategy) **+2.84%/block,
    66% of blocks positive, worst -9.8%, chained +38.3%**. `sig:Ml` (final model) +2.48%, 59% positive, worst -8.2%; `sig:Ml+follow` +2.33%, 55%, worst -12.4%;
    `sig:Ml+contra` +1.55%. First model (before the data fixes): +2.28 / +2.85 / +1.61. **The ML score does not beat the live strategy in the bot's own
    backtester**; on a smaller risk (worst block -8.2%) it is close. 29 blocks cannot separate +2.5 from +2.8.
* **Learn and improve (owner: "test in the local model, learn, improve, try different things").** (1) Models and label horizons on the bot's top-20 universe
  (`examples/candle_models.rs`, 18 combinations, all reported, rolling windows): ridge + SPY-regime interactions on the one-session label is best, IC **+0.075
  [+0.007,+0.138]**, spread +71 bps [+2,+134]; plain ridge +0.066; logistic +0.047; trees on the return +0.034 and on the rank label +0.044 (trees hurt, again);
  the ensemble +0.063. Two-session labels (best IC +0.057) and five-session labels (+0.054) have wider intervals and no better IC: the extra horizon only adds noise.
  The score generator now uses the interaction ridge. (2) **A better use of the model: as a VETO on the bot's own entries, not a replacement ranking**
  (bot repo, research only: `mlgate<percentile>` skips a stock whose score is in the lowest part of the universe; the bot's signal, trend filter, sizing and exits
  stay). Bot backtester, cut A, 29 blocks, 5,000 USD, its fees (first score file): baseline +2.84%/block, 66% positive, worst -9.8%, noisy 1st/2nd half
  +2.72/+1.95; **mlgate50 +3.43%, median +2.17, worst -5.8%, halves +3.94/+2.96, noisy +5.33/+2.74, noisy-positive 72%**; mlgate35 +3.66%, 72% positive, worst
  -6.0%, but its second half (+2.03) is below baseline (+3.08); mlgate20 +3.03%. Chained return of the consecutive blocks: baseline +38.3%, mlgate50 +36.9%,
  mlgate35 +22.6%: higher mean and a much shallower worst block, not a higher compounded return. **Cut B (offsets 2,7,12; same score file): baseline +1.92%/block (halves +1.78/+2.06), mlgate35 +3.28% (halves +6.09/+0.46), mlgate50 +4.15% (halves +7.18/+1.12, noisy +5.91/+0.86): the mean gain repeats on the second cut, but on both cuts it sits in the FIRST half; in the later half the gate is at or below baseline (cut B: 0.46 and 1.12 vs 2.06), so the lab's holdout (second half) is unlikely to pass it -- the edge looks period-specific or decaying.** The improved score file is being run; **`mlgate35` and
  `mlgate50` are now in the lab's option vocabulary**, so the improve loop validates them on all cuts (A-E, noisy tape, holdout) like any other option; the score file
  is refreshed daily (its results depend on the score version, so a changed model can change an old result).
* **The improvement round: items 2-6 (`mlround.rs`, `qwen-lab candles improve`, `examples/candle_improve.rs`; the statistician runs it daily, ~20 s).** Every trial is
  recorded in a LEDGER (`ml_trials`, 20 so far) and every interval is widened for the number of trials (Bonferroni, tail 0.025 / trials); a change counts only
  when its PAIRED week-by-week difference against the model it modifies is above zero at that adjusted level (#6). Real data, top-20 universe, rolling windows:
  * **#3 recency (the edge looked stronger in the first half):** base (expanding window) IC +0.073; rolling 85 days +0.076, **rolling 126 days +0.089** (paired
    +0.016 [-0.022,+0.055]), half-life 42 days +0.086, half-life 84 days +0.085, bag of three +0.083. All point estimates are higher, **none is significant**
    paired -> not adopted. Rolling 126 days is the lead (its own interval is the tightest, lower end +0.036).
  * **#5 seven candidate features, each added alone** (gap, gap size, first-hour volume, volume profile, beta, residual return, relative strength inside the
    5 most correlated peers): paired change -0.009 to +0.001, **none helps, several hurt slightly** -> none adopted.
  * **#2 meta-labelling on the bot's own 33,025 backtest trades** (candle context at the decision bar, 3 sequential folds, veto the worst 35%): logistic on win
    -0.05 pts per trade [-0.23,+0.17], trees -0.08, **tail label (worst 20% of trades) -0.22 [-0.42,-0.03] (worse than nothing)**, shuffled control -0.06; with the
    bot's 18 trade features too: -0.07 / -0.005 / -0.14 / -0.09. **No skill at vetoing the bot's own trades from candle context.** (The cross-sectional
    `mlgate` below works at a different level: it removes weak stocks from the candidate list.)
  * **#4 the gate's percentile is now searched, not fixed:** `mlgate` (bot repo, research) draws 10-60 in the bot's per-block parameter search like its other
    settings; the lab vocabulary has `mlgate` instead of the fixed 35/50 variants.
* **Bot backtester, cut A, improved (interaction-ridge) scores, 29 blocks:** live baseline +2.84%/block (66% positive, worst -9.8%, halves +2.58/+3.08, chained
  +38.3%). **`mlgate` with the SEARCHED percentile: +3.44%, median +2.62, 55% positive, worst -7.6%, halves +3.83/+3.07 (the later half now matches baseline), noisy tape
  +4.27/+2.73, chained +43.5%.** Fixed gates on the same scores: mlgate35 +2.68%, mlgate50 +2.62% (so the search choosing the percentile per block matters; the fixed
  numbers from the first score file, +3.7/+3.4, did not survive the better model). `sig:Ml` as the ranker: +3.24%, chained +48.8%, but worst block -17.7%. **Cut B (same scores): baseline +1.92%/block (halves +1.78/+2.06); `mlgate` searched +3.92% (64% positive, halves +6.90/+0.93, worst -10.4%); mlgate35 +4.27% (halves +6.82/+1.72); mlgate50 +3.10% (halves +6.41/-0.20).** So the mean gain repeats on both cuts but cut B's later half is again at or below baseline (0.93 vs 2.06) and its worst block is deeper (-10.4% vs -8.4%): **a lead, not validated** -- the lab's loop is judging `mlgate` on cuts A-E, noisy tape and holdout, and the strategist's `ml-lead` rule keeps it and its pairings queued.
* The statistician re-runs this daily (`candle_watch`; first time and flips go to Discord; report in logs/candle-ml.txt; `qwen-ops health` shows it). It
  qualifies only if trees are chosen by early stopping, the IC and spread intervals are above zero and the IC beats the shuffled control.

## Operate
`qwen-lab brain report` (the policies compared) | `brain ml` | `brain eval` | `brain lessons <words>` | `brain run <role>` | `brain all` |
`qwen-ops health` (services, brain runs, ML verdict) | Discord gets role failures, newly passing variants and ML verdict changes.

## Not planned
A language model. (Neural networks trained from scratch do exist here: the deep-learning trainer works on candles. For choosing variants a network would only memorise the roughly 200 labelled variants, so that job keeps the plain models.) The models grow with the data
(`brain eval` and `brain ml` re-test at any time).
