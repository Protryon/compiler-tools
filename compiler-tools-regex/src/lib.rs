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
mod matching;
mod nfa;
mod parse;
mod unicode;

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

#[derive(Debug, Clone)]
pub enum Atom {
    Literal(String),
    // (inverted, items)
    Group(bool, Vec<GroupEntry>),
    /// A zero-width `$` / `\z` assertion: the match only completes when the input
    /// is exhausted. There is no start-of-input counterpart because this engine
    /// always matches a prefix from the current position, so a leading `^` / `\A`
    /// is a no-op and is dropped at parse time.
    EndOfInput,
    /// A zero-width word-boundary assertion (`\b` when `negate` is `false`, `\B` when
    /// `true`). Holds when the word-ness of the previous and next characters differ
    /// (`\b`) or match (`\B`); the edges of the input count as non-word. Consumes
    /// nothing. `unicode` (set by `(?u)`) selects Unicode `\w` word-ness over the
    /// default ASCII `[0-9A-Za-z_]`.
    WordBoundary { negate: bool, unicode: bool },
    /// A zero-width multiline start-of-line assertion: `^` under `(?m)`. Holds at
    /// the start of the input or immediately after a `\n`. Only emitted when the
    /// multiline flag is set; otherwise a leading `^` is dropped (see `parse.rs`).
    StartOfLine,
    /// A zero-width multiline end-of-line assertion: `$` under `(?m)`. Holds at the
    /// end of the input or immediately before a `\n`. Only emitted when the
    /// multiline flag is set; otherwise a trailing `$` lowers to [`Atom::EndOfInput`].
    EndOfLine,
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
