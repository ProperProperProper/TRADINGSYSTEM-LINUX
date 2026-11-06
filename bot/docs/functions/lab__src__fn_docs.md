# `lab/src/fn_docs.rs`

> Generates docs/functions/*.md: one page per Rust source file, one entry per function (tests excluded), from the code itself (Rust port of
> tools/gen_function_docs.py). Each entry: name, signature, location and the function's doc comment (/// lines) or, without one, the NOTE/plain
> comments directly above it. Nothing is invented: a function with no comment is listed with its signature only and flagged in the index.
> Secrets guard: any line matching account/credential patterns aborts generation.
>
> `qwen-tools gen-function-docs <repo root>`

8 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `res`

`lab/src/fn_docs.rs:16`

```rust
fn res() -> Res
```

_No comment in the code; the signature above is the description._

## `files`

`lab/src/fn_docs.rs:28`

```rust
fn files(root: &Path) -> Result<Vec<String>>
```

Tracked Rust source files of the repo (build output and generated protobuf code excluded), sorted.

## `signature`

`lab/src/fn_docs.rs:37`

```rust
fn signature(r: &Res, lines: &[&str], i: usize) -> String
```

A function's signature from its first line to the opening brace or semicolon, on one line.

## `comments_above`

`lab/src/fn_docs.rs:51`

```rust
fn comments_above(r: &Res, lines: &[&str], i: usize) -> (Vec<String>, Vec<String>)
```

The doc comment (///) and other comment lines directly above line i, skipping attributes.

## `module_doc`

`lab/src/fn_docs.rs:66`

```rust
fn module_doc(lines: &[&str]) -> Vec<String>
```

The module's //! header comment lines.

## `test_ranges`

`lab/src/fn_docs.rs:76`

```rust
fn test_ranges(lines: &[&str]) -> BTreeSet<usize>
```

Line indexes of #[cfg(test)] modules (excluded).

## `generate`

`lab/src/fn_docs.rs:92`

```rust
pub fn generate(root: &Path) -> Result<String>
```

_No comment in the code; the signature above is the description._

## `main`

`lab/src/fn_docs.rs:142`

```rust
pub fn main(args: &[String]) -> i32
```

_No comment in the code; the signature above is the description._
