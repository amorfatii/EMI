//! What `cargo test` prints, read test by test. Each test binary is
//! announced by a `Running <source> (<binary>)` line, doc tests by
//! `Doc-tests <crate>`; then one `test <name> ... <outcome>` line per test
//! and a `test result:` line. A test's id is its binary's source and its
//! name, `tests/adds.rs::adds_two`, the form every driver uses.

use std::collections::BTreeSet;

use emi_driver::Summary;

/// The source file a `Running` line names (`unittests src/lib.rs (...)`,
/// `tests/adds.rs (...)`), or the doc tests' marker.
fn binary(line: &str) -> Option<String> {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("Running ") {
        let rest = rest.strip_prefix("unittests ").unwrap_or(rest);
        return Some(rest.split(" (").next().unwrap_or(rest).to_owned());
    }
    line.strip_prefix("Doc-tests ")
        .map(|krate| format!("doc-tests {}", krate.trim()))
}

/// `test <name> ... <outcome>` with the outcome as the driver names it.
fn outcome_line(line: &str) -> Option<(&str, &'static str)> {
    let rest = line.strip_prefix("test ")?;
    let (name, word) = rest.rsplit_once(" ... ")?;
    let outcome = match word.trim() {
        "ok" => "passed",
        "FAILED" => "failed",
        w if w == "ignored" || w.starts_with("ignored,") => "skipped",
        _ => return None,
    };
    // a should-panic test is printed `name - should panic`
    let name = name.split(" - should panic").next().unwrap_or(name);
    Some((name, outcome))
}

/// Per-test outcomes, `None` when the output names no test.
pub fn outcomes(output: &str) -> Option<Vec<(String, &'static str)>> {
    let mut current: Option<String> = None;
    let mut out = Vec::new();
    for line in output.lines() {
        if let Some(b) = binary(line) {
            current = Some(b);
            continue;
        }
        if let (Some(b), Some((name, outcome))) = (&current, outcome_line(line)) {
            out.push((format!("{b}::{name}"), outcome));
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The counts of every binary's `test result:` line, added up; `None`
/// when there is none.
pub fn summary(output: &str) -> Option<Summary> {
    let mut total: Option<Summary> = None;
    for line in output.lines() {
        let Some(rest) = line.trim().strip_prefix("test result: ") else {
            continue;
        };
        let s = total.get_or_insert_with(Summary::default);
        for part in rest.split(['.', ';']) {
            let mut words = part.split_whitespace();
            let (Some(n), Some(what)) = (words.next(), words.next()) else {
                continue;
            };
            let Ok(n) = n.parse::<u64>() else { continue };
            match what {
                "passed" => s.passed += n,
                "failed" => s.failed += n,
                "ignored" => s.skipped += n,
                _ => {}
            }
        }
    }
    total
}

/// The ids `cargo test -- --list` printed: `<name>: test` under each
/// binary. A listing that announced no binary is no listing at all.
pub fn listed(output: &str) -> Result<BTreeSet<String>, String> {
    let mut current: Option<String> = None;
    let mut ids = BTreeSet::new();
    let mut binaries = 0;
    for line in output.lines() {
        if let Some(b) = binary(line) {
            current = Some(b);
            binaries += 1;
            continue;
        }
        if let (Some(b), Some(name)) = (&current, line.strip_suffix(": test")) {
            ids.insert(format!("{b}::{name}"));
        }
    }
    if binaries == 0 {
        let last = output
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("no output");
        return Err(format!(
            "cargo test listed no test binary (the tests may not build): {last}"
        ));
    }
    Ok(ids)
}

#[cfg(test)]
// a test that cannot read its own fixture must fail loudly
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    const RUN: &str = "   Compiling demo v0.1.0 (/w)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.40s
     Running unittests src/lib.rs (target/debug/deps/demo-0f2c)

running 2 tests
test tests::adds ... ok
test tests::panics - should panic ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/adds.rs (target/debug/deps/adds-91aa)

running 3 tests
test adds_two ... FAILED
test slow ... ignored, takes a minute
test adds_zero ... ok

failures:

---- adds_two stdout ----
assertion failed

failures:
    adds_two

test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests demo

running 1 test
test src/lib.rs - add (line 3) ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s
";

    #[test]
    fn each_test_is_named_by_its_binarys_source() {
        let got = outcomes(RUN).expect("outcomes");
        assert_eq!(
            got,
            [
                ("src/lib.rs::tests::adds".to_owned(), "passed"),
                ("src/lib.rs::tests::panics".to_owned(), "passed"),
                ("tests/adds.rs::adds_two".to_owned(), "failed"),
                ("tests/adds.rs::slow".to_owned(), "skipped"),
                ("tests/adds.rs::adds_zero".to_owned(), "passed"),
                (
                    "doc-tests demo::src/lib.rs - add (line 3)".to_owned(),
                    "passed"
                ),
            ]
        );
        assert!(outcomes("error[E0425]: cannot find value `x`").is_none());
    }

    #[test]
    fn the_summary_adds_up_every_binary() {
        let s = summary(RUN).expect("summary");
        assert_eq!((s.passed, s.failed, s.skipped), (4, 1, 1));
        assert!(summary("could not compile `demo`").is_none());
    }

    #[test]
    fn a_listing_names_tests_under_their_binary() {
        let list = "     Running unittests src/lib.rs (target/debug/deps/demo-0f2c)
tests::adds: test

1 test, 0 benchmarks
     Running tests/adds.rs (target/debug/deps/adds-91aa)
adds_two: test
bench_x: benchmark
";
        let ids = listed(list).expect("ids");
        assert_eq!(
            ids.into_iter().collect::<Vec<_>>(),
            ["src/lib.rs::tests::adds", "tests/adds.rs::adds_two"]
        );
        assert!(listed("error: could not compile `demo`").is_err());
    }
}
