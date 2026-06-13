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
                    let entries_contain_newline = entries.iter().any(|entry| match entry {
                        GroupEntry::Char(c) => *c == '\n',
                        GroupEntry::Range(start, end) => *start <= '\n' && *end >= '\n',
                    });
                    // A negated class matches '\n' unless it explicitly excludes it; a normal
                    // class matches '\n' only when it lists it. Either way, a non-capturing
                    // atom must fall through so a later atom can still capture a newline.
                    if entries_contain_newline != *inverted {
                        return true;
                    }
                }
                // Zero-width assertions consume nothing.
                Atom::EndOfInput | Atom::WordBoundary(_) => {}
            }
        }
        false
    }

    pub fn matches(&self, from: &str) -> bool {
        let mut state = 0u32;
        for char in from.chars() {
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

    /// Run the DFA as a runtime interpreter, returning the matched prefix and the
    /// remaining input — the same `(matched, remaining)` contract the generated
    /// (`generate_parser`) matcher produces.
    ///
    /// This deliberately mirrors the generated code's per-state evaluation so the
    /// "runtime interpreter" and "compiled" engines stay in lock-step: consuming
    /// edges win over zero-width ones, `$`/`\z` only fires at end of input, an NFA
    /// `End` edge accepts unconditionally, and word boundaries are checked against
    /// the previous and lookahead chars without consuming. The match is anchored at
    /// the start of `from` (a prefix match); callers do unanchored searches by
    /// advancing the start position themselves.
    #[allow(dead_code)] // exercised by the conformance unit tests, not the macro itself
    pub fn find_prefix<'a>(&self, from: &'a str) -> Option<(&'a str, &'a str)> {
        let mut counter = 0usize;
        let mut state = 0u32;
        let mut prev: Option<char> = None;
        let mut chars = from.chars();
        let mut c = chars.next();
        loop {
            // The final state has no transition entry; any other missing state is a dead end.
            let transitions = self.dfa.transitions.get(&state)?;
            match eval_state(transitions, prev, c) {
                Step::Matched(next) => {
                    state = next;
                    if let Some(ch) = c {
                        counter += ch.len_utf8();
                    }
                    prev = c;
                    c = chars.next();
                    if next == self.dfa.final_state {
                        return Some((&from[..counter], &from[counter..]));
                    }
                }
                Step::MatchedEmpty(next) => {
                    // Zero-width transition: keep the lookahead char and byte position.
                    state = next;
                    if next == self.dfa.final_state {
                        return Some((&from[..counter], &from[counter..]));
                    }
                }
                Step::NoMatch => return None,
            }
        }
    }
}

#[allow(dead_code)]
enum Step {
    Matched(u32),
    MatchedEmpty(u32),
    NoMatch,
}

#[allow(dead_code)]
fn is_word(ch: Option<char>) -> bool {
    matches!(ch, Some('0'..='9' | 'a'..='z' | 'A'..='Z' | '_'))
}

/// Evaluate one DFA state for the runtime interpreter, matching the priority the
/// generated matcher uses: consuming edges (in declaration order) first, then a
/// fallback where an `End` edge accepts unconditionally and word boundaries are
/// tested against `prev`/`c`.
#[allow(dead_code)]
fn eval_state(transitions: &[(super::nfa::TransitionEvent, u32)], prev: Option<char>, c: Option<char>) -> Step {
    use super::nfa::TransitionEvent;
    for (transition, target) in transitions {
        match transition {
            TransitionEvent::Char(ch) => {
                if c == Some(*ch) {
                    return Step::Matched(*target);
                }
            }
            TransitionEvent::Chars(inverted, group) => {
                if let Some(ch) = c {
                    let in_group = group.iter().any(|entry| match entry {
                        GroupEntry::Char(g) => *g == ch,
                        GroupEntry::Range(start, end) => *start <= ch && *end >= ch,
                    });
                    if in_group != *inverted {
                        return Step::Matched(*target);
                    }
                }
            }
            TransitionEvent::EndOfInput => {
                if c.is_none() {
                    return Step::MatchedEmpty(*target);
                }
            }
            _ => {}
        }
    }
    // Fallback: an `End` edge (if present) accepts unconditionally and outranks
    // any word boundary, matching `generate_parser`'s fallback arm.
    for (transition, target) in transitions {
        if let TransitionEvent::End = transition {
            return Step::MatchedEmpty(*target);
        }
    }
    for (transition, target) in transitions {
        if let TransitionEvent::WordBoundary(negate) = transition {
            let boundary = is_word(prev) != is_word(c);
            if boundary != *negate {
                return Step::MatchedEmpty(*target);
            }
        }
    }
    Step::NoMatch
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
    fn could_capture_newline_dot_excludes_newline() {
        // `.` is "any char except newline", so it can never capture a '\n'.
        assert!(!newline_capture("."));
    }

    #[test]
    fn could_capture_newline_groups() {
        assert!(!newline_capture("[ \t]+"));
        assert!(newline_capture("[\n]"));
        // a negated class that excludes '\n' cannot match a newline
        assert!(!newline_capture("[^\n]*"));
        // a negated class that does not exclude '\n' can
        assert!(newline_capture("[^a]"));
        // Regression: a newline-bearing atom *after* a newline-excluding negated class
        // must still be detected (the scan used to stop at the first negated class).
        assert!(newline_capture("[^\n]*\n"));
        assert!(newline_capture("[^\n]*[\n]"));
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
    fn matches_dot_any_char_except_newline() {
        let re = SimpleRegex::parse("a.c").unwrap();
        assert!(re.matches("abc"));
        assert!(re.matches("a_c"));
        // `.` does not match a newline.
        assert!(!re.matches("a\nc"));
        assert!(!re.matches("ac"));
    }

    #[test]
    fn matches_shorthand_classes() {
        let digits = SimpleRegex::parse("\\d+").unwrap();
        assert!(digits.matches("123"));
        assert!(!digits.matches("abc"));

        let word = SimpleRegex::parse("\\w+").unwrap();
        assert!(word.matches("foo_9"));

        let non_digit = SimpleRegex::parse("\\D").unwrap();
        assert!(non_digit.matches("a"));
        assert!(!non_digit.matches("5"));
    }

    #[test]
    fn matches_control_char_escape() {
        let re = SimpleRegex::parse("a\\nb").unwrap();
        assert!(re.matches("a\nb"));
        assert!(!re.matches("anb"));
    }

    #[test]
    fn matches_counted_repetition() {
        let re = SimpleRegex::parse("a{2,3}b").unwrap();
        // Fewer than the two mandatory copies never reaches the trailing `b`.
        assert!(!re.matches("ab"));
        assert!(re.matches("aab"));
        assert!(re.matches("aaab"));
    }

    #[test]
    fn matches_exact_count() {
        let re = SimpleRegex::parse("a{3}").unwrap();
        assert!(re.matches("aaa"));
        // Only two copies never completes the third mandatory `a`.
        assert!(!re.matches("aa"));
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
