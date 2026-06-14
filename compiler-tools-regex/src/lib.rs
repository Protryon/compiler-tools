//! The compile-time "simple-regex" engine that backs `#[token(regex = "...")]`.
//!
//! It compiles a small regex dialect into a branch-only DFA and can both
//! interpret it at runtime ([`SimpleRegex::find_prefix`]) and emit a self-contained
//! Rust matcher from it ([`SimpleRegex::generate_parser`]). The proc-macro crate
//! (`compiler-tools-derive`) consumes the latter; the conformance test crate
//! exercises both against the upstream `regex` test corpus.

use proc_macro2::{Ident, TokenStream};
use quote::{ToTokens, TokenStreamExt, quote};

use self::{dfa::Dfa, nfa::Nfa};

mod dfa;
mod generate;
#[cfg(feature = "jit")]
mod jit;
mod matching;
mod nfa;
mod parse;
mod unicode;

#[cfg(feature = "jit")]
pub use jit::{JitError, JitRegex};

/// Collect an iterator of token-producing values into one [`TokenStream`].
///
/// Shared by the macro crate and this engine's code generation; lives here so
/// the engine has no dependency back on the proc-macro crate.
pub fn flatten<S: ToTokens, T: IntoIterator<Item = S>>(iter: T) -> TokenStream {
    let mut out = quote! {};
    out.append_all(iter);
    out
}

#[derive(Debug, Clone, Copy)]
pub enum Repeat {
    Once,
    ZeroOrOnce,
    OnceOrMore,
    ZeroOrMore,
}

// TODO: we should support classes (i.e. unicode ident_start)
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy)]
pub enum GroupEntry {
    Char(char),
    Range(char, char),
}

/// The variety of word-boundary assertion. Plain `\b`/`\B` test both sides; the four
/// directional forms (from the `\b{...}` syntax the `regex` crate accepts) constrain
/// one or both sides. Each maps to a boolean condition over the word-ness of the
/// preceding and following characters (input edges count as non-word) — see [`Self::holds`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WordBoundaryKind {
    /// `\b`: the two sides have differing word-ness.
    Both,
    /// `\B`: the two sides have matching word-ness.
    BothNegate,
    /// `\b{start}`: a non-word (or edge) on the left, a word char on the right.
    Start,
    /// `\b{end}`: a word char on the left, a non-word (or edge) on the right.
    End,
    /// `\b{start-half}`: a non-word (or edge) on the left; the right is unconstrained.
    StartHalf,
    /// `\b{end-half}`: a non-word (or edge) on the right; the left is unconstrained.
    EndHalf,
}

impl WordBoundaryKind {
    /// Whether the boundary holds, given the word-ness of the previous and next chars.
    pub fn holds(self, prev_word: bool, next_word: bool) -> bool {
        match self {
            WordBoundaryKind::Both => prev_word != next_word,
            WordBoundaryKind::BothNegate => prev_word == next_word,
            WordBoundaryKind::Start => !prev_word && next_word,
            WordBoundaryKind::End => prev_word && !next_word,
            WordBoundaryKind::StartHalf => !prev_word,
            WordBoundaryKind::EndHalf => !next_word,
        }
    }

    /// Whether the condition inspects the *previous* char. `end-half` looks only at the
    /// next char, so a matcher built solely from it needn't track `prev`.
    pub fn reads_prev(self) -> bool {
        !matches!(self, WordBoundaryKind::EndHalf)
    }
}

#[derive(Debug, Clone)]
pub enum Atom {
    Literal(String),
    // (inverted, items)
    Group(bool, Vec<GroupEntry>),
    /// A zero-width `$` / `\z` assertion: the match only completes when the input
    /// is exhausted (the lookahead char is `None`).
    EndOfInput,
    /// A zero-width `^` / `\A` (non-multiline start-of-text) assertion: holds only at
    /// the very start of the input, i.e. when the preceding char (`prev`) is `None`.
    /// The generated tokenizer always begins a token with `prev = None`, so a leading
    /// `^`/`\A` always holds there (a no-op); it matters for a mid-haystack search,
    /// where the slice can begin after other text.
    StartOfText,
    /// A zero-width word-boundary assertion. `kind` selects plain `\b`/`\B` or one of
    /// the directional half-boundaries (`\b{start}`/`\b{end}`/`\b{start-half}`/
    /// `\b{end-half}`); see [`WordBoundaryKind`] for the per-kind condition. Holds based
    /// on the word-ness of the previous and next characters (the input edges count as
    /// non-word) and consumes nothing. `unicode` (set by `(?u)`) selects Unicode `\w`
    /// word-ness over the default ASCII `[0-9A-Za-z_]`.
    WordBoundary {
        kind: WordBoundaryKind,
        unicode: bool,
    },
    /// A zero-width multiline start-of-line assertion: `^` under `(?m)`. Holds at
    /// the start of the input or immediately after a line terminator. Only emitted
    /// when the multiline flag is set; otherwise a leading `^` is dropped (see
    /// `parse.rs`). `crlf` (set by `(?R)`) treats `\r`, `\n` and the atomic `\r\n`
    /// all as terminators, so `^` does not hold between the `\r` and `\n` of a CRLF
    /// pair; otherwise the only terminator is `\n`.
    StartOfLine {
        crlf: bool,
    },
    /// A zero-width multiline end-of-line assertion: `$` under `(?m)`. Holds at the
    /// end of the input or immediately before a line terminator. Only emitted when
    /// the multiline flag is set; otherwise a trailing `$` lowers to
    /// [`Atom::EndOfInput`]. `crlf` (set by `(?R)`) matches the [`Atom::StartOfLine`]
    /// CRLF rule: `$` holds before a `\r` or a lone `\n`, but not between the `\r`
    /// and `\n` of a CRLF pair.
    EndOfLine {
        crlf: bool,
    },
    /// A parenthesised sub-expression with alternation: `(a|bc|d)`. Each inner
    /// `Vec<AtomRepeat>` is one `|`-separated branch (a sequence of atoms); a plain
    /// group `(...)` is just an alternation with a single branch. Capturing,
    /// non-capturing (`(?:...)`) and named (`(?P<n>...)`/`(?<n>...)`) groups are all
    /// represented identically — this engine does not extract capture spans, so the
    /// distinction is irrelevant once parsed.
    Alternation(Vec<Vec<AtomRepeat>>),
}

#[derive(Debug, Clone)]
pub struct AtomRepeat {
    pub atom: Atom,
    pub repeat: Repeat,
    /// Whether the repeat is lazy/non-greedy (`*?`, `+?`, `??`, and the optional
    /// tail of a `{n,m}?`). Only meaningful when `repeat` is not [`Repeat::Once`];
    /// it flips the priority of the skip/loop epsilon edges in the NFA so the
    /// leftmost-first matcher prefers the *shorter* match. See `nfa::build_repeat`.
    pub lazy: bool,
}

pub struct SimpleRegexAst {
    pub atoms: Vec<AtomRepeat>,
}

pub struct SimpleRegex {
    pub ast: SimpleRegexAst,
    pub dfa: Dfa,
}

impl SimpleRegex {
    pub fn parse(from: &str) -> Option<SimpleRegex> {
        let parsed = SimpleRegexAst::parse(from)?;
        let nfa = Nfa::build(&parsed);
        Some(SimpleRegex {
            ast: parsed,
            dfa: Dfa::build(&nfa),
        })
    }
}
