# `src/bin/qwen-guard.rs`

> `qwen-guard staged | push | install-hooks REPO...` -- the Discord webhook can never reach git (owner rule 2026-10-13).
> staged         pre-commit hook: blocks a commit whose staged changes add a webhook URL or the webhook file
> push           pre-push hook (reads git's stdin): blocks a push of any commit that adds one
> install-hooks  writes the two hooks (two-line stubs that exec this binary) into each repo's .git/hooks
> Fails closed: if this binary is missing the hooks error out and the commit/push does not happen.

6 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `git`

`src/bin/qwen-guard.rs:10`

```rust
fn git(args: &[&str]) -> String
```

_No comment in the code; the signature above is the description._

## `block`

`src/bin/qwen-guard.rs:14`

```rust
fn block(why: &str) -> !
```

_No comment in the code; the signature above is the description._

## `staged`

`src/bin/qwen-guard.rs:19`

```rust
fn staged()
```

_No comment in the code; the signature above is the description._

## `push`

`src/bin/qwen-guard.rs:24`

```rust
fn push()
```

_No comment in the code; the signature above is the description._

## `install`

`src/bin/qwen-guard.rs:40`

```rust
fn install(repos: &[String])
```

_No comment in the code; the signature above is the description._

## `main`

`src/bin/qwen-guard.rs:57`

```rust
fn main()
```

_No comment in the code; the signature above is the description._
