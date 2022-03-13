use std::time::{Duration, Instant};

use compiler_tools::TokenParse;
use compiler_tools_derive::token_parse;

#[token_parse]
#[derive(Clone, Copy, Debug)]
pub enum TokenSimple<'a> {
    Async = "async",
    Plus = "+",
    #[token(regex = "[a-z][a-zA-Z0-9_]*")]
    Ident(&'a str),
    #[token(regex = "/\\*.*\\*/")]
    CommentBlock(&'a str),
}

#[token_parse]
#[derive(Clone, Copy, Debug)]
pub enum TokenFull<'a> {
    Async = "async",
    Plus = "+",
    #[token(regex_full = "[a-z][a-zA-Z0-9_]*")]
    Ident(&'a str),
    #[token(regex_full = "/\\*.*?\\*/")]
    CommentBlock(&'a str),
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

const TEST_COUNT: usize = 100000;

// cargo test --release --package compiler-tools-derive --test regex_bench -- bench_simple --exact --nocapture
// took 1.21 ms for 100000 idents @ 0.0000 ms/ident
// took 4.99 ms for 100000 idents @ 0.0000 ms/ident
#[test]
fn bench_simple() {
    let idents = "test_ide123nt+".repeat(TEST_COUNT);

    let mut tokenizer = TokenizerSimple::new(&*idents);
    let start = Instant::now();
    for _ in 0..TEST_COUNT {
        assert!(tokenizer.next().is_some());
    }
    let elapsed = duration_ms(start.elapsed());
    println!("took {:.02} ms for {} idents @ {:.04} ms/ident", elapsed, TEST_COUNT, elapsed / TEST_COUNT as f64);

    let idents = "/* test * block */+".repeat(TEST_COUNT);

    let mut tokenizer = TokenizerSimple::new(&*idents);
    let start = Instant::now();
    for _ in 0..TEST_COUNT {
        assert!(tokenizer.next().is_some());
    }
    let elapsed = duration_ms(start.elapsed());
    println!("took {:.02} ms for {} idents @ {:.04} ms/ident", elapsed, TEST_COUNT, elapsed / TEST_COUNT as f64);
}

// cargo test --release --package compiler-tools-derive --test regex_bench -- bench_full --exact --nocapture
// took 10.61 ms for 100000 idents @ 0.0001 ms/ident
// took 14.23 ms for 100000 idents @ 0.0001 ms/ident
#[test]
fn bench_full() {
    let idents = "test_ide123nt+".repeat(TEST_COUNT);

    let mut tokenizer = TokenizerFull::new(&*idents);
    let start = Instant::now();
    for _ in 0..TEST_COUNT {
        assert!(tokenizer.next().is_some());
    }
    let elapsed = duration_ms(start.elapsed());
    println!("took {:.02} ms for {} idents @ {:.04} ms/ident", elapsed, TEST_COUNT, elapsed / TEST_COUNT as f64);

    let idents = "/* test * block */+".repeat(TEST_COUNT);

    let mut tokenizer = TokenizerFull::new(&*idents);
    let start = Instant::now();
    for _ in 0..TEST_COUNT {
        assert!(tokenizer.next().is_some());
    }
    let elapsed = duration_ms(start.elapsed());
    println!("took {:.02} ms for {} idents @ {:.04} ms/ident", elapsed, TEST_COUNT, elapsed / TEST_COUNT as f64);
}
