//! Code-generates the "compiled" simple-regex matcher for every test in
//! `../testdata`, so the conformance test can exercise the generated-Rust engine
//! (as opposed to the runtime DFA interpreter) without invoking the proc-macro.
//!
//! For each test with a single, parseable pattern we emit the same matcher
//! `SimpleRegex::generate_parser` produces inside `#[token(regex = ...)]`, keyed
//! by the test's full name. Patterns the engine can't parse (alternation, etc.)
//! are simply absent from the table and skipped by the test.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use compiler_tools_regex::{SimpleRegex, flatten};
use quote::{format_ident, quote};
use regex_test::{RegexTest, RegexTests};

/// Byte-for-byte copy of `regex_conformance::effective_pattern` (a build script
/// can't depend on its own crate). Folds the corpus' test-level options into a
/// leading inline-flag group — `unicode = true` → `u`, `case-insensitive = true` →
/// `i` — so the compiled matcher and the runtime interpreter parse the same
/// pattern. Keep in sync with `src/lib.rs`.
fn effective_pattern(test: &RegexTest) -> String {
    let pattern = &test.regexes()[0];
    let mut inline = String::new();
    if test.unicode() {
        inline.push('u');
    }
    if test.case_insensitive() {
        inline.push('i');
    }
    if inline.is_empty() {
        pattern.clone()
    } else {
        format!("(?{inline}){pattern}")
    }
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let testdata = manifest.parent().unwrap().join("testdata");
    println!("cargo:rerun-if-changed={}", testdata.display());

    let mut files = vec![];
    collect_toml(&testdata, &mut files);
    files.sort();

    let mut tests = RegexTests::new();
    for file in &files {
        println!("cargo:rerun-if-changed={}", file.display());
        // A file the installed regex-test version can't parse is dropped here; the
        // test harness reads the same corpus with the same crate, so both agree.
        if let Err(err) = tests.load(file) {
            println!("cargo:warning=skipping {}: {err}", file.display());
        }
    }

    // Keep generator panics from aborting the build; an unsupported pattern just
    // means no compiled matcher for that test.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let mut fns = vec![];
    let mut entries = vec![];
    for (i, test) in tests.iter().enumerate() {
        if test.regexes().len() != 1 {
            continue;
        }
        let Some(re) = SimpleRegex::parse(&effective_pattern(test)) else {
            continue;
        };
        let ident = format_ident!("compiled_{}", i);
        let parser = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| re.generate_parser(ident.clone()))) {
            Ok(parser) => parser,
            Err(_) => continue,
        };
        fns.push(parser);
        let name = test.full_name();
        entries.push(quote! { (#name, #ident as fn(&str, Option<char>) -> Option<(&str, &str)>), });
    }

    std::panic::set_hook(prev_hook);

    let fns = flatten(fns);
    let entries = flatten(entries);
    let generated = quote! {
        #fns

        /// Returns the compiled-engine matcher generated for the test with the
        /// given full name, or `None` if its pattern was unsupported.
        pub fn compiled_lookup(full_name: &str) -> Option<fn(&str, Option<char>) -> Option<(&str, &str)>> {
            const TABLE: &[(&str, fn(&str, Option<char>) -> Option<(&str, &str)>)] = &[ #entries ];
            TABLE.iter().copied().find(|(name, _)| *name == full_name).map(|(_, f)| f)
        }
    };

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join("compiled.rs"), generated.to_string()).unwrap();
}

fn collect_toml(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_toml(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "toml") {
            out.push(path);
        }
    }
}
