use quote::format_ident;

use super::*;

impl SimpleRegex {
    pub fn generate_parser(&self, fn_name: Ident) -> TokenStream {
        let mut state_fns = vec![];
        let mut state_matches = vec![];
        for (state, transitions) in &self.dfa.transitions {
            let state_fn = format_ident!("state_{}", state);
            // Consuming edges become `match target` arms; the zero-width edges (End and
            // word boundaries) are resolved in the fallback arm so they don't fight a
            // consuming edge for the same `target` value.
            let mut consuming_matches = vec![];
            let mut end_target: Option<u32> = None;
            let mut boundaries: Vec<(bool, u32)> = vec![];
            for (transition, target) in transitions {
                match transition {
                    nfa::TransitionEvent::Epsilon => unreachable!(),
                    nfa::TransitionEvent::End => end_target = Some(*target),
                    nfa::TransitionEvent::WordBoundary(negate) => boundaries.push((*negate, *target)),
                    nfa::TransitionEvent::EndOfInput => consuming_matches.push(quote! { None => ::compiler_tools::MatchResult::MatchedEmpty(#target), }),
                    nfa::TransitionEvent::Char(c) => consuming_matches.push(quote! { Some(#c) => ::compiler_tools::MatchResult::Matched(#target), }),
                    nfa::TransitionEvent::Chars(inverted, group) => {
                        let mut matching = vec![];
                        for entry in group {
                            match entry {
                                GroupEntry::Char(c) => {
                                    if !matching.is_empty() {
                                        matching.push(quote! { | })
                                    }
                                    matching.push(quote! { #c });
                                }
                                GroupEntry::Range(start, end) => {
                                    if !matching.is_empty() {
                                        matching.push(quote! { | })
                                    }
                                    matching.push(quote! { #start ..= #end });
                                }
                            }
                        }
                        let matching_empty = matching.is_empty();

                        let matching = flatten(matching);
                        consuming_matches.push(if *inverted {
                            if matching_empty {
                                quote! { _ => ::compiler_tools::MatchResult::Matched(#target), }
                            } else {
                                quote! { Some(c) if !matches!(c, #matching) => ::compiler_tools::MatchResult::Matched(#target), }
                            }
                        } else {
                            quote! { Some(c) if matches!(c, #matching) => ::compiler_tools::MatchResult::Matched(#target), }
                        });
                    }
                }
            }
            let consuming_matches = flatten(consuming_matches);

            // The fallback for `target` values no consuming edge claimed. `End` accepts
            // unconditionally; otherwise any word boundaries are checked against `prev` and
            // the lookahead char (`other`), with consuming edges already having had priority.
            let (uses_prev, fallback) = if let Some(end) = end_target {
                (false, quote! { _ => ::compiler_tools::MatchResult::MatchedEmpty(#end), })
            } else if boundaries.is_empty() {
                (false, quote! { _ => ::compiler_tools::MatchResult::NoMatch, })
            } else {
                let mut chain = quote! { ::compiler_tools::MatchResult::NoMatch };
                for (negate, target) in boundaries.iter().rev() {
                    let cmp = if *negate {
                        quote! { == }
                    } else {
                        quote! { != }
                    };
                    chain = quote! {
                        if is_word(prev) #cmp is_word(other) {
                            ::compiler_tools::MatchResult::MatchedEmpty(#target)
                        } else {
                            #chain
                        }
                    };
                }
                (
                    true,
                    quote! {
                        other => {
                            fn is_word(ch: Option<char>) -> bool {
                                matches!(ch, Some('0'..='9' | 'a'..='z' | 'A'..='Z' | '_'))
                            }
                            #chain
                        }
                    },
                )
            };
            let prev_param = if uses_prev {
                quote! { prev }
            } else {
                quote! { _prev }
            };

            state_fns.push(quote! {
                #[inline]
                fn #state_fn(#prev_param: Option<char>, target: Option<char>) -> ::compiler_tools::MatchResult {
                    match target {
                        #consuming_matches
                        #fallback
                    }
                }
            });
            state_matches.push(quote! {
                #state => #state_fn(prev, c),
            });
        }
        let state_fns = flatten(state_fns);
        let state_matches = flatten(state_matches);
        let final_state = self.dfa.final_state;
        quote! {
            fn #fn_name(from: &str) -> Option<(&str, &str)> {
                #state_fns
                let mut counter = 0usize;
                let mut state = 0u32;
                // `prev`/`c` are the chars on either side of the current position; zero-width
                // assertions inspect both without consuming, so the loop only advances `c`
                // (and `counter`) on a consuming `Matched`.
                let mut prev: Option<char> = None;
                let mut chars = from.chars();
                let mut c = chars.next();
                loop {
                    let next_state = match state {
                        #state_matches
                        _ => ::compiler_tools::MatchResult::NoMatch,
                    };
                    match next_state {
                        ::compiler_tools::MatchResult::Matched(next_state) => {
                            state = next_state;
                            if let Some(ch) = c {
                                counter += ch.len_utf8();
                            }
                            prev = c;
                            c = chars.next();
                            if next_state == #final_state {
                                return Some((&from[..counter], &from[counter..]));
                            }
                        },
                        ::compiler_tools::MatchResult::MatchedEmpty(next_state) => {
                            // Zero-width transition: keep the lookahead char and position.
                            state = next_state;
                            if next_state == #final_state {
                                return Some((&from[..counter], &from[counter..]));
                            }
                        },
                        ::compiler_tools::MatchResult::NoMatch => return None,
                    }
                }
            }
        }
    }
}
