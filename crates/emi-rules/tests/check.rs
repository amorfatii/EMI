//! `check_tree` end to end: suppressions, expiry, baselines, test scoping.

use std::collections::BTreeSet;
use std::path::PathBuf;

use emi_rules::{Check, check_tree, load};

struct Tree(PathBuf);

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tree(files: &[(&str, &str)]) -> Tree {
    let root = std::env::temp_dir().join(format!("emi-rules-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (rel, text) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap_or(&root)).unwrap_or_default();
        std::fs::write(p, text).unwrap_or_default();
    }
    Tree(root)
}

#[test]
fn suppressions_baselines_and_test_scoping() {
    let t = tree(&[
        ("app.py", "def f(x=[]):\n    breakpoint()\n    return x\n"),
        (
            "lib.js",
            "debugger; // emi-ignore[js-debugger] until=2030-01-01: demo harness\ndebugger; // emi-ignore[js-debugger]\n",
        ),
        (
            "old.js",
            "// emi-ignore[js-debugger] until=2020-01-01: expired\ndebugger;\n",
        ),
        ("widget.test.js", "it.only('x', () => {});\n"),
        ("widget.js", "it.only('x', () => {});\n"),
    ]);
    let rules = load(&[], true).unwrap_or_default();
    let run = |baseline: Option<&BTreeSet<String>>| {
        let check = Check {
            rules: &rules,
            only: &[],
            exclude_globs: &[],
            today: "2026-09-26".into(),
            baseline,
        };
        check_tree(&t.0, &check).unwrap_or_default()
    };
    let findings = run(None);
    let summary: Vec<(String, String, u32, &str)> = findings
        .iter()
        .map(|f| (f.hit.file.clone(), f.hit.rule.clone(), f.hit.line, f.state))
        .collect();
    let has = |file: &str, rule: &str, line: u32, state: &str| {
        summary
            .iter()
            .any(|s| s.0 == file && s.1 == rule && s.2 == line && s.3 == state)
    };
    assert!(has("app.py", "py-mutable-default", 1, "new"), "{summary:?}");
    assert!(has("app.py", "py-breakpoint", 2, "new"));
    assert!(
        has("lib.js", "js-debugger", 1, "suppressed"),
        "valid suppression with reason"
    );
    assert!(
        has("lib.js", "js-debugger", 2, "new"),
        "no reason: not suppressed"
    );
    assert!(
        has("lib.js", "suppression", 2, "new"),
        "and the reasonless marker is reported"
    );
    assert!(
        has("old.js", "js-debugger", 2, "new") && has("old.js", "suppression", 1, "new"),
        "expired"
    );
    assert!(has("widget.test.js", "js-focused-test", 1, "new"));
    assert!(
        !summary.iter().any(|s| s.0 == "widget.js"),
        "focused-test rules only look at tests"
    );
    let baseline: BTreeSet<String> = findings.iter().map(|f| f.fingerprint.clone()).collect();
    let again = run(Some(&baseline));
    assert!(
        again.iter().all(|f| f.state != "new"),
        "everything is baseline or suppressed"
    );
}
