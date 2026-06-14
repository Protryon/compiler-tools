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
- **A zero-width assertion that must expose a *further consuming* match in
  parallel with a live greedy thread** — e.g. `.*\bx` on `"x"`, where `.*` could
  consume the `x` *or* leave it for `\bx`. The single-thread DFA walk follows the
  greedy consume and loses the assertion-gated continuation. This is the narrow
  residue of "word-boundary non-backtracking": the common cases now work — a `\b`
  that gates an *accept* behind a greedy run (`.+\b`, `\B(?:fo|foo)\B`) backs off
  correctly (the matcher records the assertion-gated accept while the greedy thread
  keeps going), and `^`/`$`/`\A`/`\z`/`\bword\b` are all honoured in a search.
  Closing the remaining case needs a multi-thread (Pike-VM-style) step rather than
  the single-state DFA walk. (Independent of ASCII vs Unicode.)

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
| pass | 941 | 941 |
| fail-to-parse | 0 | 0 |
| fail-to-pass | 169 | 169 |
| skipped | 74 | 74 |
| per search | ~6.7 µs | ~2.3 µs |

Progression — the engine is already codepoint-based, so the class-side Unicode
features are pure parse-time range expansions: `\p{…}` property classes 682 → 748,
`(?iu)` Unicode simple case folding 748 → 752, Unicode `\d \w \s` +
negated-shorthand-in-class 752 → 759, and Unicode `\b`/`\B` word-ness (the one
matcher-side change — a shared word-range table in both engines) 759 → 767. CRLF
mode `(?R)` — line anchors and `.` treat `\r`/`\n`/`\r\n` as one terminator set —
cleared the last fail-to-parse cases (the corpus runs many tests with the `R`
flag, which the parser used to reject outright) and lifted 767 → 827. Start-/
end-of-text anchors — modelling `^`/`\A` as a real `prev`-is-`None` assertion
(instead of dropping a leading `^`) and `$`/`\z` as an end-of-text assertion
anywhere (not just trailing), so a search honours them at every position — lifted
827 → 865. Assertion-gated greedy backoff — recording an accept reachable through
zero-width assertions that hold at the current position, so `.+\b`/`\B…\B` back
off to it — 865 → 868. Directional half-boundaries — `\b{start}`/`\b{end}`/
`\b{start-half}`/`\b{end-half}`, folded into the existing word-boundary assertion as
a `WordBoundaryKind` (each kind a boolean condition over the word-ness of `prev`/
lookahead), so they reuse the same DFA edge, matcher loop and codegen as plain
`\b`/`\B` — 868 → 941.

The remaining failures cluster into the gaps above:

| bucket | ~tests | notes |
|---|---|---|
| Assertion in parallel with a live greedy thread | ~2 | a `\b` that must expose a *further consuming* match the greedy thread could also take (`.*\bx`), or a zero-width branch that must out-prioritise a consuming one (`(?:\b|%)+`); needs a multi-thread step. (The accept-backoff and `^`/`$`/`\A`/`\z` cases are now handled.) |
| Bytes-mode word boundaries at non-char-boundaries | small | `\B`/`\b{…}` matching inside a multi-byte char in a `utf8=false` haystack; unrepresentable in this `&str`-based engine |
| Custom (non-`\n`/CRLF) line terminators | small | the `regex` crate's arbitrary-terminator option; the `\n` and `(?R)` CRLF sets are modeled |
| POSIX classes, octal escapes | small | self-contained changes to `parse.rs` |
