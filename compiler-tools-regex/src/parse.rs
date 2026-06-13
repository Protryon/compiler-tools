use super::*;

/// An upper bound on how far a `{n,m}` counted repetition is unrolled. Patterns
/// that ask for more are treated as a literal `{...}` so a stray brace cannot
/// blow up macro-expansion time / generated code size.
const MAX_REPEAT: usize = 1024;

/// The inline mode flags (`(?imsxU)`) currently in effect while parsing. Flags
/// are scoped: a bare `(?flags)` directive mutates the flags for the rest of the
/// enclosing group, while a `(?flags:...)` group applies them only to its body.
///
/// The `u` (Unicode) flag is accepted but ignored — this engine is ASCII-only, so
/// `(?-u)` is already its native behaviour and `(?u)` cannot upgrade it. The `m`
/// (multiline) flag retargets `^`/`$` to `\n` line boundaries, modeled as the
/// zero-width [`Atom::StartOfLine`]/[`Atom::EndOfLine`] assertions (the matcher
/// loop evaluates them against the surrounding chars, like `\b`). The `R` (CRLF)
/// flag is still *rejected* — it would need `\r\n` treated as a single line
/// terminator, which this `\n`-only model can't yet represent.
#[derive(Clone, Copy, Default)]
struct Flags {
    /// `i` — ASCII case-insensitive. A cased literal/class member matches both cases.
    case_insensitive: bool,
    /// `s` — dot-all: `.` also matches `\n`.
    dot_matches_newline: bool,
    /// `x` — verbose: unescaped whitespace is ignored and `#` starts a line comment.
    ignore_whitespace: bool,
    /// `U` — swap-greedy: the default greediness of every quantifier is flipped, so
    /// a trailing `?` un-flips it again.
    swap_greedy: bool,
    /// `m` — multiline: `^`/`$` match at `\n` line boundaries (not just the input
    /// edges), lowered to [`Atom::StartOfLine`]/[`Atom::EndOfLine`].
    multiline: bool,
}

/// Applies one flag letter to `flags`, setting it when `negate` is false and
/// clearing it after a `-`. Returns `None` for an unsupported flag (`R`) or an
/// unknown letter so the whole pattern is rejected rather than mis-parsed.
fn apply_flag(flags: &mut Flags, c: char, negate: bool) -> Option<()> {
    let value = !negate;
    match c {
        'i' => flags.case_insensitive = value,
        's' => flags.dot_matches_newline = value,
        'x' => flags.ignore_whitespace = value,
        'U' => flags.swap_greedy = value,
        'm' => flags.multiline = value,
        // `u` (Unicode) is a no-op: this engine is ASCII-only either way.
        'u' => {}
        // `R` (CRLF) needs `\r\n` treated as one line terminator — not yet modeled.
        _ => return None,
    }
    Some(())
}

/// Expands one class entry into `out` under ASCII case-folding: a cased member is
/// emitted alongside its opposite case so `(?i)[a-c]` also matches `A`..`C`.
/// Folding the *members* (before any `[^...]` negation) is what makes
/// `(?i)[^a]` correctly exclude both `a` and `A`.
fn fold_entry(entry: GroupEntry, out: &mut Vec<GroupEntry>) {
    out.push(entry);
    match entry {
        GroupEntry::Char(c) if c.is_ascii_alphabetic() => {
            out.push(GroupEntry::Char(if c.is_ascii_lowercase() {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }));
        }
        GroupEntry::Range(a, b) => {
            // Fold the range's lowercase span up to uppercase, and vice versa.
            let (lo, hi) = (a.max('a'), b.min('z'));
            if lo <= hi {
                out.push(GroupEntry::Range(lo.to_ascii_uppercase(), hi.to_ascii_uppercase()));
            }
            let (lo, hi) = (a.max('A'), b.min('Z'));
            if lo <= hi {
                out.push(GroupEntry::Range(lo.to_ascii_lowercase(), hi.to_ascii_lowercase()));
            }
        }
        _ => {}
    }
}

/// Pops the pending `Char('-')` and the range's start char off `entries`, returning
/// the start char. Returns `None` on a malformed state rather than panicking, so a bad
/// pattern surfaces as a `compile_error!` instead of an ICE-style proc-macro panic.
fn pop_range_start(entries: &mut Vec<GroupEntry>) -> Option<char> {
    match (entries.pop(), entries.pop()) {
        (Some(GroupEntry::Char('-')), Some(GroupEntry::Char(start))) => Some(start),
        _ => None,
    }
}

/// Decodes the character following a backslash. Recognised control-char escapes
/// (`\n \r \t \0 \f \v`) become the actual control character; every other escape
/// is the literal character itself (so `\*`, `\\`, `\]` stay literal).
fn escape_char(c: char) -> char {
    match c {
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        '0' => '\0',
        'f' => '\u{0c}',
        'v' => '\u{0b}',
        other => other,
    }
}

/// Decodes a numeric/codepoint escape that has already consumed its leading
/// `\x`, `\u`, or `\U`. Accepts either a fixed run of hex digits (2 for `\x`,
/// 4 for `\u`, 8 for `\U`, matching the `regex` crate) or a braced `{...}` run of
/// any length. Returns `None` for a malformed or out-of-range codepoint so the
/// whole pattern is rejected with a `compile_error!`.
fn parse_hex_escape(kind: char, iter: &mut impl Iterator<Item = char>) -> Option<char> {
    let width = match kind {
        'x' => 2,
        'u' => 4,
        'U' => 8,
        _ => return None,
    };
    let mut hex = String::new();
    match iter.next()? {
        '{' => loop {
            match iter.next()? {
                '}' => break,
                h if h.is_ascii_hexdigit() => hex.push(h),
                _ => return None,
            }
        },
        h if h.is_ascii_hexdigit() => {
            hex.push(h);
            for _ in 1..width {
                match iter.next()? {
                    h if h.is_ascii_hexdigit() => hex.push(h),
                    _ => return None,
                }
            }
        }
        _ => return None,
    }
    char::from_u32(u32::from_str_radix(&hex, 16).ok()?)
}

/// Expands a Perl-style shorthand class escape into `(inverted, entries)`. ASCII
/// semantics are used (matching the `regex` crate's `(?-u)` byte mode); use
/// `regex_full` for Unicode-aware classes. Returns `None` for a non-shorthand
/// escape so the caller can treat it as a literal character.
fn shorthand_class(c: char) -> Option<(bool, Vec<GroupEntry>)> {
    let digit = || vec![GroupEntry::Range('0', '9')];
    let word = || {
        vec![
            GroupEntry::Range('0', '9'),
            GroupEntry::Range('A', 'Z'),
            GroupEntry::Range('a', 'z'),
            GroupEntry::Char('_'),
        ]
    };
    let space = || {
        vec![
            GroupEntry::Char(' '),
            GroupEntry::Char('\t'),
            GroupEntry::Char('\n'),
            GroupEntry::Char('\r'),
            GroupEntry::Char('\u{0c}'),
            GroupEntry::Char('\u{0b}'),
        ]
    };
    match c {
        'd' => Some((false, digit())),
        'D' => Some((true, digit())),
        'w' => Some((false, word())),
        'W' => Some((true, word())),
        's' => Some((false, space())),
        'S' => Some((true, space())),
        _ => None,
    }
}

/// Parses the body of a `{...}` repetition spec (the text between the braces)
/// into `(min, max)`, where `max == None` means unbounded (`{n,}`). Returns
/// `None` for anything that is not a well-formed, in-bounds spec so the caller
/// can fall back to treating the brace as a literal.
fn parse_repeat_spec(spec: &str) -> Option<(usize, Option<usize>)> {
    let bounded = |n: usize| (n <= MAX_REPEAT).then_some(n);
    if let Some((lo, hi)) = spec.split_once(',') {
        let min = bounded(lo.parse().ok()?)?;
        if hi.is_empty() {
            return Some((min, None));
        }
        let max = bounded(hi.parse().ok()?)?;
        (max >= min).then_some((min, Some(max)))
    } else {
        let n = bounded(spec.parse().ok()?)?;
        Some((n, Some(n)))
    }
}

/// Removes the single atom a trailing quantifier binds to and returns it. For a
/// literal run that is the final character (`ab*` binds `*` to `b`); for a group
/// the whole atom is taken. Returns `None` when the previous atom is already
/// quantified, is a zero-width anchor, or there is nothing to bind to, in which
/// case the quantifier should be treated as a literal character.
fn extract_bindable(atoms: &mut Vec<AtomRepeat>) -> Option<Atom> {
    let last = atoms.last_mut()?;
    if !matches!(last.repeat, Repeat::Once) {
        return None;
    }
    match &mut last.atom {
        Atom::Literal(lit) => {
            let c = lit.pop()?;
            // Splitting the last char off a one-char run leaves an empty literal; drop it.
            if lit.is_empty() {
                atoms.pop();
            }
            Some(Atom::Literal(c.to_string()))
        }
        Atom::Group(..) | Atom::Alternation(..) => Some(atoms.pop().unwrap().atom),
        Atom::EndOfInput | Atom::WordBoundary(_) | Atom::StartOfLine | Atom::EndOfLine => None,
    }
}

/// Applies a `{min,max}` count to the preceding atom by unrolling it: `min`
/// mandatory copies, then either a `*` tail (`max == None`) or `max - min`
/// optional copies. `lazy` marks the optional/star tail copies non-greedy
/// (`{n,m}?`); the mandatory copies are always `Once`. Returns `false` when there
/// is no atom to bind to so the brace can be emitted literally.
fn apply_counted(atoms: &mut Vec<AtomRepeat>, min: usize, max: Option<usize>, lazy: bool) -> bool {
    let Some(atom) = extract_bindable(atoms) else {
        return false;
    };
    for _ in 0..min {
        atoms.push(AtomRepeat {
            atom: atom.clone(),
            repeat: Repeat::Once,
            lazy: false,
        });
    }
    match max {
        None => atoms.push(AtomRepeat {
            atom: atom.clone(),
            repeat: Repeat::ZeroOrMore,
            lazy,
        }),
        Some(max) => {
            for _ in min..max {
                atoms.push(AtomRepeat {
                    atom: atom.clone(),
                    repeat: Repeat::ZeroOrOnce,
                    lazy,
                });
            }
        }
    }
    true
}

/// Peeks for a trailing `?` immediately after a quantifier and, if present,
/// consumes it and reports the quantifier as lazy/non-greedy (`*?`, `+?`, `??`,
/// `{n,m}?`). A `?` is only a lazy marker here; `*`/`+` after a quantifier still
/// fall through to literals (`a**`), matching the existing behaviour.
fn consume_lazy(iter: &mut std::str::Chars) -> bool {
    if iter.clone().next() == Some('?') {
        iter.next();
        true
    } else {
        false
    }
}

fn parse_group(iter: &mut impl Iterator<Item = char>, flags: Flags) -> Option<Atom> {
    let mut group_entries = vec![];
    let mut escaped = false;
    let mut in_range = false;
    let mut inverted = false;
    let mut first = true;
    loop {
        match iter.next() {
            None => return None,
            Some(']') if !escaped => break,
            Some('\\') if !escaped => {
                escaped = !escaped;
            }
            Some('-') if !escaped && matches!(group_entries.last(), Some(GroupEntry::Char(_))) => {
                group_entries.push(GroupEntry::Char('-'));
                in_range = true;
            }
            Some('^') if !escaped && first => {
                inverted = true;
            }
            Some(c) => {
                // Resolve the effective member char, expanding shorthand classes inline.
                let effective = if escaped {
                    escaped = false;
                    if let Some((inverted, entries)) = shorthand_class(c) {
                        // A negated shorthand can't be unioned into a positive class with
                        // the flat group model, and a shorthand can't be a range bound.
                        if inverted || in_range {
                            return None;
                        }
                        group_entries.extend(entries);
                        first = false;
                        continue;
                    }
                    match c {
                        'x' | 'u' | 'U' => parse_hex_escape(c, iter)?,
                        _ => escape_char(c),
                    }
                } else {
                    c
                };
                if in_range {
                    let start = pop_range_start(&mut group_entries)?;
                    in_range = false;
                    group_entries.push(GroupEntry::Range(start, effective))
                } else {
                    group_entries.push(GroupEntry::Char(effective));
                }
            }
        }
        first = false;
    }
    if escaped {
        if in_range {
            let start = pop_range_start(&mut group_entries)?;
            group_entries.push(GroupEntry::Range(start, '\\'))
        } else {
            group_entries.push(GroupEntry::Char('\\'));
        }
    }

    if flags.case_insensitive {
        let mut folded = Vec::with_capacity(group_entries.len());
        for entry in group_entries {
            fold_entry(entry, &mut folded);
        }
        group_entries = folded;
    }

    Some(Atom::Group(inverted, group_entries))
}

/// Appends `c` as a literal char, coalescing it onto a trailing unquantified
/// literal run so `abc` stays a single `Atom::Literal`.
fn push_lit(atoms: &mut Vec<AtomRepeat>, c: char) {
    if let Some(AtomRepeat {
        atom: Atom::Literal(literal),
        repeat: Repeat::Once,
        ..
    }) = atoms.last_mut()
    {
        literal.push(c);
    } else {
        atoms.push(AtomRepeat {
            atom: Atom::Literal(c.to_string()),
            repeat: Repeat::Once,
            lazy: false,
        });
    }
}

/// Appends a content character, honouring `(?i)`: a cased ASCII letter becomes a
/// two-member group matching both cases (which can't coalesce into a literal run),
/// any other char falls through to [`push_lit`].
fn push_char(atoms: &mut Vec<AtomRepeat>, c: char, flags: Flags) {
    if flags.case_insensitive && c.is_ascii_alphabetic() {
        atoms.push(AtomRepeat {
            atom: Atom::Group(false, vec![GroupEntry::Char(c.to_ascii_lowercase()), GroupEntry::Char(c.to_ascii_uppercase())]),
            repeat: Repeat::Once,
            lazy: false,
        });
    } else {
        push_lit(atoms, c);
    }
}

/// What a `(...)` prefix turned out to be once its leading `(?...)` modifier (if
/// any) was consumed.
enum GroupPrefix {
    /// A real sub-expression to recurse into, with these flags active in its body.
    Group(Flags),
    /// A bare `(?flags)` directive — no sub-expression; the returned flags become
    /// the current flags for the rest of the enclosing group.
    SetFlags(Flags),
}

/// Consumes the group-modifier prefix immediately after a `(`, if any, leaving
/// `iter` positioned at the start of the group body. `current` is the flag set in
/// effect at the `(`. Returns `None` for an unsupported `(?...)` construct
/// (lookaround, an unsupported flag) so the whole pattern is rejected rather than
/// silently mis-parsed.
///
/// Capturing `(...)`, non-capturing `(?:...)` and named `(?P<name>...)` /
/// `(?<name>...)` groups are all treated identically — this engine never extracts
/// capture spans, so the name and capturing-ness are simply skipped. Inline flags
/// come in two shapes: a bare `(?flags)` directive and a scoped `(?flags:...)`
/// group; both update the flags relative to `current`. See [`Flags`].
fn consume_group_prefix(iter: &mut std::str::Chars, current: Flags) -> Option<GroupPrefix> {
    if iter.clone().next() != Some('?') {
        // A plain capturing group inherits the surrounding flags.
        return Some(GroupPrefix::Group(current));
    }
    iter.next(); // the '?'
    match iter.clone().next()? {
        // Non-capturing group: nothing more to skip.
        ':' => {
            iter.next();
            Some(GroupPrefix::Group(current))
        }
        // Python-style named group `(?P<name>...)`.
        'P' => {
            iter.next();
            if iter.next()? != '<' {
                return None;
            }
            skip_until_gt(iter)?;
            Some(GroupPrefix::Group(current))
        }
        // `(?<name>...)`; reject lookbehind `(?<=...)` / `(?<!...)` (unsupported).
        '<' => {
            iter.next();
            match iter.clone().next() {
                Some('=') | Some('!') => None,
                _ => {
                    skip_until_gt(iter)?;
                    Some(GroupPrefix::Group(current))
                }
            }
        }
        // Lookahead `(?=...)` / `(?!...)` is unsupported.
        '=' | '!' => None,
        // An inline-flag spec: `i m s x U u`, optionally with a `-` to start
        // clearing, terminated by `)` (a bare directive) or `:` (a scoped group).
        _ => {
            let mut flags = current;
            let mut negate = false;
            loop {
                match iter.next()? {
                    ')' => return Some(GroupPrefix::SetFlags(flags)),
                    ':' => return Some(GroupPrefix::Group(flags)),
                    '-' => negate = true,
                    c => apply_flag(&mut flags, c, negate)?,
                }
            }
        }
    }
}

/// Consumes characters up to and including the closing `>` of a group name.
fn skip_until_gt(iter: &mut std::str::Chars) -> Option<()> {
    loop {
        if iter.next()? == '>' {
            return Some(());
        }
    }
}

/// Parses one alternation: a run of `|`-separated branches. When `in_group` it
/// stops at (and consumes) the matching `)`; otherwise it runs to end of input.
/// Returns `None` on a malformed pattern (unclosed `(`, stray `)`, bad escape).
fn parse_branches(iter: &mut std::str::Chars, in_group: bool, mut flags: Flags) -> Option<Vec<Vec<AtomRepeat>>> {
    let mut branches: Vec<Vec<AtomRepeat>> = vec![];
    let mut atoms: Vec<AtomRepeat> = vec![];
    let mut escaped = false;
    while let Some(next) = iter.next() {
        match next {
            '\\' if !escaped => {
                escaped = true;
            }
            '(' if !escaped => match consume_group_prefix(iter, flags)? {
                // A bare `(?flags)` directive: apply to the rest of this group.
                GroupPrefix::SetFlags(updated) => flags = updated,
                GroupPrefix::Group(inner) => atoms.push(AtomRepeat {
                    atom: Atom::Alternation(parse_branches(iter, true, inner)?),
                    repeat: Repeat::Once,
                    lazy: false,
                }),
            },
            ')' if !escaped => {
                if !in_group {
                    // A `)` with no open group is malformed (matching the `regex` crate).
                    return None;
                }
                branches.push(atoms);
                return Some(branches);
            }
            '|' if !escaped => {
                branches.push(std::mem::take(&mut atoms));
            }
            '[' if !escaped => {
                atoms.push(AtomRepeat {
                    atom: parse_group(iter, flags)?,
                    repeat: Repeat::Once,
                    lazy: false,
                });
            }
            '*' if !escaped && !atoms.is_empty() => match extract_bindable(&mut atoms) {
                Some(atom) => atoms.push(AtomRepeat {
                    atom,
                    repeat: Repeat::ZeroOrMore,
                    lazy: consume_lazy(iter) != flags.swap_greedy,
                }),
                None => push_lit(&mut atoms, '*'),
            },
            '+' if !escaped && !atoms.is_empty() => match extract_bindable(&mut atoms) {
                Some(atom) => atoms.push(AtomRepeat {
                    atom,
                    repeat: Repeat::OnceOrMore,
                    lazy: consume_lazy(iter) != flags.swap_greedy,
                }),
                None => push_lit(&mut atoms, '+'),
            },
            '?' if !escaped && !atoms.is_empty() => match extract_bindable(&mut atoms) {
                Some(atom) => atoms.push(AtomRepeat {
                    atom,
                    repeat: Repeat::ZeroOrOnce,
                    lazy: consume_lazy(iter) != flags.swap_greedy,
                }),
                None => push_lit(&mut atoms, '?'),
            },
            '{' if !escaped => {
                // Probe a clone for a well-formed `{n}`, `{n,}`, or `{n,m}` body so a
                // malformed brace can fall through to being a literal `{`.
                let mut spec = String::new();
                let mut closed = false;
                for ch in iter.clone() {
                    match ch {
                        '}' => {
                            closed = true;
                            break;
                        }
                        '0'..='9' | ',' => spec.push(ch),
                        _ => break,
                    }
                }
                match closed.then(|| parse_repeat_spec(&spec)).flatten() {
                    Some((min, max)) => {
                        // Commit: consume the spec and its closing brace, then unroll.
                        for _ in 0..spec.len() + 1 {
                            iter.next();
                        }
                        // A trailing `?` (`{n,m}?`) flips the tail's greediness. Peek
                        // first and only consume it once the count actually binds, so a
                        // literal-fallback brace leaves the `?` for normal parsing. Under
                        // `(?U)` the default is already lazy, so the `?` un-flips it.
                        let has_q = iter.clone().next() == Some('?');
                        let lazy = has_q != flags.swap_greedy;
                        if apply_counted(&mut atoms, min, max, lazy) {
                            if has_q {
                                iter.next();
                            }
                        } else {
                            push_lit(&mut atoms, '{');
                            spec.chars().for_each(|ch| push_lit(&mut atoms, ch));
                            push_lit(&mut atoms, '}');
                        }
                    }
                    // Not a valid count: leave the rest for normal parsing.
                    None => push_lit(&mut atoms, '{'),
                }
            }
            // Under `(?m)`, `^`/`$` are line anchors anywhere in the pattern; lower
            // them to zero-width assertions the matcher loop evaluates against the
            // surrounding chars. These arms must precede the non-multiline ones below.
            '^' if !escaped && flags.multiline => atoms.push(AtomRepeat {
                atom: Atom::StartOfLine,
                repeat: Repeat::Once,
                lazy: false,
            }),
            '$' if !escaped && flags.multiline => atoms.push(AtomRepeat {
                atom: Atom::EndOfLine,
                repeat: Repeat::Once,
                lazy: false,
            }),
            '^' if !escaped && atoms.is_empty() => {
                // Leading start-of-input anchor: a no-op for a prefix matcher.
            }
            '$' if !escaped && iter.clone().next().is_none() => atoms.push(AtomRepeat {
                atom: Atom::EndOfInput,
                repeat: Repeat::Once,
                lazy: false,
            }),
            '.' if !escaped => atoms.push(AtomRepeat {
                // `.` matches any char except a newline, matching the `regex` crate;
                // under `(?s)` (dot-all) an inverted *empty* class matches everything.
                atom: if flags.dot_matches_newline {
                    Atom::Group(true, vec![])
                } else {
                    Atom::Group(true, vec![GroupEntry::Char('\n')])
                },
                repeat: Repeat::Once,
                lazy: false,
            }),
            // Verbose mode (`(?x)`): an unescaped `#` starts a line comment, and
            // unescaped whitespace is layout. Neither applies inside `[...]`, which
            // `parse_group` handles with its own loop.
            '#' if flags.ignore_whitespace && !escaped => {
                for ch in iter.by_ref() {
                    if ch == '\n' {
                        break;
                    }
                }
            }
            c if flags.ignore_whitespace && !escaped && c.is_whitespace() => {}
            c => {
                if escaped {
                    escaped = false;
                    if let Some((inverted, entries)) = shorthand_class(c) {
                        atoms.push(AtomRepeat {
                            atom: Atom::Group(inverted, entries),
                            repeat: Repeat::Once,
                            lazy: false,
                        });
                    } else {
                        match c {
                            // `\A` (start-of-text) is a no-op at the start of a prefix
                            // match; anywhere else it can never hold, so reject the pattern.
                            'A' if atoms.is_empty() => {}
                            'A' => return None,
                            // `\z` (end-of-text) is exactly a trailing `$` for this engine.
                            'z' if iter.clone().next().is_none() => atoms.push(AtomRepeat {
                                atom: Atom::EndOfInput,
                                repeat: Repeat::Once,
                                lazy: false,
                            }),
                            'z' => return None,
                            'b' => atoms.push(AtomRepeat {
                                atom: Atom::WordBoundary(false),
                                repeat: Repeat::Once,
                                lazy: false,
                            }),
                            'B' => atoms.push(AtomRepeat {
                                atom: Atom::WordBoundary(true),
                                repeat: Repeat::Once,
                                lazy: false,
                            }),
                            'x' | 'u' | 'U' => push_char(&mut atoms, parse_hex_escape(c, iter)?, flags),
                            _ => push_char(&mut atoms, escape_char(c), flags),
                        }
                    }
                } else {
                    push_char(&mut atoms, c, flags);
                }
            }
        }
    }
    if in_group {
        // Reached end of input without a closing `)`.
        return None;
    }
    if escaped {
        push_lit(&mut atoms, '\\');
    }
    branches.push(atoms);
    Some(branches)
}

impl SimpleRegexAst {
    pub fn parse(from: &str) -> Option<SimpleRegexAst> {
        let mut iter = from.chars();
        let branches = parse_branches(&mut iter, false, Flags::default())?;
        // A single branch stays a flat atom sequence (no wrapper); multiple
        // top-level branches become one alternation atom.
        let atoms = if branches.len() == 1 {
            branches.into_iter().next().unwrap()
        } else {
            vec![AtomRepeat {
                atom: Atom::Alternation(branches),
                repeat: Repeat::Once,
                lazy: false,
            }]
        };
        Some(SimpleRegexAst {
            atoms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atoms(pattern: &str) -> Vec<AtomRepeat> {
        SimpleRegexAst::parse(pattern).expect("valid pattern").atoms
    }

    #[track_caller]
    fn assert_lit(atom: &AtomRepeat, expected: &str) {
        match &atom.atom {
            Atom::Literal(s) => assert_eq!(s, expected),
            other => panic!("expected literal {expected:?}, got {other:?}"),
        }
    }

    #[track_caller]
    fn assert_group(atom: &AtomRepeat, inverted: bool, entries: &[GroupEntry]) {
        match &atom.atom {
            Atom::Group(inv, ents) => {
                assert_eq!(*inv, inverted, "inversion mismatch");
                assert_eq!(ents.as_slice(), entries);
            }
            other => panic!("expected group, got {other:?}"),
        }
    }

    #[test]
    fn single_literal_run_is_coalesced() {
        let atoms = atoms("abc");
        assert_eq!(atoms.len(), 1);
        assert_lit(&atoms[0], "abc");
        assert!(matches!(atoms[0].repeat, Repeat::Once));
    }

    #[test]
    fn empty_pattern_has_no_atoms() {
        assert!(atoms("").is_empty());
    }

    #[test]
    fn escaped_metachar_is_literal() {
        // `\*` should be a literal star, coalesced into the surrounding run.
        let atoms = atoms("a\\*b");
        assert_eq!(atoms.len(), 1);
        assert_lit(&atoms[0], "a*b");
    }

    #[test]
    fn escaped_backslash_is_literal() {
        let atoms = atoms("a\\\\b");
        assert_eq!(atoms.len(), 1);
        assert_lit(&atoms[0], "a\\b");
    }

    #[test]
    fn trailing_backslash_is_literal_backslash() {
        let atoms = atoms("a\\");
        assert_eq!(atoms.len(), 1);
        assert_lit(&atoms[0], "a\\");
    }

    #[test]
    fn repeat_splits_last_char_off_run() {
        // `ab*` -> literal "a" (Once) followed by "b" (ZeroOrMore).
        let atoms = atoms("ab*");
        assert_eq!(atoms.len(), 2);
        assert_lit(&atoms[0], "a");
        assert!(matches!(atoms[0].repeat, Repeat::Once));
        assert_lit(&atoms[1], "b");
        assert!(matches!(atoms[1].repeat, Repeat::ZeroOrMore));
    }

    #[test]
    fn plus_and_question_repeats() {
        let plus = atoms("ab+");
        assert!(matches!(plus.last().unwrap().repeat, Repeat::OnceOrMore));
        let question = atoms("ab?");
        assert!(matches!(question.last().unwrap().repeat, Repeat::ZeroOrOnce));
    }

    #[test]
    fn repeat_with_no_preceding_atom_is_literal() {
        // A leading quantifier has nothing to bind to, so it is a literal char.
        let atoms = atoms("*");
        assert_eq!(atoms.len(), 1);
        assert_lit(&atoms[0], "*");
        assert!(matches!(atoms[0].repeat, Repeat::Once));
    }

    #[test]
    fn double_repeat_treats_second_as_literal() {
        // The second `*` cannot re-quantify an already-quantified atom, so it
        // falls through to a literal.
        let atoms = atoms("ab**");
        assert!(matches!(atoms[atoms.len() - 2].repeat, Repeat::ZeroOrMore));
        assert_lit(atoms.last().unwrap(), "*");
        assert!(matches!(atoms.last().unwrap().repeat, Repeat::Once));
    }

    #[test]
    fn lazy_quantifiers_set_the_lazy_flag() {
        // `*?`, `+?`, `??` mark the (single) repeat lazy and consume the `?`.
        for (pattern, repeat) in [("a*?", Repeat::ZeroOrMore), ("a+?", Repeat::OnceOrMore), ("a??", Repeat::ZeroOrOnce)] {
            let a = atoms(pattern);
            assert_eq!(a.len(), 1, "{pattern}");
            assert!(std::mem::discriminant(&a[0].repeat) == std::mem::discriminant(&repeat), "{pattern} repeat kind");
            assert!(a[0].lazy, "{pattern} should be lazy");
        }
        // Greedy quantifiers leave `lazy` false.
        assert!(!atoms("a*")[0].lazy);
        assert!(!atoms("a+")[0].lazy);
        assert!(!atoms("a?")[0].lazy);
    }

    #[test]
    fn lazy_counted_marks_only_the_optional_tail() {
        // `a{1,3}?` -> one mandatory (greedy `Once`), two lazy optional copies.
        let a = atoms("a{1,3}?");
        assert_eq!(a.len(), 3);
        assert!(matches!(a[0].repeat, Repeat::Once) && !a[0].lazy);
        assert!(matches!(a[1].repeat, Repeat::ZeroOrOnce) && a[1].lazy);
        assert!(matches!(a[2].repeat, Repeat::ZeroOrOnce) && a[2].lazy);
        // `a{2,}?` -> two mandatory then a lazy `*` tail.
        let unbounded = atoms("a{2,}?");
        assert!(matches!(unbounded[2].repeat, Repeat::ZeroOrMore) && unbounded[2].lazy);
    }

    #[test]
    fn triple_question_leaves_third_literal() {
        // `a??` is lazy `ZeroOrOnce`; a third `?` has nothing to re-quantify and is a
        // literal char.
        let a = atoms("a???");
        assert!(matches!(a[0].repeat, Repeat::ZeroOrOnce) && a[0].lazy);
        assert_lit(a.last().unwrap(), "?");
        assert!(matches!(a.last().unwrap().repeat, Repeat::Once));
    }

    #[test]
    fn group_chars_and_ranges() {
        assert_group(&atoms("[abc]")[0], false, &[GroupEntry::Char('a'), GroupEntry::Char('b'), GroupEntry::Char('c')]);
        assert_group(&atoms("[a-z]")[0], false, &[GroupEntry::Range('a', 'z')]);
        assert_group(
            &atoms("[a-zA-Z0-9_]")[0],
            false,
            &[
                GroupEntry::Range('a', 'z'),
                GroupEntry::Range('A', 'Z'),
                GroupEntry::Range('0', '9'),
                GroupEntry::Char('_'),
            ],
        );
    }

    #[test]
    fn group_negation_only_leading_caret() {
        assert_group(&atoms("[^a]")[0], true, &[GroupEntry::Char('a')]);
        // A caret that is not first is an ordinary member.
        assert_group(&atoms("[a^]")[0], false, &[GroupEntry::Char('a'), GroupEntry::Char('^')]);
    }

    #[test]
    fn group_escaped_close_bracket_is_member() {
        assert_group(&atoms("[\\]]")[0], false, &[GroupEntry::Char(']')]);
    }

    #[test]
    fn dot_is_negated_newline() {
        // `.` matches any char except a newline, like the `regex` crate.
        assert_group(&atoms(".")[0], true, &[GroupEntry::Char('\n')]);
    }

    #[test]
    fn control_char_escapes_decode() {
        assert_lit(&atoms("\\n")[0], "\n");
        assert_lit(&atoms("a\\tb")[0], "a\tb");
        assert_lit(&atoms("\\r")[0], "\r");
    }

    #[test]
    fn shorthand_classes_expand() {
        assert_group(&atoms("\\d")[0], false, &[GroupEntry::Range('0', '9')]);
        assert_group(&atoms("\\D")[0], true, &[GroupEntry::Range('0', '9')]);
        assert_group(
            &atoms("\\w")[0],
            false,
            &[
                GroupEntry::Range('0', '9'),
                GroupEntry::Range('A', 'Z'),
                GroupEntry::Range('a', 'z'),
                GroupEntry::Char('_'),
            ],
        );
    }

    #[test]
    fn shorthand_inside_class_is_merged() {
        assert_group(&atoms("[\\d_]")[0], false, &[GroupEntry::Range('0', '9'), GroupEntry::Char('_')]);
        // A negated shorthand can't be represented inside a positive class.
        assert!(SimpleRegexAst::parse("[\\D]").is_none());
    }

    #[test]
    fn counted_repetition_unrolls() {
        // `a{3}` -> three mandatory copies.
        let exact = atoms("a{3}");
        assert_eq!(exact.len(), 3);
        assert!(exact.iter().all(|a| matches!(a.repeat, Repeat::Once)));

        // `a{1,3}` -> one mandatory, two optional.
        let bounded = atoms("a{1,3}");
        assert_eq!(bounded.len(), 3);
        assert!(matches!(bounded[0].repeat, Repeat::Once));
        assert!(matches!(bounded[1].repeat, Repeat::ZeroOrOnce));
        assert!(matches!(bounded[2].repeat, Repeat::ZeroOrOnce));

        // `a{2,}` -> two mandatory then a `*` tail.
        let unbounded = atoms("a{2,}");
        assert_eq!(unbounded.len(), 3);
        assert!(matches!(unbounded[2].repeat, Repeat::ZeroOrMore));
    }

    #[test]
    fn counted_repetition_binds_last_char_only() {
        // `ab{2}` applies only to `b`: literal "a" then "bb".
        let a = atoms("ab{2}");
        assert_lit(&a[0], "a");
        assert!(matches!(a[1].repeat, Repeat::Once));
        assert!(matches!(a[2].repeat, Repeat::Once));
    }

    #[test]
    fn malformed_count_is_literal() {
        // No valid spec -> the brace and its contents are literal text.
        assert_lit(&atoms("a{x}")[0], "a{x}");
        assert_lit(&atoms("a{}")[0], "a{}");
        // A leading count has nothing to bind to, so the brace is literal.
        assert_lit(&atoms("{2}")[0], "{2}");
    }

    #[test]
    fn leading_caret_is_dropped() {
        // A leading `^` is a no-op anchor for this prefix matcher.
        let a = atoms("^ab");
        assert_eq!(a.len(), 1);
        assert_lit(&a[0], "ab");
        // A non-leading caret is an ordinary literal.
        assert_lit(&atoms("a^b")[0], "a^b");
    }

    #[test]
    fn multiline_flag_makes_caret_and_dollar_line_anchors() {
        // Under `(?m)`, `^`/`$` become zero-width line anchors anywhere in the pattern.
        let a = atoms("(?m)^a$");
        assert!(matches!(a.first().unwrap().atom, Atom::StartOfLine));
        assert!(matches!(a.last().unwrap().atom, Atom::EndOfLine));
        // A non-leading/non-trailing anchor is still a line anchor under `(?m)`.
        let mid = atoms("(?m)$a");
        assert!(matches!(mid.first().unwrap().atom, Atom::EndOfLine));
        let mid = atoms("(?m)a^b");
        assert!(matches!(mid[1].atom, Atom::StartOfLine));
        // Without `(?m)`, behaviour is unchanged: leading `^` dropped, trailing `$`
        // is end-of-input, and a mid-pattern anchor stays literal.
        assert!(matches!(atoms("^a$").last().unwrap().atom, Atom::EndOfInput));
        assert_lit(&atoms("a^b")[0], "a^b");
    }

    #[test]
    fn trailing_dollar_is_end_anchor() {
        let a = atoms("ab$");
        assert!(matches!(a.last().unwrap().atom, Atom::EndOfInput));
        // A non-trailing `$` is an ordinary literal.
        assert_lit(&atoms("a$b")[0], "a$b");
    }

    #[test]
    fn hex_escapes_decode() {
        // \x with two hex digits, and braced forms for \x \u \U.
        assert_lit(&atoms("\\x41")[0], "A");
        assert_lit(&atoms("\\x{41}")[0], "A");
        assert_lit(&atoms("\\u{2764}")[0], "\u{2764}");
        assert_lit(&atoms("\\u0041")[0], "A");
        assert_lit(&atoms("\\U00000041")[0], "A");
        // A hex escape can sit in the middle of a literal run.
        assert_lit(&atoms("a\\x42c")[0], "aBc");
    }

    #[test]
    fn malformed_hex_escape_is_rejected() {
        assert!(SimpleRegexAst::parse("\\x4").is_none()); // too few digits
        assert!(SimpleRegexAst::parse("\\xZZ").is_none()); // non-hex
        assert!(SimpleRegexAst::parse("\\x{}").is_none()); // empty braces
        assert!(SimpleRegexAst::parse("\\u{110000}").is_none()); // not a scalar value
    }

    #[test]
    fn hex_escape_inside_class() {
        // \x41 -> 'A', used as a plain member and as a range bound.
        assert_group(&atoms("[\\x41]")[0], false, &[GroupEntry::Char('A')]);
        assert_group(&atoms("[\\x41-\\x5A]")[0], false, &[GroupEntry::Range('A', 'Z')]);
    }

    #[test]
    fn start_text_anchor_matches_caret() {
        // \A at the start is a no-op anchor and is dropped.
        let a = atoms("\\Aab");
        assert_eq!(a.len(), 1);
        assert_lit(&a[0], "ab");
        // \A anywhere else can never hold, so the pattern is rejected.
        assert!(SimpleRegexAst::parse("a\\Ab").is_none());
    }

    #[test]
    fn end_text_anchor_matches_dollar() {
        // \z at the end lowers to the same end-of-input assertion as `$`.
        let a = atoms("ab\\z");
        assert!(matches!(a.last().unwrap().atom, Atom::EndOfInput));
        assert!(SimpleRegexAst::parse("a\\zb").is_none());
    }

    #[test]
    fn word_boundaries_parse() {
        let b = atoms("\\bword\\b");
        assert!(matches!(b.first().unwrap().atom, Atom::WordBoundary(false)));
        assert!(matches!(b.last().unwrap().atom, Atom::WordBoundary(false)));
        assert!(matches!(atoms("\\B")[0].atom, Atom::WordBoundary(true)));
    }

    #[test]
    fn group_with_repeat() {
        let atoms = atoms("[a-z]+");
        assert_eq!(atoms.len(), 1);
        assert_group(&atoms[0], false, &[GroupEntry::Range('a', 'z')]);
        assert!(matches!(atoms[0].repeat, Repeat::OnceOrMore));
    }

    #[test]
    fn unclosed_group_is_rejected() {
        assert!(SimpleRegexAst::parse("[abc").is_none());
        assert!(SimpleRegexAst::parse("[a-").is_none());
    }

    /// Helper: assert an atom is an alternation and return its branches.
    #[track_caller]
    fn branches(atom: &AtomRepeat) -> &[Vec<AtomRepeat>] {
        match &atom.atom {
            Atom::Alternation(branches) => branches,
            other => panic!("expected alternation, got {other:?}"),
        }
    }

    #[test]
    fn top_level_alternation_wraps_in_single_atom() {
        // `a|bc` -> one alternation atom with branches ["a"], ["bc"].
        let a = atoms("a|bc");
        assert_eq!(a.len(), 1);
        let b = branches(&a[0]);
        assert_eq!(b.len(), 2);
        assert_lit(&b[0][0], "a");
        assert_lit(&b[1][0], "bc");
    }

    #[test]
    fn single_branch_stays_flat() {
        // No `|` means no alternation wrapper, just the bare sequence.
        let a = atoms("abc");
        assert_eq!(a.len(), 1);
        assert_lit(&a[0], "abc");
    }

    #[test]
    fn group_is_alternation_atom() {
        // `(ab)` -> a single-branch alternation atom holding "ab".
        let a = atoms("(ab)");
        assert_eq!(a.len(), 1);
        let b = branches(&a[0]);
        assert_eq!(b.len(), 1);
        assert_lit(&b[0][0], "ab");
    }

    #[test]
    fn quantifier_binds_to_group() {
        // `(ab)*` -> the alternation atom carries the ZeroOrMore repeat.
        let a = atoms("(ab)*");
        assert_eq!(a.len(), 1);
        assert!(matches!(a[0].repeat, Repeat::ZeroOrMore));
        let b = branches(&a[0]);
        assert_lit(&b[0][0], "ab");
    }

    #[test]
    fn nested_group_inside_branch() {
        // `(a(b|c))` -> outer single-branch alternation whose branch is `a` then an
        // inner alternation of `b` / `c`.
        let a = atoms("(a(b|c))");
        let outer = branches(&a[0]);
        assert_eq!(outer.len(), 1);
        assert_lit(&outer[0][0], "a");
        let inner = branches(&outer[0][1]);
        assert_eq!(inner.len(), 2);
        assert_lit(&inner[0][0], "b");
        assert_lit(&inner[1][0], "c");
    }

    #[test]
    fn empty_branches_are_allowed() {
        // `(a||b)` -> three branches, the middle one empty.
        let a = atoms("(a||b)");
        let b = branches(&a[0]);
        assert_eq!(b.len(), 3);
        assert!(b[1].is_empty());
    }

    #[test]
    fn group_modifiers_are_skipped() {
        // Non-capturing and named groups parse to a plain alternation.
        for pattern in ["(?:a|b)", "(?P<n>a|b)", "(?<n>a|b)"] {
            let a = atoms(pattern);
            let b = branches(&a[0]);
            assert_eq!(b.len(), 2, "{pattern}");
            assert_lit(&b[0][0], "a");
            assert_lit(&b[1][0], "b");
        }
    }

    #[test]
    fn malformed_group_syntax_is_rejected() {
        assert!(SimpleRegexAst::parse("(abc").is_none()); // unclosed
        assert!(SimpleRegexAst::parse("abc)").is_none()); // stray close
        assert!(SimpleRegexAst::parse("(?=x)").is_none()); // lookahead
        assert!(SimpleRegexAst::parse("(?<=x)").is_none()); // lookbehind
        assert!(SimpleRegexAst::parse("(?R)x").is_none()); // CRLF: unmodellable
        assert!(SimpleRegexAst::parse("(?Q)x").is_none()); // unknown flag
        assert!(SimpleRegexAst::parse("(?ix").is_none()); // unterminated flag spec
    }

    #[test]
    fn case_insensitive_literal_folds_both_cases() {
        // `(?i)ab` -> each cased letter becomes a two-member group matching both cases.
        let a = atoms("(?i)ab");
        assert_eq!(a.len(), 2);
        assert_group(&a[0], false, &[GroupEntry::Char('a'), GroupEntry::Char('A')]);
        assert_group(&a[1], false, &[GroupEntry::Char('b'), GroupEntry::Char('B')]);
        // A non-letter is left as a plain literal even under `(?i)`.
        assert_lit(&atoms("(?i)5")[0], "5");
    }

    #[test]
    fn case_insensitive_class_folds_chars_and_ranges() {
        // `(?i)[a-c]` folds the range up to the matching uppercase range.
        assert_group(&atoms("(?i)[a-c]")[0], false, &[GroupEntry::Range('a', 'c'), GroupEntry::Range('A', 'C')]);
        // A single class member folds to both cases; non-letters are untouched.
        assert_group(&atoms("(?i)[k_]")[0], false, &[GroupEntry::Char('k'), GroupEntry::Char('K'), GroupEntry::Char('_')]);
        // Negation still applies after folding, so `(?i)[^a]` excludes both cases.
        assert_group(&atoms("(?i)[^a]")[0], true, &[GroupEntry::Char('a'), GroupEntry::Char('A')]);
    }

    #[test]
    fn scoped_flags_only_apply_inside_the_group() {
        // `a(?i:b)c` -> `a` and `c` stay case-sensitive, only `b` folds.
        let a = atoms("a(?i:b)c");
        assert_lit(&a[0], "a");
        let inner = branches(&a[1]);
        assert_group(&inner[0][0], false, &[GroupEntry::Char('b'), GroupEntry::Char('B')]);
        assert_lit(&a[2], "c");
    }

    #[test]
    fn flags_can_be_cleared_with_a_dash() {
        // `(?i)a(?-i)b` -> `a` folds, `b` does not.
        let a = atoms("(?i)a(?-i)b");
        assert_group(&a[0], false, &[GroupEntry::Char('a'), GroupEntry::Char('A')]);
        assert_lit(&a[1], "b");
    }

    #[test]
    fn dotall_flag_makes_dot_match_newline() {
        // `(?s).` -> an inverted *empty* class (matches anything, including `\n`).
        assert_group(&atoms("(?s).")[0], true, &[]);
        // Without `(?s)`, `.` excludes `\n`.
        assert_group(&atoms(".")[0], true, &[GroupEntry::Char('\n')]);
    }

    #[test]
    fn unicode_flag_is_accepted_as_a_noop() {
        // This engine is ASCII-only, so `u` toggles parse but change nothing.
        assert_lit(&atoms("(?-u)ab")[0], "ab");
        assert_lit(&atoms("(?u)ab")[0], "ab");
    }

    #[test]
    fn swap_greedy_flag_flips_quantifier_defaults() {
        // `(?U)a*` is lazy by default; the trailing `?` un-flips it back to greedy.
        assert!(atoms("(?U)a*")[0].lazy);
        assert!(!atoms("(?U)a*?")[0].lazy);
        // The flip applies to a counted tail too.
        assert!(atoms("(?U)a{1,2}").last().unwrap().lazy);
    }

    #[test]
    fn verbose_flag_ignores_whitespace_and_comments() {
        // `(?x)` drops unescaped whitespace and `#` line comments.
        assert_lit(&atoms("(?x) a b c ")[0], "abc");
        assert_lit(&atoms("(?x)ab # trailing comment\ncd")[0], "abcd");
        // Escaped whitespace is still a literal space.
        assert_lit(&atoms("(?x)a\\ b")[0], "a b");
    }

    #[test]
    fn escaped_group_metachars_are_literal() {
        // `\(`, `\|`, `\)` are literal characters, not structural.
        let a = atoms("\\(a\\|b\\)");
        assert_eq!(a.len(), 1);
        assert_lit(&a[0], "(a|b)");
    }
}
