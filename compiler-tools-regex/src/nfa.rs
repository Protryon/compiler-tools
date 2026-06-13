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
    /// Zero-width word-boundary assertion (`\b` / `\B`); the bool negates it.
    WordBoundary(bool),
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
            TransitionEvent::EndOfInput | TransitionEvent::WordBoundary(_) => false,
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
/// feeding one fragment's end in as the next fragment's start. Consuming edges
/// are pushed onto `start` before any skip/loop epsilon, which keeps greedy
/// quantifiers and earlier alternation branches first in declaration order — the
/// subset construction in `dfa.rs` then makes the machine deterministic.
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

    fn build_repeat(&mut self, atom: &AtomRepeat, start: u32) -> u32 {
        match atom.repeat {
            Repeat::Once => self.build_atom(&atom.atom, start),
            Repeat::ZeroOrOnce => {
                // Consuming body first, then a skip epsilon (greedy: prefer to match).
                let end = self.build_atom(&atom.atom, start);
                self.edge(start, TransitionEvent::Epsilon, end);
                end
            }
            Repeat::ZeroOrMore => {
                let body_end = self.build_atom(&atom.atom, start);
                self.edge(body_end, TransitionEvent::Epsilon, start);
                let end = self.new_state();
                self.edge(start, TransitionEvent::Epsilon, end);
                end
            }
            Repeat::OnceOrMore => {
                let body_end = self.build_atom(&atom.atom, start);
                let end = self.new_state();
                // From the body's end, prefer looping back (consume more) over exiting.
                self.edge(body_end, TransitionEvent::Epsilon, start);
                self.edge(body_end, TransitionEvent::Epsilon, end);
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
            Atom::WordBoundary(negate) => {
                let end = self.new_state();
                self.edge(start, TransitionEvent::WordBoundary(*negate), end);
                end
            }
            Atom::Alternation(branches) => {
                // Every branch leaves from the shared `start` (so its leading edges
                // sit on `start` in declaration order) and merges into one `end`.
                let end = self.new_state();
                for branch in branches {
                    let branch_end = self.build_sequence(branch, start);
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

    #[test]
    fn zero_or_more_adds_epsilon_skip_and_loops_back() {
        // "a*" binds the star to the trailing char. The Thompson fragment has the
        // start state offer the consuming body edge plus an epsilon skip to the
        // final state, and the body's end loops back to the start via epsilon.
        let nfa = Nfa::build(&SimpleRegexAst::parse("a*").unwrap());
        let start = nfa.transitions.get(&0).expect("start state present");
        let (_, body) = start
            .iter()
            .find(|(e, _)| matches!(e, TransitionEvent::Char('a')))
            .expect("consuming body edge present");
        assert!(
            start.iter().any(|(e, t)| matches!(e, TransitionEvent::Epsilon) && *t == nfa.final_state),
            "epsilon skip to final: {start:?}"
        );
        let body_trans = nfa.transitions.get(body).expect("body state present");
        assert!(body_trans.contains(&(TransitionEvent::Epsilon, 0)), "body loops back to start via epsilon: {body_trans:?}");
    }
}
