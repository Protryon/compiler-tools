use super::*;

/// Pops the pending `Char('-')` and the range's start char off `entries`, returning
/// the start char. Returns `None` on a malformed state rather than panicking, so a bad
/// pattern surfaces as a `compile_error!` instead of an ICE-style proc-macro panic.
fn pop_range_start(entries: &mut Vec<GroupEntry>) -> Option<char> {
    match (entries.pop(), entries.pop()) {
        (Some(GroupEntry::Char('-')), Some(GroupEntry::Char(start))) => Some(start),
        _ => None,
    }
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
                if in_range {
                    let start = pop_range_start(&mut group_entries)?;
                    in_range = false;
                    group_entries.push(GroupEntry::Range(start, c))
                } else {
                    group_entries.push(GroupEntry::Char(c));
                }
                escaped = false;
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
                '*' if !escaped && !atoms.is_empty() => {
                    let last_atom = atoms.last_mut().unwrap();
                    if !matches!(last_atom.repeat, Repeat::Once) {
                        push_lit(&mut atoms, '*');
                        continue;
                    }
                    let atom = match &mut last_atom.atom {
                        Atom::Literal(lit) => Atom::Literal(lit.pop().unwrap().to_string()),
                        Atom::Group(..) => atoms.pop().unwrap().atom,
                    };
                    atoms.push(AtomRepeat {
                        atom,
                        repeat: Repeat::ZeroOrMore,
                    })
                }
                '+' if !escaped && !atoms.is_empty() => {
                    let last_atom = atoms.last_mut().unwrap();
                    if !matches!(last_atom.repeat, Repeat::Once) {
                        push_lit(&mut atoms, '+');
                        continue;
                    }
                    let atom = match &mut last_atom.atom {
                        Atom::Literal(lit) => Atom::Literal(lit.pop().unwrap().to_string()),
                        Atom::Group(..) => atoms.pop().unwrap().atom,
                    };
                    atoms.push(AtomRepeat {
                        atom,
                        repeat: Repeat::OnceOrMore,
                    })
                }
                '?' if !escaped && !atoms.is_empty() => {
                    let last_atom = atoms.last_mut().unwrap();
                    if !matches!(last_atom.repeat, Repeat::Once) {
                        push_lit(&mut atoms, '?');
                        continue;
                    }
                    let atom = match &mut last_atom.atom {
                        Atom::Literal(lit) => Atom::Literal(lit.pop().unwrap().to_string()),
                        Atom::Group(..) => atoms.pop().unwrap().atom,
                    };
                    atoms.push(AtomRepeat {
                        atom,
                        repeat: Repeat::ZeroOrOnce,
                    })
                }
                '.' if !escaped => atoms.push(AtomRepeat {
                    atom: Atom::Group(true, vec![]),
                    repeat: Repeat::Once,
                }),
                c => {
                    push_lit(&mut atoms, c);
                    escaped = false;
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
    fn dot_is_inverted_empty_group() {
        assert_group(&atoms(".")[0], true, &[]);
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
