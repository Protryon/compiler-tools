use quote::format_ident;

use super::*;

impl SimpleRegex {
    pub fn generate_parser(&self, fn_name: Ident) -> TokenStream {
        let mut state_fns = vec![];
        let mut state_matches = vec![];
        // States that accept (carry an `End` edge), plus the sink `final_state`.
        let mut accepting_states: Vec<TokenStream> = vec![];
        for (state, transitions) in &self.dfa.transitions {
            let state_fn = format_ident!("state_{}", state);
            // Consuming edges become `match target` arms. `End` only marks the state
            // accepting; the zero-width moves (`$`/`\z`, `^`/`$` line anchors and word
            // boundaries) are collected in stored (priority) order and emitted as a
            // single `prev`/lookahead-aware catch-all arm.
            let mut consuming_matches = vec![];
            let mut zero_width: Vec<(&nfa::TransitionEvent, u32)> = vec![];
            let mut is_accepting = false;
            for (transition, target) in transitions {
                match transition {
                    nfa::TransitionEvent::Epsilon => unreachable!(),
                    nfa::TransitionEvent::End => is_accepting = true,
                    nfa::TransitionEvent::WordBoundary(_)
                    | nfa::TransitionEvent::EndOfInput
                    | nfa::TransitionEvent::EndOfLine
                    | nfa::TransitionEvent::StartOfLine => zero_width.push((transition, *target)),
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
                        } else if matching_empty {
                            // An empty, non-negated class matches no character, so emit no
                            // arm at all (an empty `matches!(c, )` would be invalid Rust).
                            quote! {}
                        } else {
                            quote! { Some(c) if matches!(c, #matching) => ::compiler_tools::MatchResult::Matched(#target), }
                        });
                    }
                }
            }
            if is_accepting {
                accepting_states.push(quote! { #state });
            }
            let consuming_matches = flatten(consuming_matches);

            // The zero-width moves are evaluated only when no consuming edge claimed
            // the lookahead char (`other`), in stored priority order:
            //   * `$`/`\z` (`EndOfInput`) accepts only at end of input;
            //   * `$` under `(?m)` (`EndOfLine`) at end of input or before a `\n`;
            //   * `^` under `(?m)` (`StartOfLine`) at start of input or after a `\n`
            //     (tested against `prev`, independent of the lookahead);
            //   * `\b`/`\B` (`WordBoundary`) when the boundary holds for `prev`/`other`.
            // `^`/`\b` consult `prev`; word boundaries also need the `is_word` helper.
            let uses_prev = zero_width
                .iter()
                .any(|(e, _)| matches!(e, nfa::TransitionEvent::StartOfLine | nfa::TransitionEvent::WordBoundary(_)));
            let needs_is_word = zero_width.iter().any(|(e, _)| matches!(e, nfa::TransitionEvent::WordBoundary(_)));
            let fallback = if zero_width.is_empty() {
                quote! { _ => ::compiler_tools::MatchResult::NoMatch, }
            } else {
                let mut chain = quote! { ::compiler_tools::MatchResult::NoMatch };
                for (event, target) in zero_width.iter().rev() {
                    let cond = match event {
                        nfa::TransitionEvent::EndOfInput => quote! { other.is_none() },
                        nfa::TransitionEvent::EndOfLine => quote! { matches!(other, None | Some('\n')) },
                        nfa::TransitionEvent::StartOfLine => quote! { matches!(prev, None | Some('\n')) },
                        nfa::TransitionEvent::WordBoundary(negate) => {
                            let cmp = if *negate {
                                quote! { == }
                            } else {
                                quote! { != }
                            };
                            quote! { is_word(prev) #cmp is_word(other) }
                        }
                        _ => unreachable!(),
                    };
                    chain = quote! {
                        if #cond {
                            ::compiler_tools::MatchResult::MatchedEmpty(#target)
                        } else {
                            #chain
                        }
                    };
                }
                let is_word_fn = needs_is_word.then(|| {
                    quote! {
                        fn is_word(ch: Option<char>) -> bool {
                            matches!(ch, Some('0'..='9' | 'a'..='z' | 'A'..='Z' | '_'))
                        }
                    }
                });
                quote! {
                    other => {
                        #is_word_fn
                        #chain
                    }
                }
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
        // The sink `final_state` has no transition entry of its own but is accepting.
        let final_state = self.dfa.final_state;
        accepting_states.push(quote! { #final_state });
        let zero_width_limit = self.dfa.transitions.len() + 1;
        quote! {
            // `prev` is the char immediately before `from` in the larger input (the
            // caller supplies it; `None` means start of text). It seeds the zero-width
            // assertions — `^` under `(?m)` and `\b` — so a slice taken mid-input still
            // sees the correct preceding context.
            fn #fn_name(from: &str, prev_in: Option<char>) -> Option<(&str, &str)> {
                #state_fns
                #[inline]
                fn is_accepting(state: u32) -> bool {
                    matches!(state, #(#accepting_states)|*)
                }
                let mut counter = 0usize;
                let mut state = 0u32;
                // Remember the byte offset of the last accepting state and fall back to
                // it on a dead end.
                let mut last: Option<usize> = None;
                // Leftmost-first: priority is baked into the DFA, so following consuming
                // edges and backing off to the last accept yields the regex-crate match.
                // `prev`/`c` are the chars on either side of the current position; zero-width
                // assertions inspect both without consuming, so the loop only advances `c`
                // (and `counter`) on a consuming `Matched`.
                let mut prev: Option<char> = prev_in;
                let mut chars = from.chars();
                let mut c = chars.next();
                let mut zero_width = 0usize;
                loop {
                    if is_accepting(state) {
                        last = Some(counter);
                    }
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
                            zero_width = 0;
                        },
                        ::compiler_tools::MatchResult::MatchedEmpty(next_state) => {
                            // Zero-width transition: keep the lookahead char and position.
                            state = next_state;
                            zero_width += 1;
                            // A zero-width cycle (e.g. `\b*`) can never extend the match.
                            if zero_width > #zero_width_limit {
                                break;
                            }
                        },
                        ::compiler_tools::MatchResult::NoMatch => break,
                    }
                }
                match last {
                    Some(n) => Some((&from[..n], &from[n..])),
                    None => None,
                }
            }
        }
    }
}
