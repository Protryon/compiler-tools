//! A `regex`-crate-shaped search API layered over the engine's anchored prefix
//! matcher.
//!
//! Both runtime engines expose the same primitive — an *anchored* prefix match
//! ([`Regex::find_prefix`](crate::Regex::find_prefix) and
//! [`JitRegex::find_prefix`](crate::JitRegex::find_prefix), under the `jit`
//! feature): given a slice and the char before it, return `(matched, remaining)`
//! if the regex matches a prefix. That's the leftmost-first building block; what
//! the `regex` crate's `Regex` adds on top is *search orchestration* — trying
//! successive start positions, iterating non-overlapping matches, replacing, and
//! splitting.
//!
//! [`RegexSearch`] is that orchestration, written once as default methods over a
//! single required [`find_prefix`](RegexSearch::find_prefix) and implemented for
//! every engine, so `Regex` (the interpreter) and `JitRegex` (the Cranelift
//! JIT) share one API. Bring the trait into scope to call [`is_match`],
//! [`find`], [`find_iter`], [`replace`]/[`replace_all`], [`split`], etc.
//!
//! [`is_match`]: RegexSearch::is_match
//! [`find`]: RegexSearch::find
//! [`find_iter`]: RegexSearch::find_iter
//! [`replace`]: RegexSearch::replace
//! [`replace_all`]: RegexSearch::replace_all
//! [`split`]: RegexSearch::split
//!
//! # What's missing (needs engine work)
//!
//! Capture groups — [`Regex::captures`](https://docs.rs/regex)/`captures_iter`, a
//! `Captures` type, and `$1`/`$name` expansion in replacements — are *not* here.
//! This engine never tracks capture-span positions (every `(...)`, `(?:...)` and
//! `(?P<n>...)` lowers to the same [`Atom::Alternation`](crate::Atom::Alternation)
//! and the spans are discarded), so the replacement helpers take a literal string
//! rather than a `Replacer`. See `REGEX_PARITY.md` for the engine work that gap
//! needs.

use std::borrow::Cow;

/// A single non-overlapping match within a haystack: a byte range plus the
/// haystack it indexes, mirroring `regex::Match`.
#[derive(Debug, Clone, Copy)]
pub struct Match<'h> {
    haystack: &'h str,
    start: usize,
    end: usize,
}

impl<'h> Match<'h> {
    /// The byte offset of the start of the match.
    pub fn start(&self) -> usize {
        self.start
    }

    /// The byte offset just past the end of the match.
    pub fn end(&self) -> usize {
        self.end
    }

    /// The match as a byte range, `start..end`.
    pub fn range(&self) -> std::ops::Range<usize> {
        self.start..self.end
    }

    /// The matched text.
    pub fn as_str(&self) -> &'h str {
        &self.haystack[self.start..self.end]
    }

    /// Whether the match is empty (a zero-width match, e.g. from `a*` or `\b`).
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// The length of the match in bytes.
    pub fn len(&self) -> usize {
        self.end - self.start
    }
}

impl<'h> From<Match<'h>> for &'h str {
    fn from(m: Match<'h>) -> &'h str {
        m.as_str()
    }
}

impl From<Match<'_>> for std::ops::Range<usize> {
    fn from(m: Match<'_>) -> std::ops::Range<usize> {
        m.range()
    }
}

/// The `regex`-crate-shaped search interface over an anchored prefix matcher.
///
/// Implementors supply one method — [`find_prefix`](Self::find_prefix), the
/// anchored leftmost-first prefix match the engines already expose — and get the
/// search/replace/split family for free. The search is leftmost-first and
/// non-overlapping, matching the `regex` crate's default `find`/`find_iter`
/// semantics (including the empty-match adjacency rule in [`find_iter`]).
///
/// [`find_iter`]: Self::find_iter
pub trait RegexSearch {
    /// Anchored prefix match: if the regex matches a prefix of `from`, return
    /// `(matched, remaining)`; otherwise `None`. `prev` is the char immediately
    /// before `from` in the larger haystack (`None` at the start of text), which
    /// seeds the zero-width assertions (`^`, `\b`, `(?m)` line anchors) so a slice
    /// taken mid-haystack still sees the right preceding context.
    ///
    /// This is the single primitive the rest of the trait builds on; the engines
    /// implement it by forwarding to their inherent `find_prefix`.
    fn find_prefix<'h>(&self, from: &'h str, prev: Option<char>) -> Option<(&'h str, &'h str)>;

    /// Whether the regex matches anywhere in `haystack`.
    fn is_match(&self, haystack: &str) -> bool {
        self.find(haystack).is_some()
    }

    /// Whether the regex matches anywhere in `haystack` at or after byte offset
    /// `start`. `start` must be a char boundary's lower bound; non-boundary offsets
    /// are skipped, as in [`find_at`](Self::find_at).
    fn is_match_at(&self, haystack: &str, start: usize) -> bool {
        self.find_at(haystack, start).is_some()
    }

    /// The leftmost match in `haystack`, or `None`. Equivalent to
    /// [`find_at`](Self::find_at) with `start == 0`.
    fn find<'h>(&self, haystack: &'h str) -> Option<Match<'h>> {
        self.find_at(haystack, 0)
    }

    /// The leftmost match in `haystack` starting at or after byte offset `start`.
    ///
    /// Walks successive start positions, evaluating the anchored
    /// [`find_prefix`](Self::find_prefix) at each (with the correct preceding char
    /// so `^`/`\b`/`(?m)` behave) and returning the first that matches — the same
    /// leftmost search the `regex` crate performs.
    fn find_at<'h>(&self, haystack: &'h str, start: usize) -> Option<Match<'h>> {
        let mut pos = start;
        while pos <= haystack.len() {
            if !haystack.is_char_boundary(pos) {
                pos += 1;
                continue;
            }
            // The char immediately before `pos`, so the matcher's zero-width
            // assertions see the right context rather than start-of-text everywhere.
            let prev = haystack[..pos].chars().next_back();
            if let Some((matched, _)) = self.find_prefix(&haystack[pos..], prev) {
                return Some(Match {
                    haystack,
                    start: pos,
                    end: pos + matched.len(),
                });
            }
            pos += 1;
        }
        None
    }

    /// An iterator over all non-overlapping leftmost matches in `haystack`.
    ///
    /// Like `regex::Regex::find_iter`, it suppresses an empty match immediately
    /// adjacent to the end of the previous match, so e.g. `a*` over `"aba"` yields
    /// the two `a` runs (and the trailing empty match) rather than a degenerate
    /// stream of empties.
    fn find_iter<'r, 'h>(&'r self, haystack: &'h str) -> Matches<'r, 'h, Self>
    where
        Self: Sized,
    {
        Matches {
            re: self,
            haystack,
            last_end: 0,
            last_match: None,
        }
    }

    /// Replace the leftmost match of the regex in `haystack` with `rep`, returning
    /// a [`Cow`] that borrows `haystack` untouched when there is no match.
    ///
    /// `rep` is inserted literally — unlike the `regex` crate, `$1`/`$name`
    /// references are *not* expanded, because this engine does not track capture
    /// groups (see the module docs).
    fn replace<'h>(&self, haystack: &'h str, rep: &str) -> Cow<'h, str>
    where
        Self: Sized,
    {
        self.replacen(haystack, 1, rep)
    }

    /// Replace all non-overlapping matches in `haystack` with `rep`. See
    /// [`replace`](Self::replace) for the literal-replacement caveat.
    fn replace_all<'h>(&self, haystack: &'h str, rep: &str) -> Cow<'h, str>
    where
        Self: Sized,
    {
        self.replacen(haystack, 0, rep)
    }

    /// Replace the first `limit` non-overlapping matches in `haystack` with `rep`
    /// (a `limit` of `0` means replace all), mirroring `regex::Regex::replacen`.
    /// See [`replace`](Self::replace) for the literal-replacement caveat.
    fn replacen<'h>(&self, haystack: &'h str, limit: usize, rep: &str) -> Cow<'h, str>
    where
        Self: Sized,
    {
        let mut out: Option<String> = None;
        let mut last = 0;
        for (i, m) in self.find_iter(haystack).enumerate() {
            if limit != 0 && i >= limit {
                break;
            }
            let buf = out.get_or_insert_with(|| String::with_capacity(haystack.len()));
            buf.push_str(&haystack[last..m.start()]);
            buf.push_str(rep);
            last = m.end();
        }
        match out {
            None => Cow::Borrowed(haystack),
            Some(mut buf) => {
                buf.push_str(&haystack[last..]);
                Cow::Owned(buf)
            }
        }
    }

    /// An iterator over the substrings of `haystack` delimited by matches of the
    /// regex, like `regex::Regex::split`.
    fn split<'r, 'h>(&'r self, haystack: &'h str) -> Split<'r, 'h, Self>
    where
        Self: Sized,
    {
        Split {
            finder: self.find_iter(haystack),
            haystack,
            last: 0,
            done: false,
        }
    }

    /// Like [`split`](Self::split) but yields at most `limit` substrings; the last
    /// one is the unsplit remainder of `haystack`. A `limit` of `0` yields nothing.
    fn splitn<'r, 'h>(&'r self, haystack: &'h str, limit: usize) -> SplitN<'r, 'h, Self>
    where
        Self: Sized,
    {
        SplitN {
            splits: self.split(haystack),
            limit,
        }
    }
}

/// Iterator over non-overlapping matches, returned by [`RegexSearch::find_iter`].
pub struct Matches<'r, 'h, M: ?Sized> {
    re: &'r M,
    haystack: &'h str,
    last_end: usize,
    last_match: Option<usize>,
}

impl<'h, M: RegexSearch> Iterator for Matches<'_, 'h, M> {
    type Item = Match<'h>;

    fn next(&mut self) -> Option<Match<'h>> {
        loop {
            if self.last_end > self.haystack.len() {
                return None;
            }
            let m = self.re.find_at(self.haystack, self.last_end)?;
            if m.is_empty() {
                // Empty match: advance past it so the search makes progress, and
                // drop it if it sits exactly where the previous match ended (the
                // `regex` crate's empty-match-adjacency rule).
                self.last_end = next_char_boundary(self.haystack, m.end);
                if Some(m.end) == self.last_match {
                    continue;
                }
            } else {
                self.last_end = m.end;
            }
            self.last_match = Some(m.end);
            return Some(m);
        }
    }
}

/// Iterator over the substrings between matches, returned by [`RegexSearch::split`].
pub struct Split<'r, 'h, M: ?Sized> {
    finder: Matches<'r, 'h, M>,
    haystack: &'h str,
    last: usize,
    done: bool,
}

impl<'h, M: RegexSearch> Iterator for Split<'_, 'h, M> {
    type Item = &'h str;

    fn next(&mut self) -> Option<&'h str> {
        if self.done {
            return None;
        }
        match self.finder.next() {
            None => {
                self.done = true;
                Some(&self.haystack[self.last..])
            }
            Some(m) => {
                let piece = &self.haystack[self.last..m.start()];
                self.last = m.end();
                Some(piece)
            }
        }
    }
}

/// Iterator returned by [`RegexSearch::splitn`]: at most `limit` substrings.
pub struct SplitN<'r, 'h, M: ?Sized> {
    splits: Split<'r, 'h, M>,
    limit: usize,
}

impl<'h, M: RegexSearch> Iterator for SplitN<'_, 'h, M> {
    type Item = &'h str;

    fn next(&mut self) -> Option<&'h str> {
        if self.limit == 0 {
            return None;
        }
        self.limit -= 1;
        if self.limit == 0 {
            // Last allowed piece: the whole unsplit remainder from here on.
            self.splits.done = true;
            let rest = &self.splits.haystack[self.splits.last..];
            Some(rest)
        } else {
            self.splits.next()
        }
    }
}

/// The next char boundary strictly after `pos` (used to advance past an empty match).
fn next_char_boundary(haystack: &str, mut pos: usize) -> usize {
    pos += 1;
    while pos < haystack.len() && !haystack.is_char_boundary(pos) {
        pos += 1;
    }
    pos
}

impl RegexSearch for crate::Regex {
    fn find_prefix<'h>(&self, from: &'h str, prev: Option<char>) -> Option<(&'h str, &'h str)> {
        // Forward to the inherent interpreter; fully-qualified so this never recurses.
        crate::Regex::find_prefix(self, from, prev)
    }
}

#[cfg(feature = "jit")]
impl RegexSearch for crate::JitRegex {
    fn find_prefix<'h>(&self, from: &'h str, prev: Option<char>) -> Option<(&'h str, &'h str)> {
        crate::JitRegex::find_prefix(self, from, prev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Regex;

    fn re(pattern: &str) -> Regex {
        Regex::parse(pattern).expect("valid pattern")
    }

    #[test]
    fn is_match_searches_unanchored() {
        let re = re("[0-9]+");
        assert!(re.is_match("abc123"));
        assert!(!re.is_match("abc"));
    }

    #[test]
    fn find_reports_leftmost_span() {
        let re = re("[0-9]+");
        let m = re.find("abc123def456").unwrap();
        assert_eq!(m.range(), 3..6);
        assert_eq!(m.as_str(), "123");
    }

    #[test]
    fn find_at_respects_start_offset() {
        let re = re("[0-9]+");
        let m = re.find_at("12ab34", 2).unwrap();
        assert_eq!(m.as_str(), "34");
        assert_eq!(m.start(), 4);
    }

    #[test]
    fn find_iter_collects_all_non_overlapping() {
        let re = re("[0-9]+");
        let nums: Vec<_> = re.find_iter("a1bb22ccc333").map(|m| m.as_str()).collect();
        assert_eq!(nums, ["1", "22", "333"]);
    }

    #[test]
    fn find_iter_handles_empty_matches() {
        // `a*` matches the two runs of `a`; the empty matches that would land right
        // where a previous match ended (at offsets 1 and 3) are suppressed, matching
        // the `regex` crate's empty-match-adjacency rule.
        let star = re("a*");
        let spans: Vec<_> = star.find_iter("aba").map(|m| (m.start(), m.end())).collect();
        assert_eq!(spans, [(0, 1), (2, 3)]);

        // A pattern that matches empty at some positions still makes progress and
        // terminates, interleaving empty and non-empty matches.
        let empties: Vec<_> = re("b*").find_iter("ab").map(|m| (m.start(), m.end())).collect();
        assert_eq!(empties, [(0, 0), (1, 2)]);
    }

    #[test]
    fn replace_first_only() {
        let re = re("[0-9]+");
        assert_eq!(re.replace("a1b2c3", "#"), "a#b2c3");
    }

    #[test]
    fn replace_all_replaces_every_match() {
        let re = re("[0-9]+");
        assert_eq!(re.replace_all("a1b2c3", "#"), "a#b#c#");
    }

    #[test]
    fn replacen_limits_replacements() {
        let re = re("[0-9]+");
        assert_eq!(re.replacen("a1b2c3", 2, "#"), "a#b#c3");
        // limit 0 means replace all.
        assert_eq!(re.replacen("a1b2c3", 0, "#"), "a#b#c#");
    }

    #[test]
    fn replace_borrows_when_no_match() {
        let re = re("[0-9]+");
        assert!(matches!(re.replace_all("abc", "#"), Cow::Borrowed("abc")));
    }

    #[test]
    fn split_on_matches() {
        let re = re("[, ]+");
        let parts: Vec<_> = re.split("a, b,  c").collect();
        assert_eq!(parts, ["a", "b", "c"]);
    }

    #[test]
    fn splitn_caps_the_pieces() {
        let re = re("[, ]+");
        let parts: Vec<_> = re.splitn("a, b,  c", 2).collect();
        assert_eq!(parts, ["a", "b,  c"]);
    }
}
