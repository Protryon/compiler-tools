use super::*;

//todo: unit tests
fn parse_group(iter: &mut impl Iterator<Item=char>) -> Option<Atom> {
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
            },
            Some('-') if !escaped && matches!(group_entries.last(), Some(GroupEntry::Char(_))) => {
                group_entries.push(GroupEntry::Char('-'));
                in_range = true;
            },
            Some('^') if !escaped && first => {
                inverted = true;
            },
            Some(c) => {
                if in_range {
                    assert_eq!(group_entries.pop(), Some(GroupEntry::Char('-')));
                    let start = group_entries.pop().expect("malformed state during group formation");
                    let start = if let GroupEntry::Char(c) = start {
                        c
                    } else {
                        panic!("malformed state during group formation");
                    };
                    in_range = false;
                    group_entries.push(GroupEntry::Range(start, c))
                } else {
                    group_entries.push(GroupEntry::Char(c));
                }
                escaped = false;
            },
        }
        first = false;
    }
    if escaped {
        if in_range {
            assert_eq!(group_entries.pop(), Some(GroupEntry::Char('-')));
            let start = group_entries.pop().expect("malformed state during group formation");
            let start = if let GroupEntry::Char(c) = start {
                c
            } else {
                panic!("malformed state during group formation");
            };
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
            if let Some(AtomRepeat { atom: Atom::Literal(literal), repeat: Repeat::Once }) = atoms.last_mut() {
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
                },    
                '[' if !escaped => {
                    atoms.push(AtomRepeat {
                        atom: parse_group(&mut iter)?,
                        repeat: Repeat::Once,
                    });
                },
                '*' if !escaped && !atoms.is_empty() => {
                    let last_atom = atoms.last_mut().unwrap();
                    if !matches!(last_atom.repeat, Repeat::Once) {
                        push_lit(&mut atoms, '*');
                        continue;
                    }
                    let atom = match &mut last_atom.atom {
                        Atom::Literal(lit) => {
                            Atom::Literal(lit.pop().unwrap().to_string())
                        },
                        Atom::Group(..) => atoms.pop().unwrap().atom,
                    };
                    atoms.push(AtomRepeat {
                        atom,
                        repeat: Repeat::ZeroOrMore,
                    })
                },
                '+' if !escaped && !atoms.is_empty() => {
                    let last_atom = atoms.last_mut().unwrap();
                    if !matches!(last_atom.repeat, Repeat::Once) {
                        push_lit(&mut atoms, '+');
                        continue;
                    }
                    let atom = match &mut last_atom.atom {
                        Atom::Literal(lit) => {
                            Atom::Literal(lit.pop().unwrap().to_string())
                        },
                        Atom::Group(..) => atoms.pop().unwrap().atom,
                    };
                    atoms.push(AtomRepeat {
                        atom,
                        repeat: Repeat::OnceOrMore,
                    })
                },
                '?' if !escaped && !atoms.is_empty() => {
                    let last_atom = atoms.last_mut().unwrap();
                    if !matches!(last_atom.repeat, Repeat::Once) {
                        push_lit(&mut atoms, '?');
                        continue;
                    }
                    let atom = match &mut last_atom.atom {
                        Atom::Literal(lit) => {
                            Atom::Literal(lit.pop().unwrap().to_string())
                        },
                        Atom::Group(..) => atoms.pop().unwrap().atom,
                    };
                    atoms.push(AtomRepeat {
                        atom,
                        repeat: Repeat::ZeroOrOnce,
                    })
                },
                '.' if !escaped => {
                    atoms.push(AtomRepeat {
                        atom: Atom::Group(true, vec![]),
                        repeat: Repeat::Once,
                    })
                },
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