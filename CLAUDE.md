# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build --workspace --all-features      # build everything
cargo test --workspace --all-features       # run all tests
cargo test --package compiler-tools          # runtime-crate unit tests only
cargo test --package compiler-tools-derive --test behavior   # one integration test binary
cargo test --package compiler-tools-derive --test behavior -- spans_track_newlines   # a single test
cargo test --package regex-conformance --test conformance -- --nocapture   # run the regex corpus against both engines; prints failing names + pass/parse/match counts + timings
cargo clippy --all
cargo +nightly-2025-03-01 fmt --all -- --check   # CI pins this exact nightly for rustfmt
```

`rustfmt.toml` uses a wide layout (`max_width = 160`, `struct_lit_width = 0`) — generated/`quote!` code and tests are expected to follow it. Several width options are unstable, so the formatting check requires nightly; the CI `rustfmt` job pins the exact nightly toolchain.

The crates are on `edition = "2024"` with `rust-version = "1.85.0"` (the CI `tests_min_compat` job pins that MSRV — bump both together if you raise it).

## Architecture

A workspace for building fast tokenizers from an enum definition:

- **`compiler-tools`** — the runtime library: `Span`/`Spanned`, the `TokenParse`/`TokenExt` traits, the `TokenizerWrap` helper, and small reusable parse helpers (`util::parse_str`, `MatchResult`).
- **`compiler-tools-regex`** — the compile-time simple-regex engine (the `parse`/`nfa`/`dfa`/`generate` pipeline plus the `matching` runtime interpreter, and `unicode` for build-time Unicode-table lookups via `regex-syntax`). A plain library crate (not a proc-macro) so it can be reused: `compiler-tools-derive` depends on it for code generation, and `regex-conformance` exercises it directly. Generated matchers refer to `::compiler_tools::...` by absolute path.
- **`compiler-tools-derive`** — the `#[token_parse]` proc-macro that generates a tokenizer. Depends on `compiler-tools-regex`, and `dev-depends` back on `compiler-tools` so the integration tests in `tests/` exercise generated code.
- **`regex-conformance`** — a test-only crate (`publish = false`) that runs the upstream `regex` test corpus (`testdata/`) against both forms of the engine; see *Regex conformance* below.

Generated code refers to the runtime crate by absolute path (`::compiler_tools::...`), so the macro output is self-contained at any call site.

### The `#[token_parse]` pipeline (`compiler-tools-derive/src/lib.rs`)

Applied to an enum, the macro emits: the enum re-declared with `#[token(...)]` attributes and string discriminants stripped; a `Display` impl; a `TokenExt` impl (`matches_class`, which compares variant *kind* and ignores payload); and a `XxxTokenizer<'a>` struct implementing `TokenParse`. The tokenizer name is the enum name with `Token` replaced by `Tokenizer`, or `Tokenizer` appended.

Each variant is specified by one of:
- a string discriminant (`Async = "async"`) or `#[token(literal = "...")]` → exact literal
- `#[token(regex = "...")]` → the **custom** compile-time regex engine (see below)
- `#[token(regex_full = "...")]` → the `regex` crate, compiled at runtime into a `OnceLock`
- `#[token(parse_fn = "path::to::fn")]` → a user `fn(&str) -> Option<(&str, &str)>` returning `(matched, remaining)`
- `#[token(illegal)]` → single-char fallback when nothing else matches

A variant's payload determines construction: unit (no payload), `&'a str` (raw matched slice), or any other type `T` (parsed via `passed.parse()`, where a parse failure rejects the match).

### Matching order and literal/regex conflict resolution

The generated `next()` tries matchers in **declaration order** (token index). Literals are compiled into a character trie (`LitTable`, `lit_table.rs`) emitted at the first literal variant's index and matched longest-first; each regex/`parse_fn` emits its own block at its variant's index. First match wins.

The subtle part: a literal that would *also* be matched by a **later-declared** regex (the canonical keyword-vs-identifier case — `let` vs `[a-z][a-zA-Z0-9_]*`) is **removed from the trie** and instead injected as a special-case arm inside that regex's generated parser. So the longer regex match wins (`letx` → identifier), but when the matched text equals the literal exactly it yields the keyword token (`let` → keyword). This conflict detection lives in `impl_token_parse` and uses `SimpleRegex::matches` / `Regex::is_match` at macro-expansion time.

### The custom simple-regex engine (`compiler-tools-regex/src/`)

A small regex compiler used by `#[token(regex = ...)]`, producing a branch-only state machine with no per-call allocation (faster than the `regex` crate for token-sized inputs; see `compiler-tools-derive/tests/regex_bench.rs`):

`parse.rs` (pattern → AST of literal/group/**alternation** atoms with `* + ?` repeats, each carrying an `AtomRepeat::lazy` flag for non-greedy `*? +? ?? {n,m}?`; `(...)` groups and `|` are parsed recursively into `Atom::Alternation(Vec<branch>)`) → `nfa.rs` (Thompson construction: each atom/repeat/branch becomes an epsilon-wired fragment, with branching routed through **pure-epsilon split states** whose epsilon-edge *order* encodes thread priority — body-before-exit for greedy, flipped for lazy, earlier-branch-first for alternation) → `dfa.rs` (**priority-preserving** subset construction — each DFA state is a *priority-ordered* epsilon-closure `Vec<u32>` truncated at the accepting state, with consuming edges partitioned into disjoint char classes with **ordered-unioned** targets, so shared-prefix alternation like `a|ab` is deterministic *and* leftmost-first) → `generate.rs` (emits a per-state `match` function returning `MatchResult::{Matched, MatchedEmpty, NoMatch}`). `matching.rs` interprets the same DFA at runtime: `SimpleRegex::matches` (a bool prefix check used for conflict detection at macro-expansion time) and `SimpleRegex::find_prefix` (returns `(matched, remaining)`, mirroring the generated matcher's semantics — used by the conformance harness). Both `find_prefix` and the generated matcher are **leftmost-first** (like the `regex` crate): they follow the highest-priority surviving thread, remember the last accepting position, and back off to it on a dead end. The accept-truncation in `dfa.rs` is the trick — any consuming edge left in a state outranks that state's accept, so "prefer to consume" is greedy, while a lazy/earlier-branch path has no surviving consuming edge and accepts immediately (e.g. `a|ab` on `"ab"` → `"a"`, `a.*?b` stops at the first `b`). `End` edges only mark a state accepting; the zero-width moves are `EndOfInput` (`$`/`\z`), `WordBoundary`, and the `(?m)` line anchors `StartOfLine` (`^`) / `EndOfLine` (`$`). `find_prefix` and the generated matcher take a `prev: Option<char>` (the char before the slice) so a mid-input slice still evaluates `^`/`\b` against the correct preceding context (the conformance search passes the real preceding char; the generated tokenizer passes `None`).

Supported syntax: literals, `[...]` classes and `a-z` ranges, negation `[^...]`, `.` (any char except `\n`, like the `regex` crate), the quantifiers `* + ?` and counted `{n} {n,} {n,m}` (each in a greedy or lazy `*? +? ?? {n,m}?` form), control-char escapes (`\n \t \r \0 \f \v`), hex/codepoint escapes (`\x41`, `\uXXXX`, `\UXXXXXXXX`, and braced `\x{..}`/`\u{..}`/`\U{..}`), Perl shorthand classes (`\d \D \w \W \s \S` — ASCII by default, full Unicode sets under `(?u)` via `regex-syntax`; a negated shorthand inside a `[...]` class is materialised as its complement), Unicode property classes `\p{..}`/`\P{..}` (and the `\pL` shorthand; resolved to codepoint ranges at build time via `regex-syntax` in `unicode.rs`, usable top-level and inside `[...]`), a leading `^`/`\A` (no-op for a prefix matcher), a trailing `$`/`\z` (end-of-input assertion, lowered to `Atom::EndOfInput`/`TransitionEvent::EndOfInput`), and word boundaries `\b`/`\B` (`Atom::WordBoundary { negate, unicode }` — ASCII `[0-9A-Za-z_]` word-ness by default, Unicode `\w` word-ness under `(?u)`; the matcher tests word-ness against `prev`/lookahead, the Unicode set emitted once as a binary-searched range table), and `(?m)` multiline `^`/`$` (`Atom::StartOfLine`/`EndOfLine`: `^` holds at start-of-input or after a `\n`, `$` at end-of-input or before a `\n`, and they may appear anywhere/quantified). The word-boundary/anchor zero-width assertions are evaluated in the generated matcher loop, which holds a lookahead char + `prev` and only advances on a consuming match (so `MatchedEmpty` transitions are truly zero-width). **Alternation `a|b` and grouping `(...)`** are supported, including nesting, a quantifier on a group (`(ab)+`), and non-capturing/named groups (`(?:...)`, `(?P<n>...)`, `(?<n>...)` — all treated identically since this engine never extracts capture spans). **Inline flags / modes** are supported — both the bare `(?flags)` directive (applies to the rest of the enclosing group) and the scoped `(?flags:...)` group, with `-` clearing: `i` (case-insensitive — a cased literal becomes a multi-case group and class members/ranges are folded; ASCII folding by default, full Unicode simple case folding under `(?iu)` via `regex-syntax`, see `unicode::case_fold`), `s` (dot-all — `.` lowers to an inverted empty class), `x` (verbose — drop unescaped whitespace + `#` comments outside `[...]`), `U` (swap-greedy), and `u` (Unicode toggle — sets a `unicode` flag, defaulting *off* so `#[token(regex = ...)]` stays ASCII; when on, `\d \w \s` use their full Unicode sets, `\b`/`\B` use Unicode word-ness, and `(?i)` does Unicode case folding). `m` (multiline — `^`/`$` become `\n` line anchors, see above). See `Flags`/`apply_flag`/`fold_entry` in `parse.rs`. Lookaround and the `R` (CRLF) flag are rejected at parse time — the line-anchor model is `\n`-only and can't yet treat `\r\n` as one terminator. Still missing: POSIX classes, directional half-boundaries (`\b{start}`/`\b{end}`), octal escapes, CRLF/custom line terminators — use `regex_full` for those. `REGEX_PARITY.md` tracks the remaining gaps against the `regex` crate.

### Regex conformance (`regex-conformance/`)

The `testdata/` directory holds the upstream `regex` crate's TOML test corpus, parsed with the `regex-test` crate. The `regex-conformance` crate runs every test against **both** forms of the engine and prints a per-engine summary (the two should always agree):

- the **runtime interpreter** — `SimpleRegex::find_prefix`, walking the DFA directly;
- the **compiled-Rust engine** — `fn(&str) -> Option<(&str, &str)>` matchers that `regex-conformance/build.rs` code-generates via `SimpleRegex::generate_parser` (the exact code `#[token(regex = ...)]` expands to), one per test, looked up by full name.

Both engines are anchored prefix matchers, so the harness (`run_search` in `src/lib.rs`) turns each into a leftmost, non-overlapping search to match the corpus' expectations. The `build.rs` codegen is why the engine had to be a normal crate rather than living inside the proc-macro crate. Each test lands in one of: pass, fail-to-parse (parser rejected it), fail-to-pass (parsed but wrong matches), or skipped (regex set / non-UTF-8 haystack). The test always passes — it's a bring-up harness, not a gate — and prints failing test names plus counts and timings under `--nocapture`. `REGEX_PARITY.md` records the latest tallies.

### Span tracking

Every matcher computes a `Span { line_start, col_start, line_stop, col_stop }` and advances the tokenizer's running `line`/`col`. `Span`'s `PartialEq` always returns `true` and `Hash` is a no-op, so `Spanned<T>` tokens compare by value, not by location. Matchers pick a newline-counting path vs a fast column-only path at compile time via `SimpleRegex::could_capture_newline()`. When editing span code, note the four emission sites must stay consistent: the literal trie (`lit_table.rs`), `codegen/simple_regex.rs`, `codegen/full_regex.rs`, and the inline `illegal`/`parse_fn` blocks in `lib.rs`.
