use std::collections::{BTreeMap, BTreeSet};

use super::{Atom, GroupEntry, Repeat, SimpleRegexAst};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TransitionEvent {
    Epsilon,
    Char(char),
    // (inverted, set)
    Chars(bool, Vec<GroupEntry>),
    /// Zero-width `$` assertion: only taken when the input is exhausted.
    EndOfInput,
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
            TransitionEvent::EndOfInput => false,
            TransitionEvent::End => true,
        }
    }

    pub fn completely_shadows(&self, other: &TransitionEvent) -> bool {
        match (self, other) {
            (TransitionEvent::Epsilon, _) | (_, TransitionEvent::Epsilon) | (_, TransitionEvent::End) => false,
            // A `$` assertion is conditioned on EOF rather than a character, so it neither
            // shadows nor is shadowed by any other edge; keep it independent of `End`.
            (TransitionEvent::EndOfInput, _) | (_, TransitionEvent::EndOfInput) => false,
            (TransitionEvent::End, _) => true,
            (TransitionEvent::Char(c1), TransitionEvent::Char(c2)) => c1 == c2,
            //TODO: this could be true, investigate
            (TransitionEvent::Char(_), TransitionEvent::Chars(_, _)) => false,
            (e1, TransitionEvent::Char(c2)) => e1.matches(*c2),
            //TODO: this could be true, investigate
            (TransitionEvent::Chars(_, _), TransitionEvent::Chars(_, _)) => false,
        }
    }
}

#[derive(Debug)]
pub struct Nfa {
    // state => [(event, state)]
    pub transitions: BTreeMap<u32, Vec<(TransitionEvent, u32)>>,
    pub final_state: u32,
}

fn event_from_atom(atom: &Atom) -> Vec<TransitionEvent> {
    match atom {
        Atom::Literal(l) => l.chars().map(TransitionEvent::Char).collect(),
        Atom::Group(inverted, entries) => {
            vec![TransitionEvent::Chars(*inverted, entries.clone())]
        }
        Atom::EndOfInput => vec![TransitionEvent::EndOfInput],
    }
}

impl Nfa {
    pub fn epsilon_closure(&self, state: u32) -> Vec<(TransitionEvent, u32)> {
        let mut output: Vec<(TransitionEvent, u32)> = vec![];
        let mut stack = vec![state];
        let mut observed = BTreeSet::new();
        while let Some(state) = stack.pop() {
            if state == self.final_state {
                output.push((TransitionEvent::End, state));
                continue;
            }
            let transitions = self.transitions.get(&state).expect("invalid state");
            for (event, target) in transitions {
                if observed.contains(&(event, target)) {
                    continue;
                }
                observed.insert((event, target));

                match event {
                    TransitionEvent::Epsilon => stack.push(*target),
                    transition => {
                        output.push((transition.clone(), *target));
                    }
                }
            }
        }
        output
    }

    pub fn build(from: &SimpleRegexAst) -> Self {
        let mut self_ = Self {
            transitions: Default::default(),
            final_state: 0,
        };

        let mut current_state = 0u32;
        for atom in &from.atoms {
            let mut events = event_from_atom(&atom.atom);

            match atom.repeat {
                Repeat::Once => {
                    for event in events {
                        self_.transitions.insert(current_state, vec![(event, current_state + 1)]);
                        current_state += 1;
                    }
                }
                Repeat::ZeroOrOnce => {
                    let first_event = events.remove(0);
                    self_.transitions.insert(
                        current_state,
                        vec![
                            (first_event, current_state + 1),
                            (TransitionEvent::Epsilon, current_state + events.len() as u32 + 1),
                        ],
                    );
                    current_state += 1;

                    for event in events {
                        self_.transitions.insert(current_state, vec![(event, current_state + 1)]);
                        current_state += 1;
                    }
                }
                Repeat::OnceOrMore => {
                    for event in events.iter().cloned() {
                        self_.transitions.insert(current_state, vec![(event, current_state + 1)]);
                        current_state += 1;
                    }

                    let first_event = events.remove(0);
                    let initialization = current_state;
                    let next_state = if events.is_empty() { initialization } else { current_state + 1 };
                    self_
                        .transitions
                        .insert(current_state, vec![(first_event, next_state), (TransitionEvent::Epsilon, current_state + events.len() as u32 + 1)]);
                    current_state += 1;

                    let len = events.len();
                    for (i, event) in events.into_iter().enumerate() {
                        let next_state = if i + 1 == len { initialization } else { current_state + 1 };
                        self_.transitions.insert(current_state, vec![(event, next_state)]);
                        current_state += 1;
                    }
                }
                Repeat::ZeroOrMore => {
                    let first_event = events.remove(0);
                    let initialization = current_state;
                    let next_state = if events.is_empty() { initialization } else { current_state + 1 };
                    self_
                        .transitions
                        .insert(current_state, vec![(first_event, next_state), (TransitionEvent::Epsilon, current_state + events.len() as u32 + 1)]);
                    current_state += 1;

                    let len = events.len();
                    for (i, event) in events.into_iter().enumerate() {
                        let next_state = if i + 1 == len { initialization } else { current_state + 1 };
                        self_.transitions.insert(current_state, vec![(event, next_state)]);
                        current_state += 1;
                    }
                }
            }
        }
        self_.final_state = current_state;

        self_
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
    fn end_shadows_everything_but_epsilon_and_end() {
        let end = TransitionEvent::End;
        assert!(end.completely_shadows(&TransitionEvent::Char('a')));
        assert!(end.completely_shadows(&chars(false, &[GroupEntry::Char('a')])));
        assert!(!end.completely_shadows(&TransitionEvent::Epsilon));
        assert!(!end.completely_shadows(&TransitionEvent::End));
    }

    #[test]
    fn char_shadows_equal_char_only() {
        assert!(TransitionEvent::Char('a').completely_shadows(&TransitionEvent::Char('a')));
        assert!(!TransitionEvent::Char('a').completely_shadows(&TransitionEvent::Char('b')));
        // Conservatively, a single char never claims to shadow a class.
        assert!(!TransitionEvent::Char('a').completely_shadows(&chars(false, &[GroupEntry::Char('a')])));
    }

    #[test]
    fn class_shadows_a_char_it_covers() {
        let lower = chars(false, &[GroupEntry::Range('a', 'z')]);
        assert!(lower.completely_shadows(&TransitionEvent::Char('m')));
        assert!(!lower.completely_shadows(&TransitionEvent::Char('A')));
        // Class-vs-class is conservatively never a shadow.
        assert!(!lower.completely_shadows(&chars(false, &[GroupEntry::Char('a')])));
    }

    #[test]
    fn epsilon_shadows_nothing() {
        assert!(!TransitionEvent::Epsilon.completely_shadows(&TransitionEvent::Char('a')));
        assert!(!TransitionEvent::Char('a').completely_shadows(&TransitionEvent::Epsilon));
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
        // "a*" binds the star to the trailing char; the start state offers an
        // epsilon to skip and the consuming edge loops back to itself.
        let nfa = Nfa::build(&SimpleRegexAst::parse("a*").unwrap());
        let start = nfa.final_state - 1;
        let trans = nfa.transitions.get(&start).expect("repeat state present");
        assert!(trans.contains(&(TransitionEvent::Char('a'), start)), "consuming edge loops back: {trans:?}");
        assert!(
            trans.iter().any(|(e, t)| matches!(e, TransitionEvent::Epsilon) && *t == nfa.final_state),
            "epsilon skip to final: {trans:?}"
        );
    }

    #[test]
    fn epsilon_closure_reports_end_at_final_state() {
        let nfa = Nfa::build(&SimpleRegexAst::parse("a").unwrap());
        let closure = nfa.epsilon_closure(nfa.final_state);
        assert_eq!(closure, vec![(TransitionEvent::End, nfa.final_state)]);
    }
}
