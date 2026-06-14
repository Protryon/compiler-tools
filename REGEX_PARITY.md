# Regex Parity: `simple_regex` vs the `regex` crate

The differences between the custom compile-time engine
(`compiler-tools-regex/src/`) and the `regex` crate, so we can close the gaps
one at a time. For what the engine *does* support, see the engine section in
`CLAUDE.md` or the code.

Until a gap is closed, `#[token(regex_full = "...")]` routes to the real `regex`
crate at runtime.

## Gaps

### Anchors & boundaries
- **CRLF line terminator** — `(?R)`, treating `\r\n` as a single line terminator
  for `^`/`$`/`.`. Rejected at parse time; the line-anchor model is `\n`-only.
  Custom (non-`\n`) line terminators are likewise unsupported.
- **ASCII word boundaries are non-backtracking** — a `\b` that would require
  *un*-consuming a greedy match (e.g. `.*\bx`) won't match where the `regex`
  crate would. `\bword\b`-style usage is fine.

### Character classes
- **Negated shorthands inside a class** — e.g. `[\D]`, `[a\W]`. Rejected: the
  flat `Group(bool, Vec<GroupEntry>)` can't represent the union of a positive set
  and a negated subset.
- **POSIX classes** — `[[:alpha:]]`, etc.
- **Unicode shorthands & semantics** — `\p{...}`/`\P{...}` property classes and
  `(?iu)` simple case folding *are* supported now (resolved to codepoint ranges at
  build time via `regex-syntax`; see `unicode.rs`). Still ASCII-only: the shorthands
  `\d \w \s` and `\b` word-ness. `(?u)` toggles a `unicode` flag (which already
  drives case folding) but doesn't yet retarget the shorthands/word-boundary to
  Unicode definitions. The engine defaults to ASCII (fast tokenizers); the
  conformance harness opts into Unicode where the corpus expects it (`(?u)`).

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
| pass | 752 | 752 |
| fail-to-parse | 109 | 109 |
| fail-to-pass | 249 | 249 |
| skipped | 74 | 74 |
| per search | ~3.1 µs | ~1.9 µs |

`\p{…}` property classes took pass 682 → 748, and `(?iu)` Unicode simple case
folding 748 → 752 — both pure parse-time range expansions, since the engine is
already codepoint-based.

The remaining failures cluster into the gaps above:

| bucket | ~tests | notes |
|---|---|---|
| Unicode `\w`/`\b`/`\s` | ~120–160 | dominates fail-to-pass; the shorthands/word-boundary need their Unicode range sets (same `regex-syntax` tables as `\p{…}`) |
| ASCII word-boundary correctness | ~30–60 | a subset is pure-ASCII and fixable without Unicode (zero-width `\b` empty matches, `.*\bx` non-backtracking) |
| CRLF / `(?R)` + custom line terminators | ~40 | reuses the `prev`/lookahead machinery but must treat `\r\n` as one terminator |
| Negated shorthands in classes, POSIX classes | small | self-contained changes to `parse.rs`'s group model |
