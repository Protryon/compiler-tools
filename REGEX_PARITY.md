# Regex Parity: `simple_regex` vs the `regex` crate

Tracking how the custom compile-time engine
(`compiler-tools-derive/src/simple_regex/`) compares to the `regex` crate, so we
can close the gaps one at a time.

Pipeline: `parse.rs` (pattern → AST) → `nfa.rs` → `dfa.rs` → `generate.rs`.
Atoms are a flat `Vec<AtomRepeat>`; there is no nested sub-expression model, which
is the main thing blocking alternation/grouping.

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

## Still missing

### Structural
- [ ] **Alternation** — `a|b`. No `|` operator at all.
- [ ] **Grouping** — `(...)`, capture groups, non-capturing `(?:...)`, named
  groups `(?P<name>...)`. The parser has no concept of nested sub-expressions; this
  is the largest item and unblocks several others.

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
  `// TODO: ... unicode ident_start` note in `mod.rs:22`.

### Escapes & flags
- [ ] **Octal escapes** — `\123`, `\o{...}`. (Hex/codepoint escapes are supported.)
- [ ] **Inline flags / modes** — `(?i)` case-insensitive, `(?m)` multiline,
  `(?s)` dotall, `(?x)` verbose, and the grouped `(?i:...)` forms.

## Not a gap (the `regex` crate doesn't support these either)

- Backreferences and lookaround (`(?=...)`, `(?<=...)`) — the `regex` crate forbids
  these to keep its finite-automaton guarantee.

## Escape hatch

Until a feature lands here, `#[token(regex_full = "...")]` routes to the real
`regex` crate at runtime.
