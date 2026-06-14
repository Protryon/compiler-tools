//! Runs the regex test corpus against both simple-regex engines and prints a
//! per-engine summary of pass / fail-to-parse / fail-to-pass / skipped.
//!
//! This is a bring-up harness, not a gate: the engine implements a small subset
//! of the `regex` syntax, so a large fraction of the corpus is expected to land
//! in fail-to-parse (unsupported syntax) or fail-to-pass (different semantics).
//! The test itself always passes; run with `--nocapture` to see the summary.
//!
//! Categories:
//! * **pass** — the engine parsed the pattern and produced exactly the matches
//!   the corpus expects.
//! * **fail-to-parse** — the engine's parser rejected the pattern (e.g.
//!   alternation/groups it doesn't support).
//! * **fail-to-pass** — the pattern parsed, but the search produced different
//!   matches than expected.
//! * **skipped** — not applicable to this engine (a multi-pattern set, or a
//!   non-UTF-8 haystack this `&str`-based engine can't represent).

use std::time::{Duration, Instant};

use regex_conformance::{BoxedMatcher, SimpleRegex, compiled_lookup, effective_pattern, load_corpus, passes, run_search};
use regex_test::{RegexTest, RegexTests};

/// What an engine could do with a given test before we try to run it.
enum Prepared {
    /// Not applicable to this engine (e.g. a regex set).
    Skip,
    /// The engine's parser rejected the pattern.
    FailToParse,
    /// A ready-to-run anchored prefix matcher for the (single) pattern. The second
    /// argument is the char preceding the slice (for `^`/`\b` context).
    Run(BoxedMatcher),
}

struct Summary {
    label: String,
    pass: u32,
    /// Full names of tests whose pattern the engine's parser rejected.
    fail_to_parse: Vec<String>,
    /// Full names of tests that parsed but produced the wrong matches.
    fail_to_pass: Vec<String>,
    skipped: u32,
    /// Number of (applicable) searches that contributed to `match_time`.
    searches: u32,
    /// Wall time spent purely in the engine's matcher/search, excluding the
    /// `regex-test` comparison harness.
    match_time: Duration,
    /// Wall time for the whole pass, including parsing, searching and comparison.
    wall_time: Duration,
}

impl Summary {
    /// Print the full name of every failing test, grouped by failure kind. Done
    /// up front so the numeric summaries can sit at the end of the output.
    fn report_failures(&self) {
        println!("\n--- {} fail-to-parse ({}) ---", self.label, self.fail_to_parse.len());
        for name in &self.fail_to_parse {
            println!("  {name}");
        }
        println!("\n--- {} fail-to-pass ({}) ---", self.label, self.fail_to_pass.len());
        for name in &self.fail_to_pass {
            println!("  {name}");
        }
    }

    fn report(&self) {
        let total = self.pass + self.fail_to_parse.len() as u32 + self.fail_to_pass.len() as u32 + self.skipped;
        println!("\n=== {} ===", self.label);
        println!("  total:         {total}");
        println!("  pass:          {}", self.pass);
        println!("  fail-to-parse: {}", self.fail_to_parse.len());
        println!("  fail-to-pass:  {}", self.fail_to_pass.len());
        println!("  skipped:       {}", self.skipped);
        println!("  match time:    {:.3?} over {} searches", self.match_time, self.searches);
        if self.searches > 0 {
            println!("  per search:    {:.3?}", self.match_time / self.searches);
        }
        println!("  wall time:     {:.3?}", self.wall_time);
    }
}

fn summarize(label: &str, tests: &RegexTests, prepare: impl Fn(&RegexTest) -> Prepared) -> Summary {
    let mut summary = Summary {
        label: label.to_string(),
        pass: 0,
        fail_to_parse: vec![],
        fail_to_pass: vec![],
        skipped: 0,
        searches: 0,
        match_time: Duration::ZERO,
        wall_time: Duration::ZERO,
    };
    let started = Instant::now();
    for test in tests.iter() {
        match prepare(test) {
            Prepared::Skip => summary.skipped += 1,
            Prepared::FailToParse => summary.fail_to_parse.push(test.full_name().to_string()),
            Prepared::Run(matcher) => {
                // This engine works on `&str`; a non-UTF-8 haystack isn't representable.
                if std::str::from_utf8(test.haystack()).is_err() {
                    summary.skipped += 1;
                    continue;
                }
                // Time the engine's search in isolation (the comparison harness in
                // `test_passes` adds its own, unrelated overhead).
                let search_started = Instant::now();
                let _ = run_search(&matcher, test);
                summary.match_time += search_started.elapsed();
                summary.searches += 1;

                if passes(test, matcher) {
                    summary.pass += 1;
                } else {
                    summary.fail_to_pass.push(test.full_name().to_string());
                }
            }
        }
    }
    summary.wall_time = started.elapsed();
    summary
}

/// Both engines share one `#[test]` so they run sequentially — `test_passes`
/// swaps the global panic hook, which would race across parallel test threads.
#[test]
fn conformance_summary() {
    let tests = load_corpus();

    // Runtime interpreter: `SimpleRegex::find_prefix` walks the DFA directly.
    let runtime = summarize("runtime interpreter", &tests, |test| {
        let [_] = test.regexes() else {
            return Prepared::Skip; // regex sets are out of scope for this engine
        };
        match SimpleRegex::parse(&effective_pattern(test)) {
            Some(regex) => Prepared::Run(Box::new(move |input, prev| regex.find_prefix(input, prev))),
            None => Prepared::FailToParse,
        }
    });

    // Compiled-Rust engine: the matchers `build.rs` emitted via `generate_parser`.
    let compiled = summarize("compiled-rust engine", &tests, |test| {
        let [_] = test.regexes() else {
            return Prepared::Skip;
        };
        // Use the parser to tell "couldn't parse" apart from "parsed but no matcher".
        if SimpleRegex::parse(&effective_pattern(test)).is_none() {
            return Prepared::FailToParse;
        }
        match compiled_lookup(test.full_name()) {
            Some(matcher) => Prepared::Run(Box::new(matcher)),
            None => Prepared::FailToParse,
        }
    });

    // JIT engine (feature-gated): Cranelift compiles the same DFA to native code. It must
    // agree with the other two, since all three are driven from the same DFA.
    #[cfg(feature = "jit")]
    let jit = summarize("jit engine", &tests, |test| {
        let [_] = test.regexes() else {
            return Prepared::Skip;
        };
        match SimpleRegex::parse(&effective_pattern(test)) {
            // A JIT build failure (e.g. a non-64-bit host) counts as fail-to-parse, the same
            // bucket the compiled engine uses when it has no matcher for a parsed pattern.
            Some(regex) => match regex.compile_jit() {
                Ok(jit) => Prepared::Run(Box::new(move |input, prev| jit.find_prefix(input, prev))),
                Err(_) => Prepared::FailToParse,
            },
            None => Prepared::FailToParse,
        }
    });

    // Failing-test names first, so the numeric summaries land at the end.
    runtime.report_failures();
    compiled.report_failures();
    #[cfg(feature = "jit")]
    jit.report_failures();

    runtime.report();
    compiled.report();
    #[cfg(feature = "jit")]
    jit.report();

    println!("\n=== total ===");
    #[cfg(not(feature = "jit"))]
    println!("  wall time:     {:.3?}", runtime.wall_time + compiled.wall_time);
    #[cfg(feature = "jit")]
    println!("  wall time:     {:.3?}", runtime.wall_time + compiled.wall_time + jit.wall_time);

    // Unlike the interpreter-vs-compiled comparison (a bring-up harness that never fails),
    // the JIT *must* agree with the interpreter exactly — both walk the same DFA — so any
    // divergence is a JIT lowering bug and fails the test.
    #[cfg(feature = "jit")]
    {
        assert_eq!(jit.pass, runtime.pass, "JIT pass count diverged from the interpreter");
        assert_eq!(jit.skipped, runtime.skipped, "JIT skipped count diverged from the interpreter");
        assert_eq!(jit.fail_to_parse, runtime.fail_to_parse, "JIT fail-to-parse set diverged from the interpreter");
        assert_eq!(jit.fail_to_pass, runtime.fail_to_pass, "JIT fail-to-pass set diverged from the interpreter");
    }
}
