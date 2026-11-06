# `src/testutil.rs`

> Test helpers: build a synthetic lab with ingested data in a temp folder.

1 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `lab`

`src/testutil.rs:16`

```rust
pub fn lab(spec: &SynthSpec) -> Lab
```

A test lab in a temporary folder filled with synthetic exports and ingested.
