# `lab/src/notify.rs`

> Discord change notifications (owner 2026-10-13: "push all change notifications to my Discord").
>
> One webhook, nothing else. The webhook URL is a credential: it is read from `QWEN_LAB_DISCORD_WEBHOOK` or from
> `~/.config/qwen-lab/discord_webhook` (outside every repo), never logged, never printed, and only Discord hosts are accepted so the
> command cannot be turned into a way to post text anywhere else. Every message is redacted before it leaves the machine: account-like
> numbers, e-mail addresses, home-directory names, long key-like tokens, `key=value` secrets and webhook URLs are masked. Identical
> messages within ten minutes are sent once (a flapping job must not flood the channel). Without a webhook it is a silent no-op.

12 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `home`

`lab/src/notify.rs:22`

```rust
fn home() -> PathBuf
```

_No comment in the code; the signature above is the description._

## `webhook_url`

`lab/src/notify.rs:25`

```rust
pub fn webhook_url() -> Option<String>
```

The configured webhook URL, if any and if it is a Discord one.

## `valid_webhook`

`lab/src/notify.rs:33`

```rust
pub fn valid_webhook(url: &str) -> bool
```

Only Discord's own webhook endpoints are accepted.

## `is_key_like`

`lab/src/notify.rs:38`

```rust
fn is_key_like(tok: &str) -> bool
```

_No comment in the code; the signature above is the description._

## `mask_digits`

`lab/src/notify.rs:44`

```rust
fn mask_digits(tok: &str) -> String
```

_No comment in the code; the signature above is the description._

## `redact_token`

`lab/src/notify.rs:56`

```rust
fn redact_token(tok: &str) -> String
```

_No comment in the code; the signature above is the description._

## `redact`

`lab/src/notify.rs:79`

```rust
pub fn redact(text: &str) -> String
```

Masks anything that looks like a secret or account detail. Whitespace and line breaks are kept.

## `clip`

`lab/src/notify.rs:90`

```rust
pub fn clip(text: &str) -> String
```

_No comment in the code; the signature above is the description._

## `state_path`

`lab/src/notify.rs:95`

```rust
fn state_path() -> PathBuf
```

_No comment in the code; the signature above is the description._

## `digest`

`lab/src/notify.rs:97`

```rust
fn digest(msg: &str) -> String
```

_No comment in the code; the signature above is the description._

## `seen_recently`

`lab/src/notify.rs:100`

```rust
fn seen_recently(msg: &str, now: u64) -> bool
```

True if this exact message was sent less than `DEDUPE_SECS` ago; otherwise records it.

## `send`

`lab/src/notify.rs:117`

```rust
pub fn send(title: &str, body: &str) -> Result<Outcome>
```

Sends `title` + `body` to the configured Discord webhook after redaction.
