use std::collections::{BTreeMap, BTreeSet};

use super::{Atom, GroupEntry, Repeat, SimpleRegexAst};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TransitionEvent {
    Epsilon,
    Char(char),
    // (inverted, set)
    Chars(bool, Vec<GroupEntry>),
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
            TransitionEvent::End => true,
        }
    }

    pub fn completely_shadows(&self, other: &TransitionEvent) -> bool {
        match (self, other) {
            (TransitionEvent::Epsilon, _) | (_, TransitionEvent::Epsilon) | (_, TransitionEvent::End) => false,
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
        Atom::Literal(l) => l.chars().map(|x| TransitionEvent::Char(x)).collect(),
        Atom::Group(inverted, entries) => {
            vec![TransitionEvent::Chars(*inverted, entries.clone())]
        }
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

    #[test]
    fn test_nfa() {
        let regex = SimpleRegexAst::parse("[a-z][a-zA-Z0-9_]*").unwrap();
        let nfa = Nfa::build(&regex);
        println!("{:?}", nfa);
    }
}
