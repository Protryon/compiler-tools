use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::GroupEntry;
use super::nfa::{Nfa, TransitionEvent};

/// An ordered, de-duplicated set of NFA states — the key for a DFA state. The
/// order is **thread priority** (highest first); see [`ordered_closure`].
type Closure = Vec<u32>;

/// The largest Unicode scalar value, used as the upper bound when complementing a
/// character class (`[^...]`, `\D`, `.`).
const MAX_CP: u32 = 0x10FFFF;

/// A character class as a list of inclusive codepoint ranges (kept sorted and
/// disjoint by [`normalize`]).
type Ranges = Vec<(u32, u32)>;

#[derive(Debug)]
pub struct Dfa {
    // state => [(event, state)]
    pub transitions: BTreeMap<u32, Vec<(TransitionEvent, u32)>>,
    pub final_state: u32,
}

/// Priority-ordered epsilon-closure of an (ordered) seed of NFA states: every
/// state reachable by following only `Epsilon` edges, listed in **thread
/// priority** order. This is a pre-order DFS that visits each state's epsilon
/// successors in their stored edge order (which `nfa.rs` arranges as
/// greedy-body-before-exit / earlier-branch-before-later), de-duplicating so the
/// highest-priority occurrence of each state wins. It replaces the old unordered
/// `BTreeSet` closure and is what makes the DFA leftmost-first.
///
/// **Accept-truncation**: as soon as the NFA accepting state (`final_state`) is
/// reached, the closure stops — every still-pending (lower-priority) thread is
/// cut. This is the heart of first-match semantics: once a higher-priority thread
/// has matched, lower-priority continuations can never be preferred, so dropping
/// them is both correct and what keeps the ordered-subset state space finite.
fn ordered_closure(nfa: &Nfa, seed: &[u32], final_state: u32) -> Closure {
    let mut out: Closure = vec![];
    let mut seen: HashSet<u32> = HashSet::new();
    // Pre-order DFS with an explicit stack (avoids recursion blowups on large
    // unrolled `{n}` machines). Successors are pushed in reverse so they pop in
    // stored order; the whole seed is pushed reversed for the same reason.
    let mut stack: Vec<u32> = seed.iter().rev().copied().collect();
    while let Some(state) = stack.pop() {
        if !seen.insert(state) {
            continue;
        }
        out.push(state);
        if state == final_state {
            // Cut lower-priority threads still on the stack.
            break;
        }
        if let Some(transitions) = nfa.transitions.get(&state) {
            for (event, target) in transitions.iter().rev() {
                if matches!(event, TransitionEvent::Epsilon) {
                    stack.push(*target);
                }
            }
        }
    }
    out
}

/// Sorts and merges a list of inclusive codepoint ranges into disjoint, ordered
/// pieces (adjacent ranges are coalesced).
fn normalize(ranges: &mut Ranges) {
    ranges.retain(|(lo, hi)| lo <= hi);
    ranges.sort_unstable();
    let mut merged: Vec<(u32, u32)> = vec![];
    for (lo, hi) in ranges.drain(..) {
        match merged.last_mut() {
            Some(last) if lo <= last.1.saturating_add(1) => last.1 = last.1.max(hi),
            _ => merged.push((lo, hi)),
        }
    }
    *ranges = merged;
}

/// The complement of a normalized range list over the whole codepoint space.
fn complement(ranges: &[(u32, u32)]) -> Ranges {
    let mut out = vec![];
    let mut next = 0u32;
    for (lo, hi) in ranges {
        if *lo > next {
            out.push((next, lo - 1));
        }
        next = hi.saturating_add(1);
        if next > MAX_CP {
            return out;
        }
    }
    out.push((next, MAX_CP));
    out
}

/// The set of codepoints a consuming transition accepts, as normalized ranges.
fn event_ranges(event: &TransitionEvent) -> Ranges {
    match event {
        TransitionEvent::Char(c) => vec![(*c as u32, *c as u32)],
        TransitionEvent::Chars(inverted, entries) => {
            let mut ranges: Vec<(u32, u32)> = entries
                .iter()
                .map(|entry| match entry {
                    GroupEntry::Char(c) => (*c as u32, *c as u32),
                    GroupEntry::Range(start, end) => (*start as u32, *end as u32),
                })
                .collect();
            normalize(&mut ranges);
            if *inverted { complement(&ranges) } else { ranges }
        }
        _ => vec![],
    }
}

/// Splits a codepoint range so it never spans the UTF-16 surrogate gap, then
/// renders each piece as a `GroupEntry`. Surrogates can never appear in a `&str`,
/// so dropping that sub-range is sound; this just keeps `char::from_u32` total.
fn ranges_to_entries(ranges: &[(u32, u32)]) -> Vec<GroupEntry> {
    const SURROGATE_LO: u32 = 0xD800;
    const SURROGATE_HI: u32 = 0xDFFF;
    let mut out = vec![];
    let mut push = |lo: u32, hi: u32| {
        let (lo, hi) = (char::from_u32(lo).unwrap(), char::from_u32(hi).unwrap());
        out.push(if lo == hi { GroupEntry::Char(lo) } else { GroupEntry::Range(lo, hi) });
    };
    for &(lo, hi) in ranges {
        if hi < SURROGATE_LO || lo > SURROGATE_HI {
            push(lo, hi);
        } else {
            if lo < SURROGATE_LO {
                push(lo, SURROGATE_LO - 1);
            }
            if hi > SURROGATE_HI {
                push(SURROGATE_HI + 1, hi);
            }
        }
    }
    out
}

/// Renders a disjoint range list as a single consuming `TransitionEvent`, using a
/// bare `Char` for a one-codepoint class (the common keyword/literal case).
fn ranges_to_event(ranges: &[(u32, u32)]) -> TransitionEvent {
    if let [(lo, hi)] = ranges {
        if lo == hi {
            if let Some(c) = char::from_u32(*lo) {
                return TransitionEvent::Char(c);
            }
        }
    }
    TransitionEvent::Chars(false, ranges_to_entries(ranges))
}

/// Partitions the consuming edges leaving a DFA state into disjoint character
/// classes. `edges` must be in **priority order** (closure order). Each returned
/// entry maps the *ordered, de-duplicated* list of NFA target states reached on a
/// class to the (merged) ranges that reach it, so the resulting transitions are
/// deterministic (a character lands in exactly one class) while preserving the
/// priority of overlapping edges (a higher-priority source's target comes first).
fn partition(edges: &[(Ranges, u32)]) -> Vec<(Closure, Ranges)> {
    // The sweep boundaries: every range start, and every range end + 1.
    let mut points: BTreeSet<u32> = BTreeSet::new();
    for (ranges, _) in edges {
        for (lo, hi) in ranges {
            points.insert(*lo);
            if *hi < MAX_CP {
                points.insert(hi + 1);
            }
        }
    }
    let points: Vec<u32> = points.into_iter().collect();

    // Group elementary intervals by their ordered target list. `index` maps a
    // target list to its slot in `groups` so identical lists merge; `groups` keeps
    // first-encounter order for deterministic output.
    let mut index: HashMap<Closure, usize> = HashMap::new();
    let mut groups: Vec<(Closure, Ranges)> = vec![];
    for (i, &lo) in points.iter().enumerate() {
        let hi = points.get(i + 1).map(|next| next - 1).unwrap_or(MAX_CP);
        // Every codepoint in [lo, hi] belongs to the same edges, so probing `lo`
        // decides membership for the whole elementary interval. Walk edges in
        // priority order, keeping the first occurrence of each target.
        let mut targets: Closure = vec![];
        for (ranges, target) in edges {
            if ranges.iter().any(|(a, b)| *a <= lo && lo <= *b) && !targets.contains(target) {
                targets.push(*target);
            }
        }
        if targets.is_empty() {
            continue;
        }
        match index.get(&targets) {
            Some(&slot) => groups[slot].1.push((lo, hi)),
            None => {
                index.insert(targets.clone(), groups.len());
                groups.push((targets, vec![(lo, hi)]));
            }
        }
    }

    for (_, ranges) in &mut groups {
        normalize(ranges);
    }
    groups
}

/// Interns ordered NFA-state closures to small, stable DFA state ids. The start
/// closure is interned first so it gets id 0 (the matcher always begins at state
/// 0). Two closures with the same states in a *different* order are distinct DFA
/// states, because the order is thread priority and can change the match.
struct Interner {
    ids: HashMap<Closure, u32>,
    next: u32,
}

impl Interner {
    fn intern(&mut self, set: &Closure) -> u32 {
        if let Some(id) = self.ids.get(set) {
            return *id;
        }
        let id = self.next;
        self.next += 1;
        self.ids.insert(set.clone(), id);
        id
    }
}

impl Dfa {
    /// Priority-preserving subset construction. Each DFA state is an
    /// [`ordered_closure`] of NFA states (highest priority first, truncated at the
    /// accepting state); consuming edges are partitioned into disjoint classes whose
    /// ordered targets are unioned, which makes alternation and shared-prefix groups
    /// (`a|ab`, `(a|ab)z`) deterministic *and* leftmost-first — the higher-priority
    /// branch's continuation comes first, so greedy/lazy quantifiers and alternation
    /// order match the `regex` crate.
    ///
    /// Zero-width assertions (`$`/`\z` and `\b`/`\B`) stay as their own transitions
    /// to a follow-on state, exactly as the generated matcher and the runtime
    /// interpreter expect: they are evaluated against the input position without
    /// consuming. The single NFA accepting state is collapsed to a transition-less
    /// sink (`final_state`); any DFA state whose closure contains it emits an `End`
    /// edge. Because the closure is truncated at the accept, any consuming edge still
    /// present is higher priority than that accept, so the matcher's "prefer to
    /// consume" rule is exactly leftmost-first (greedy keeps going; a lazy or
    /// preferred-empty path has no surviving consuming edge and accepts immediately).
    pub fn build(nfa: &Nfa) -> Self {
        let final_singleton: Closure = vec![nfa.final_state];
        let mut interner = Interner {
            ids: HashMap::new(),
            next: 0,
        };

        let start = ordered_closure(nfa, &[0], nfa.final_state);
        interner.intern(&start); // id 0
        let final_state = interner.intern(&final_singleton);

        let mut transitions: BTreeMap<u32, Vec<(TransitionEvent, u32)>> = BTreeMap::new();
        let mut processed: HashSet<u32> = HashSet::new();
        let mut worklist = vec![start];
        while let Some(set) = worklist.pop() {
            let id = interner.intern(&set);
            // The accepting closure is a transition-less sink; the rest are built once.
            if set == final_singleton || !processed.insert(id) {
                continue;
            }

            // Gather the non-epsilon edges leaving the closure, in priority order.
            let mut consuming: Vec<(Ranges, u32)> = vec![];
            let mut end_of_input: Closure = vec![];
            let mut end_of_line: Closure = vec![];
            let mut start_of_line: Closure = vec![];
            // Word-boundary follow-ons, keyed by `negate as usize | (unicode as usize) << 1`
            // so the ASCII and Unicode `\b`/`\B` variants stay distinct edges.
            let mut boundaries: [Closure; 4] = [vec![], vec![], vec![], vec![]];
            let push_unique = |targets: &mut Closure, target: u32| {
                if !targets.contains(&target) {
                    targets.push(target);
                }
            };
            for state in &set {
                let Some(state_transitions) = nfa.transitions.get(state) else { continue };
                for (event, target) in state_transitions {
                    match event {
                        TransitionEvent::Epsilon => {}
                        TransitionEvent::Char(_) | TransitionEvent::Chars(..) => consuming.push((event_ranges(event), *target)),
                        TransitionEvent::EndOfInput => push_unique(&mut end_of_input, *target),
                        TransitionEvent::EndOfLine => push_unique(&mut end_of_line, *target),
                        TransitionEvent::StartOfLine => push_unique(&mut start_of_line, *target),
                        TransitionEvent::WordBoundary { negate, unicode } => {
                            push_unique(&mut boundaries[*negate as usize | (*unicode as usize) << 1], *target)
                        }
                        // The NFA never stores an explicit `End` edge.
                        TransitionEvent::End => {}
                    }
                }
            }

            let mut out: Vec<(TransitionEvent, u32)> = vec![];
            let wire = |targets: Closure, interner: &mut Interner, worklist: &mut Vec<Closure>| -> u32 {
                let closed = ordered_closure(nfa, &targets, nfa.final_state);
                let target_id = interner.intern(&closed);
                worklist.push(closed);
                target_id
            };

            // Deterministic consuming transitions (ordered targets preserve priority).
            for (targets, ranges) in partition(&consuming) {
                let target_id = wire(targets, &mut interner, &mut worklist);
                out.push((ranges_to_event(&ranges), target_id));
            }
            // Zero-width assertions: each kind collapses to one follow-on state. The
            // emission order here is the priority the matcher loop reads back (both
            // engines iterate transitions in this stored order).
            if !end_of_input.is_empty() {
                let target_id = wire(end_of_input, &mut interner, &mut worklist);
                out.push((TransitionEvent::EndOfInput, target_id));
            }
            if !end_of_line.is_empty() {
                let target_id = wire(end_of_line, &mut interner, &mut worklist);
                out.push((TransitionEvent::EndOfLine, target_id));
            }
            if !start_of_line.is_empty() {
                let target_id = wire(start_of_line, &mut interner, &mut worklist);
                out.push((TransitionEvent::StartOfLine, target_id));
            }
            for unicode in [false, true] {
                for negate in [false, true] {
                    let targets = std::mem::take(&mut boundaries[negate as usize | (unicode as usize) << 1]);
                    if !targets.is_empty() {
                        let target_id = wire(targets, &mut interner, &mut worklist);
                        out.push((TransitionEvent::WordBoundary { negate, unicode }, target_id));
                    }
                }
            }
            // Accepting closure: offer an `End` edge to the sink. Truncation
            // guarantees any consuming edge above outranks this accept, so trying
            // them first is leftmost-first (greedy); a lazy/empty-preferred state has
            // no surviving consuming edge and accepts here immediately.
            if set.contains(&nfa.final_state) {
                out.push((TransitionEvent::End, final_state));
            }

            transitions.insert(id, out);
        }

        Self {
            transitions,
            final_state,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::SimpleRegexAst;

    use super::*;

    fn build(pattern: &str) -> Dfa {
        Dfa::build(&Nfa::build(&SimpleRegexAst::parse(pattern).expect("valid pattern")))
    }

    /// Every state a transition can reach must either be the final (sink) state or
    /// itself have an entry in the transition table; otherwise the generated
    /// matcher would land on a state with no arms.
    #[track_caller]
    fn assert_total(dfa: &Dfa) {
        assert!(dfa.transitions.contains_key(&0), "start state must be present");
        for transitions in dfa.transitions.values() {
            for (_, target) in transitions {
                assert!(
                    *target == dfa.final_state || dfa.transitions.contains_key(target),
                    "state {target} is reachable but has no transition entry",
                );
            }
        }
    }

    /// After subset construction the consuming classes of a state must be disjoint:
    /// no codepoint may be accepted by two different arms (that would make the
    /// generated `match` non-deterministic).
    #[track_caller]
    fn assert_disjoint_consuming(dfa: &Dfa) {
        for (state, transitions) in &dfa.transitions {
            let mut ranges: Vec<(u32, u32)> = vec![];
            for (event, _) in transitions {
                if matches!(event, TransitionEvent::Char(_) | TransitionEvent::Chars(..)) {
                    ranges.extend(event_ranges(event));
                }
            }
            let mut sorted = ranges.clone();
            sorted.sort_unstable();
            for pair in sorted.windows(2) {
                assert!(pair[0].1 < pair[1].0, "state {state} has overlapping consuming classes: {ranges:?}");
            }
        }
    }

    #[test]
    fn final_state_is_a_sink() {
        let dfa = build("abc");
        assert!(!dfa.transitions.contains_key(&dfa.final_state), "final state has no transitions");
    }

    #[test]
    fn ident_dfa_is_total_and_disjoint() {
        let dfa = build("[a-z][a-zA-Z0-9_]*");
        assert_total(&dfa);
        assert_disjoint_consuming(&dfa);
    }

    #[test]
    fn block_comment_dfa_is_total_and_disjoint() {
        // `/\*.*\*/` mixes literals, `.`, and a star — the trickiest construction.
        let dfa = build("/\\*.*\\*/");
        assert_total(&dfa);
        assert_disjoint_consuming(&dfa);
    }

    #[test]
    fn overlapping_class_and_exit_char_is_merged() {
        // In `[a-z]*x` the start state can both loop on `[a-z]` and exit on `x`,
        // which the class covers. Subset construction merges these into disjoint
        // classes (`x` vs the rest of `[a-z]`) with unioned targets.
        let dfa = build("[a-z]*x");
        assert_total(&dfa);
        assert_disjoint_consuming(&dfa);
    }

    fn consuming_edges(state: &[(TransitionEvent, u32)]) -> usize {
        state
            .iter()
            .filter(|(e, _)| matches!(e, TransitionEvent::Char(_) | TransitionEvent::Chars(..)))
            .count()
    }

    fn has_end(state: &[(TransitionEvent, u32)]) -> bool {
        state.iter().any(|(e, _)| matches!(e, TransitionEvent::End))
    }

    #[test]
    fn lazy_star_accepts_without_consuming() {
        // `a*?` truncates the consuming thread behind the higher-priority accept, so
        // the start state accepts immediately and offers no consuming edge.
        let dfa = build("a*?");
        let start = &dfa.transitions[&0];
        assert!(has_end(start), "lazy start accepts");
        assert_eq!(consuming_edges(start), 0, "lazy start has no surviving consuming edge");
        // The greedy form keeps the consuming edge (and also accepts).
        let greedy = build("a*");
        let gstart = &greedy.transitions[&0];
        assert!(has_end(gstart));
        assert_eq!(consuming_edges(gstart), 1, "greedy start still consumes");
    }

    #[test]
    fn leftmost_first_alternation_cuts_the_longer_branch() {
        // `a|ab`: consuming `a` lands in an accepting state with no `b` edge — the
        // second branch's continuation is cut once the first branch matched.
        let dfa = build("a|ab");
        let (_, after_a) = dfa.transitions[&0]
            .iter()
            .find(|(e, _)| matches!(e, TransitionEvent::Char('a')))
            .expect("start consumes a");
        let state = &dfa.transitions[after_a];
        assert!(has_end(state), "first branch accepts after `a`");
        assert_eq!(consuming_edges(state), 0, "the `b` of the second branch was cut");
    }

    #[test]
    fn shared_prefix_alternation_is_total_and_disjoint() {
        // `a|ab` and `(a|ab)z` are exactly the cases a non-merging construction got
        // wrong: two branches consuming the same `a` need their targets unioned.
        for pattern in ["a|ab", "(a|ab)z", "(foo|bar|baz)+"] {
            let dfa = build(pattern);
            assert_total(&dfa);
            assert_disjoint_consuming(&dfa);
        }
    }
}
