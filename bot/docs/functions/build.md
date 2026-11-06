# `build.rs`

> Compiles moomoo's published OpenD protocol definitions (proto/, Apache-2.0, from
> github.com/MoomooOpen/py-moomoo-api) into Rust types, and embeds the git commit of the build
> (`engine::ENGINE_COMMIT`) so every archived backtest and paper record says which code produced it.

1 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `main`

`build.rs:7`

```rust
fn main() -> std::io::Result<()>
```

Build script: records the git commit (`git describe`) as `BOT_GIT_COMMIT` so every run and archive row names the code that made it.
