use super::*;

impl SimpleRegex {
    pub fn could_capture_newline(&self) -> bool {
        for atom in &self.ast.atoms {
            match &atom.atom {
                Atom::Literal(lit) => {
                    if lit.contains('\n') {
                        return true;
                    }
                }
                Atom::Group(inverted, entries) => {
                    if *inverted {
                        for entry in entries {
                            match entry {
                                GroupEntry::Char(c) => {
                                    if *c == '\n' {
                                        return false;
                                    }
                                }
                                GroupEntry::Range(start, end) => {
                                    if *start <= '\n' && *end >= '\n' {
                                        return false;
                                    }
                                }
                            }
                        }
                        return true;
                    } else {
                        for entry in entries {
                            let matched = match entry {
                                GroupEntry::Char(c) => *c == '\n',
                                GroupEntry::Range(start, end) => *start <= '\n' && *end >= '\n',
                            };
                            if matched {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        return false;
    }

    pub fn matches(&self, from: &str) -> bool {
        let mut state = 0u32;
        let mut chars = from.chars();
        while let Some(char) = chars.next() {
            for (transition, target) in self.dfa.transitions.get(&state).unwrap() {
                if transition.matches(char) {
                    state = *target;
                    if state == self.dfa.final_state {
                        return true;
                    }
                }
            }
        }
        false
    }
}
