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

    #[test]
    fn matches_exact_literal() {
        let re = SimpleRegex::parse("let").unwrap();
        assert!(re.matches("let"));
        // `matches` reports a prefix match, as used for keyword/identifier
        // conflict detection.
        assert!(re.matches("lets"));
        // An input that never completes the literal does not match.
        assert!(!re.matches("le"));
        assert!(!re.matches("abc"));
    }

    #[test]
    fn matches_escaped_metachar_literally() {
        let re = SimpleRegex::parse("a\\*b").unwrap();
        assert!(re.matches("a*b"));
        assert!(!re.matches("aaab"));
    }

    #[test]
    fn matches_optional_atom() {
        let re = SimpleRegex::parse("ab?c").unwrap();
        assert!(re.matches("abc"));
        assert!(re.matches("ac"));
        // The trailing `c` is required, so an input that never reaches it fails.
        assert!(!re.matches("ab"));
    }

    #[test]
    fn matches_dot_any_char() {
        let re = SimpleRegex::parse("a.c").unwrap();
        assert!(re.matches("abc"));
        assert!(re.matches("a_c"));
        assert!(re.matches("a\nc"));
        assert!(!re.matches("ac"));
    }

    #[test]
    fn matches_inverted_class() {
        let re = SimpleRegex::parse("[^0-9]+").unwrap();
        assert!(re.matches("abc"));
        assert!(!re.matches("123"));
    }

    #[test]
    fn matches_leading_star_run() {
        // A char immediately followed by `*` (no prefix) still matches greedily.
        let re = SimpleRegex::parse("a*b").unwrap();
        assert!(re.matches("aaab"));
        assert!(re.matches("b"));
        assert!(!re.matches("c"));
    }

    #[test]
    fn matches_unicode_chars() {
        let re = SimpleRegex::parse("[α-ω]+").unwrap();
        assert!(re.matches("λαμβδα"));
        assert!(!re.matches("abc"));
    }
}
