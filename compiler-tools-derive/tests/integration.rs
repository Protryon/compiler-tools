use compiler_tools::TokenParse;
use compiler_tools_derive::token_parse;

#[token_parse]
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Token<'a> {
    Async = "async",
    Await = "await",
    AwaitYe = "awaitye",
    Percent = "%",
    Plus = "+",
    Minus = "-",
    #[token(parse_fn = "compiler_tools::util::parse_str::<'\\''>")]
    String(&'a str),
    #[token(regex = "[0-9]+")]
    Int(i32),
    #[token(regex = "[a-z][a-zA-Z0-9_]*")]
    Ident(&'a str),
    #[token(regex = "//[^\n]*")]
    Comment(&'a str),
    #[token(regex = "/\\*.*\\*/")]
    CommentBlock(&'a str),
    #[token(regex = "[ \n]+")]
    Whitespace,
    #[token(illegal)]
    Illegal(char),
}

#[test]
fn test_token() {
    let mut tokenizer = Tokenizer::new(
        r#"async%+await+
    +%awaitye
    await
    test_ident+
    awaityematies
    //test comment
    1234
    -1234
    'test str'
    'test '' str'
    'test \d str'
    'test \' str'
    /* test
    *
    block */
    new_ident
    "#,
    );
    while let Some(next) = tokenizer.next() {
        println!("{:?}", next);
    }
}

#[test]
fn test_token_illegal() {
    let mut tokenizer = Tokenizer::new(
        r#"async%+await+
    +%awaitye
    ^
    *
    1234
    -1234
    async
    await
    "#,
    );
    while let Some(next) = tokenizer.next() {
        println!("{:?}", next);
    }
}
