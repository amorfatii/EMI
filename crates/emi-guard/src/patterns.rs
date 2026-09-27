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

/// `line` with the contents of its string literals removed (quotes kept),
/// so a marker quoted in a test name or fixture string is not a marker.
pub fn strip_strings(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in line.chars() {
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                    out.push(c);
                }
            }
            None => {
                if matches!(c, '"' | '\'' | '`') {
                    quote = Some(c);
                }
                out.push(c);
            }
        }
    }
    out
}

/// What a line of source starts as, for telling a marker in code from one
/// in fixture text or prose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Code,
    /// Starts with a line comment, or inside a block comment.
    Comment,
    /// Starts inside a string literal that spans lines: a Python
    /// triple-quoted string, a JS template literal, a Go raw string, ...
    InString,
}

/// Lexer state between characters.
#[derive(Clone, Copy, PartialEq, Eq)]
enum St {
    Code,
    Str(char),
    Triple(char),
    Block,
}

/// Comment and quote syntax of a language.
struct Syntax {
    hash_comment: bool,
    slash_comments: bool,
    triple: bool,
    /// Quotes whose strings may span lines.
    multiline: &'static [char],
    /// Quotes whose strings end at the line (apostrophes that are not
    /// always quotes, as in Rust lifetimes, are left out).
    single: &'static [char],
}

fn syntax(lang: Lang) -> Syntax {
    let (hash_comment, triple, multiline, single): (bool, bool, &[char], &[char]) = match lang {
        Lang::Python => (true, true, &[], &['"', '\'']),
        Lang::Js | Lang::Go => (false, false, &['`'], &['"', '\'']),
        Lang::Rust => (false, false, &['"'], &[]),
        Lang::Java => (false, true, &[], &['"', '\'']),
    };
    Syntax {
        hash_comment,
        slash_comments: !hash_comment,
        triple,
        multiline,
        single,
    }
}

/// The kind of each line of `text` (index 0 is line 1). Approximate by
/// design: it only has to tell code from comments and multi-line strings.
pub fn line_kinds(lang: Lang, text: &str) -> Vec<LineKind> {
    let syn = syntax(lang);
    let mut st = St::Code;
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        out.push(match st {
            St::Triple(_) | St::Str(_) => LineKind::InString,
            St::Block => LineKind::Comment,
            St::Code
                if (syn.hash_comment && t.starts_with('#'))
                    || (syn.slash_comments && (t.starts_with("//") || t.starts_with("/*"))) =>
            {
                LineKind::Comment
            }
            St::Code => LineKind::Code,
        });
        st = lex_line(&syn, line, st);
    }
    out
}

/// Lexer state after `line`, starting from `st`.
fn lex_line(syn: &Syntax, line: &str, mut st: St) -> St {
    let c: Vec<char> = line.chars().collect();
    let three = |i: usize, q: char| c.get(i..i + 3).is_some_and(|w| w.iter().all(|&x| x == q));
    let mut i = 0;
    while i < c.len() {
        let next = c.get(i + 1).copied();
        match st {
            St::Code if (syn.hash_comment && c[i] == '#') => break,
            St::Code if syn.slash_comments && c[i] == '/' && next == Some('/') => break,
            St::Code if syn.slash_comments && c[i] == '/' && next == Some('*') => {
                st = St::Block;
                i += 1;
            }
            St::Code if syn.triple && (three(i, '"') || three(i, '\'')) => {
                st = St::Triple(c[i]);
                i += 2;
            }
            St::Code if syn.multiline.contains(&c[i]) || syn.single.contains(&c[i]) => {
                st = St::Str(c[i]);
            }
            St::Str(_) | St::Triple(_) if c[i] == '\\' => i += 1,
            St::Str(q) if c[i] == q => st = St::Code,
            St::Triple(q) if three(i, q) => {
                st = St::Code;
                i += 2;
            }
            St::Block if c[i] == '*' && next == Some('/') => {
                st = St::Code;
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    match st {
        St::Str(q) if !syn.multiline.contains(&q) => St::Code,
        other => other,
    }
}

/// First pattern of `set` matching `line` outside string literals.
pub fn first_match(set: &[(&'static str, Regex)], line: &str) -> Option<&'static str> {
    let code = strip_strings(line);
    set.iter()
        .find(|(_, r)| r.is_match(&code))
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
            "it(\"should run with conditional `this.skip()`\", function (done) {",
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
            "  \"test.js\": \"/* eslint-disable eqeqeq */\",",
            "type Ignore = string",
        ];
        for line in miss {
            assert!(first_match(&SUPPRESS, line).is_none(), "{line}");
        }
    }

    #[test]
    fn line_kinds_tell_code_from_fixture_text() {
        use LineKind::{Code, Comment, InString};
        let py = "def test_x(testdir):\n    testdir.makepyfile(\n        \"\"\"\n        @pytest.mark.skip\n        def test_a(): pass\n    \"\"\"\n    )\n# @unittest.skipIf here\n@pytest.mark.skip\ns = '''one'''\n@x\n";
        assert_eq!(
            line_kinds(Lang::Python, py),
            vec![
                Code, Code, Code, InString, InString, InString, Code, Comment, Code, Code, Code
            ]
        );
        let js = "const src = `\nit.skip('a', () => {})\n`;\n/* block\n xit('b')\n*/\nit.skip('c')\nconst s = \"`\";\nx()\n";
        assert_eq!(
            line_kinds(Lang::Js, js),
            vec![
                Code, InString, InString, Comment, Comment, Comment, Code, Code, Code
            ]
        );
        let rs = "fn f<'a>(x: &'a str) {}\nlet s = \"a\n#[ignore]\nb\";\n#[ignore]\n";
        assert_eq!(
            line_kinds(Lang::Rust, rs),
            vec![Code, Code, InString, InString, Code]
        );
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
