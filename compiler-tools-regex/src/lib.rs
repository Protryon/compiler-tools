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
    /// A zero-width word-boundary assertion (`\b` when `false`, `\B` when `true`).
    /// Holds when the word-ness (`[0-9A-Za-z_]`, ASCII) of the previous and next
    /// characters differ (`\b`) or match (`\B`); the edges of the input count as
    /// non-word. Consumes nothing.
    WordBoundary(bool),
}

#[derive(Debug, Clone)]
pub struct AtomRepeat {
    pub atom: Atom,
    pub repeat: Repeat,
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
