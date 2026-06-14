# Regex Parity: `simple_regex` vs the `regex` crate

The differences between the custom compile-time engine
(`compiler-tools-regex/src/`) and the `regex` crate, so we can close the gaps
one at a time. For what the engine *does* support, see the engine section in
`CLAUDE.md` or the code.

Until a gap is closed, `#[token(regex_full = "...")]` routes to the real `regex`
crate at runtime.

## Gaps

Unicode coverage is broad now and all built on the same idea — the engine is
already codepoint-based, so Unicode features expand into the existing range model
using `regex-syntax`'s tables at build time (see `unicode.rs`), with no runtime
dependency. Supported: `\p{...}`/`\P{...}` property classes, Unicode `\d \w \s`
(and negated `\D \W \S`, including inside a `[...]` class), `(?iu)` simple case
folding, and Unicode `\b`/`\B` word-ness. The engine defaults to ASCII (fast
tokenizers) and opts into Unicode with `(?u)`; the conformance harness sets it
where the corpus expects it.

### Anchors & boundaries
- **Custom line terminators** — a line terminator other than `\n` or (under `(?R)`)
  `\r`/`\n`/`\r\n`. The `regex` crate lets you set an arbitrary terminator byte;
  this engine only models the `\n` and CRLF sets. (CRLF itself — `(?R)`, treating
  `\r\n` as a single terminator for `^`/`$`/`.` — *is* supported now.)
- **Directional half-boundaries** — `\b{start}`, `\b{end}`, `\b{start-half}`,
  `\b{end-half}`. A separate assertion type from plain `\b`; not parsed.
- **Word boundaries are non-backtracking** — a `\b` that would require
  *un*-consuming a greedy match (e.g. `.*\bx`) won't match where the `regex`
  crate would. `\bword\b`-style usage is fine. (Independent of ASCII vs Unicode.)

### Character classes
- **POSIX classes** — `[[:alpha:]]`, etc.

### Escapes
- **Octal escapes** — `\123`, `\o{...}`. (Hex/codepoint escapes *are* supported.)

## Not a gap (the `regex` crate doesn't support these either)

- Backreferences and lookaround (`(?=...)`, `(?<=...)`) — the `regex` crate
  forbids these to keep its finite-automaton guarantee. This engine rejects the
  lookaround syntax at parse time.

## Conformance corpus

The `regex-conformance` crate runs the upstream `regex` test corpus (`testdata/`,
parsed with the `regex-test` crate) against **both** forms of the engine — the
runtime DFA interpreter (`SimpleRegex::find_prefix`) and the generated-Rust
matcher (`generate_parser`, code-gen'd per-test in `regex-conformance/build.rs`).
Each test is one of: **pass**, **fail-to-parse** (parser rejected the pattern),
**fail-to-pass** (parsed but wrong matches), or **skipped** (regex set, or a
non-UTF-8 haystack this `&str`-based engine can't represent).

Run: `cargo test --package regex-conformance --test conformance -- --nocapture`
(lists every failing test name, plus counts and timings).

Latest results (both engines identical, confirming the interpreter and the
generated matcher stay in lock-step):

| | runtime interpreter | compiled-rust engine |
|---|---|---|
| total | 1184 | 1184 |
| pass | 827 | 827 |
| fail-to-parse | 0 | 0 |
| fail-to-pass | 283 | 283 |
| skipped | 74 | 74 |
| per search | ~3.3 µs | ~2.2 µs |

Progression — the engine is already codepoint-based, so the class-side Unicode
features are pure parse-time range expansions: `\p{…}` property classes 682 → 748,
`(?iu)` Unicode simple case folding 748 → 752, Unicode `\d \w \s` +
negated-shorthand-in-class 752 → 759, and Unicode `\b`/`\B` word-ness (the one
matcher-side change — a shared word-range table in both engines) 759 → 767. CRLF
mode `(?R)` — line anchors and `.` treat `\r`/`\n`/`\r\n` as one terminator set —
cleared the last fail-to-parse cases (the corpus runs many tests with the `R`
flag, which the parser used to reject outright) and lifted 767 → 827.

The remaining failures cluster into the gaps above:

| bucket | ~tests | notes |
|---|---|---|
| Directional half-boundaries (`\b{start}`/`\b{end}`/…) | ~70 | a distinct assertion type the parser doesn't yet recognise (both ASCII and Unicode variants) |
| Word-boundary non-backtracking + zero-width empty matches | ~15 | a `\b` requiring an un-consumed greedy match (`.*\bx`) or an empty match at a string/word edge; independent of Unicode |
| Custom (non-`\n`/CRLF) line terminators | small | the `regex` crate's arbitrary-terminator option; the `\n` and `(?R)` CRLF sets are modeled |
| POSIX classes, octal escapes | small | self-contained changes to `parse.rs` |
