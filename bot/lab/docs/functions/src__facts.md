# `src/facts.rs`

> HARD RULE (owner 2026-10-11): the lab never invents data or results, ever.
>
> Enforced by code, not by asking nicely:
> - [`known`] is every number the lab's tools can show the brain (results, win rates, trades, the win/lose analysis, tuning facts, options,
> highlights' facts, the research search results it was given, stored in `research_cache`);
> - [`gate`] refuses any text the brain wants to store or send (notes, test reasons, research ideas, whiteboard edits) that contains a
> decimal or percentage number found in none of them (rounding a real number is fine; anything else is refused, costs [`PENALTY`]
> points, and nothing is stored);
> - [`scrub_stored`] removes such numbers from text already stored and shown on the dashboard (hourly, in the tick).
>
> Plain integers (counts, dates, candle sizes like 5h) are not checked; decimals and percentages are what results are made of.

9 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `init`

`src/facts.rs:23`

```rust
pub fn init(mem: &Connection) -> Result<()>
```

Creates the research cache (the search results the brain was shown, kept 14 days, so numbers they quote from a paper can be verified).

## `remember_search`

`src/facts.rs:29`

```rust
pub fn remember_search(mem: &Connection, text: &str) -> Result<()>
```

Stores search results the brain was shown.

## `numbers`

`src/facts.rs:38`

```rust
pub fn numbers(text: &str) -> Vec<(f64, usize)>
```

The decimal and percentage numbers in `text`: (absolute value, decimals written).

## `is_read_failure`

`src/facts.rs:67`

```rust
fn is_read_failure(e: &str) -> bool
```

True for an error that means the lab could not be read (so the known numbers would be incomplete).

## `known`

`src/facts.rs:75`

```rust
pub fn known(ctx: &Ctx) -> Result<Vec<f64>>
```

Every number the lab's tools can show the brain (absolute values), see the module docs. An error when any source cannot be read: an incomplete list would make real numbers look invented (2026-10-11 a restore removed real numbers such as the current strategy's -0.71%/block on 1d), so every caller then changes nothing and penalises no one.

## `unverified`

`src/facts.rs:114`

```rust
pub fn unverified(known: &[f64], text: &str) -> Vec<String>
```

The numbers in `text` that match no known number (a known number rounded to the decimals written counts as a match).

## `gate`

`src/facts.rs:128`

```rust
pub fn gate(ctx: &Ctx, author: &str, what: &str, text: &str) -> Result<()>
```

Refuses `text` from `author` when it contains a number the lab never showed (costs 10 points; nothing is stored).

## `redact`

`src/facts.rs:148`

```rust
pub fn redact(known: &[f64], text: &str) -> Option<String>
```

Replaces every unverified number in `text` with "[unverified number removed]" (None when nothing changes).

## `scrub_stored`

`src/facts.rs:185`

```rust
pub fn scrub_stored(ctx: &Ctx) -> Result<Vec<String>>
```

Owner (2026-10-11): no fake or manipulated data or results may reach the final strategy or the dashboard. Strategy results, validation and promotion are written by code only (the brain has no tool that writes them); this scrubs the brain-written text the dashboard shows (notes, whiteboard, research ideas): every number the lab never showed is replaced in place (the text is kept, never deleted). Text from claude-code is not touched. Returns what was changed.
