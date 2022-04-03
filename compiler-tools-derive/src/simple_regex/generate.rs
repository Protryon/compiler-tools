use quote::format_ident;

use super::*;

impl SimpleRegex {
    pub fn generate_parser(&self, fn_name: Ident) -> TokenStream {
        let mut state_fns = vec![];
        let mut state_matches = vec![];
        for (state, transitions) in &self.dfa.transitions {
            let state_fn = format_ident!("state_{}", state);
            let mut transition_matches = vec![];
            for (transition, target) in transitions {
                let match_expr = match transition {
                    nfa::TransitionEvent::Epsilon => unreachable!(),
                    nfa::TransitionEvent::End => quote! { _ => ::compiler_tools::MatchResult::MatchedEmpty(#target), },
                    nfa::TransitionEvent::Char(c) => quote! { Some(#c) => ::compiler_tools::MatchResult::Matched(#target), },
                    nfa::TransitionEvent::Chars(inverted, group) => {
                        let mut matching = vec![];
                        for entry in group {
                            match entry {
                                GroupEntry::Char(c) => {
                                    if !matching.is_empty() {
                                        matching.push(quote! { | })
                                    }
                                    matching.push(quote! { Some(#c) });
                                }
                                GroupEntry::Range(start, end) => {
                                    if !matching.is_empty() {
                                        matching.push(quote! { | })
                                    }
                                    matching.push(quote! { Some(#start ..= #end) });
                                }
                            }
                        }
                        let matching_empty = matching.is_empty();

                        let matching = flatten(matching);
                        if *inverted {
                            if matching_empty {
                                quote! {
                                    _ => ::compiler_tools::MatchResult::Matched(#target),
                                }
                            } else {
                                quote! {
                                    c if !matches!(c, #matching) => ::compiler_tools::MatchResult::Matched(#target),
                                }
                            }
                        } else {
                            quote! {
                                #matching => ::compiler_tools::MatchResult::Matched(#target),
                            }
                        }
                    }
                };
                transition_matches.push(match_expr);
            }
            let transition_matches = flatten(transition_matches);

            state_fns.push(quote! {
                #[inline]
                fn #state_fn(target: Option<char>) -> ::compiler_tools::MatchResult {
                    match target {
                        #transition_matches
                        _ => ::compiler_tools::MatchResult::NoMatch,
                    }
                }
            });
            state_matches.push(quote! {
                #state => #state_fn(c),
            });
        }
        /*
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
        */
        let state_fns = flatten(state_fns);
        let state_matches = flatten(state_matches);
        let final_state = self.dfa.final_state;
        quote! {
            fn #fn_name(from: &str) -> Option<(&str, &str)> {
                #state_fns
                let mut counter = 0usize;
                let mut state = 0u32;
                let mut chars = from.chars();
                loop {
                    let c = chars.next();
                    let next_state = match state {
                        #state_matches
                        _ => ::compiler_tools::MatchResult::NoMatch,
                    };
                    match next_state {
                        ::compiler_tools::MatchResult::Matched(next_state) => {
                            state = next_state;
                            counter += c.unwrap().len_utf8();
                            if next_state == #final_state {
                                return Some((&from[..counter], &from[counter..]));
                            }
                        },
                        ::compiler_tools::MatchResult::MatchedEmpty(next_state) => {
                            state = next_state;
                            //TODO: backtrack iterator (but this only occurs at End sequence right now)
                            if next_state == #final_state {
                                return Some((&from[..counter], &from[counter..]));
                            }
                        },
                        ::compiler_tools::MatchResult::NoMatch => return None,
                    }
                }
                None
            }
        }
    }
}
