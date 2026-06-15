# Regex Parity: `simple_regex` vs the `regex` crate

The differences between the custom compile-time engine
(`compiler-tools-regex/src/`) and the `regex` crate, so we can close the gaps
one at a time. For what the engine *does* support, see the engine section in
`CLAUDE.md` or the code.

Until a gap is closed, `#[token(regex_full = "...")]` routes to the real `regex`
crate at runtime.

## API parity

Both runtime engines expose one anchored primitive — `find_prefix(from, prev) ->
Option<(matched, remaining)>` on `Regex` (the DFA interpreter) and `JitRegex`
(the Cranelift JIT). The `RegexSearch` trait (`src/search.rs`) layers the
`regex`-crate-shaped *search orchestration* over that primitive as default methods,
so both engines share one API. Bring the trait into scope to use it.

Provided (leftmost-first, non-overlapping, matching the `regex` crate's defaults):

| method | notes |
|---|---|
| `is_match` / `is_match_at` | unanchored search for any match |
| `find` / `find_at` | leftmost match as a `Match` (`start`/`end`/`range`/`as_str`/`is_empty`/`len`) |
| `find_iter` → `Matches` | all non-overlapping matches, with the empty-match-adjacency rule |
| `replace` / `replace_all` / `replacen` | literal-string replacement, returning `Cow` |
| `split` / `splitn` → `Split`/`SplitN` | substrings between matches |

### API that needs engine work

- **Capture groups** — `captures`/`captures_iter`, a `Captures` type, capture-group
  spans (`m.get(1)`, named `["name"]`), and `$1`/`$name` expansion in the `replace*`
  family. This engine never tracks capture-span positions: every `(...)`, `(?:...)`
  and `(?P<n>...)` lowers to the same `Atom::Alternation` and the span identity is
  discarded during NFA construction. Exposing captures means threading capture-slot
  marks through the NFA/DFA (or running a Pike-VM-style submatch pass), so the
  replacement helpers take a literal string for now rather than a `regex::Replacer`.
  This is the single largest API gap and overlaps the multi-thread engine work the
  conformance buckets call out below.
- **`shortest_match` / `shortest_match_at`** — the engine is leftmost-first/greedy by
  DFA priority; returning the *shortest* accepting prefix would need a separate
  shortest-match DFA pass (or an earliest-match search mode), so it isn't offered.
- **Overlapping / `match-kind = all` / `earliest` search** — search-orchestration
  modes the leftmost-first `find_iter` doesn't model; see the conformance buckets.

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
- **A zero-width assertion explored under a repetition, or in parallel with a live
  consuming thread** — the single biggest cluster of remaining failures (~69, see the
  conformance table). Two shapes: (1) a zero-width branch *alternated with* a consuming
  one inside a repeat, where both must advance in lock-step — `(?m)(?:^|a)*`, `^*`/`$+`,
  `(?:\b|%)+`; and (2) an assertion that must expose a *further consuming* match a greedy
  thread could also take — `.*\bx` on `"x"` (`.*` could eat the `x` or leave it for `\bx`),
  `\B.*`. The single-thread DFA walk follows the highest-priority thread and loses the
  other. The *common* cases already work: a `\b` that gates an *accept* behind a greedy
  run (`.+\b`, `\B(?:fo|foo)\B`) backs off correctly (the matcher records the assertion-
  gated accept while the greedy thread keeps going), and `^`/`$`/`\A`/`\z`/`\bword\b` are
  honoured in a search. Closing the rest needs a multi-thread (Pike-VM-style) step rather
  than the single-state DFA walk. (Independent of ASCII vs Unicode.)

### Character classes
- **POSIX classes** — `[[:alpha:]]`, etc. (also inside a negated class, `[^[:space:],]`).
- **A leading `]` in a class** — `[]]`/`[^]…]`, where the `regex` crate treats the first
  `]` as a literal member rather than closing the class (`a[]]b`, `a[^]b]c`).

### Escapes
- **Octal escapes** — `\123`, `\o{...}`. (Hex/codepoint escapes *are* supported.)

## Not a gap (the `regex` crate doesn't support these either)

- Backreferences and lookaround (`(?=...)`, `(?<=...)`) — the `regex` crate
  forbids these to keep its finite-automaton guarantee. This engine rejects the
  lookaround syntax at parse time.

## Conformance corpus

The `regex-conformance` crate runs the upstream `regex` test corpus (`testdata/`,
parsed with the `regex-test` crate) against each form of the engine — the
runtime DFA interpreter (`Regex::find_prefix`), the generated-Rust
matcher (`generate_parser`, code-gen'd per-test in `regex-conformance/build.rs`),
and — under `--features jit` — the Cranelift JIT (`Regex::compile_jit`),
which the harness *asserts* matches the interpreter exactly (same DFA, so any
divergence is a lowering bug). Each test is one of: **pass**, **fail-to-parse** (parser rejected the pattern),
**fail-to-pass** (parsed but wrong matches), or **skipped** (regex set, or a
non-UTF-8 haystack this `&str`-based engine can't represent).

Run: `cargo test --package regex-conformance --test conformance -- --nocapture`
(lists every failing test name, plus counts and timings). Add `--features jit` to
also run the Cranelift JIT column.

Latest results (all engines identical, confirming the interpreter, the generated
matcher, and the JIT stay in lock-step):

| | runtime interpreter | compiled-rust engine | cranelift jit |
|---|---|---|---|
| total | 1184 | 1184 | 1184 |
| pass | 941 | 941 | 941 |
| fail-to-parse | 0 | 0 | 0 |
| fail-to-pass | 169 | 169 | 169 |
| skipped | 74 | 74 | 74 |
| per search | ~6.7 µs | ~2.3 µs | ~10.3 µs |

The per-search figures here are dominated by the harness's tiny haystacks and
per-position overhead, not the matchers themselves, so they don't reflect raw matcher
throughput — on a long input the JIT (with its inline UTF-8 decode) runs ~4.8× faster
than the interpreter (`jit::tests::jit_timing`, `--release --ignored`). The JIT's
identical pass/fail tallies are *asserted* by the harness, not just observed.

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

The 169 remaining failures split into three groups — a true **engine gap** the
single-pass DFA can't express, a set of **search-orchestration** modes the prefix-
matcher harness (`run_search`) doesn't emulate, and a handful of **representational /
unsupported-syntax** cases. The buckets below are exhaustive (they sum to 169):

| bucket | tests | kind | notes |
|---|---|---|---|
| Zero-width assertion explored under a repetition / in parallel with a consuming thread | 69 | engine gap | the single-thread DFA walk follows the highest-priority thread and can't run a zero-width branch *and* a consuming one in lock-step. Dominated by the `(?m)` repeat family — `(?:^|a)*`, `^*`/`^+`, `$*`/`$+`, `(?:^[a-z]{3}\n?)*` and their `(?R)`/no-multi variants — plus `(?:\b|%)+`, `\B.*`, `a*(^a)`, `(?m)^(?:[^ ]+?)$`. Needs a Pike-VM-style multi-thread step. (Accept-backoff and bare `^`/`$`/`\A`/`\z`/`\b…\b` already work.) |
| Non-leftmost-first search / match modes | 35 | harness | the corpus' `search-kind = "overlapping"`, `match-kind = "all"`, and `search-kind = "earliest"` tests. `run_search` only emulates a leftmost-first, non-overlapping search, so it reports one match where these expect the overlapping / all / earliest set. The engine's per-position match is correct. |
| Bytes mode / `utf8 = false` haystacks | 20 | representational | byte-level semantics in a `&str` engine: `\B`/`[^a]` matching *inside* a multi-byte char, scoped byte-vs-Unicode boundary mixing (`(?:(?-u:\b)|(?u:…))+`), empty matches at non-char boundaries. Fundamentally unrepresentable here. |
| Empty-match iteration / adjacency | 13 | harness | the `regex` crate suppresses an empty match adjacent to a prior match and has specific empty-iteration rules (`b\|`, `abc\|.*?`, `b\|\|`, `(?:\|a)*`); `run_search` just steps one char on an empty match, so the match *set* differs. |
| POSIX classes & bracket-edge syntax | 12 | engine gap | `[[:alpha:]]`/`[[:upper:]]`/`[[:word:]]` (also inside `[^…]`) and a leading `]` in a class (`a[]]b`, `a[^]b]c`). Self-contained `parse.rs` work. |
| Custom (non-`\n`/CRLF) line terminators | 7 | engine gap | the `regex` crate's arbitrary-terminator byte option; only the `\n` and `(?R)` CRLF sets are modeled. |
| regex-lite ASCII-only baseline | 6 | harness | regex-lite is ASCII-only, but the harness folds the corpus' `unicode = true` default into a leading `(?u)`, so the engine uses Unicode `\d \w \s`/`\b` where these expect ASCII. A flag artifact, not an engine bug. |
| Patterns the `regex` crate rejects | 5 | engine gap | `(*)`, `*`, `(?:?)`, `(?)`, `(?m){1,1}` — the corpus expects a compile error; this engine accepts or mis-parses them rather than failing the build. |
| Misc | 2 | mixed | `^.{1,2500}` exceeds the `MAX_REPEAT` unroll cap (treated as a literal brace); `\b[0-9]+\b` against a Unicode digit haystack. |

So of the 169, ~93 (`multi-thread 69 + POSIX 12 + line-terminator 7 + invalid 5`) are
genuine engine work, ~48 (`search-mode 35 + empty-iter 13`) are harness search-
orchestration the prefix matcher doesn't model, and ~28 (`bytes 20 + regex-lite 6 +
misc 2`) are representational or `(?u)`-flag artifacts. The single biggest lever is the
multi-thread step (69 tests, almost all the `(?m)` repeat family).
