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
