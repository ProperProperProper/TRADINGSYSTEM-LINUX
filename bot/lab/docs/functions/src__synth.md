# `src/synth.rs`

> Synthetic exporter (spec 4.2). Used ONLY to test the machinery: every file is flagged `synthetic` and the ingester
> refuses to mix synthetic and real data. It can plant a known effect (to test recall) or plant none (to test that
> noise does not produce discoveries).

3 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `default`

`src/synth.rs:27`  (method / nested)

```rust
fn default() -> Self
```

Default synthetic spec (tests only): 3 windows of 21 days.

## `normal`

`src/synth.rs:41`

```rust
fn normal(rng: &mut ChaCha8Rng) -> f64
```

Standard normal random number (Box-Muller).

## `generate`

`src/synth.rs:55`

```rust
pub fn generate(out: &Path, spec: &SynthSpec) -> Result<usize>
```

Writes synthetic export files for tests (never used on a real lab).
