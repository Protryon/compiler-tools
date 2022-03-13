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

    #[test]
    fn test_dfa() {
        let regex = SimpleRegexAst::parse("/\\*.*\\*/").unwrap();
        let nfa = Nfa::build(&regex);
        println!("{:?}", nfa);
        let dfa = Dfa::build(&nfa);
        println!("{:?}", dfa);
    }

    #[test]
    fn test_dfa_ident() {
        let regex = SimpleRegexAst::parse("[a-z][a-zA-Z0-9_]*").unwrap();
        let nfa = Nfa::build(&regex);
        println!("{:?}", nfa);
        let dfa = Dfa::build(&nfa);
        println!("{:?}", dfa);
    }
}
