//! Build-time Unicode table lookups, delegated to `regex-syntax` (the `regex`
//! crate's own parser) so this engine neither vendors nor hand-rolls Unicode data.
//!
//! Everything here runs at macro-expansion / build time. The output is always a
//! list of [`GroupEntry`] codepoint ranges, which the rest of the pipeline already
//! handles for arbitrary scalar values — so a `\p{...}` class compiles to the same
//! plain range checks as `[a-z]`, with no Unicode dependency in the generated
//! matcher or the runtime crate.

use regex_syntax::hir::{Class, ClassUnicode, ClassUnicodeRange, HirKind};

use super::GroupEntry;

/// One inclusive codepoint range as a [`GroupEntry`], collapsing a single-codepoint
/// range to a `Char` (the common case).
fn entry_for(start: char, end: char) -> GroupEntry {
    if start == end { GroupEntry::Char(start) } else { GroupEntry::Range(start, end) }
}

/// Build a `regex-syntax` Unicode class from our [`GroupEntry`] list (overlapping /
/// unsorted ranges are fine — `ClassUnicode::new` canonicalises them).
fn class_from(entries: &[GroupEntry]) -> ClassUnicode {
    ClassUnicode::new(entries.iter().map(|entry| {
        let (lo, hi) = match entry {
            GroupEntry::Char(c) => (*c, *c),
            GroupEntry::Range(a, b) => (*a, *b),
        };
        ClassUnicodeRange::new(lo, hi)
    }))
}

/// Render a `regex-syntax` Unicode class back as our [`GroupEntry`] list.
fn entries_of(class: &ClassUnicode) -> Vec<GroupEntry> {
    class.ranges().iter().map(|range| entry_for(range.start(), range.end())).collect()
}

/// Resolve a Unicode property class to a sorted, disjoint list of codepoint ranges
/// as [`GroupEntry`]s. `body` is the text the user wrote between the braces of
/// `\p{...}` (or the single letter of the shorthand `\pL`); `negated` selects the
/// `\P{...}` complement.
///
/// We ask `regex-syntax` to translate the class and read back its ranges. When
/// `negated`, we let `regex-syntax` compute the complement so the result is always
/// a *positive* set of entries — that way `\P{...}` works both at top level and
/// unioned inside a `[...]` class without the flat group model needing to represent
/// a negated subset.
///
/// Returns `None` if the property name is unknown (so the caller rejects the whole
/// pattern with a `compile_error!`, rather than silently mis-parsing).
pub fn property_entries(body: &str, negated: bool) -> Option<Vec<GroupEntry>> {
    // Reconstruct the escape and let `regex-syntax` do the lookup + complement.
    let sigil = if negated { 'P' } else { 'p' };
    let pattern = format!(r"\{sigil}{{{body}}}");
    let hir = regex_syntax::parse(&pattern).ok()?;
    match hir.into_kind() {
        HirKind::Class(Class::Unicode(class)) => Some(class.iter().map(|range| entry_for(range.start(), range.end())).collect()),
        // `regex-syntax` folds a property that resolves to a *single* codepoint
        // (e.g. `\p{Line_Separator}` = U+2028) into a `Literal` of its UTF-8 bytes.
        HirKind::Literal(lit) => std::str::from_utf8(&lit.0).ok().map(|s| s.chars().map(GroupEntry::Char).collect()),
        // Any other shape is not a property class.
        _ => None,
    }
}

/// Expand a set of class entries with their Unicode "simple" case-folded
/// equivalents (the original members are kept). For example `[a-z]` also gains
/// `A-Z` — plus the Unicode cases ASCII folding misses, like the Kelvin sign
/// (U+212A) folding to `k` and the long s (U+017F) to `s`. Used for `(?i)` under
/// Unicode mode; the ASCII path stays in `parse.rs`'s `fold_entry`.
///
/// Delegates to `regex-syntax`'s case-fold tables. If those tables are somehow
/// unavailable (the `unicode-case` feature off), the entries are returned
/// unchanged rather than panicking.
pub fn case_fold(entries: &[GroupEntry]) -> Vec<GroupEntry> {
    let mut class = class_from(entries);
    if class.try_case_fold_simple().is_err() {
        return entries.to_vec();
    }
    entries_of(&class)
}

/// The complement of a class over the whole scalar-value space, as a *positive*
/// entry list. Lets a negated shorthand inside a class (`[\D]`, `[a\W]`) be unioned
/// into the flat group model — the complement is materialised as ordinary ranges
/// rather than needing the group to carry a negated subset.
pub fn negate(entries: &[GroupEntry]) -> Vec<GroupEntry> {
    let mut class = class_from(entries);
    class.negate();
    entries_of(&class)
}

/// Resolve a Perl shorthand class (`\d \w \s`) to its *positive* Unicode codepoint
/// ranges via `regex-syntax`, used under Unicode mode in place of the ASCII sets.
/// `escape` is the lowercase base letter; the caller handles the negated `\D \W \S`
/// forms by inverting. Returns `None` only if the tables are unavailable.
pub fn perl_class(escape: char) -> Option<Vec<GroupEntry>> {
    let pattern = format!(r"\{escape}");
    match regex_syntax::parse(&pattern).ok()?.into_kind() {
        HirKind::Class(Class::Unicode(class)) => Some(entries_of(&class)),
        _ => None,
    }
}

/// The Unicode `\w` word-character set as sorted, disjoint `(lo, hi)` codepoint
/// ranges. Used to evaluate Unicode `\b`/`\B` word-ness: the interpreter binary-
/// searches the cached table ([`is_word`]), and the code generator embeds the same
/// ranges as a `const` so the generated matcher needs no runtime dependency.
pub fn word_ranges() -> Vec<(char, char)> {
    perl_class('w')
        .unwrap_or_default()
        .into_iter()
        .map(|entry| match entry {
            GroupEntry::Char(c) => (c, c),
            GroupEntry::Range(lo, hi) => (lo, hi),
        })
        .collect()
}

/// Whether `ch` is a Unicode `\w` word character (used by the runtime interpreter's
/// Unicode `\b`). Binary-searches a once-computed copy of [`word_ranges`].
pub fn is_word(ch: char) -> bool {
    use std::sync::OnceLock;
    static RANGES: OnceLock<Vec<(char, char)>> = OnceLock::new();
    RANGES
        .get_or_init(word_ranges)
        .binary_search_by(|&(lo, hi)| {
            if ch < lo {
                std::cmp::Ordering::Greater
            } else if ch > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_a_script_property() {
        // Greek includes the lowercase block; the entries are real codepoint ranges.
        let entries = property_entries("Greek", false).expect("Greek is a known script");
        let contains = |c: char| {
            entries.iter().any(|e| match e {
                GroupEntry::Char(g) => *g == c,
                GroupEntry::Range(lo, hi) => *lo <= c && c <= *hi,
            })
        };
        assert!(contains('λ'));
        assert!(!contains('a'));
    }

    #[test]
    fn negation_is_a_positive_complement() {
        // `\P{L}` (not-a-letter) must include ASCII digits and exclude letters.
        let entries = property_entries("L", true).expect("L is a known category");
        let contains = |c: char| {
            entries.iter().any(|e| match e {
                GroupEntry::Char(g) => *g == c,
                GroupEntry::Range(lo, hi) => *lo <= c && c <= *hi,
            })
        };
        assert!(contains('5'));
        assert!(!contains('a'));
    }

    #[test]
    fn single_letter_shorthand_and_unknown() {
        assert!(property_entries("L", false).is_some());
        assert!(property_entries("NotARealProperty", false).is_none());
    }

    #[test]
    fn case_fold_adds_unicode_equivalents() {
        let contains = |entries: &[GroupEntry], c: char| {
            entries.iter().any(|e| match e {
                GroupEntry::Char(g) => *g == c,
                GroupEntry::Range(lo, hi) => *lo <= c && c <= *hi,
            })
        };
        // `k` folds to its uppercase *and* the Kelvin sign (U+212A).
        let k = case_fold(&[GroupEntry::Char('k')]);
        assert!(contains(&k, 'K'));
        assert!(contains(&k, '\u{212A}'));
        // A Greek range gains its uppercase forms.
        let greek = case_fold(&[GroupEntry::Range('α', 'ω')]);
        assert!(contains(&greek, 'Λ'));
    }
}
