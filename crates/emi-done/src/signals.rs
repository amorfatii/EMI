//! Escape-hatch signals: lines the change added to product code that are a
//! known way to make checks pass without the code doing what they check.
//! They are patterns, not proof: code written another way passes them,
//! and a line that matches may be fine. So they are reported for a person
//! and never change the action, and the receipt labels them `pattern`.

use std::path::Path;

use serde_json::{Value, json};

/// (kind, what it may mean, whether `line` shows it).
type Rule = (&'static str, &'static str, fn(&str) -> bool);

fn code(line: &str) -> &str {
    line.split('#').next().unwrap_or("")
}

const RULES: &[Rule] = &[
    ("suppression", "switches a checker off for this line", |l| {
        l.contains("# type: ignore") || l.contains("# noqa") || l.contains("pragma: no cover")
    }),
    (
        "dynamic_code",
        "runs code built at run time, which no reading of the source covers",
        |l| {
            let c = code(l);
            c.contains("eval(") || c.contains("exec(")
        },
    ),
    (
        "test_sniffing",
        "asks whether a test runner is loaded: product code can then behave one way under test",
        |l| {
            let c = code(l);
            c.contains("PYTEST_CURRENT_TEST")
                || c.contains("\"pytest\" in sys.modules")
                || c.contains("'pytest' in sys.modules")
                || c.contains("\"hypothesis\" in sys.modules")
                || c.contains("'hypothesis' in sys.modules")
                || c.trim_start().starts_with("import pytest")
                || c.trim_start().starts_with("import hypothesis")
        },
    ),
];

/// Whether an added line defines an `__eq__` that cannot be anything but
/// true: every equality law about such objects holds whatever they hold.
fn always_equal(lines: &[(u32, String)], at: usize) -> bool {
    let def = lines[at].1.trim_start();
    if !def.starts_with("def __eq__") {
        return false;
    }
    if def.contains("return True") {
        return true;
    }
    lines
        .get(at + 1)
        .is_some_and(|(n, next)| *n == lines[at].0 + 1 && next.trim() == "return True")
}

fn signal(kind: &str, meaning: &str, file: &str, (line, text): &(u32, String)) -> Value {
    json!({"kind": kind, "file": file, "line": line, "text": text.trim(), "means": meaning,
           "assurance": "pattern"})
}

/// The signals in the lines the change from `baseline` to `rev` added to
/// Python product files (not tests, not laws), or why they could not be
/// looked for.
pub fn find(
    view: &Path,
    (baseline, rev): (&str, &str),
    is_test: impl Fn(&str) -> bool,
) -> Result<Vec<Value>, String> {
    let files = emi_tree::hunks::changes(view, baseline, Some(rev))
        .map_err(|e| format!("the change's files could not be read: {e}"))?;
    let mut out = Vec::new();
    for f in files
        .iter()
        .filter(|f| f.path.ends_with(".py") && !is_test(&f.path))
    {
        for (i, added) in f.added.iter().enumerate() {
            for (kind, meaning, matches) in RULES {
                if matches(&added.1) {
                    out.push(signal(kind, meaning, &f.path, added));
                }
            }
            if always_equal(&f.added, i) {
                out.push(signal(
                    "always_equal",
                    "an __eq__ that returns True whatever it compares: every equality law holds",
                    &f.path,
                    added,
                ));
            }
        }
    }
    Ok(out)
}

/// The receipt's limits line for the signals, when there are any or they
/// could not be looked for.
pub fn limits(signals: &Result<Vec<Value>, String>) -> Vec<String> {
    let signals = match signals {
        Ok(found) if found.is_empty() => return Vec::new(),
        Ok(found) => found,
        Err(why) => {
            return vec![format!("Escape-hatch signals were not looked for: {why}.")];
        }
    };
    let first = &signals[0];
    vec![format!(
        "{} escape-hatch signal(s) in the change's product code, found by pattern and not acted \
         on (first: {}:{} {}).",
        signals.len(),
        first["file"].as_str().unwrap_or("?"),
        first["line"],
        first["kind"].as_str().unwrap_or("?")
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str) -> Vec<&'static str> {
        RULES
            .iter()
            .filter(|(_, _, m)| m(line))
            .map(|(k, _, _)| *k)
            .collect()
    }

    #[test]
    fn each_rule_and_what_it_leaves_alone() {
        assert_eq!(kinds("x = f()  # type: ignore"), ["suppression"]);
        assert_eq!(kinds("    return eval(text)"), ["dynamic_code"]);
        assert_eq!(kinds("if 'pytest' in sys.modules:"), ["test_sniffing"]);
        assert_eq!(
            kinds("if os.environ.get('PYTEST_CURRENT_TEST'):"),
            ["test_sniffing"]
        );
        assert!(kinds("x = evaluate(y)  # not eval( really").is_empty());
        assert!(kinds("# eval(...) is never used here").is_empty());
        assert!(kinds("return a + b").is_empty());
    }

    #[test]
    fn an_eq_that_returns_true_is_always_equal() {
        let one = vec![(3, "    def __eq__(self, other): return True".to_owned())];
        assert!(always_equal(&one, 0));
        let two = vec![
            (3, "    def __eq__(self, other):".to_owned()),
            (4, "        return True".to_owned()),
        ];
        assert!(always_equal(&two, 0));
        let real = vec![
            (3, "    def __eq__(self, other):".to_owned()),
            (4, "        return self.id == other.id".to_owned()),
        ];
        assert!(!always_equal(&real, 0));
    }

    #[test]
    fn signals_that_could_not_be_looked_for_are_said_not_found() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let found = find(dir.path(), ("base", "rev"), |_| false);
        assert!(found.is_err(), "{found:?}");
        let lines = limits(&found);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("Escape-hatch signals were not looked for"));
        assert!(limits(&Ok(Vec::new())).is_empty());
    }

    #[test]
    fn ast_signals_ignore_string_literals_and_catch_multiline_escapes() {
        let src = br#"
msg1 = "never call eval(x) or exec(y)"
msg2 = "use # type: ignore if needed"
msg3 = "PYTEST_CURRENT_TEST in strings is fine"
from pytest import fixture

class Sneaky:
    def __eq__(
        self,
        other: object,
    ) -> bool:
        """Always equal."""
        return True
"#;
        let added: std::collections::BTreeSet<u32> = (1..=14).collect();
        let found: Vec<(u32, &str)> = emi_python::syntax::ast_signals(src, &added)
            .into_iter()
            .map(|(l, k, _)| (l, k))
            .collect();
        assert_eq!(found, vec![(5, "test_sniffing"), (8, "always_equal")]);
    }
}
