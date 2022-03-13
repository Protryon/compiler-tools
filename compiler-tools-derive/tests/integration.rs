use compiler_tools::TokenParse;
use compiler_tools_derive::token_parse;

#[token_parse]
#[derive(Clone, Copy, Debug)]
pub enum Token<'a> {
    Async = "async",
    Await = "await",
    AwaitYe = "awaitye",
    Percent = "%",
    Plus = "+",
    #[token(regex = "[a-z][a-zA-Z0-9_]*")]
    Ident(&'a str),
    #[token(regex = "//[^\n]*")]
    Comment(&'a str),
    #[token(regex = "/\\*.*\\*/")]
    CommentBlock(&'a str),
    #[token(regex = "[ \n]+")]
    Whitespace,
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
    /* test
    *
    block */
    new_ident
    "#,
    );
    while let Some(next) = tokenizer.next() {
        println!("{}", next);
    }
}
