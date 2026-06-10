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

#[cfg(test)]
mod tests {
    use super::*;

    fn newline_capture(pattern: &str) -> bool {
        SimpleRegex::parse(pattern).expect("valid pattern").could_capture_newline()
    }

    #[test]
    fn could_capture_newline_literal() {
        assert!(!newline_capture("abc"));
        assert!(newline_capture("a\nb"));
    }

    #[test]
    fn could_capture_newline_dot_matches_everything() {
        // `.` becomes an inverted empty group, which matches any char including '\n'.
        assert!(newline_capture("."));
    }

    #[test]
    fn could_capture_newline_groups() {
        assert!(!newline_capture("[ \t]+"));
        assert!(newline_capture("[\n]"));
        // a negated class that excludes '\n' cannot match a newline
        assert!(!newline_capture("[^\n]*"));
        // a negated class that does not exclude '\n' can
        assert!(newline_capture("[^a]"));
    }

    #[test]
    fn parse_rejects_unclosed_group() {
        assert!(SimpleRegex::parse("[abc").is_none());
    }

    #[test]
    fn matches_digit_run() {
        let re = SimpleRegex::parse("[0-9]+").unwrap();
        assert!(re.matches("123"));
        assert!(!re.matches("abc"));
    }

    #[test]
    fn matches_ident_prefix() {
        // Used at compile time to detect keyword/identifier conflicts.
        let re = SimpleRegex::parse("[a-z][a-zA-Z0-9_]*").unwrap();
        assert!(re.matches("let"));
        assert!(re.matches("foo_bar"));
        assert!(!re.matches("123"));
    }
}
