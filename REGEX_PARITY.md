# Regex Parity: `simple_regex` vs the `regex` crate

Tracking how the custom compile-time engine
(`compiler-tools-regex/src/`) compares to the `regex` crate, so we
can close the gaps one at a time.

Pipeline: `parse.rs` (pattern → AST) → `nfa.rs` (Thompson construction) → `dfa.rs`
(real subset construction) → `generate.rs`. The AST is a `Vec<AtomRepeat>`, where an
`Atom::Alternation(Vec<branch>)` holds nested sub-expressions, so groups and
alternation compose recursively. The matchers (`find_prefix` and the generated code)
are leftmost-longest: consume greedily, remember the last accepting position, back off
to it on a dead end.

## Supported today

- Literal characters and `\`-escaping of metacharacters.
- Character classes `[...]`, ranges (`a-z`), negation `[^...]`.
- `.` — any char **except `\n`** (matches the `regex` crate default).
- Quantifiers `*`, `+`, `?`.
- Counted repetition `{n}`, `{n,}`, `{n,m}` — unrolled at parse time into the
  existing repeat atoms (capped at `MAX_REPEAT = 1024`); a malformed/oversized
  brace falls back to a literal `{`. See `apply_counted` / `parse_repeat_spec`.
- Control-char escapes `\n \t \r \0 \f \v` decode to the real control character.
  See `escape_char`.
- Perl shorthand classes `\d \D \w \W \s \S`, ASCII semantics (matching the
  `regex` crate's `(?-u)` mode). Work top-level and inside `[...]`; a *negated*
  shorthand inside a positive class is rejected (the flat group model can't union
  it). See `shorthand_class`.
- Anchors `^`/`\A` (leading) and `$`/`\z` (trailing): a leading `^`/`\A` is a no-op
  for this prefix matcher and is dropped; a trailing `$`/`\z` lowers to an
  `Atom::EndOfInput` zero-width assertion (`TransitionEvent::EndOfInput`, only taken
  at EOF). A literal `^`/`$` elsewhere stays literal; a `\A`/`\z` elsewhere is rejected
  (it can never hold).
- Word boundaries `\b` / `\B` (ASCII `[0-9A-Za-z_]` word-ness, input edges count as
  non-word). Lowered to `Atom::WordBoundary` / `TransitionEvent::WordBoundary`,
  evaluated against the previous and lookahead chars in the matcher loop. The matcher
  does not backtrack, so a boundary that would require *un*-consuming a greedy match
  (e.g. `.*\bx`) won't match where the `regex` crate would — `\bword\b`-style usage is
  fine. See the loop in `generate.rs`.
- Numeric / codepoint escapes: fixed-width hex `\x41` (2), `\uXXXX` (4),
  `\UXXXXXXXX` (8) and the braced `\x{..}` / `\u{..}` / `\U{..}` forms, top-level and
  inside `[...]` (including as range bounds). See `parse_hex_escape`.
- **Alternation** `a|b` and **grouping** `(...)`, including arbitrary nesting and a
  quantifier on a group (`(ab)+`, `(a|bc)*`). Parsed recursively into
  `Atom::Alternation`; the real subset construction in `dfa.rs` partitions consuming
  edges into disjoint classes with unioned targets, so shared-prefix alternation
  (`a|ab`, `(a|ab)z`) is deterministic and correct without backtracking. Matching is
  leftmost-longest (greedy), like the `regex` crate — e.g. `/\*.*\*/` spans to the
  *last* `*/`; use the classic `/\*([^*]|\*[^/])*\*/` to stop at the first.
- **Non-capturing / named groups** `(?:...)`, `(?P<name>...)`, `(?<name>...)` — all
  treated identically (this engine extracts no capture spans). See
  `consume_group_prefix`.

## Still missing

### Quantifiers
- [ ] **Lazy / non-greedy** — `*?`, `+?`, `??`, `{n,m}?`. A second quantifier is
  currently consumed as a literal (see `double_repeat_treats_second_as_literal`).

### Anchors & boundaries
- [ ] **Multiline / mid-pattern anchor semantics** — `^`/`$` as line anchors under
  `(?m)`. Only the leading-`^`/trailing-`$` (start/end of haystack) cases are modeled;
  a prefix matcher has no line context. `\A`, `\z`, `\b`, `\B` are done.

### Character-class shorthands
- [ ] **Negated shorthands inside a class** — e.g. `[\D]`, `[a\W]`. Currently
  rejected because the flat `Group(bool, Vec<GroupEntry>)` can't represent the
  union of a positive set and a negated subset.
- [ ] **POSIX classes** — `[[:alpha:]]`, etc.
- [ ] **Unicode classes** — `\p{...}`, `\P{...}` and Unicode-aware class semantics.
  (Shorthand classes and `\b` word-ness are ASCII-only too.) See the
  `// TODO: ... unicode ident_start` note in `lib.rs`.

### Escapes & flags
- [ ] **Octal escapes** — `\123`, `\o{...}`. (Hex/codepoint escapes are supported.)
- [ ] **Inline flags / modes** — `(?i)` case-insensitive, `(?m)` multiline,
  `(?s)` dotall, `(?x)` verbose, and the grouped `(?i:...)` forms. These are now
  *rejected* at parse time (a hard `compile_error!`) rather than silently
  mis-parsed.

## Not a gap (the `regex` crate doesn't support these either)

- Backreferences and lookaround (`(?=...)`, `(?<=...)`) — the `regex` crate forbids
  these to keep its finite-automaton guarantee. This engine rejects the lookaround
  syntax at parse time.

## Conformance corpus

The `regex-conformance` crate runs the upstream `regex` test corpus (`testdata/`,
parsed with the `regex-test` crate) against **both** forms of the engine — the
runtime DFA interpreter (`SimpleRegex::find_prefix`) and the generated-Rust
matcher (`generate_parser`, code-gen'd per-test in `regex-conformance/build.rs`) —
and reports them separately. Each test is one of: **pass** (parsed + exact
expected matches), **fail-to-parse** (parser rejected the pattern),
**fail-to-pass** (parsed but wrong matches), or **skipped** (regex set, or a
non-UTF-8 haystack this `&str`-based engine can't represent).

Run: `cargo test --package regex-conformance --test conformance -- --nocapture`

Latest results (both engines are identical, confirming the interpreter and the
generated matcher stay in lock-step):

| | runtime interpreter | compiled-rust engine |
|---|---|---|
| total | 1184 | 1184 |
| pass | 568 | 568 |
| fail-to-parse | 207 | 207 |
| fail-to-pass | 340 | 340 |
| skipped | 69 | 69 |
| per search | ~2.8 µs | ~1.7 µs |

Adding alternation/grouping (and switching to a real subset construction with
leftmost-longest matching) more than doubled the pass count (269 → 568) and roughly
halved the fail-to-pass count. The flip side is that fail-to-parse rose (1 → 207):
now that `(` / `)` / `|` are structural, patterns using unsupported group syntax
(inline flags, lookaround) are explicitly rejected instead of silently mis-parsed as
literals — a cleaner failure mode. The remaining fail-to-pass cases stem from the
gaps above (lazy quantifiers, ASCII-only Unicode, no backtracking for boundaries).
The `--nocapture` output lists every failing test name to make triage easy.

## Escape hatch

Until a feature lands here, `#[token(regex_full = "...")]` routes to the real
`regex` crate at runtime.
