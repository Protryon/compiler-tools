# Improvements backlog

Ergonomics and feature ideas surfaced during an audit of the workspace. These are
**not** bugs (the confirmed bugs from that audit are already fixed); they are API and
feature gaps worth prioritizing. Roughly ordered by impact.

## API ergonomics

### 1. Implement `Iterator` for the generated tokenizer and `TokenizerWrap`
Today both expose an inherent `next(&self) -> Option<Spanned<..>>`, so every consumer
hand-writes `while let Some(t) = tok.next() { .. }`. Implementing `Iterator` (and
`IntoIterator`) on the generated `XxxTokenizer` and on `TokenizerWrap` would unlock
`for`, `collect`, `map`, `filter`, `peekable`, etc. for free. This is the single
biggest ergonomic win.

- Generated tokenizer: `compiler-tools-derive/src/lib.rs` (the `impl TokenParse` block).
- Wrapper: `compiler-tools/src/tokenizer.rs`.

### 2. Distinguish "lex error" from "end of input"
With no `#[token(illegal)]` handler, `next()` returns `None` both at EOF *and* when it
is stuck on an unmatchable character with input still remaining. The two are
indistinguishable, so a malformed input looks like a clean, short token stream.

Options:
- Expose the remaining input / position (e.g. `fn remaining(&self) -> &str`,
  `fn is_exhausted(&self) -> bool`) so callers can detect a stuck tokenizer.
- Offer a `Result`-returning variant of `next` that reports the offending span.

### 3. Grammar-level `#[token(skip)]`
Whitespace currently has to be wired up by hand via
`TokenizerWrap::new(inner, [Token::Ws])`. A `#[token(skip)]` attribute (or generating
the ignore-list from the grammar) would be more discoverable and harder to get wrong.

## Correctness / fidelity

### 4. Character columns instead of byte columns
Span columns are byte offsets, not character counts (see the `//todo: handle utf8`
markers in `codegen/simple_regex.rs`, `codegen/full_regex.rs`, and `lib.rs`). For
non-ASCII source this yields misleading column numbers. The newline column math is now
correct in *bytes*; converting to character columns is a separate, larger change that
touches all four span-emission sites.

## Performance

### 10. Runtime interpreter (`find_prefix`) regressed ~2x from assertion-gated accepts
Adding assertion-gated greedy backoff (`.+\b` / `\B(?:fo|foo)\B`) made the runtime
interpreter call `accepts_via_assertions` at the top of every loop iteration, which
roughly doubled its per-search time in the conformance harness (~3.3 µs → ~6.6 µs).
A fast-path guard (`matching.rs`, `accepts_via_assertions`) already returns early for
states with no zero-width edge, but it still re-scans the state's transitions for a
zero-width edge every iteration, and the slow path allocates a `HashSet`/`Vec` per call.

Scope: this is the **runtime DFA interpreter only** (`Regex::find_prefix`), which
is exercised by the `regex-conformance` harness — **not** the compiled-Rust matcher
that `#[token(regex = ...)]` actually emits (`generate.rs`), whose accept conditions are
precomputed at build time and which stayed at ~2.2 µs/search. So the regression has no
production impact today; it only matters if `find_prefix` ever becomes a hot path.

Ideas: precompute per state (once, at DFA build) whether it has any zero-width edge and
whether any zero-width path can reach an accept, so the hot loop does an O(1) check and
only walks when it can pay off; and avoid the `HashSet` (states are few — a small
stack-allocated visited set or a depth bound suffices). See `matching.rs`
(`accepts_via_assertions`) and the build-time analog `zero_width_accept_conditions` in
`generate.rs`.

## Macro diagnostics (polish)

### 7. Honor or reject the attribute-macro arguments
`token_parse(_metadata, input)` silently ignores `_metadata`
(`compiler-tools-derive/src/lib.rs`), so `#[token_parse(anything)]` is accepted with no
effect. Either give the args meaning or emit a `compile_error!` for non-empty args.

### 8. Fix a misleading `compile_error!` message
The `regex_full` parse-failure arm reports `"invalid simple regex"`
(`compiler-tools-derive/src/lib.rs`, the `Regex::new` error branch); it should say
`"invalid regex"` since that path uses the full `regex` crate, not the simple engine.

### 9. Better parse-failure diagnostics for payload `.parse()`
Variants whose payload type needs `.parse()` reject a match on parse failure with no
explanation (`//TODO: emit better error for parsefail` in `lib.rs` and `lit_table.rs`).
A clearer message — or a way to surface the underlying parse error — would help.
