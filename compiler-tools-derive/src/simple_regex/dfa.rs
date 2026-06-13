use std::collections::{BTreeMap, HashMap, HashSet};

use super::nfa::{Nfa, TransitionEvent};

#[derive(Debug)]
pub struct Dfa {
    // state => [(event, state)]
    pub transitions: BTreeMap<u32, Vec<(TransitionEvent, u32)>>,
    pub final_state: u32,
}

impl Dfa {
    pub fn build(nfa: &Nfa) -> Self {
        let mut self_ = Self {
            transitions: Default::default(),
            final_state: nfa.final_state,
        };

        let mut state_set: Vec<(u32, Vec<(TransitionEvent, u32)>)> = vec![(0u32, vec![])];
        let mut observed = HashSet::new();
        while let Some((state, shadows)) = state_set.pop() {
            // println!("oshadow {} {:?}", state, shadows);
            if state == nfa.final_state {
                continue;
            }
            let mut epsilon_closure = nfa.epsilon_closure(state);
            epsilon_closure.extend(shadows);

            let mut epsilon_closure = epsilon_closure.into_iter().enumerate().collect::<HashMap<_, _>>();

            let mut shadowed_closures: HashMap<usize, Vec<usize>> = HashMap::new();
            for (i, (transition1, _)) in epsilon_closure.iter() {
                for (j, (transition2, _)) in epsilon_closure.iter() {
                    if j == i {
                        continue;
                    }
                    if transition1.completely_shadows(transition2) {
                        shadowed_closures.entry(*j).or_default().push(*i);
                    }
                }
            }
            let mut original_shadowed_closures: HashMap<u32, Vec<(TransitionEvent, u32)>> = shadowed_closures
                .iter()
                .map(|(shadowed, shadows)| {
                    (
                        epsilon_closure.get(shadowed).unwrap().clone().1,
                        shadows.into_iter().map(|shadow| epsilon_closure.get(shadow).unwrap().clone()).collect(),
                    )
                })
                .collect();

            let total = epsilon_closure.len();
            let mut emitted_closures = vec![];
            while emitted_closures.len() < total {
                for i in 0..total {
                    if !epsilon_closure.contains_key(&i) {
                        continue;
                    }
                    if !shadowed_closures.contains_key(&i) || shadowed_closures.get(&i).unwrap().is_empty() {
                        emitted_closures.push(epsilon_closure.remove(&i).unwrap());
                        for (_shadowed, shadowing) in shadowed_closures.iter_mut() {
                            shadowing.retain(|x| *x != i);
                        }
                    }
                }
            }
            emitted_closures.reverse();

            // println!("eps closure {} = {:?}", state, emitted_closures);
            for (_, target) in &emitted_closures {
                if observed.contains(target) {
                    continue;
                }
                observed.insert(*target);
                state_set.push((*target, original_shadowed_closures.remove(target).unwrap_or_default()));
            }
            self_.transitions.insert(state, emitted_closures);
        }

        self_
    }
}

#[cfg(test)]
mod tests {
    use crate::simple_regex::SimpleRegexAst;

    use super::*;

    fn build(pattern: &str) -> Dfa {
        Dfa::build(&Nfa::build(&SimpleRegexAst::parse(pattern).expect("valid pattern")))
    }

    /// Every state a transition can reach must either be the final state or
    /// itself have an entry in the transition table; otherwise the generated
    /// matcher would land on a state with no arms.
    #[track_caller]
    fn assert_total(dfa: &Dfa) {
        assert!(dfa.transitions.contains_key(&0), "start state must be present");
        for (_, transitions) in &dfa.transitions {
            for (_, target) in transitions {
                assert!(
                    *target == dfa.final_state || dfa.transitions.contains_key(target),
                    "state {target} is reachable but has no transition entry",
                );
            }
        }
    }

    /// A DFA state must not have two arms keyed on the same exact char, or the
    /// generated `match` would be ambiguous / unreachable.
    #[track_caller]
    fn assert_deterministic_chars(dfa: &Dfa) {
        for (state, transitions) in &dfa.transitions {
            let mut seen = HashSet::new();
            for (event, _) in transitions {
                if let TransitionEvent::Char(c) = event {
                    assert!(seen.insert(*c), "state {state} has duplicate char arm for {c:?}");
                }
            }
        }
    }

    #[test]
    fn final_state_is_carried_from_nfa() {
        let nfa = Nfa::build(&SimpleRegexAst::parse("[a-z][a-zA-Z0-9_]*").unwrap());
        let dfa = Dfa::build(&nfa);
        assert_eq!(dfa.final_state, nfa.final_state);
    }

    #[test]
    fn ident_dfa_is_total_and_deterministic() {
        let dfa = build("[a-z][a-zA-Z0-9_]*");
        assert_total(&dfa);
        assert_deterministic_chars(&dfa);
    }

    #[test]
    fn block_comment_dfa_is_total_and_deterministic() {
        // `/\*.*\*/` mixes literals, `.`, and a star — the trickiest construction.
        let dfa = build("/\\*.*\\*/");
        assert_total(&dfa);
        assert_deterministic_chars(&dfa);
    }

    #[test]
    fn overlapping_class_and_exit_char_stays_total_and_deterministic() {
        // In `[a-z]*x` the start state can both loop on the `[a-z]` class and
        // exit on the literal `x`, which the class covers. `completely_shadows`
        // resolves the overlap during subset construction; the result must still
        // be a valid, total DFA with no duplicate single-char arm.
        let dfa = build("[a-z]*x");
        assert_total(&dfa);
        assert_deterministic_chars(&dfa);
        let start = dfa.transitions.get(&0).expect("start state");
        assert!(start.iter().any(|(e, _)| matches!(e, TransitionEvent::Char('x'))), "exit edge present: {start:?}");
        assert!(start.iter().any(|(e, _)| matches!(e, TransitionEvent::Chars(false, _))), "loop class edge present: {start:?}");
    }
}
