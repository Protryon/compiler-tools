use super::*;

impl SimpleRegex {
    /// Emit a self-contained `fn(&str, Option<char>) -> Option<(&str, &str)>` matcher
    /// for this DFA.
    ///
    /// The body is a single `loop { match state { .. } }`: each DFA state is one arm
    /// that inspects the lookahead char `c`, mutates `state`/`counter`/`c` in place, and
    /// either falls through (re-entering the loop) or `break`s. There is no per-state
    /// function and no intermediate `MatchResult` enum — folding the old two-level
    /// dispatch into one `match` lets the optimizer build a single jump table.
    ///
    /// Accepting states record `last = counter` at the top of their own arm (instead of
    /// a per-iteration `is_accepting` check), and the zero-width bookkeeping — `prev`
    /// (for `^` under `(?m)` and `\b`/`\B`) and the `zero_width` cycle guard — is only
    /// emitted when the DFA actually contains those edges, so the common no-assertion
    /// loop stays tight.
    pub fn generate_parser(&self, fn_name: Ident) -> TokenStream {
        // Which zero-width machinery is actually reachable in this DFA? Omitting the
        // unused parts keeps the hot loop small for the overwhelmingly common case of a
        // pattern with no `$`/`^`/`\b`.
        let mut needs_prev = false; // any `^`(?m)/`\b`/`\B` edge → we must track the previous char
        let mut has_zero_width = false; // any zero-width edge → keep the cycle guard + counter
        let mut needs_word_ascii = false; // any ASCII `\b`/`\B` → emit `is_word_ascii`
        let mut needs_word_unicode = false; // any Unicode `\b`/`\B` → emit `is_word_unicode` + table
        for transitions in self.dfa.transitions.values() {
            for (transition, _) in transitions {
                match transition {
                    nfa::TransitionEvent::WordBoundary {
                        unicode,
                        ..
                    } => {
                        needs_prev = true;
                        has_zero_width = true;
                        if *unicode {
                            needs_word_unicode = true;
                        } else {
                            needs_word_ascii = true;
                        }
                    }
                    nfa::TransitionEvent::StartOfLine {
                        ..
                    } => {
                        needs_prev = true;
                        has_zero_width = true;
                    }
                    // A CRLF `$` checks `prev != '\r'`, so it needs the previous char too.
                    nfa::TransitionEvent::EndOfLine {
                        crlf,
                    } => {
                        has_zero_width = true;
                        needs_prev |= *crlf;
                    }
                    nfa::TransitionEvent::EndOfInput => has_zero_width = true,
                    // `^`/`\A` checks `prev`, so it needs the previous char tracked.
                    nfa::TransitionEvent::StartOfText => {
                        needs_prev = true;
                        has_zero_width = true;
                    }
                    _ => {}
                }
            }
        }

        // The word-ness helpers are emitted once at the top of the generated fn (not
        // per boundary arm): the Unicode table is large, so inlining it per arm would
        // bloat the output. Each takes `Option<char>` (an input edge is non-word).
        let is_word_fns = {
            let ascii = needs_word_ascii.then(|| {
                quote! {
                    fn is_word_ascii(ch: Option<char>) -> bool {
                        matches!(ch, Some('0'..='9' | 'a'..='z' | 'A'..='Z' | '_'))
                    }
                }
            });
            let unicode = needs_word_unicode.then(|| {
                let ranges = crate::unicode::word_ranges().into_iter().map(|(lo, hi)| quote! { (#lo, #hi) });
                let ranges = flatten(ranges.map(|r| quote! { #r, }));
                quote! {
                    fn is_word_unicode(ch: Option<char>) -> bool {
                        // Sorted, disjoint `\w` codepoint ranges; membership by binary search.
                        const RANGES: &[(char, char)] = &[ #ranges ];
                        match ch {
                            Some(c) => RANGES
                                .binary_search_by(|&(lo, hi)| {
                                    if c < lo {
                                        ::core::cmp::Ordering::Greater
                                    } else if c > hi {
                                        ::core::cmp::Ordering::Less
                                    } else {
                                        ::core::cmp::Ordering::Equal
                                    }
                                })
                                .is_ok(),
                            None => false,
                        }
                    }
                }
            });
            quote! { #ascii #unicode }
        };

        // A zero-width move keeps the lookahead char and byte position, so a cycle of
        // them (e.g. `\b*`) can never extend the match; bound it by the state count.
        let zero_width_limit = self.dfa.transitions.len() + 1;

        // Shared fragments injected into every consuming arm. `prev`/`zero_width` are
        // only maintained when some state needs them.
        let reset_zw = if has_zero_width {
            quote! { zero_width = 0; }
        } else {
            quote! {}
        };
        let set_prev = |val: TokenStream| {
            if needs_prev {
                quote! { prev = #val; }
            } else {
                quote! {}
            }
        };

        let mut state_arms = vec![];
        for (state, transitions) in &self.dfa.transitions {
            // Consuming edges become `Some(..)` arms that advance the cursor. `End` only
            // marks the state accepting; the zero-width moves (`$`/`\z`, the `(?m)` line
            // anchors, and word boundaries) are collected in stored (priority) order and
            // emitted as a single `prev`/lookahead-aware catch-all arm.
            let mut consuming_arms = vec![];
            let mut zero_width: Vec<(&nfa::TransitionEvent, u32)> = vec![];
            let mut is_accepting = false;
            for (transition, target) in transitions {
                match transition {
                    nfa::TransitionEvent::Epsilon => unreachable!(),
                    nfa::TransitionEvent::End => is_accepting = true,
                    nfa::TransitionEvent::WordBoundary {
                        ..
                    }
                    | nfa::TransitionEvent::EndOfInput
                    | nfa::TransitionEvent::StartOfText
                    | nfa::TransitionEvent::EndOfLine {
                        ..
                    }
                    | nfa::TransitionEvent::StartOfLine {
                        ..
                    } => zero_width.push((transition, *target)),
                    nfa::TransitionEvent::Char(c) => {
                        let prev_set = set_prev(quote! { Some(#c) });
                        consuming_arms.push(quote! {
                            Some(#c) => {
                                state = #target;
                                counter += #c.len_utf8();
                                #prev_set
                                #reset_zw
                                c = chars.next();
                            }
                        });
                    }
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
                        let prev_set = set_prev(quote! { Some(ch) });
                        let advance = quote! {
                            state = #target;
                            counter += ch.len_utf8();
                            #prev_set
                            #reset_zw
                            c = chars.next();
                        };
                        if *inverted && matching_empty {
                            // An inverted empty class (`.` under `(?s)`) matches any char,
                            // but only a real one — at end of input there is nothing to
                            // consume, matching the runtime interpreter (`eval_state`),
                            // which guards every consuming edge with `if let Some(ch) = c`.
                            consuming_arms.push(quote! { Some(ch) => { #advance } });
                        } else if matching_empty {
                            // A non-inverted empty class matches no character, so emit no
                            // arm at all (an empty `matches!(ch, )` would be invalid Rust).
                        } else {
                            let guard = if *inverted {
                                quote! { !matches!(ch, #matching) }
                            } else {
                                quote! { matches!(ch, #matching) }
                            };
                            consuming_arms.push(quote! { Some(ch) if #guard => { #advance } });
                        }
                    }
                }
            }

            // Fold the accept into the arm: an accepting state records the position the
            // moment it is (re-)entered, replacing the old per-iteration `is_accepting`.
            // A non-accepting state still records the position if it can reach an accept
            // purely through zero-width assertion edges that hold here (`.+\b`: remember
            // the `\b`-gated accept while the greedy `.+` consumes past it, then back off
            // to it). The conditions use the lookahead `c` (the accept check runs before
            // the `match c` below); a zero-width hop never moves, so every edge on a path
            // is evaluated at the same `prev`/`c`.
            let accept = if is_accepting {
                quote! { last = counter; }
            } else {
                let conds = self.zero_width_accept_conditions(*state);
                if conds.is_empty() {
                    quote! {}
                } else {
                    let cond = flatten(conds.into_iter().enumerate().map(|(i, c)| {
                        if i == 0 {
                            quote! { (#c) }
                        } else {
                            quote! { || (#c) }
                        }
                    }));
                    quote! { if #cond { last = counter; } }
                }
            };

            // The zero-width moves are evaluated only when no consuming edge claimed the
            // lookahead char (`other`), in stored priority order:
            //   * `$`/`\z` (`EndOfInput`) accepts only at end of input;
            //   * `$` under `(?m)` (`EndOfLine`) at end of input or before a `\n`;
            //   * `^` under `(?m)` (`StartOfLine`) at start of input or after a `\n`
            //     (tested against `prev`, independent of the lookahead);
            //   * `\b`/`\B` (`WordBoundary`) when the boundary holds for `prev`/`other`.
            // Each is a zero-width move (state changes, lookahead/position do not),
            // guarded against an infinite zero-width cycle.
            let fallback = if zero_width.is_empty() {
                quote! { _ => break, }
            } else {
                let mut chain = quote! { break };
                for (event, target) in zero_width.iter().rev() {
                    // Evaluated against the lookahead `other` (the `match c` arm binding).
                    let cond = zw_cond(event, &quote! { other });
                    chain = quote! {
                        if #cond {
                            state = #target;
                            zero_width += 1;
                            if zero_width > #zero_width_limit { break; }
                        } else {
                            #chain
                        }
                    };
                }
                quote! {
                    other => {
                        #chain
                    }
                }
            };

            let consuming_arms = flatten(consuming_arms);
            state_arms.push(quote! {
                #state => {
                    #accept
                    match c {
                        #consuming_arms
                        #fallback
                    }
                }
            });
        }

        let state_arms = flatten(state_arms);
        // The sink `final_state` carries no transition entry of its own. The matcher can
        // still transition *into* it (e.g. after the last char of `a`), so it gets an
        // explicit arm: it is always accepting and has no edges, so record the accept and
        // stop.
        let final_state = self.dfa.final_state;

        // Optional state, declared only when some arm references it.
        let prev_param = if needs_prev {
            quote! { prev_in }
        } else {
            quote! { _prev_in }
        };
        let prev_decl = if needs_prev {
            quote! { let mut prev: Option<char> = prev_in; }
        } else {
            quote! {}
        };
        let zero_width_decl = if has_zero_width {
            quote! { let mut zero_width = 0usize; }
        } else {
            quote! {}
        };

        quote! {
            // `prev_in` is the char immediately before `from` in the larger input (the
            // caller supplies it; `None` means start of text). It seeds the zero-width
            // assertions — `^` under `(?m)` and `\b` — so a slice taken mid-input still
            // sees the correct preceding context.
            // A trivial pattern (e.g. the empty regex or a bare anchor) compiles to a
            // `loop` whose every arm `break`s; that is correct, so silence `never_loop`.
            #[allow(clippy::never_loop)]
            fn #fn_name(from: &str, #prev_param: Option<char>) -> Option<(&str, &str)> {
                // Word-ness helpers, emitted once for the whole matcher (only when a
                // `\b`/`\B` of the matching mode is reachable).
                #is_word_fns
                let mut state = 0u32;
                // Byte offset of the last accepting position; `usize::MAX` is the "none
                // yet" sentinel (cheaper in the hot loop than an `Option`).
                let mut last = usize::MAX;
                let mut counter = 0usize;
                #prev_decl
                let mut chars = from.chars();
                let mut c = chars.next();
                #zero_width_decl
                // Leftmost-first: priority is baked into the DFA, so following consuming
                // edges and backing off to the last accept yields the regex-crate match.
                // `prev`/`c` are the chars on either side of the current position;
                // zero-width assertions inspect both without consuming, so the loop only
                // advances `c` (and `counter`) on a consuming match.
                loop {
                    match state {
                        #state_arms
                        #final_state => {
                            last = counter;
                            break;
                        }
                        _ => break,
                    }
                }
                if last == usize::MAX {
                    None
                } else {
                    Some((&from[..last], &from[last..]))
                }
            }
        }
    }

    /// Whether `state` is accepting: the transition-less sink, or any state carrying
    /// an `End` edge. Mirrors `matching::SimpleRegex::accepts`.
    fn state_accepts(&self, state: u32) -> bool {
        state == self.dfa.final_state
            || self
                .dfa
                .transitions
                .get(&state)
                .is_some_and(|transitions| transitions.iter().any(|(t, _)| matches!(t, nfa::TransitionEvent::End)))
    }

    /// One condition per simple path of zero-width assertion edges from `state` to an
    /// accepting state — the `&&` of the edges' conditions (lookahead `c`); the caller
    /// `||`s them. This is the build-time counterpart of the interpreter's
    /// `accepts_via_assertions`: it lets a non-accepting state record an accept that is
    /// only reachable through assertions holding at the current position. `state` itself
    /// accepting is handled separately (unconditional accept), so paths start one hop in.
    fn zero_width_accept_conditions(&self, state: u32) -> Vec<TokenStream> {
        let mut out = vec![];
        let mut on_path = std::collections::HashSet::new();
        self.zero_width_accept_dfs(state, &mut vec![], &mut on_path, &mut out);
        out
    }

    fn zero_width_accept_dfs(&self, state: u32, acc: &mut Vec<TokenStream>, on_path: &mut std::collections::HashSet<u32>, out: &mut Vec<TokenStream>) {
        // A repeated state means a zero-width cycle; it can never reach a *new* accept,
        // so stop (any reachable accept is found via the simple sub-path).
        if !on_path.insert(state) {
            return;
        }
        if let Some(transitions) = self.dfa.transitions.get(&state) {
            for (transition, target) in transitions {
                if matches!(transition, nfa::TransitionEvent::Char(_) | nfa::TransitionEvent::Chars(..) | nfa::TransitionEvent::End) {
                    continue;
                }
                acc.push(zw_cond(transition, &quote! { c }));
                if self.state_accepts(*target) {
                    out.push(flatten(acc.iter().enumerate().map(|(i, c)| {
                        if i == 0 {
                            quote! { #c }
                        } else {
                            quote! { && #c }
                        }
                    })));
                }
                self.zero_width_accept_dfs(*target, acc, on_path, out);
                acc.pop();
            }
        }
        on_path.remove(&state);
    }
}

/// The boolean condition under which a zero-width assertion edge holds, evaluated
/// against `prev` and the given lookahead token (`c` at the top of a state arm, the
/// `other` binding in the consuming-`match`'s fallback). Compound conditions are
/// parenthesised so they compose under `&&`/`||`.
fn zw_cond(event: &nfa::TransitionEvent, look: &TokenStream) -> TokenStream {
    match event {
        nfa::TransitionEvent::EndOfInput => quote! { #look.is_none() },
        // `^`/`\A` (non-multiline): only at the very start of the input.
        nfa::TransitionEvent::StartOfText => quote! { prev.is_none() },
        // `$` under `(?m)`: end of input or before a line terminator. CRLF mode widens
        // the terminator set to `\r`/`\n`/`\r\n`, holding before a `\r` or a lone `\n`
        // but not between the `\r` and `\n` of a pair.
        nfa::TransitionEvent::EndOfLine {
            crlf: false,
        } => quote! { matches!(#look, None | Some('\n')) },
        nfa::TransitionEvent::EndOfLine {
            crlf: true,
        } => quote! { (matches!(#look, None | Some('\r')) || (#look == Some('\n') && prev != Some('\r'))) },
        // `^` under `(?m)`: start of input or after a line terminator; the CRLF rule
        // mirrors `$` (after `\n` or a lone `\r`, not inside `\r\n`).
        nfa::TransitionEvent::StartOfLine {
            crlf: false,
        } => quote! { matches!(prev, None | Some('\n')) },
        nfa::TransitionEvent::StartOfLine {
            crlf: true,
        } => quote! { (matches!(prev, None | Some('\n')) || (prev == Some('\r') && #look != Some('\n'))) },
        nfa::TransitionEvent::WordBoundary {
            negate,
            unicode,
        } => {
            // Word-ness via the once-emitted helper for this boundary's mode.
            let is_word = if *unicode {
                quote! { is_word_unicode }
            } else {
                quote! { is_word_ascii }
            };
            let cmp = if *negate {
                quote! { == }
            } else {
                quote! { != }
            };
            quote! { (#is_word(prev) #cmp #is_word(#look)) }
        }
        nfa::TransitionEvent::Epsilon | nfa::TransitionEvent::Char(_) | nfa::TransitionEvent::Chars(..) | nfa::TransitionEvent::End => {
            unreachable!("not a zero-width assertion")
        }
    }
}
