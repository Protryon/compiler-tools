//! Test harness that runs the upstream `regex` test corpus (`../testdata`,
//! parsed with the `regex-test` crate) against compiler-tools' simple-regex
//! engine, in both of its forms:
//!
//! * the **runtime interpreter** — [`compiler_tools_regex::SimpleRegex::find_prefix`],
//!   which walks the compiled DFA directly, and
//! * the **compiled-Rust engine** — the `fn(&str) -> Option<(&str, &str)>` matchers
//!   that `build.rs` emits via `SimpleRegex::generate_parser` (the exact code
//!   `#[token(regex = ...)]` expands to), looked up here via [`compiled_lookup`].
//!
//! Both engines are anchored prefix matchers, so [`run_search`] turns one into a
//! leftmost, non-overlapping search to line up with the corpus' expectations.
//! The aim is to *exercise* the engines against the corpus; many tests are
//! expected to fail or be skipped (unsupported syntax, byte/Unicode semantics),
//! and that's fine — see the test crate for how results are summarized.

use std::path::{Path, PathBuf};

pub use compiler_tools_regex::SimpleRegex;
use regex_test::{Match, RegexTest, RegexTests, Span, TestResult};

// `compiled_lookup` plus one `compiled_<n>` matcher per supported test. The
// generated matchers carry the usual dead-`prev`/dead-store warnings from
// `generate_parser`, so silence them for the whole generated module.
#[allow(warnings)]
mod compiled {
    include!(concat!(env!("OUT_DIR"), "/compiled.rs"));
}
pub use compiled::compiled_lookup;

/// The directory holding the TOML test corpus (`<workspace>/testdata`).
pub fn testdata_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("testdata")
}

/// Load every `*.toml` test in the corpus (recursively). Files the installed
/// `regex-test` can't parse are skipped, mirroring `build.rs`.
pub fn load_corpus() -> RegexTests {
    let mut files = vec![];
    collect_toml(&testdata_dir(), &mut files);
    files.sort();

    let mut tests = RegexTests::new();
    for file in &files {
        if let Err(err) = tests.load(file) {
            eprintln!("skipping {}: {err}", file.display());
        }
    }
    tests
}

fn collect_toml(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_toml(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "toml") {
            out.push(path);
        }
    }
}

/// Drive a prefix matcher (anchored at the start of the slice it's given) over a
/// test's haystack as a leftmost, non-overlapping search, and report the spans as
/// a [`TestResult`] the `regex-test` runner can check.
///
/// Tests whose haystack isn't valid UTF-8 (this engine is `&str`-based) are
/// skipped rather than failed.
pub fn run_search(matcher: impl Fn(&str) -> Option<(&str, &str)>, test: &RegexTest) -> TestResult {
    let Ok(haystack) = std::str::from_utf8(test.haystack()) else {
        return TestResult::skip();
    };
    let bounds = test.bounds();
    let anchored = test.anchored();
    let limit = test.match_limit();

    let mut matches = vec![];
    let mut pos = bounds.start;
    while pos <= bounds.end && pos <= haystack.len() {
        if !haystack.is_char_boundary(pos) {
            pos += 1;
            continue;
        }
        let end = bounds.end.min(haystack.len());
        match matcher(&haystack[pos..end]) {
            Some((matched, _)) => {
                let match_end = pos + matched.len();
                matches.push(Match {
                    id: 0,
                    span: Span {
                        start: pos,
                        end: match_end,
                    },
                });
                if limit.is_some_and(|limit| matches.len() >= limit) {
                    break;
                }
                // Advance past the match; a zero-width match must still move forward.
                pos = if matched.is_empty() { next_char_boundary(haystack, pos) } else { match_end };
            }
            None => {
                if anchored {
                    break;
                }
                pos += 1;
            }
        }
    }
    TestResult::matches(matches)
}

fn next_char_boundary(haystack: &str, mut pos: usize) -> usize {
    pos += 1;
    while pos < haystack.len() && !haystack.is_char_boundary(pos) {
        pos += 1;
    }
    pos
}
