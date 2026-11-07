//! Escape-hatch signals in Python: lines a change added to product code
//! that are a known way to make checks pass without the code doing what
//! they check. They are patterns, not proof: code written another way
//! passes them, and a line that matches may be fine.

use emi_driver::Signal;

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

const ALWAYS_EQUAL: &str =
    "an __eq__ that returns True whatever it compares: every equality law holds";

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

/// The signals in `added`, the lines a change added to one file, with
/// their line numbers.
pub fn find(added: &[(u32, String)]) -> Vec<Signal> {
    let mut out = Vec::new();
    for (i, (line, text)) in added.iter().enumerate() {
        for (kind, means, matches) in RULES {
            if matches(text) {
                out.push(Signal {
                    line: *line,
                    kind,
                    means,
                });
            }
        }
        if always_equal(added, i) {
            out.push(Signal {
                line: *line,
                kind: "always_equal",
                means: ALWAYS_EQUAL,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str) -> Vec<&'static str> {
        find(&[(1, line.to_owned())])
            .into_iter()
            .map(|s| s.kind)
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
}
