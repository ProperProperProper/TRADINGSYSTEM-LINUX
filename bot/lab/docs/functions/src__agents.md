# `src/agents.rs`

> The read-only tools the lab's MCP server (`qwen-lab mcp`, used by Claude Code) and the brain share. The restricted view (`human = false`)
> never contains the HOLDOUT half of any result (only the training half); the human-facing view shows everything.

7 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `tool_defs`

`src/agents.rs:11`

```rust
pub fn tool_defs() -> Vec<(&'static str, &'static str, Value, bool)>
```

A tool both the agents and the MCP server offer: (name, description, input schema, may an agent call it).

## `best_lines`

`src/agents.rs:26`

```rust
pub fn best_lines(mem: &Connection, n: usize, human: bool) -> Result<String>
```

The best results, one per line. `human` adds the holdout half and the verdict; an agent never sees the holdout.

## `bot_status`

`src/agents.rs:48`

```rust
fn bot_status(minutes: i64) -> String
```

Runs the bot's `check.sh` (last `minutes` of logs) and returns its output, for the bot_status tool.

## `call_tool`

`src/agents.rs:57`

```rust
pub fn call_tool(ctx: &Ctx, name: &str, args: &Value, human: bool) -> Result<String>
```

Run a tool. `human` is the MCP/Claude view; an agent (`human = false`) gets the restricted view and only the tools marked for it.

## `init_messages`

`src/agents.rs:94`

```rust
pub fn init_messages(mem: &rusqlite::Connection) -> Result<()>
```

Creates the agents' message table if missing.

## `recent_messages`

`src/agents.rs:100`

```rust
pub fn recent_messages(mem: &rusqlite::Connection, limit: usize) -> Result<Vec<Value>>
```

The newest notes between agents, for the dashboard: (time, from, to, text, read).

## `serve_mcp_view`

`src/agents.rs:108`

```rust
pub fn serve_mcp_view(ctx: &Ctx, human: bool) -> Result<()>
```

The MCP server (JSON-RPC over stdio). `human = false` is the restricted view: only the tools marked for it, never a holdout number.
