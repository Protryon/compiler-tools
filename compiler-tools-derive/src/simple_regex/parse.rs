use super::*;

/// An upper bound on how far a `{n,m}` counted repetition is unrolled. Patterns
/// that ask for more are treated as a literal `{...}` so a stray brace cannot
/// blow up macro-expansion time / generated code size.
const MAX_REPEAT: usize = 1024;

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
        Atom::Group(..) => Some(atoms.pop().unwrap().atom),
        Atom::EndOfInput => None,
    }
}

/// Applies a `{min,max}` count to the preceding atom by unrolling it: `min`
/// mandatory copies, then either a `*` tail (`max == None`) or `max - min`
/// optional copies. Returns `false` when there is no atom to bind to so the
/// brace can be emitted literally.
fn apply_counted(atoms: &mut Vec<AtomRepeat>, min: usize, max: Option<usize>) -> bool {
    let Some(atom) = extract_bindable(atoms) else {
        return false;
    };
    for _ in 0..min {
        atoms.push(AtomRepeat {
            atom: atom.clone(),
            repeat: Repeat::Once,
        });
    }
    match max {
        None => atoms.push(AtomRepeat {
            atom: atom.clone(),
            repeat: Repeat::ZeroOrMore,
        }),
        Some(max) => {
            for _ in min..max {
                atoms.push(AtomRepeat {
                    atom: atom.clone(),
                    repeat: Repeat::ZeroOrOnce,
                });
            }
        }
    }
    true
}

fn parse_group(iter: &mut impl Iterator<Item = char>) -> Option<Atom> {
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
                    escape_char(c)
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

    Some(Atom::Group(inverted, group_entries))
}

impl SimpleRegexAst {
    pub fn parse(from: &str) -> Option<SimpleRegexAst> {
        let mut iter = from.chars();
        let mut atoms = vec![];
        let mut escaped = false;
        let push_lit = |atoms: &mut Vec<AtomRepeat>, c: char| {
            if let Some(AtomRepeat {
                atom: Atom::Literal(literal),
                repeat: Repeat::Once,
            }) = atoms.last_mut()
            {
                literal.push(c);
            } else {
                atoms.push(AtomRepeat {
                    atom: Atom::Literal(c.to_string()),
                    repeat: Repeat::Once,
                });
            }
        };
        while let Some(next) = iter.next() {
            match next {
                '\\' if !escaped => {
                    escaped = !escaped;
                }
                '[' if !escaped => {
                    atoms.push(AtomRepeat {
                        atom: parse_group(&mut iter)?,
                        repeat: Repeat::Once,
                    });
                }
                '*' if !escaped && !atoms.is_empty() => match extract_bindable(&mut atoms) {
                    Some(atom) => atoms.push(AtomRepeat {
                        atom,
                        repeat: Repeat::ZeroOrMore,
                    }),
                    None => push_lit(&mut atoms, '*'),
                },
                '+' if !escaped && !atoms.is_empty() => match extract_bindable(&mut atoms) {
                    Some(atom) => atoms.push(AtomRepeat {
                        atom,
                        repeat: Repeat::OnceOrMore,
                    }),
                    None => push_lit(&mut atoms, '+'),
                },
                '?' if !escaped && !atoms.is_empty() => match extract_bindable(&mut atoms) {
                    Some(atom) => atoms.push(AtomRepeat {
                        atom,
                        repeat: Repeat::ZeroOrOnce,
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
                            if !apply_counted(&mut atoms, min, max) {
                                push_lit(&mut atoms, '{');
                                spec.chars().for_each(|ch| push_lit(&mut atoms, ch));
                                push_lit(&mut atoms, '}');
                            }
                        }
                        // Not a valid count: leave the rest for normal parsing.
                        None => push_lit(&mut atoms, '{'),
                    }
                }
                '^' if !escaped && atoms.is_empty() => {
                    // Leading start-of-input anchor: a no-op for a prefix matcher.
                }
                '$' if !escaped && iter.clone().next().is_none() => atoms.push(AtomRepeat {
                    atom: Atom::EndOfInput,
                    repeat: Repeat::Once,
                }),
                '.' if !escaped => atoms.push(AtomRepeat {
                    // `.` matches any char except a newline, matching the `regex` crate.
                    atom: Atom::Group(true, vec![GroupEntry::Char('\n')]),
                    repeat: Repeat::Once,
                }),
                c => {
                    if escaped {
                        escaped = false;
                        if let Some((inverted, entries)) = shorthand_class(c) {
                            atoms.push(AtomRepeat {
                                atom: Atom::Group(inverted, entries),
                                repeat: Repeat::Once,
                            });
                        } else {
                            push_lit(&mut atoms, escape_char(c));
                        }
                    } else {
                        push_lit(&mut atoms, c);
                    }
                }
            }
        }
        if escaped {
            push_lit(&mut atoms, '\\');
        }
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
    fn trailing_dollar_is_end_anchor() {
        let a = atoms("ab$");
        assert!(matches!(a.last().unwrap().atom, Atom::EndOfInput));
        // A non-trailing `$` is an ordinary literal.
        assert_lit(&atoms("a$b")[0], "a$b");
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
}
