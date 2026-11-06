# `lab/src/webhook_guard.rs`

> Keeps Discord webhook URLs (and the file that holds one) out of git. Owner rule 2026-10-13: "do not push webhook to repo ever!".
> Used by the `qwen-guard` binary, which the pre-commit and pre-push hooks of the lab repo and the bot repo call.
> A real webhook is `discord.com/api/webhooks/<15+ digit id>/<30+ char token>`; the short fake URLs in unit tests do not match.

3 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `has_webhook`

`lab/src/webhook_guard.rs:6`

```rust
pub fn has_webhook(text: &str) -> bool
```

True if `text` contains something shaped like a real Discord webhook URL.

## `is_webhook_file`

`lab/src/webhook_guard.rs:26`

```rust
pub fn is_webhook_file(path: &str) -> bool
```

True if `path` is the webhook file by name.

## `diff_leaks`

`lab/src/webhook_guard.rs:29`

```rust
pub fn diff_leaks(diff: &str) -> bool
```

Scans a unified diff: only added lines (`+...`, not the `+++` header) count.
