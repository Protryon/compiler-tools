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

## Custom regex engine features

### 5. Alternation and grouping
The custom `#[token(regex = ...)]` engine supports literals, classes/ranges, negation,
`.`, and `* + ?`, but has **no alternation or grouping** — users are forced onto the
slower `regex_full` (the `regex` crate) for those. Adding `(...)` groups and `a|b`
alternation to `simple_regex/parse.rs` + `nfa.rs` would let more grammars stay on the
fast, allocation-free path. Most-requested feature, largest effort.

### 6. Named character classes
`simple_regex/mod.rs` already carries a `// TODO: we should support classes (i.e.
unicode ident_start)`. Shorthands like `\d \w \s` (and ideally Unicode classes such as
`ident_start`/`ident_continue`) would remove a lot of verbose `[a-zA-Z0-9_]` patterns.

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
