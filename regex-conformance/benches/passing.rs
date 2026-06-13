//! Criterion benchmark over the *passing* slice of the regex corpus.
//!
//! Methodology mirrors the two-step process you'd do by hand — "run the tests,
//! then benchmark only the ones that pass" — but in a single process:
//!
//! 1. **Select** (untimed setup): walk the whole corpus and keep only tests the
//!    simple-regex engine actually passes — i.e. it parses the pattern, the
//!    haystack is UTF-8, a compiled matcher exists, and [`passes`] confirms the
//!    engine reproduces the corpus' expected matches. We additionally require
//!    the upstream `regex` crate to compile the pattern, so all three engines
//!    benchmark the *identical* workload.
//! 2. **Benchmark** (timed): run that passing set, as a whole, through three
//!    engines so cargo-criterion can compare them:
//!      * `simple-runtime`  — `SimpleRegex::find_prefix` (DFA interpreter),
//!      * `simple-compiled` — the generated-Rust matchers (`compiled_lookup`),
//!      * `regex-crate`     — the upstream `regex` crate, for comparison.
//!
//! All three are driven through the same [`run_search`] loop so the comparison
//! reflects matcher cost, not differing search harnesses. The `regex` matcher is
//! anchored with `\A(?:…)` to behave as the prefix matcher `run_search` expects.
//!
//! Run with: `cargo criterion --bench passing` (or `cargo bench --bench passing`).

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use regex_conformance::{SimpleRegex, compiled_lookup, load_corpus, passes, run_search};
use regex_test::{RegexTest, RegexTests};
use std::hint::black_box;

/// Everything needed to run one passing test through each of the three engines,
/// with all per-test compilation done once, up front (outside the timed loop).
struct Case<'a> {
    test: &'a RegexTest,
    simple: SimpleRegex,
    compiled: fn(&str) -> Option<(&str, &str)>,
    full: regex::Regex,
}

/// Build the passing set. Kept out of the timed region entirely.
fn select(corpus: &RegexTests) -> Vec<Case<'_>> {
    corpus
        .iter()
        .filter_map(|test| {
            let [pattern] = test.regexes() else {
                return None; // regex sets are out of scope for this engine
            };
            // This engine is `&str`-based; a non-UTF-8 haystack isn't representable.
            if std::str::from_utf8(test.haystack()).is_err() {
                return None;
            }
            let simple = SimpleRegex::parse(pattern)?;
            let compiled = compiled_lookup(test.full_name())?;
            // Anchor so `regex::Regex::find` behaves like the engine's prefix matcher.
            let full = regex::Regex::new(&format!(r"\A(?:{pattern})")).ok()?;

            // Only keep tests the simple engine genuinely passes. `passes` parses a
            // fresh `SimpleRegex` for the check since the one we store is borrowed by
            // `find_prefix` and `SimpleRegex` isn't `Clone`.
            let check = SimpleRegex::parse(pattern)?;
            if !passes(test, Box::new(move |input| check.find_prefix(input))) {
                return None;
            }

            Some(Case {
                test,
                simple,
                compiled,
                full,
            })
        })
        .collect()
}

fn bench_passing(c: &mut Criterion) {
    let corpus = load_corpus();
    let cases = select(&corpus);
    eprintln!("benchmarking {} passing tests", cases.len());

    let mut group = c.benchmark_group("passing-corpus");
    // One "element" per test, so criterion reports throughput in tests/sec.
    group.throughput(Throughput::Elements(cases.len() as u64));

    group.bench_function("simple-runtime", |b| {
        b.iter(|| {
            for case in &cases {
                black_box(run_search(|input| case.simple.find_prefix(input), case.test));
            }
        });
    });

    group.bench_function("simple-compiled", |b| {
        b.iter(|| {
            for case in &cases {
                black_box(run_search(|input| (case.compiled)(input), case.test));
            }
        });
    });

    group.bench_function("regex-crate", |b| {
        b.iter(|| {
            for case in &cases {
                black_box(run_search(|input| case.full.find(input).map(|m| (m.as_str(), &input[m.end()..])), case.test));
            }
        });
    });

    group.finish();
}

criterion_group!(benches, bench_passing);
criterion_main!(benches);
