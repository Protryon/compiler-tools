use compiler_tools::TokenParse;
use compiler_tools_derive::token_parse;

#[token_parse]
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Token<'a> {
    #[token(regex = "\\d+")]
    Int(&'a str),
    #[token(regex = "[a-z]{2,3}")]
    Word(&'a str),
    #[token(regex = "\\s+")]
    Ws(&'a str),
    #[token(illegal)]
    Illegal(char),
}

#[test]
fn shorthand_and_count() {
    let mut t = Tokenizer::new("12 ab abc abcd");
    let toks: Vec<_> = std::iter::from_fn(|| t.next().map(|s| s.token)).collect();
    println!("{:?}", toks);
    assert!(matches!(toks[0], Token::Int("12")));
    assert!(matches!(toks[2], Token::Word("ab")));
    assert!(matches!(toks[4], Token::Word("abc")));
    // {2,3} is greedy but capped at 3, so "abcd" yields Word("abc") then leftover 'd'.
    assert!(matches!(toks[6], Token::Word("abc")));
    assert!(matches!(toks[7], Token::Illegal('d')));
}

// `$` anchored token declared first so it isn't shadowed by the fallback Word.
#[token_parse]
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Anchored<'a> {
    #[token(regex = "ab$")]
    AbEnd(&'a str),
    #[token(regex = "[a-z]+")]
    Word(&'a str),
    #[token(illegal)]
    Illegal(char),
}

#[test]
fn dollar_anchors_to_eof() {
    // "ab" at end of input -> matches the `$`-anchored token.
    let mut t = AnchoredTokenizer::new("ab");
    assert!(matches!(t.next().unwrap().token, Anchored::AbEnd("ab")));

    // "abc": the anchor fails (not at EOF) so it falls through to Word.
    let mut t2 = AnchoredTokenizer::new("abc");
    assert!(matches!(t2.next().unwrap().token, Anchored::Word("abc")));
}

#[test]
fn dot_excludes_newline() {
    #[token_parse]
    #[derive(PartialEq, Clone, Copy, Debug)]
    pub enum T2<'a> {
        #[token(regex = "a.c")]
        AnyC(&'a str),
        #[token(illegal)]
        Illegal(char),
    }
    let mut t = T2Tokenizer::new("a c");
    assert!(matches!(t.next().unwrap().token, T2::AnyC("a c")));
    let mut t2 = T2Tokenizer::new("a\nc");
    assert!(!matches!(t2.next().unwrap().token, T2::AnyC(_)));
}

// Anchored patterns combined with repeats/classes must still build a valid tokenizer.
#[token_parse]
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Combo<'a> {
    #[token(regex = "\\d{2,4}$")]
    NumEnd(&'a str),
    #[token(regex = "[a-z]+$")]
    WordEnd(&'a str),
    #[token(regex = "[a-z0-9]+")]
    Other(&'a str),
    #[token(illegal)]
    Illegal(char),
}

#[test]
fn anchored_repeats_compose() {
    let mut t = ComboTokenizer::new("12");
    assert!(matches!(t.next().unwrap().token, Combo::NumEnd("12")));

    // "123x": NumEnd's $ fails (not EOF), WordEnd needs [a-z]+ -> no, falls to Other.
    let mut t2 = ComboTokenizer::new("123x");
    assert!(matches!(t2.next().unwrap().token, Combo::Other("123x")));

    let mut t3 = ComboTokenizer::new("abc");
    assert!(matches!(t3.next().unwrap().token, Combo::WordEnd("abc")));
}

// --- numeric escapes, \A/\z, and word boundaries through the generated tokenizer ---

#[token_parse]
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Hex<'a> {
    // \x41-\x5A == [A-Z]; '\u{2764}' is a heart.
    #[token(regex = "[\\x41-\\x5A]+")]
    Upper(&'a str),
    #[token(regex = "\\u{2764}")]
    Heart(&'a str),
    #[token(illegal)]
    Illegal(char),
}

#[test]
fn hex_escapes_runtime() {
    let mut t = HexTokenizer::new("ABC");
    assert!(matches!(t.next().unwrap().token, Hex::Upper("ABC")));
    let mut t2 = HexTokenizer::new("\u{2764}");
    assert!(matches!(t2.next().unwrap().token, Hex::Heart(_)));
}

// `\bword\b` only matches "word" when it stands as a whole word.
#[token_parse]
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Boundary<'a> {
    #[token(regex = "\\bword\\b")]
    Word(&'a str),
    #[token(regex = "[a-z]+")]
    Ident(&'a str),
    #[token(regex = "[ ]+")]
    Ws(&'a str),
    #[token(illegal)]
    Illegal(char),
}

#[test]
fn word_boundary_runtime() {
    // Standalone "word" -> Word (leading \b holds at start, trailing \b holds at EOF).
    let mut t = BoundaryTokenizer::new("word");
    assert!(matches!(t.next().unwrap().token, Boundary::Word("word")));

    // "word " -> Word, the trailing \b holding before the space.
    let mut t2 = BoundaryTokenizer::new("word ");
    assert!(matches!(t2.next().unwrap().token, Boundary::Word("word")));

    // "words": the trailing \b fails between 'd' and 's' (both word chars),
    // so the `\bword\b` token does not match and it falls back to Ident.
    let mut t3 = BoundaryTokenizer::new("words");
    assert!(matches!(t3.next().unwrap().token, Boundary::Ident("words")));
}

// \B is the negation: matches a position *inside* a word.
#[token_parse]
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum NonBoundary<'a> {
    // "x\Bx" -> two x's with a non-boundary between them (both word chars).
    #[token(regex = "x\\Bx")]
    XX(&'a str),
    #[token(illegal)]
    Illegal(char),
}

#[test]
fn non_word_boundary_runtime() {
    let mut t = NonBoundaryTokenizer::new("xx");
    assert!(matches!(t.next().unwrap().token, NonBoundary::XX("xx")));
}

// \z behaves like a trailing `$`.
#[token_parse]
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum ZAnchor<'a> {
    #[token(regex = "ab\\z")]
    AbEnd(&'a str),
    #[token(regex = "[a-z]+")]
    Word(&'a str),
    #[token(illegal)]
    Illegal(char),
}

#[test]
fn z_anchor_runtime() {
    let mut t = ZAnchorTokenizer::new("ab");
    assert!(matches!(t.next().unwrap().token, ZAnchor::AbEnd("ab")));
    let mut t2 = ZAnchorTokenizer::new("abc");
    assert!(matches!(t2.next().unwrap().token, ZAnchor::Word("abc")));
}
