use std::collections::BTreeMap;

use super::{Atom, AtomRepeat, GroupEntry, Repeat, SimpleRegexAst};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TransitionEvent {
    Epsilon,
    Char(char),
    // (inverted, set)
    Chars(bool, Vec<GroupEntry>),
    /// Zero-width `$` / `\z` assertion: only taken when the input is exhausted.
    EndOfInput,
    /// Zero-width word-boundary assertion (`\b` / `\B`); `negate` flips it and
    /// `unicode` selects Unicode `\w` word-ness over ASCII.
    WordBoundary {
        negate: bool,
        unicode: bool,
    },
    /// Zero-width multiline start-of-line assertion (`^` under `(?m)`): taken at the
    /// start of input or immediately after a line terminator. `crlf` (set by `(?R)`)
    /// also treats `\r` and the atomic `\r\n` as terminators.
    StartOfLine {
        crlf: bool,
    },
    /// Zero-width multiline end-of-line assertion (`$` under `(?m)`): taken at the
    /// end of input or immediately before a line terminator. `crlf` (set by `(?R)`)
    /// also treats `\r` and the atomic `\r\n` as terminators.
    EndOfLine {
        crlf: bool,
    },
    End,
}

impl TransitionEvent {
    pub fn matches(&self, target: char) -> bool {
        match self {
            TransitionEvent::Epsilon => false,
            TransitionEvent::Char(c) => *c == target,
            TransitionEvent::Chars(inverted, group) => {
                for entry in group {
                    match entry {
                        GroupEntry::Char(c) => {
                            if *c == target {
                                return !*inverted;
                            }
                        }
                        GroupEntry::Range(start, end) => {
                            if *start <= target && *end >= target {
                                return !*inverted;
                            }
                        }
                    }
                }
                *inverted
            }
            // A zero-width assertion never consumes a character.
            TransitionEvent::EndOfInput
            | TransitionEvent::WordBoundary {
                ..
            }
            | TransitionEvent::StartOfLine {
                ..
            }
            | TransitionEvent::EndOfLine {
                ..
            } => false,
            TransitionEvent::End => true,
        }
    }
}

#[derive(Debug)]
pub struct Nfa {
    // state => [(event, state)]
    pub transitions: BTreeMap<u32, Vec<(TransitionEvent, u32)>>,
    pub final_state: u32,
}

/// Thompson-style NFA construction. Builds the machine recursively as a set of
/// fragments wired together with epsilon transitions, which is what lets nested
/// groups and alternation (`(a|bc)*`) compose; the per-call epsilons are erased
/// again by the subset construction in `dfa.rs`.
///
/// Every fragment is built between an existing `start` state and a freshly
/// allocated `end` state that the builder returns, so a sequence chains by
/// feeding one fragment's end in as the next fragment's start.
///
/// Branching (`* + ?` repeats and alternation) routes through **pure-epsilon
/// split states**: a split carries only epsilon edges, and every consuming state
/// holds exactly one consuming edge. The *order* of a split's epsilon edges is its
/// thread priority — the body/loop edge first for a greedy repeat, the skip/exit
/// edge first for a lazy one (and earlier alternation branches before later ones).
/// The priority-preserving subset construction in `dfa.rs` reads that order back
/// out, which is what makes greedy-vs-lazy and leftmost-first alternation work.
struct Builder {
    transitions: BTreeMap<u32, Vec<(TransitionEvent, u32)>>,
    next: u32,
}

impl Builder {
    fn new_state(&mut self) -> u32 {
        let state = self.next;
        self.next += 1;
        // Ensure every allocated state has an entry, so `epsilon_closure`'s
        // `.expect("invalid state")` can never fire on a reachable state.
        self.transitions.entry(state).or_default();
        state
    }

    fn edge(&mut self, from: u32, event: TransitionEvent, to: u32) {
        self.transitions.entry(from).or_default().push((event, to));
    }

    /// Build a `|`-free sequence of atoms, returning the fragment's end state.
    fn build_sequence(&mut self, atoms: &[AtomRepeat], start: u32) -> u32 {
        let mut current = start;
        for atom in atoms {
            current = self.build_repeat(atom, current);
        }
        current
    }

    /// Wire a split state's two epsilon edges in priority order. A greedy repeat
    /// prefers the body/loop (`first`) over the exit (`second`); a lazy repeat
    /// flips them, so the leftmost-first matcher takes the shorter match.
    fn split(&mut self, from: u32, first: u32, second: u32, lazy: bool) {
        let (hi, lo) = if lazy { (second, first) } else { (first, second) };
        self.edge(from, TransitionEvent::Epsilon, hi);
        self.edge(from, TransitionEvent::Epsilon, lo);
    }

    fn build_repeat(&mut self, atom: &AtomRepeat, start: u32) -> u32 {
        let lazy = atom.lazy;
        match atom.repeat {
            Repeat::Once => self.build_atom(&atom.atom, start),
            Repeat::ZeroOrOnce => {
                // `start` is a pure-epsilon split: body-or-skip. The body is built
                // from a dedicated state so the split carries only epsilon edges.
                let end = self.new_state();
                let body_start = self.new_state();
                self.split(start, body_start, end, lazy);
                let body_end = self.build_atom(&atom.atom, body_start);
                self.edge(body_end, TransitionEvent::Epsilon, end);
                end
            }
            Repeat::ZeroOrMore => {
                // `start` is the loop split: body-or-exit. The body loops back to the
                // split, so re-entry re-reads the same priority order.
                let end = self.new_state();
                let body_start = self.new_state();
                self.split(start, body_start, end, lazy);
                let body_end = self.build_atom(&atom.atom, body_start);
                self.edge(body_end, TransitionEvent::Epsilon, start);
                end
            }
            Repeat::OnceOrMore => {
                // Run the body once, then `body_end` becomes the loop split:
                // loop-back (re-run the body) or exit.
                let body_end = self.build_atom(&atom.atom, start);
                let end = self.new_state();
                self.split(body_end, start, end, lazy);
                end
            }
        }
    }

    fn build_atom(&mut self, atom: &Atom, start: u32) -> u32 {
        match atom {
            Atom::Literal(literal) => {
                let mut current = start;
                for c in literal.chars() {
                    let next = self.new_state();
                    self.edge(current, TransitionEvent::Char(c), next);
                    current = next;
                }
                current
            }
            Atom::Group(inverted, entries) => {
                let end = self.new_state();
                self.edge(start, TransitionEvent::Chars(*inverted, entries.clone()), end);
                end
            }
            Atom::EndOfInput => {
                let end = self.new_state();
                self.edge(start, TransitionEvent::EndOfInput, end);
                end
            }
            Atom::WordBoundary {
                negate,
                unicode,
            } => {
                let end = self.new_state();
                self.edge(
                    start,
                    TransitionEvent::WordBoundary {
                        negate: *negate,
                        unicode: *unicode,
                    },
                    end,
                );
                end
            }
            Atom::StartOfLine {
                crlf,
            } => {
                let end = self.new_state();
                self.edge(
                    start,
                    TransitionEvent::StartOfLine {
                        crlf: *crlf,
                    },
                    end,
                );
                end
            }
            Atom::EndOfLine {
                crlf,
            } => {
                let end = self.new_state();
                self.edge(
                    start,
                    TransitionEvent::EndOfLine {
                        crlf: *crlf,
                    },
                    end,
                );
                end
            }
            Atom::Alternation(branches) => {
                // `start` is a pure-epsilon split: one epsilon edge per branch, in
                // declaration order (so earlier branches have higher priority for the
                // leftmost-first matcher). Each branch is built from its own dedicated
                // start state and merges into one `end`.
                let end = self.new_state();
                for branch in branches {
                    let branch_start = self.new_state();
                    self.edge(start, TransitionEvent::Epsilon, branch_start);
                    let branch_end = self.build_sequence(branch, branch_start);
                    self.edge(branch_end, TransitionEvent::Epsilon, end);
                }
                end
            }
        }
    }
}

impl Nfa {
    pub fn build(from: &SimpleRegexAst) -> Self {
        let mut builder = Builder {
            transitions: Default::default(),
            next: 0,
        };
        // State 0 is the start; the whole pattern is one sequence fragment, whose
        // end state becomes the single accepting state.
        let start = builder.new_state();
        let final_state = builder.build_sequence(&from.atoms, start);
        Self {
            transitions: builder.transitions,
            final_state,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(inverted: bool, entries: &[GroupEntry]) -> TransitionEvent {
        TransitionEvent::Chars(inverted, entries.to_vec())
    }

    #[test]
    fn char_event_matches_exact() {
        assert!(TransitionEvent::Char('a').matches('a'));
        assert!(!TransitionEvent::Char('a').matches('b'));
    }

    #[test]
    fn epsilon_never_matches_and_end_always_matches() {
        assert!(!TransitionEvent::Epsilon.matches('a'));
        assert!(TransitionEvent::End.matches('a'));
        assert!(TransitionEvent::End.matches('\n'));
    }

    #[test]
    fn group_event_membership_and_ranges() {
        let event = chars(false, &[GroupEntry::Range('a', 'z'), GroupEntry::Char('_')]);
        assert!(event.matches('a'));
        assert!(event.matches('m'));
        assert!(event.matches('z'));
        assert!(event.matches('_'));
        assert!(!event.matches('A'));
        assert!(!event.matches('0'));
    }

    #[test]
    fn inverted_group_event() {
        let event = chars(true, &[GroupEntry::Char('a')]);
        assert!(!event.matches('a'));
        assert!(event.matches('b'));
        // An inverted empty group (`.`) matches anything.
        assert!(chars(true, &[]).matches('x'));
        assert!(chars(true, &[]).matches('\n'));
    }

    #[test]
    fn literal_chain_is_a_linear_state_machine() {
        // "abc" -> states 0->1->2->3 each on one Char, final state 3.
        let nfa = Nfa::build(&SimpleRegexAst::parse("abc").unwrap());
        assert_eq!(nfa.final_state, 3);
        for (i, expected) in ['a', 'b', 'c'].into_iter().enumerate() {
            let trans = nfa.transitions.get(&(i as u32)).expect("state present");
            assert_eq!(trans.as_slice(), &[(TransitionEvent::Char(expected), i as u32 + 1)]);
        }
    }

    /// The epsilon targets leaving `state`, in stored (priority) order.
    fn epsilon_targets(nfa: &Nfa, state: u32) -> Vec<u32> {
        nfa.transitions
            .get(&state)
            .expect("state present")
            .iter()
            .filter_map(|(e, t)| matches!(e, TransitionEvent::Epsilon).then_some(*t))
            .collect()
    }

    #[test]
    fn zero_or_more_is_a_pure_epsilon_split() {
        // "a*" makes the start a pure-epsilon split: greedy order is body first, exit
        // (the final state) second. The body consumes from its own dedicated state and
        // loops back to the split. The priority order is what `dfa.rs` reads back out.
        let nfa = Nfa::build(&SimpleRegexAst::parse("a*").unwrap());
        let [body_start, exit] = epsilon_targets(&nfa, 0)[..] else {
            panic!("start must be a split with two epsilon edges: {:?}", nfa.transitions.get(&0));
        };
        // Greedy: the body edge comes before the exit-to-final edge.
        assert_eq!(exit, nfa.final_state, "second (lower-priority) edge exits to final");
        let body = nfa.transitions.get(&body_start).expect("body start present");
        let (_, body_end) = body
            .iter()
            .find(|(e, _)| matches!(e, TransitionEvent::Char('a')))
            .expect("body start holds the single consuming edge");
        assert!(
            nfa.transitions
                .get(body_end)
                .expect("body end present")
                .contains(&(TransitionEvent::Epsilon, 0)),
            "body loops back to the split via epsilon",
        );
    }

    #[test]
    fn lazy_repeat_flips_split_priority() {
        // "a*?" is the same machine with the split's two epsilon edges swapped: the
        // exit (final state) now has higher priority than the body.
        let nfa = Nfa::build(&SimpleRegexAst::parse("a*?").unwrap());
        let targets = epsilon_targets(&nfa, 0);
        assert_eq!(targets.first(), Some(&nfa.final_state), "lazy: exit-to-final is the higher-priority edge: {targets:?}");
    }
}
