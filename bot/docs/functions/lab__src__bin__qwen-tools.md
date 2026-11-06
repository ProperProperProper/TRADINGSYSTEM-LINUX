# `lab/src/bin/qwen-tools.rs`

> `qwen-tools <command> ...` -- the lab's small helper commands, all Rust (they replace the Python scripts under hermes/ and tools/):
> action-items                                                                   quant-supervisor action items from the dashboard snapshot on stdin
> gen-function-docs <repo root>                                                  docs/functions/*.md from the Rust sources
> warnings scan|list|show|fixed|expected|report|counts   the bot's warnings ledger (log doctor)
> fix-report check|fallback|secrets|index ...             code checks for the daily fix report
> spy-holdings-xlsx <xlsx> <csv>                          SPY holdings spreadsheet -> CSV
> bot-status | bot-winners (JSON on stdin)                check.sh summaries
> agent-results (snapshot on stdin) | mcp-text (reply on stdin) | market   former inline Python of the shell scripts

1 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `main`

`lab/src/bin/qwen-tools.rs:9`

```rust
fn main()
```

Notes from the code: ! `qwen-tools <command> ...` -- the lab's small helper commands, all Rust (they replace the Python scripts under hermes/ and tools/): !   action-items                                                                   quant-supervisor action items from the dashboard snapshot on stdin !   gen-function-docs <repo root>                                                  docs/functions/*.md from the Rust sources !   warnings scan|list|show|fixed|expected|report|counts   the bot's warnings ledger (log doctor) !   fix-report check|fallback|secrets|index ...             code checks for the daily fix report !   spy-holdings-xlsx <xlsx> <csv>                          SPY holdings spreadsheet -> CSV !   bot-status | bot-winners (JSON on stdin)                check.sh summaries !   agent-results (snapshot on stdin) | mcp-text (reply on stdin) | market   former inline Python of the shell scripts
