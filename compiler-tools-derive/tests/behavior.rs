//! Behavioral assertions for the generated tokenizer.
//!
//! Unlike `integration.rs` (which only smoke-tests that lexing runs), these tests
//! assert the exact token stream, payloads, `Display` output, and span tracking.

use compiler_tools::{Spanned, TokenParse};
use compiler_tools_derive::token_parse;

#[token_parse]
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Token<'a> {
    Let = "let",
    Plus = "+",
    Eq = "=",
    EqEq = "==",
    #[token(regex = "[0-9]+")]
    Int(u64),
    #[token(regex = "[a-z][a-zA-Z0-9_]*")]
    Ident(&'a str),
    #[token(regex = "[ \t\n]+")]
    Ws,
    #[token(parse_fn = "compiler_tools::util::parse_str::<'\\''>")]
    Str(&'a str),
    #[token(illegal)]
    Illegal(char),
}

fn lex(input: &str) -> Vec<Token<'_>> {
    let mut tokenizer = Tokenizer::new(input);
    let mut out = vec![];
    while let Some(next) = tokenizer.next() {
        out.push(next.token);
    }
    out
}

fn lex_spans(input: &str) -> Vec<Spanned<Token<'_>>> {
    let mut tokenizer = Tokenizer::new(input);
    let mut out = vec![];
    while let Some(next) = tokenizer.next() {
        out.push(next);
    }
    out
}

#[test]
fn keyword_vs_ident_conflict_resolution() {
    // "let" exactly is the keyword; anything matching the longer ident regex is an ident,
    // even when it has the keyword as a prefix.
    assert_eq!(lex("let"), vec![Token::Let]);
    assert_eq!(lex("letx"), vec![Token::Ident("letx")]);
    assert_eq!(lex("lettuce"), vec![Token::Ident("lettuce")]);
    assert_eq!(lex("let letx"), vec![Token::Let, Token::Ws, Token::Ident("letx")]);
}

#[test]
fn longest_match_operators() {
    assert_eq!(lex("=="), vec![Token::EqEq]);
    assert_eq!(lex("="), vec![Token::Eq]);
    assert_eq!(lex("=+"), vec![Token::Eq, Token::Plus]);
    assert_eq!(lex("+=="), vec![Token::Plus, Token::EqEq]);
    assert_eq!(lex("==="), vec![Token::EqEq, Token::Eq]);
}

#[test]
fn int_payload_is_parsed() {
    assert_eq!(lex("42"), vec![Token::Int(42)]);
    assert_eq!(lex("0 999"), vec![Token::Int(0), Token::Ws, Token::Int(999)]);
}

#[test]
fn string_parse_fn() {
    // parse_str keeps the surrounding delimiters in the payload.
    assert_eq!(lex("'hi'"), vec![Token::Str("'hi'")]);
    // a doubled delimiter is an escaped quote, so it stays inside one string token
    assert_eq!(lex("'a''b'"), vec![Token::Str("'a''b'")]);
}

#[test]
fn illegal_fallback() {
    assert_eq!(lex("@#"), vec![Token::Illegal('@'), Token::Illegal('#')]);
}

#[test]
fn full_expression() {
    assert_eq!(
        lex("let x = 42 + y"),
        vec![
            Token::Let,
            Token::Ws,
            Token::Ident("x"),
            Token::Ws,
            Token::Eq,
            Token::Ws,
            Token::Int(42),
            Token::Ws,
            Token::Plus,
            Token::Ws,
            Token::Ident("y"),
        ]
    );
}

#[test]
fn display_round_trips_punctuation_and_payloads() {
    assert_eq!(Token::Let.to_string(), "let");
    assert_eq!(Token::Plus.to_string(), "+");
    assert_eq!(Token::EqEq.to_string(), "==");
    assert_eq!(Token::Int(42).to_string(), "42");
    assert_eq!(Token::Ident("foo").to_string(), "foo");
    assert_eq!(Token::Str("'hi'").to_string(), "'hi'");
    assert_eq!(Token::Illegal('@').to_string(), "@");
    // a regex-only variant with no literal falls back to the variant name
    assert_eq!(Token::Ws.to_string(), "Ws");
}

#[test]
fn matches_class_ignores_payload() {
    use compiler_tools::TokenExt;
    assert!(Token::Ident("a").matches_class(&Token::Ident("b")));
    assert!(Token::Int(1).matches_class(&Token::Int(2)));
    assert!(Token::Plus.matches_class(&Token::Plus));
    assert!(!Token::Plus.matches_class(&Token::Eq));
    assert!(!Token::Ident("a").matches_class(&Token::Int(1)));
}

#[test]
fn spans_single_line() {
    let spans = lex_spans("let x");
    let cols: Vec<(u64, u64)> = spans.iter().map(|s| (s.span.col_start, s.span.col_stop)).collect();
    assert_eq!(cols, vec![(0, 3), (3, 4), (4, 5)]);
    assert!(spans.iter().all(|s| s.span.line_start == 0 && s.span.line_stop == 0));
}

#[test]
fn spans_track_newlines() {
    // Whitespace containing a newline advances the line counter for following tokens.
    let spans = lex_spans("a\nbc");
    assert_eq!(spans.len(), 3);
    // "a" on line 0
    assert_eq!((spans[0].span.line_start, spans[0].span.col_start), (0, 0));
    // the whitespace crosses the newline: starts line 0, ends line 1
    assert_eq!(spans[1].span.line_start, 0);
    assert_eq!(spans[1].span.line_stop, 1);
    // "bc" is now on line 1, starting at column 0
    assert_eq!((spans[2].span.line_start, spans[2].span.col_start), (1, 0));
    assert_eq!((spans[2].span.line_stop, spans[2].span.col_stop), (1, 2));
}

// A grammar where newlines are *not* consumed by any token, so they fall through
// to the illegal handler. This exercises line/column tracking in the illegal path.
#[token_parse]
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Sym {
    Dot = ".",
    #[token(illegal)]
    Bad(char),
}

fn lex_sym_spans(input: &str) -> Vec<Spanned<Sym>> {
    let mut tokenizer = SymTokenizer::new(input);
    let mut out = vec![];
    while let Some(next) = tokenizer.next() {
        out.push(next);
    }
    out
}

#[test]
fn illegal_tokens_track_lines_and_columns() {
    // Regression: illegal non-newline characters must not advance the line counter,
    // and an illegal newline must advance it exactly once.
    let spans = lex_sym_spans("@@\n@");
    let tuples: Vec<(u64, u64, u64, u64)> = spans
        .iter()
        .map(|s| (s.span.line_start, s.span.col_start, s.span.line_stop, s.span.col_stop))
        .collect();
    assert_eq!(
        tuples,
        vec![
            (0, 0, 0, 1), // first '@' on line 0
            (0, 1, 0, 2), // second '@' still on line 0
            (0, 2, 1, 0), // newline: crosses from line 0 to line 1
            (1, 0, 1, 1), // final '@' on line 1
        ]
    );
}

#[test]
fn no_lifetime_grammar_lexes() {
    let toks: Vec<Sym> = {
        let mut tokenizer = SymTokenizer::new("..@");
        let mut out = vec![];
        while let Some(next) = tokenizer.next() {
            out.push(next.token);
        }
        out
    };
    assert_eq!(toks, vec![Sym::Dot, Sym::Dot, Sym::Bad('@')]);
}
