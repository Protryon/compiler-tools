# Regex Parity: `simple_regex` vs the `regex` crate

Tracking how the custom compile-time engine
(`compiler-tools-regex/src/`) compares to the `regex` crate, so we
can close the gaps one at a time.

Pipeline: `parse.rs` (pattern → AST) → `nfa.rs` (Thompson construction) → `dfa.rs`
(priority-preserving subset construction) → `generate.rs`. The AST is a `Vec<AtomRepeat>`,
where an `Atom::Alternation(Vec<branch>)` holds nested sub-expressions, so groups and
alternation compose recursively. The matchers (`find_prefix` and the generated code)
are **leftmost-first**, like the `regex` crate: each DFA state is a *priority-ordered*
closure of NFA states truncated at the accepting state, so the matcher follows the
highest-priority surviving thread, remembers the last accepting position, and backs off
to it on a dead end. This is what makes greedy vs lazy quantifiers and alternation order
match the `regex` crate (e.g. `a|ab` on `"ab"` → `"a"`).

## Supported today

- Literal characters and `\`-escaping of metacharacters.
- Character classes `[...]`, ranges (`a-z`), negation `[^...]`.
- `.` — any char **except `\n`** (matches the `regex` crate default).
- Quantifiers `*`, `+`, `?`, both greedy and **lazy / non-greedy** (`*?`, `+?`, `??`,
  `{n,m}?`). Laziness is an `AtomRepeat::lazy` flag set in `parse.rs` (`consume_lazy`)
  that flips the priority of a split state's epsilon edges in `nfa.rs`; the
  priority-ordered DFA then prefers the shorter match (`a.*?b`, `<.+?>`).
- Counted repetition `{n}`, `{n,}`, `{n,m}` (and lazy `{n,m}?`) — unrolled at parse
  time into the existing repeat atoms (capped at `MAX_REPEAT = 1024`); a
  malformed/oversized brace falls back to a literal `{`. See `apply_counted` /
  `parse_repeat_spec`.
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
- **Multiline anchors** `^`/`$` under `(?m)`: lowered to the zero-width
  `Atom::StartOfLine` / `Atom::EndOfLine` assertions, which can appear anywhere in the
  pattern (`(?m)^[a-z]+$`, `(?m)$\n^`). `^` holds at the start of input or just after a
  `\n`; `$` at the end of input or just before a `\n`. Like `\b`, they are evaluated in
  the matcher loop against the surrounding chars. To know whether a *mid-input* slice
  begins at a line boundary, the matcher takes a `prev: Option<char>` argument (the char
  before the slice; `None` = start of text); the conformance search supplies the real
  preceding char, and the generated tokenizer passes `None` (each token match is
  anchored at its start). Line terminator is `\n` only — `(?R)` CRLF mode is still
  rejected. Threading `prev` also fixes `\b` at a non-zero search offset.
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
  **leftmost-first**, like the `regex` crate: the earlier branch wins when both are
  viable (`a|ab` on `"ab"` → `"a"`), and greedy quantifiers still consume maximally —
  e.g. `/\*.*\*/` spans to the *last* `*/`; use the classic `/\*([^*]|\*[^/])*\*/` or
  the lazy `/\*.*?\*/` to stop at the first.
- **Non-capturing / named groups** `(?:...)`, `(?P<name>...)`, `(?<name>...)` — all
  treated identically (this engine extracts no capture spans). See
  `consume_group_prefix`.
- **Inline flags / modes** — both the bare `(?flags)` directive (applies to the rest
  of the enclosing group, crossing `|`) and the scoped `(?flags:...)` group, including
  the clearing `-` (`(?i-s:...)`). Supported letters:
  - `i` — ASCII case-insensitive. A cased literal becomes a two-case group and a class
    member/range is folded to both cases (`(?i)[a-c]` also matches `A`..`C`); folding
    the members *before* `[^...]` negation makes `(?i)[^a]` exclude both cases. Unicode
    case-folding is **not** done — use `regex_full` for that.
  - `s` — dot-all: `.` lowers to an inverted *empty* class, so it also matches `\n`.
  - `x` — verbose: unescaped whitespace is dropped and `#` starts a line comment
    (neither applies inside `[...]`); `\ ` is still a literal space.
  - `U` — swap-greedy: every quantifier's default greediness is flipped, so a trailing
    `?` un-flips it (`(?U)a*` is lazy, `(?U)a*?` greedy).
  - `m` — multiline: `^`/`$` become the `\n` line anchors described above
    (`Atom::StartOfLine` / `Atom::EndOfLine`).
  - `u` — Unicode toggle, accepted as a **no-op** (this engine is ASCII-only, so `(?-u)`
    is already its native mode and `(?u)` can't upgrade it). This is what lets the
    many `(?-u:...)` no-unicode patterns parse.
  See `Flags` / `apply_flag` / `consume_group_prefix` in `parse.rs`.

## Still missing

### Anchors & boundaries
- [x] **Multiline anchors** — `^`/`$` as `\n` line anchors under `(?m)`. Done (the
  matcher takes the preceding char so a mid-input slice knows its line context).
- [ ] **CRLF line terminator** — `(?R)`, which treats `\r\n` as a single line
  terminator for `^`/`$`/`.`. Still rejected; the line-anchor model is `\n`-only.

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
- [ ] **CRLF flag** — `(?R)`. Retargets `^`/`$`/`.` to `\r\n` line boundaries; still
  *rejected* at parse time (a hard `compile_error!`) because the line-anchor model is
  `\n`-only. `(?i) (?m) (?s) (?x) (?U) (?u)` are supported — see above.

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

The harness also folds the corpus' *test-level* options into inline flags so a
pattern behaves like it would under the `regex` crate's builder switches: a test with
`case-insensitive = true` is run as `(?i)<pattern>` (see `effective_pattern`, mirrored
in both `src/lib.rs` and `build.rs`). Only `case-insensitive` is wired up so far.

Latest results (both engines are identical, confirming the interpreter and the
generated matcher stay in lock-step):

| | runtime interpreter | compiled-rust engine |
|---|---|---|
| total | 1184 | 1184 |
| pass | 682 | 682 |
| fail-to-parse | 109 | 109 |
| fail-to-pass | 319 | 319 |
| skipped | 74 | 74 |
| per search | ~3.2 µs | ~2.5 µs |

Adding `(?m)` multiline line anchors (`^`/`$` as zero-width `\n` boundaries) and
threading the preceding char into the matcher raised the pass count 622 → 682 and
dropped fail-to-parse 161 → 109. fail-to-pass also fell (328 → 319): the `prev` argument
fixes `\b` at a non-zero search offset, so some word-boundary tests now pass too.
Before that, adding inline flags (`(?imsxU)` directives and scoped groups) plus the
harness' `case-insensitive` → `(?i)` folding raised the pass count 590 → 622 and dropped
fail-to-parse 207 → 161 (46 patterns that used to be rejected now parse — case-folding,
dot-all, verbose, swap-greedy and the no-op `(?-u)`/`(?u)` toggles); some of those landed
in fail-to-pass instead (chiefly Unicode case-folding the ASCII fold can't reproduce).
Earlier milestones: switching to a priority-preserving (leftmost-first) subset
construction with lazy quantifiers took 568 → 590; alternation/grouping before that more
than doubled it (269 → 568). The remaining fail-to-pass cases stem from the gaps above
(ASCII-only Unicode, no backtracking for boundaries, CRLF). The `--nocapture` output
lists every failing test name to make triage easy.

## Highest-impact gaps (ranked by conformance count)

The 161 fail-to-parse and 328 fail-to-pass failures cluster into a few features, so
impact is concentrated. Counts below are from grouping the `--nocapture` failure list
by test suite.

### 1. Multiline / line anchors — ~~`(?m)`~~, CRLF, custom line-terminator
**`(?m)` is now done** (pass 622 → 682). The fix was the calling convention: the
matcher now takes a `prev: Option<char>` argument (the char before its slice), so
`run_search` can feed the real preceding char and `^`/`$` lower to zero-width
`StartOfLine`/`EndOfLine` assertions evaluated in the matcher loop like `\b`. This also
fixed `\b` at a non-zero offset, so fail-to-pass fell too.

Remaining in this bucket:

| suite | ~count | needs |
|---|---|---|
| crlf / `(?R)` | ~30 | `\r\n` treated as a single line terminator |
| line-terminator | ~9 | custom (non-`\n`) line terminators |

CRLF mode reuses the same `prev`/lookahead machinery but must treat `\r\n` as one
terminator (a `$` before `\r`, a `^` after `\n`) and exclude `\r\n` from `.` — a
follow-on once the `\n`-only model is generalised to a configurable terminator.

### 2. Unicode awareness — `\p{…}`, Unicode `\w`/`\b`/`.`, Unicode case folding → ~150–200 tests
Dominates fail-to-pass.

| suite | count |
|---|---|
| unicode | 72 |
| word-boundary-special | 74 |
| word-boundary | 40 |
| utf8 | 16 |

Most fail because the engine is ASCII/byte-class based: Unicode word-ness for `\b`,
`\p{...}` classes, and full Unicode case folding (the `(?i)` ASCII fold can't reproduce
it). Highest raw count, but by far the most expensive — needs Unicode property tables
and UTF-8-aware transitions, i.e. a near-rewrite of the class model. A long-term
direction, not a quick win.

### 3. ASCII word-boundary correctness → ~30–60 tests (subset of #2's buckets)
A meaningful slice of the word-boundary failures are pure ASCII and fixable *without*
Unicode — `\b` / `^\b` / `\b^` producing empty matches at string/word edges (`wb2`,
`wb4`, `wb43`), and `.*\bx`-style patterns the non-backtracking matcher gets wrong.
Fixing zero-width `\b` handling and empty-match iteration in the search loop is medium
effort and decoupled from the big Unicode lift.

### 4. Cheap, localized parser wins → small but easy
- **Negated shorthands inside classes** — `[\D]`, `[a\W]` (the flat
  `Group(bool, Vec<GroupEntry>)` can't represent a union with a negated subset).
- **POSIX classes** — `[[:alpha:]]`.

Low counts, but each is a self-contained change to `parse.rs`'s group model.

**Bottom line:** the multiline/line-anchor plumbing (#1) is done — `(?m)` landed +60
tests by threading the preceding char into the matcher. Next, for raw pass count Unicode
(#2) is the largest pool but the most work; the CRLF/line-terminator remainder of #1 and
the cheap parser wins (#4) are smaller, lower-effort follow-ons. Scope #2 as a separate
Unicode initiative.

## Escape hatch

Until a feature lands here, `#[token(regex_full = "...")]` routes to the real
`regex` crate at runtime.
