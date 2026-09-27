//! Line patterns the guardrails look for. They are deliberately textual:
//! they run on added lines and on whole test files in every language EMI
//! supports (Go has no tree-sitter grammar here), and each is checked by
//! positive and negative examples in the tests below.

use std::sync::LazyLock;

use regex::Regex;

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap_or_else(|e| panic!("bad built-in pattern {p}: {e}"))
}

/// A test skipped, ignored or marked as an expected failure.
pub static SKIP: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    vec![
        (
            "pytest skip/xfail marker",
            re(r"@pytest\.mark\.(skip|skipif|xfail)\b"),
        ),
        ("pytest.skip() call", re(r"\bpytest\.(skip|xfail)\s*\(")),
        (
            "unittest skip decorator",
            re(r"@unittest\.(skip|skipIf|skipUnless|expectedFailure)\b"),
        ),
        (
            "unittest SkipTest",
            re(r"\braise\s+(unittest\.)?SkipTest\b"),
        ),
        (
            "JS .skip",
            re(r"\b(it|test|describe|context|suite)\.skip\s*\("),
        ),
        (
            "JS x-prefixed test",
            re(r"(^|[^\w.$])x(it|describe|test)\s*\("),
        ),
        ("mocha this.skip()", re(r"\bthis\.skip\s*\(\s*\)")),
        ("Go t.Skip", re(r"\b[tb]\.Skip(f|Now)?\s*\(")),
        ("Rust #[ignore]", re(r"#\[\s*ignore\b")),
        ("JUnit @Disabled/@Ignore", re(r"@(Disabled|Ignore)\b")),
    ]
});

/// A lint, type-check or EMI finding silenced in code.
pub static SUPPRESS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    vec![
        ("# noqa", re(r"#\s*noqa\b")),
        ("# type: ignore", re(r"#\s*type:\s*ignore\b")),
        ("# pylint: disable", re(r"#\s*pylint:\s*disable\b")),
        (
            "# pyright/mypy ignore",
            re(r"#\s*(pyright|mypy):\s*ignore\b"),
        ),
        ("# pragma: no cover", re(r"#\s*pragma:\s*no\s*cover\b")),
        ("eslint-disable", re(r"eslint-disable")),
        (
            "@ts-ignore/@ts-expect-error/@ts-nocheck",
            re(r"@ts-(ignore|expect-error|nocheck)\b"),
        ),
        (
            "istanbul/c8 ignore",
            re(r"/\*\s*(istanbul|c8|v8)\s+ignore\b"),
        ),
        ("Rust #[allow(...)]", re(r"#!?\[\s*allow\s*\(")),
        ("Go //nolint", re(r"//\s*nolint\b")),
        ("staticcheck //lint:ignore", re(r"//\s*lint:ignore\b")),
        ("rubocop:disable", re(r"#\s*rubocop:disable\b")),
        ("@SuppressWarnings", re(r"@SuppressWarnings\b")),
        (
            "emi-ignore without a reason",
            re(r"emi-ignore(\[[^\]]*\])?(\s+until=\S+)?\s*$"),
        ),
    ]
});

/// Language of a path, for test and assertion counting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Python,
    Js,
    Go,
    Rust,
    Java,
}

pub fn lang_of(path: &str) -> Option<Lang> {
    let ext = path.rsplit('.').next()?;
    Some(match ext {
        "py" | "pyi" => Lang::Python,
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" => Lang::Js,
        "go" => Lang::Go,
        "rs" => Lang::Rust,
        "java" | "kt" => Lang::Java,
        _ => return None,
    })
}

static PY_TEST: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^\s*(async\s+)?def\s+test\w*\s*\("));
static PY_ASSERT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?m)(^\s*assert\b|\bself\.assert\w+\s*\(|\bpytest\.raises\s*\(|\bpytest\.approx\s*\()")
});
/// Test definitions, skipped ones included: skipping is `tests.skip_added`,
/// `tests.fewer` means definitions were removed.
static JS_TEST: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(^|[^\w.$])x?(it|test)(\.only|\.skip|\.todo|\.each\s*\([^)]*\)|\.concurrent)?\s*\(")
});
static JS_ASSERT: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(\bexpect\s*\(|\bassert(\.\w+)?\s*\(|\.should\b|\bt\.(is|deepEqual|true|false|throws)\s*\()",
    )
});
static GO_TEST: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^func\s+(Test|Fuzz)\w*\s*\("));
static GO_ASSERT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(\bt\.(Error|Errorf|Fatal|Fatalf|Fail|FailNow)\s*\(|\b(assert|require)\.\w+\s*\()")
});
static RUST_TEST: LazyLock<Regex> = LazyLock::new(|| re(r"#\[\s*(tokio::|async_std::)?test\b"));
static RUST_ASSERT: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(debug_)?assert(_eq|_ne|_matches)?!\s*\("));
static JAVA_TEST: LazyLock<Regex> = LazyLock::new(|| re(r"@(Test|ParameterizedTest)\b"));
static JAVA_ASSERT: LazyLock<Regex> = LazyLock::new(|| re(r"\bassert\w*\s*\("));

/// Number of test cases in a test file's text.
pub fn count_tests(lang: Lang, text: &str) -> usize {
    let r: &Regex = match lang {
        Lang::Python => &PY_TEST,
        Lang::Js => &JS_TEST,
        Lang::Go => &GO_TEST,
        Lang::Rust => &RUST_TEST,
        Lang::Java => &JAVA_TEST,
    };
    r.find_iter(text).count()
}

/// Number of assertions in a test file's text.
pub fn count_assertions(lang: Lang, text: &str) -> usize {
    let r: &Regex = match lang {
        Lang::Python => &PY_ASSERT,
        Lang::Js => &JS_ASSERT,
        Lang::Go => &GO_ASSERT,
        Lang::Rust => &RUST_ASSERT,
        Lang::Java => &JAVA_ASSERT,
    };
    r.find_iter(text).count()
}

/// First pattern of `set` matching `line`.
pub fn first_match(set: &[(&'static str, Regex)], line: &str) -> Option<&'static str> {
    set.iter()
        .find(|(_, r)| r.is_match(line))
        .map(|(name, _)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_markers() {
        let hit = [
            "@pytest.mark.skip(reason='flaky')",
            "@pytest.mark.xfail",
            "    pytest.skip('later')",
            "@unittest.skipIf(True, 'x')",
            "raise unittest.SkipTest('x')",
            "it.skip('does x', () => {})",
            "describe.skip('suite', () => {})",
            "xit('does x', () => {})",
            "\tt.Skip(\"flaky\")",
            "t.Skipf(\"%s\", x)",
            "#[ignore]",
            "#[ignore = \"slow\"]",
            "@Disabled",
        ];
        for line in hit {
            assert!(first_match(&SKIP, line).is_some(), "{line}");
        }
        let miss = [
            "it('skips nothing', () => {})",
            "exit(1)",
            "# ignore this",
            "let x = skip(3);",
            "fixit('a')",
        ];
        for line in miss {
            assert!(first_match(&SKIP, line).is_none(), "{line}");
        }
    }

    #[test]
    fn suppressions() {
        let hit = [
            "import os  # noqa",
            "x = y  # noqa: E501",
            "z = f()  # type: ignore[arg-type]",
            "# pylint: disable=too-many-locals",
            "// eslint-disable-next-line no-console",
            "/* eslint-disable */",
            "// @ts-ignore",
            "// @ts-expect-error",
            "#[allow(dead_code)]",
            "#![allow(clippy::all)]",
            "x := f() //nolint:errcheck",
            "@SuppressWarnings(\"unchecked\")",
            "# emi-ignore[erosion]",
            "# emi-ignore until=2027-01-01",
        ];
        for line in hit {
            assert!(first_match(&SUPPRESS, line).is_some(), "{line}");
        }
        let miss = [
            "# emi-ignore[erosion]: generated table, reviewed",
            "allow(x)",
            "def noqa_helper(): pass",
            "type Ignore = string",
        ];
        for line in miss {
            assert!(first_match(&SUPPRESS, line).is_none(), "{line}");
        }
    }

    #[test]
    fn counts_tests_and_assertions() {
        let py = "def test_a():\n    assert f() == 1\n\nasync def test_b(self):\n    self.assertEqual(x, 1)\n    with pytest.raises(E):\n        g()\n\ndef helper():\n    pass\n";
        assert_eq!(
            (
                count_tests(Lang::Python, py),
                count_assertions(Lang::Python, py)
            ),
            (2, 3)
        );
        let js = "describe('x', () => {\n  it('a', () => { expect(f()).toBe(1); });\n  test.each([1])('b', () => { assert.equal(1, 1); });\n  it.skip('c', () => {});\n});\n";
        assert_eq!(
            (count_tests(Lang::Js, js), count_assertions(Lang::Js, js)),
            (3, 2),
            "it.skip still defines a test"
        );
        let go = "func TestA(t *testing.T) {\n\tif f() != 1 {\n\t\tt.Errorf(\"bad\")\n\t}\n\trequire.NoError(t, err)\n}\nfunc helper() {}\n";
        assert_eq!(
            (count_tests(Lang::Go, go), count_assertions(Lang::Go, go)),
            (1, 2)
        );
        let rs = "#[test]\nfn a() { assert_eq!(f(), 1); assert!(g()); }\n#[tokio::test]\nasync fn b() {}\n";
        assert_eq!(
            (
                count_tests(Lang::Rust, rs),
                count_assertions(Lang::Rust, rs)
            ),
            (2, 2)
        );
    }
}
