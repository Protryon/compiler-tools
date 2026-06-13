use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::GroupEntry;
use super::nfa::{Nfa, TransitionEvent};

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

/// Epsilon-closure of a set of NFA states: every state reachable by following
/// only `Epsilon` edges. The result is the canonical key for a DFA state.
fn epsilon_closure(nfa: &Nfa, seed: impl IntoIterator<Item = u32>) -> BTreeSet<u32> {
    let mut set = BTreeSet::new();
    let mut stack: Vec<u32> = seed.into_iter().collect();
    while let Some(state) = stack.pop() {
        if !set.insert(state) {
            continue;
        }
        if let Some(transitions) = nfa.transitions.get(&state) {
            for (event, target) in transitions {
                if matches!(event, TransitionEvent::Epsilon) && !set.contains(target) {
                    stack.push(*target);
                }
            }
        }
    }
    set
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
/// classes. Each returned entry maps the set of NFA target states reached on that
/// class to the (merged) ranges that reach it, so the resulting transitions are
/// truly deterministic — a single character lands in exactly one class.
fn partition(edges: &[(Ranges, u32)]) -> Vec<(BTreeSet<u32>, Ranges)> {
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

    let mut groups: HashMap<BTreeSet<u32>, Vec<(u32, u32)>> = HashMap::new();
    for (i, &lo) in points.iter().enumerate() {
        let hi = points.get(i + 1).map(|next| next - 1).unwrap_or(MAX_CP);
        // Every codepoint in [lo, hi] belongs to the same set of edges, so probing
        // `lo` is enough to decide membership for the whole elementary interval.
        let targets: BTreeSet<u32> = edges
            .iter()
            .filter(|(ranges, _)| ranges.iter().any(|(a, b)| *a <= lo && lo <= *b))
            .map(|(_, target)| *target)
            .collect();
        if !targets.is_empty() {
            groups.entry(targets).or_default().push((lo, hi));
        }
    }

    groups
        .into_iter()
        .map(|(targets, mut ranges)| {
            normalize(&mut ranges);
            (targets, ranges)
        })
        .collect()
}

/// Interns NFA-state sets to small, stable DFA state ids. The start set is
/// interned first so it gets id 0 (the matcher always begins at state 0).
struct Interner {
    ids: HashMap<BTreeSet<u32>, u32>,
    next: u32,
}

impl Interner {
    fn intern(&mut self, set: &BTreeSet<u32>) -> u32 {
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
    /// Classic subset construction. Each DFA state is the epsilon-closure of a set
    /// of NFA states; consuming edges are partitioned into disjoint classes whose
    /// targets are unioned, which is what makes alternation and shared-prefix
    /// groups (`a|ab`, `(a|ab)z`) match correctly without backtracking.
    ///
    /// Zero-width assertions (`$`/`\z` and `\b`/`\B`) stay as their own transitions
    /// to a follow-on state, exactly as the generated matcher and the runtime
    /// interpreter expect: they are evaluated against the input position without
    /// consuming. The single NFA accepting state is collapsed to a transition-less
    /// sink (`final_state`); any DFA state whose set contains it emits an `End`
    /// edge so the matcher can accept there while still preferring to consume
    /// (greedy / leftmost-longest).
    pub fn build(nfa: &Nfa) -> Self {
        let final_singleton: BTreeSet<u32> = std::iter::once(nfa.final_state).collect();
        let mut interner = Interner {
            ids: HashMap::new(),
            next: 0,
        };

        let start = epsilon_closure(nfa, [0]);
        interner.intern(&start); // id 0
        let final_state = interner.intern(&final_singleton);

        let mut transitions: BTreeMap<u32, Vec<(TransitionEvent, u32)>> = BTreeMap::new();
        let mut processed: HashSet<u32> = HashSet::new();
        let mut worklist = vec![start];
        while let Some(set) = worklist.pop() {
            let id = interner.intern(&set);
            // The accepting set is a transition-less sink; the rest are built once.
            if set == final_singleton || !processed.insert(id) {
                continue;
            }

            // Gather the non-epsilon edges leaving the closure.
            let mut consuming: Vec<(Ranges, u32)> = vec![];
            let mut end_of_input: BTreeSet<u32> = BTreeSet::new();
            let mut boundaries: [BTreeSet<u32>; 2] = [BTreeSet::new(), BTreeSet::new()];
            for state in &set {
                let Some(state_transitions) = nfa.transitions.get(state) else { continue };
                for (event, target) in state_transitions {
                    match event {
                        TransitionEvent::Epsilon => {}
                        TransitionEvent::Char(_) | TransitionEvent::Chars(..) => consuming.push((event_ranges(event), *target)),
                        TransitionEvent::EndOfInput => {
                            end_of_input.insert(*target);
                        }
                        TransitionEvent::WordBoundary(negate) => {
                            boundaries[*negate as usize].insert(*target);
                        }
                        // The NFA never stores an explicit `End` edge.
                        TransitionEvent::End => {}
                    }
                }
            }

            let mut out: Vec<(TransitionEvent, u32)> = vec![];
            let wire = |targets: BTreeSet<u32>, interner: &mut Interner, worklist: &mut Vec<BTreeSet<u32>>| -> u32 {
                let closed = epsilon_closure(nfa, targets);
                let target_id = interner.intern(&closed);
                worklist.push(closed);
                target_id
            };

            // Deterministic consuming transitions.
            for (targets, ranges) in partition(&consuming) {
                let target_id = wire(targets, &mut interner, &mut worklist);
                out.push((ranges_to_event(&ranges), target_id));
            }
            // Zero-width assertions: each kind collapses to one follow-on state.
            if !end_of_input.is_empty() {
                let target_id = wire(end_of_input, &mut interner, &mut worklist);
                out.push((TransitionEvent::EndOfInput, target_id));
            }
            for negate in [false, true] {
                let targets = std::mem::take(&mut boundaries[negate as usize]);
                if !targets.is_empty() {
                    let target_id = wire(targets, &mut interner, &mut worklist);
                    out.push((TransitionEvent::WordBoundary(negate), target_id));
                }
            }
            // Accepting state: offer an `End` edge to the sink (greedy: consuming
            // edges above are tried first).
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
