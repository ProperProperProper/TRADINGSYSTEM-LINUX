# Archived Rust lab function reference

> Generated references for the retired `bot/lab/` research system; they do not describe current live bots or services.

One page per source file, one entry per function (tests excluded), generated from the code by `qwen-tools gen-function-docs`.
No account numbers, credentials or API keys appear in these pages (the generator refuses to run if it finds any).

805 functions in 75 files; 272 have no comment in the code (listed with their signature only).

| File | Functions | Without comment |
|---|---|---|
| [`examples/brain_build.rs`](examples__brain_build.md) | 1 | 1 |
| [`examples/brain_eval.rs`](examples__brain_eval.md) | 5 | 4 |
| [`examples/brain_learning.rs`](examples__brain_learning.md) | 2 | 2 |
| [`examples/candle_bot.rs`](examples__candle_bot.md) | 1 | 1 |
| [`examples/candle_improve.rs`](examples__candle_improve.md) | 1 | 0 |
| [`examples/candle_libs.rs`](examples__candle_libs.md) | 3 | 3 |
| [`examples/candle_ml.rs`](examples__candle_ml.md) | 1 | 1 |
| [`examples/candle_models.rs`](examples__candle_models.md) | 3 | 2 |
| [`examples/candle_v2.rs`](examples__candle_v2.md) | 1 | 0 |
| [`examples/mltrade_eval.rs`](examples__mltrade_eval.md) | 3 | 3 |
| [`src/action_items.rs`](src__action_items.md) | 7 | 5 |
| [`src/agents.rs`](src__agents.md) | 7 | 0 |
| [`src/aimodel.rs`](src__aimodel.md) | 28 | 11 |
| [`src/announce.rs`](src__announce.md) | 9 | 6 |
| [`src/autodiff.rs`](src__autodiff.md) | 32 | 26 |
| [`src/automl.rs`](src__automl.md) | 34 | 17 |
| [`src/bin/qwen-guard.rs`](src__bin__qwen-guard.md) | 6 | 6 |
| [`src/bin/qwen-notify.rs`](src__bin__qwen-notify.md) | 1 | 1 |
| [`src/bin/qwen-ops.rs`](src__bin__qwen-ops.md) | 1 | 0 |
| [`src/bin/qwen-tools.rs`](src__bin__qwen-tools.md) | 1 | 0 |
| [`src/bot_status.rs`](src__bot_status.md) | 6 | 6 |
| [`src/brain.rs`](src__brain.md) | 27 | 10 |
| [`src/brain_agents.rs`](src__brain_agents.md) | 19 | 10 |
| [`src/brain_roles.rs`](src__brain_roles.md) | 30 | 12 |
| [`src/btarchive.rs`](src__btarchive.md) | 13 | 0 |
| [`src/candles.rs`](src__candles.md) | 12 | 3 |
| [`src/config.rs`](src__config.md) | 21 | 1 |
| [`src/context.rs`](src__context.md) | 2 | 0 |
| [`src/contract.rs`](src__contract.md) | 5 | 0 |
| [`src/cull.rs`](src__cull.md) | 4 | 0 |
| [`src/dash.rs`](src__dash.md) | 18 | 1 |
| [`src/db.rs`](src__db.md) | 11 | 0 |
| [`src/deepnet.rs`](src__deepnet.md) | 22 | 15 |
| [`src/facts.rs`](src__facts.md) | 9 | 0 |
| [`src/fix_report.rs`](src__fix_report.md) | 14 | 11 |
| [`src/fn_docs.rs`](src__fn_docs.md) | 8 | 3 |
| [`src/guard.rs`](src__guard.md) | 11 | 0 |
| [`src/highlights.rs`](src__highlights.md) | 6 | 0 |
| [`src/holdings_xlsx.rs`](src__holdings_xlsx.md) | 7 | 4 |
| [`src/indicators.rs`](src__indicators.md) | 2 | 2 |
| [`src/ingest.rs`](src__ingest.md) | 5 | 0 |
| [`src/intel.rs`](src__intel.md) | 11 | 1 |
| [`src/labtools.rs`](src__labtools.md) | 17 | 0 |
| [`src/main.rs`](src__main.md) | 6 | 0 |
| [`src/misc_tools.rs`](src__misc_tools.md) | 19 | 8 |
| [`src/mlbot.rs`](src__mlbot.md) | 5 | 0 |
| [`src/mlcandle.rs`](src__mlcandle.md) | 23 | 10 |
| [`src/mlcandle2.rs`](src__mlcandle2.md) | 12 | 5 |
| [`src/mlfeat.rs`](src__mlfeat.md) | 3 | 2 |
| [`src/mlimprove.rs`](src__mlimprove.md) | 8 | 3 |
| [`src/mlmeta.rs`](src__mlmeta.md) | 2 | 0 |
| [`src/mlround.rs`](src__mlround.md) | 5 | 4 |
| [`src/mlscores.rs`](src__mlscores.md) | 8 | 2 |
| [`src/mltrade.rs`](src__mltrade.md) | 21 | 11 |
| [`src/mlwalk.rs`](src__mlwalk.md) | 9 | 6 |
| [`src/notify.rs`](src__notify.md) | 12 | 7 |
| [`src/ops.rs`](src__ops.md) | 34 | 21 |
| [`src/paper.rs`](src__paper.md) | 14 | 0 |
| [`src/paper_moomoo.rs`](src__paper_moomoo.md) | 4 | 0 |
| [`src/portfolio.rs`](src__portfolio.md) | 5 | 1 |
| [`src/promote.rs`](src__promote.md) | 6 | 0 |
| [`src/quant.rs`](src__quant.md) | 9 | 0 |
| [`src/report.rs`](src__report.md) | 3 | 0 |
| [`src/schedule.rs`](src__schedule.md) | 22 | 4 |
| [`src/scout.rs`](src__scout.md) | 10 | 0 |
| [`src/stats.rs`](src__stats.md) | 15 | 0 |
| [`src/strategy.rs`](src__strategy.md) | 52 | 2 |
| [`src/synth.rs`](src__synth.md) | 3 | 0 |
| [`src/sysinfo.rs`](src__sysinfo.md) | 9 | 0 |
| [`src/testutil.rs`](src__testutil.md) | 1 | 0 |
| [`src/tick.rs`](src__tick.md) | 9 | 0 |
| [`src/util.rs`](src__util.md) | 7 | 0 |
| [`src/warnings_ledger.rs`](src__warnings_ledger.md) | 31 | 18 |
| [`src/webhook_guard.rs`](src__webhook_guard.md) | 3 | 0 |
| [`src/winrate.rs`](src__winrate.md) | 8 | 0 |
