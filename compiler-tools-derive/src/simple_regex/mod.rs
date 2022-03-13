use proc_macro2::{TokenStream, Ident};
use quote::quote;

use crate::flatten;

use self::{dfa::Dfa, nfa::Nfa};

mod parse;
mod matching;
mod generate;
mod nfa;
mod dfa;

#[derive(Debug)]
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

#[derive(Debug)]
pub enum Atom {
    Literal(String),
    // (inverted, items)
    Group(bool, Vec<GroupEntry>),
}

#[derive(Debug)]
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