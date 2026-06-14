use super::*;

/// Whether any atom in a sequence could consume a `\n`. Recurses into alternation
/// branches so a newline inside a group (`(a\n|b)`) is still detected, picking the
/// newline-counting span path at compile time.
fn atoms_could_capture_newline(atoms: &[AtomRepeat]) -> bool {
    atoms.iter().any(|atom| match &atom.atom {
        Atom::Literal(lit) => lit.contains('\n'),
        Atom::Group(inverted, entries) => {
            let entries_contain_newline = entries.iter().any(|entry| match entry {
                GroupEntry::Char(c) => *c == '\n',
                GroupEntry::Range(start, end) => *start <= '\n' && *end >= '\n',
            });
            // A negated class matches '\n' unless it explicitly excludes it; a normal
            // class matches '\n' only when it lists it.
            entries_contain_newline != *inverted
        }
        // Zero-width assertions consume nothing.
        Atom::EndOfInput | Atom::WordBoundary(_) | Atom::StartOfLine | Atom::EndOfLine => false,
        Atom::Alternation(branches) => branches.iter().any(|branch| atoms_could_capture_newline(branch)),
    })
}

impl SimpleRegex {
    pub fn could_capture_newline(&self) -> bool {
        atoms_could_capture_newline(&self.ast.atoms)
    }

    /// Whether the regex matches some prefix of `from`. Used at macro-expansion
    /// time for keyword/identifier conflict detection, so it answers an unanchored
    /// "is there an accepting prefix" question rather than returning the span.
    ///
    /// Because the DFA is now a real (deterministic) subset construction, each
    /// character advances to at most one state, and a prefix matches whenever any
    /// state visited along the way is accepting — i.e. it is the sink `final_state`
    /// or it carries an `End` edge (an "accept here, but you may also keep going"
    /// state, which alternation and `*`/`?` produce). Zero-width assertions are not
    /// consulted here, matching the historical behaviour of this check.
    pub fn matches(&self, from: &str) -> bool {
        use super::nfa::TransitionEvent;
        let mut state = 0u32;
        if self.accepts(state) {
            return true;
        }
        for ch in from.chars() {
            let Some(transitions) = self.dfa.transitions.get(&state) else {
                return false;
            };
            let next = transitions
                .iter()
                .find(|(transition, _)| matches!(transition, TransitionEvent::Char(_) | TransitionEvent::Chars(..)) && transition.matches(ch));
            match next {
                Some((_, target)) => state = *target,
                None => return false,
            }
            if self.accepts(state) {
                return true;
            }
        }
        false
    }

    /// Whether `state` is accepting: the transition-less sink, or any state with an
    /// `End` edge to it.
    fn accepts(&self, state: u32) -> bool {
        use super::nfa::TransitionEvent;
        state == self.dfa.final_state
            || self
                .dfa
                .transitions
                .get(&state)
                .is_some_and(|transitions| transitions.iter().any(|(transition, _)| matches!(transition, TransitionEvent::End)))
    }

    /// Run the DFA as a runtime interpreter, returning the matched prefix and the
    /// remaining input — the same `(matched, remaining)` contract the generated
    /// (`generate_parser`) matcher produces.
    ///
    /// This is a leftmost-first match (like the `regex` crate): it follows the
    /// highest-priority surviving thread, remembers the byte position of the last
    /// accepting state seen, and on a dead end (or end of input) returns the match
    /// up to that remembered position. The priority is baked into the DFA (see
    /// `dfa.rs`): a greedy `.*` keeps consuming and backs off to the last accept
    /// (`/\*.*\*/` spans to the last `*/`), while a lazy `.*?` or an earlier
    /// alternation branch accepts first (`a|ab` on `"ab"` yields `"a"`), because the
    /// accept-truncated DFA leaves no lower-priority consuming edge to follow.
    ///
    /// It mirrors the generated matcher exactly so the two engines stay in
    /// lock-step: consuming edges win over zero-width ones; `$`/`\z` only fires at
    /// end of input; word boundaries are tested against `prev`/`c` without
    /// consuming; and an `End` edge marks a state accepting (it never moves). The
    /// match is anchored at the start of `from`; callers search by advancing the
    /// start position themselves.
    /// `prev` is the char immediately before `from` in the larger input (`None` for
    /// start of text); it seeds the zero-width assertions (`^` under `(?m)`, `\b`) so
    /// a slice taken mid-input still sees the correct preceding context.
    #[allow(dead_code)] // exercised by the conformance unit tests, not the macro itself
    pub fn find_prefix<'a>(&self, from: &'a str, prev: Option<char>) -> Option<(&'a str, &'a str)> {
        let mut counter = 0usize;
        let mut state = 0u32;
        let mut last: Option<usize> = None;
        let mut prev: Option<char> = prev;
        let mut chars = from.chars();
        let mut c = chars.next();
        // A bound on consecutive zero-width moves; more than one per state would
        // mean a zero-width cycle (e.g. `\b*`), so this can never truncate a real match.
        let zero_width_limit = self.dfa.transitions.len() + 1;
        let mut zero_width = 0usize;
        loop {
            if self.accepts(state) {
                last = Some(counter);
            }
            let Some(transitions) = self.dfa.transitions.get(&state) else { break };
            match eval_state(transitions, prev, c) {
                Step::Matched(next) => {
                    state = next;
                    if let Some(ch) = c {
                        counter += ch.len_utf8();
                    }
                    prev = c;
                    c = chars.next();
                    zero_width = 0;
                }
                Step::MatchedEmpty(next) => {
                    // Zero-width transition: keep the lookahead char and byte position.
                    state = next;
                    zero_width += 1;
                    if zero_width > zero_width_limit {
                        break;
                    }
                }
                Step::NoMatch => break,
            }
        }
        last.map(|n| (&from[..n], &from[n..]))
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
/// generated matcher uses: consuming edges (in declaration order) first, then the
/// zero-width moves in stored order — `$`/`\z` at end of input, `$`/`^` under `(?m)`
/// at `\n` line boundaries, and word boundaries tested against `prev`/`c`. `End`
/// edges are *not* moves; acceptance is handled by `accepts`.
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
            _ => {}
        }
    }
    // Zero-width assertions, only when no consuming edge claimed `c`, in stored order.
    for (transition, target) in transitions {
        let holds = match transition {
            TransitionEvent::EndOfInput => c.is_none(),
            TransitionEvent::EndOfLine => matches!(c, None | Some('\n')),
            TransitionEvent::StartOfLine => matches!(prev, None | Some('\n')),
            TransitionEvent::WordBoundary(negate) => (is_word(prev) != is_word(c)) != *negate,
            _ => false,
        };
        if holds {
            return Step::MatchedEmpty(*target);
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

    #[test]
    fn matches_top_level_alternation() {
        let re = SimpleRegex::parse("abc|def").unwrap();
        assert!(re.matches("abc"));
        assert!(re.matches("def"));
        assert!(!re.matches("abx"));
        assert!(!re.matches("xyz"));
    }

    #[test]
    fn matches_grouped_alternation() {
        // The group bounds the alternation: `foo` or `bar`, then a mandatory `!`.
        let re = SimpleRegex::parse("(foo|bar)!").unwrap();
        assert!(re.matches("foo!"));
        assert!(re.matches("bar!"));
        // Without the group, `|` would split into `(foo)` and `(bar!)`.
        assert!(!re.matches("foo"));
        assert!(!re.matches("bar"));
        assert!(!re.matches("baz!"));
    }

    #[test]
    fn matches_quantified_group() {
        // `(ab)+` — one or more repetitions of the whole group.
        let re = SimpleRegex::parse("(ab)+").unwrap();
        assert!(re.matches("ab"));
        assert!(re.matches("abab"));
        assert!(re.matches("ababab"));
        assert!(!re.matches("a"));
        assert!(!re.matches("ba"));

        // `(ab)*c` — zero or more, then a required `c`.
        let re = SimpleRegex::parse("(ab)*c").unwrap();
        assert!(re.matches("c"));
        assert!(re.matches("abc"));
        assert!(re.matches("ababc"));
        assert!(!re.matches("ab"));
    }

    #[test]
    fn matches_optional_group() {
        let re = SimpleRegex::parse("a(bc)?d").unwrap();
        assert!(re.matches("ad"));
        assert!(re.matches("abcd"));
        assert!(!re.matches("abd"));
    }

    #[test]
    fn matches_nested_groups() {
        // `(a(b|c)d)+` — alternation nested inside a quantified group.
        let re = SimpleRegex::parse("(a(b|c)d)+").unwrap();
        assert!(re.matches("abd"));
        assert!(re.matches("acd"));
        assert!(re.matches("abdacd"));
        assert!(!re.matches("ad"));
        // `matches` is a prefix check, so trailing junk is fine (`abd` matches), but
        // a non-`a` start has no matching prefix at all.
        assert!(re.matches("abd_"));
        assert!(!re.matches("xbd"));
    }

    #[test]
    fn matches_empty_alternation_branch() {
        // `a(b|)c` — the second branch is empty, so `bc` is optional just like `ab?c`.
        let re = SimpleRegex::parse("a(b|)c").unwrap();
        assert!(re.matches("abc"));
        assert!(re.matches("ac"));
        assert!(!re.matches("axc"));
    }

    #[test]
    fn non_capturing_and_named_groups_behave_like_groups() {
        for pattern in ["(?:foo|bar)!", "(?P<word>foo|bar)!", "(?<word>foo|bar)!"] {
            let re = SimpleRegex::parse(pattern).unwrap_or_else(|| panic!("parse {pattern}"));
            assert!(re.matches("foo!"), "{pattern} should match foo!");
            assert!(re.matches("bar!"), "{pattern} should match bar!");
            assert!(!re.matches("baz!"), "{pattern} should not match baz!");
        }
    }

    #[test]
    fn alternation_prefers_the_first_branch() {
        // This engine is leftmost-first, like the `regex` crate: the earlier branch
        // wins when both are viable, so `a|ab` takes `a` even though `ab` is longer.
        let re = SimpleRegex::parse("a|ab").unwrap();
        assert_eq!(re.find_prefix("ab", None), Some(("a", "b")));
        // Reordering the branches flips the result to the (now-first) longer branch.
        assert_eq!(SimpleRegex::parse("ab|a").unwrap().find_prefix("ab", None), Some(("ab", "")));
        // When only the short branch can complete, that one wins regardless of order.
        assert_eq!(re.find_prefix("ac", None), Some(("a", "c")));
    }

    #[test]
    fn lazy_quantifiers_prefer_the_shorter_match() {
        // `.*?` stops at the first place the rest of the pattern can match, unlike
        // the greedy `.*` which runs to the last. Matches the `regex` crate.
        let lazy = SimpleRegex::parse("a.*?b").unwrap();
        assert_eq!(lazy.find_prefix("axbxb", None), Some(("axb", "xb")));
        let greedy = SimpleRegex::parse("a.*b").unwrap();
        assert_eq!(greedy.find_prefix("axbxb", None), Some(("axbxb", "")));

        // `<.+?>` — the canonical "match one tag, not across tags" case.
        let tag = SimpleRegex::parse("<.+?>").unwrap();
        assert_eq!(tag.find_prefix("<a><b>", None), Some(("<a>", "<b>")));

        // Lazy `??` takes zero copies when the rest can still match.
        let opt = SimpleRegex::parse("a??a").unwrap();
        assert_eq!(opt.find_prefix("aa", None), Some(("a", "a")));

        // Lazy `+?` takes the minimum one copy.
        let plus = SimpleRegex::parse("a+?").unwrap();
        assert_eq!(plus.find_prefix("aaa", None), Some(("a", "aa")));

        // Lazy counted `{2,4}?` takes the two mandatory copies, no more.
        let counted = SimpleRegex::parse("a{2,4}?").unwrap();
        assert_eq!(counted.find_prefix("aaaa", None), Some(("aa", "aa")));
    }

    #[test]
    fn group_with_alternation_finds_longest_required_suffix() {
        // The trailing `z` forces the longer branch when the short one can't complete.
        let re = SimpleRegex::parse("(a|ab)z").unwrap();
        assert_eq!(re.find_prefix("abz", None), Some(("abz", "")));
        assert_eq!(re.find_prefix("az", None), Some(("az", "")));
    }

    #[test]
    fn unsupported_or_malformed_group_syntax_is_rejected() {
        // Unclosed group, stray close, lookaround and unmodellable flags are rejected.
        assert!(SimpleRegex::parse("(abc").is_none());
        assert!(SimpleRegex::parse("abc)").is_none());
        assert!(SimpleRegex::parse("(?=foo)").is_none());
        assert!(SimpleRegex::parse("(?<=foo)bar").is_none());
        assert!(SimpleRegex::parse("(?R)foo").is_none()); // CRLF: still unmodellable
    }

    #[test]
    fn multiline_anchors_match_line_boundaries() {
        // `^` under `(?m)` holds at start of text or just after a `\n` (seeded by the
        // `prev` argument); `$` holds at end of text or just before a `\n`.
        let re = SimpleRegex::parse("(?m)^[a-z]+$").unwrap();
        // Anchored at the start of the slice: prev = None counts as a line start.
        assert_eq!(re.find_prefix("abc\ndef", None), Some(("abc", "\ndef")));
        // Mid-input slice whose preceding char is the `\n`: `^` still holds.
        assert_eq!(re.find_prefix("def", Some('\n')), Some(("def", "")));
        // Same slice but preceded by a letter (mid-line): `^` must NOT hold.
        assert_eq!(re.find_prefix("def", Some('x')), None);
        // `$` fires before a `\n` even when more input follows.
        assert_eq!(SimpleRegex::parse("(?m)[a-z]+$").unwrap().find_prefix("abc\nxyz", None), Some(("abc", "\nxyz")));
    }

    #[test]
    fn case_insensitive_flag_matches_either_case() {
        let re = SimpleRegex::parse("(?i)foo").unwrap();
        assert_eq!(re.find_prefix("FoObar", None), Some(("FoO", "bar")));
        assert_eq!(re.find_prefix("FOO", None), Some(("FOO", "")));
        assert_eq!(re.find_prefix("bar", None), None);
        // Scoped form only folds the group body.
        let scoped = SimpleRegex::parse("a(?i:b)c").unwrap();
        assert_eq!(scoped.find_prefix("aBc", None), Some(("aBc", "")));
        assert_eq!(scoped.find_prefix("Abc", None), None);
    }

    #[test]
    fn unicode_case_folding_under_iu() {
        // Plain `(?i)` is ASCII-only: a non-ASCII letter is not folded.
        let ascii = SimpleRegex::parse("(?i)Σ").unwrap();
        assert_eq!(ascii.find_prefix("σ", None), None);
        // `(?iu)` folds with the full Unicode tables: Σ matches σ and final-sigma ς.
        let uni = SimpleRegex::parse("(?iu)Σ").unwrap();
        assert_eq!(uni.find_prefix("σ", None), Some(("σ", "")));
        assert_eq!(uni.find_prefix("ς", None), Some(("ς", "")));
        // The Kelvin sign (U+212A) simple-folds to `k`, which ASCII folding misses.
        let kelvin = SimpleRegex::parse("(?iu)k").unwrap();
        assert_eq!(kelvin.find_prefix("\u{212A}", None), Some(("\u{212A}", "")));
        assert_eq!(SimpleRegex::parse("(?i)k").unwrap().find_prefix("\u{212A}", None), None);
        // A class folds with Unicode equivalents too.
        let class = SimpleRegex::parse("(?iu)[α-ω]").unwrap();
        assert_eq!(class.find_prefix("Λ", None), Some(("Λ", "")));
    }

    #[test]
    fn unicode_shorthands_match_non_ascii() {
        // ASCII by default: a non-ASCII letter/digit is not a `\w`/`\d`.
        assert_eq!(SimpleRegex::parse(r"\w").unwrap().find_prefix("é", None), None);
        assert_eq!(SimpleRegex::parse(r"\d").unwrap().find_prefix("٧", None), None);
        // Under `(?u)`, the shorthands use the full Unicode sets.
        assert_eq!(SimpleRegex::parse(r"(?u)\w+").unwrap().find_prefix("café", None), Some(("café", "")));
        // Arabic-Indic digit seven is `\d` under Unicode.
        assert_eq!(SimpleRegex::parse(r"(?u)\d").unwrap().find_prefix("٧", None), Some(("٧", "")));
        // `(?u)\s` matches a Unicode space (no-break space U+00A0).
        assert_eq!(SimpleRegex::parse(r"(?u)\s").unwrap().find_prefix("\u{A0}", None), Some(("\u{A0}", "")));
        // Negated `(?u)\D` excludes a Unicode digit.
        assert_eq!(SimpleRegex::parse(r"(?u)\D").unwrap().find_prefix("٧", None), None);
        assert_eq!(SimpleRegex::parse(r"(?u)\D").unwrap().find_prefix("x", None), Some(("x", "")));
    }

    #[test]
    fn dotall_flag_matches_newline() {
        let re = SimpleRegex::parse("(?s)a.b").unwrap();
        assert_eq!(re.find_prefix("a\nb", None), Some(("a\nb", "")));
        // Without the flag, `.` will not cross a newline.
        assert_eq!(SimpleRegex::parse("a.b").unwrap().find_prefix("a\nb", None), None);
    }

    #[test]
    fn escaped_parens_and_pipe_stay_literal() {
        let re = SimpleRegex::parse("\\(a\\|b\\)").unwrap();
        assert!(re.matches("(a|b)"));
        assert!(!re.matches("a"));
    }
}
