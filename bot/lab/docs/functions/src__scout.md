# `src/scout.rs`

> Research scout (user 2026-10-09: "research on internet and try things, legit sources"). Reads recent quantitative-finance
> papers from arXiv's public API (export.arxiv.org) and, when arXiv is busy, from OpenAlex (api.openalex.org, an open index of
> journals, working papers and preprints incl. SSRN); these are the only hosts this module contacts. It asks the local `scout` agent to turn
> each relevant abstract into an entry idea: existing option tokens the strategy loop can test right away, or a new mechanism
> that needs a code change in the bot repo (listed for review, never implemented automatically). Every idea keeps its paper
> link; nothing a paper claims is treated as a result; ideas count only after the lab's own backtests pass them.

10 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `init`

`src/scout.rs:27`

```rust
pub fn init(mem: &Connection) -> Result<()>
```

Creates the research-ideas table if missing.

## `tag`

`src/scout.rs:39`

```rust
fn tag(s: &str, name: &str) -> Option<String>
```

Text between the first `<tag ...>` and `</tag>` in `s`, XML entities and whitespace tidied.

## `parse_feed`

`src/scout.rs:48`

```rust
pub fn parse_feed(xml: &str) -> Vec<Paper>
```

Papers from an Atom feed (arXiv API response).

## `fetch`

`src/scout.rs:56`

```rust
pub fn fetch(query: &str, max: usize) -> Result<Vec<Paper>>
```

The newest papers for `query` from arXiv (most recently submitted first).

## `fetch_openalex`

`src/scout.rs:85`

```rust
pub fn fetch_openalex(search: &str, max: usize) -> Result<Vec<Paper>>
```

The newest papers with abstracts for a plain-text search from OpenAlex (abstract rebuilt from its inverted index).

## `untested`

`src/scout.rs:108`

```rust
pub fn untested(mem: &Connection, cfg: &crate::config::StrategyConfig) -> Result<Vec<(String, String)>>
```

Relevant research ideas that map to options and have not been measured yet (interval, variant key), newest first: the strategy loop tests these before its own picks.

## `ideas_with_results`

`src/scout.rs:125`

```rust
pub fn ideas_with_results(mem: &Connection, leaderboard: &[Value], limit: usize) -> Result<Vec<Value>>
```

Every stored research idea for the dashboard, newest first: paper, link, idea, options and the test status of those options.

## `board_text`

`src/scout.rs:145`

```rust
pub fn board_text(mem: &Connection, limit: usize) -> Result<String>
```

The newest relevant ideas as whiteboard text for the proposer: idea, tokens to test, source link.

## `is_market_paper`

`src/scout.rs:169`

```rust
pub fn is_market_paper(mem: &Connection, url: &str, title: &str) -> bool
```

True when a paper is about markets: its title, or the search-result summary the agent was shown for its link (research cache), contains a market or trading term. On 2026-10-11 papers about plant invasions, marine environments and energy transitions were recorded as "relevant" ideas, with trading ideas attached that the papers do not contain.

## `demote_off_topic`

`src/scout.rs:183`

```rust
pub fn demote_off_topic(mem: &Connection) -> Result<usize>
```

Marks recorded ideas whose paper is not about markets as not relevant (kept, never deleted); returns how many.
