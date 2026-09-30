//! `emi task holdout`: split a Python test file into the part the agent
//! sees and held-out tests it never does, by test function or by assert
//! statement, or add designed variant cases (`--vary`, see `vary`). Code
//! that special-cases the visible tests meets cases it has not seen; the
//! held-out file goes to `task draft --held-out` and is written into the
//! workspace only at verify.

use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use emi_guard::patterns::{Lang, LineKind, line_kinds};
use emi_ledger::pyfmt::digest;
use serde_json::json;

/// What a split moves: whole test functions, or single asserts.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Unit {
    Tests,
    Asserts,
}

impl Unit {
    fn name(self) -> &'static str {
        match self {
            Unit::Tests => "tests",
            Unit::Asserts => "asserts",
        }
    }
}

#[derive(Args)]
pub struct HoldoutArgs {
    /// The test file to split (rewritten in place without the held-out tests).
    #[arg(long)]
    file: PathBuf,
    /// Where the held-out tests go: outside the repository, where the agent
    /// cannot read them.
    #[arg(long)]
    out: PathBuf,
    /// Share of the file's units (test functions and methods, or asserts) to
    /// hold out.
    #[arg(long, default_value_t = 0.3)]
    share: f64,
    /// Selects which units are held out (the same seed, the same split).
    #[arg(long, default_value = "0")]
    seed: String,
    /// What to hold out: whole test functions, or the assert statements of
    /// functions with at least two (a `check(candidate)` helper, say).
    #[arg(long, value_enum, default_value_t = Unit::Tests)]
    unit: Unit,
    /// Add designed variant cases instead of splitting: new inputs derived
    /// from the visible ones, with expected values from `--reference`.
    #[arg(long)]
    vary: bool,
    /// A trusted reference implementation (a Python file outside the
    /// repository) that supplies the expected values of variant cases.
    #[arg(long)]
    reference: Option<PathBuf>,
    /// The most variant cases to write.
    #[arg(long, default_value_t = 20)]
    count: usize,
}

/// A test function or method: its lines `[start, end)`, decorators
/// included, and its name (`Class.test_x` for a method).
struct Block {
    name: String,
    start: usize,
    end: usize,
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// A test definition on `line`: its indentation and name.
fn test_def(line: &str) -> Option<(usize, String)> {
    let t = line.trim_start();
    let rest = t
        .strip_prefix("async def ")
        .or_else(|| t.strip_prefix("def "))?;
    let end = rest.find('(')?;
    rest.starts_with("test")
        .then(|| (indent(line), rest[..end].to_owned()))
}

/// The decorator ending on the line above `from`, if one does: the line
/// it starts on. A decorator spanning lines ends with its closing bracket
/// at the definition's indentation.
fn decorator_above(lines: &[&str], code: &[bool], from: usize, at: usize) -> Option<usize> {
    let above = from.checked_sub(1)?;
    let t = lines[above].trim_start();
    let at_def = |i: usize| code[i] && indent(lines[i]) == at;
    if at_def(above) && t.starts_with('@') {
        return Some(above);
    }
    if !(at_def(above) && t.starts_with([')', ']', '}'])) {
        return None;
    }
    (0..above)
        .rev()
        .take_while(|&i| !lines[i].trim().is_empty())
        .find(|&i| at_def(i) && !lines[i].trim_start().starts_with([')', ']', '}']))
        .filter(|&i| lines[i].trim_start().starts_with('@'))
}

/// Where the block defined at `def_line` begins: its decorators.
fn block_start(lines: &[&str], code: &[bool], def_line: usize, at: usize) -> usize {
    let mut start = def_line;
    while let Some(d) = decorator_above(lines, code, start, at) {
        start = d;
    }
    start
}

/// Where the block defined at `def_line` ends: the next code line indented
/// no deeper than the definition (blank lines, comments and the inside of
/// multi-line strings stay with it).
fn block_end(lines: &[&str], code: &[bool], def_line: usize, at: usize) -> usize {
    (def_line + 1..lines.len())
        .find(|&i| {
            let t = lines[i].trim_start();
            code[i] && !t.is_empty() && !t.starts_with([')', ']', '}']) && indent(lines[i]) <= at
        })
        .unwrap_or(lines.len())
}

/// The class a method at indentation `at` belongs to, if any.
fn enclosing_class<'a>(
    lines: &[&'a str],
    code: &[bool],
    def_line: usize,
    at: usize,
) -> Option<&'a str> {
    let line = (0..def_line)
        .rev()
        .map(|i| (i, lines[i]))
        .find(|(i, l)| code[*i] && !l.trim().is_empty() && indent(l) < at)?
        .1;
    let rest = line.trim_start().strip_prefix("class ")?;
    let end = rest.find(['(', ':']).unwrap_or(rest.len());
    Some(&rest[..end])
}

fn blocks(text: &str, lines: &[&str]) -> Vec<Block> {
    // multi-line strings (fixture text at column 0, say) are not code
    let code: Vec<bool> = line_kinds(Lang::Python, text)
        .into_iter()
        .map(|k| k != LineKind::InString)
        .collect();
    let code = &code[..];
    let all: Vec<Block> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| code[*i])
        .filter_map(|(i, l)| {
            let (at, name) = test_def(l)?;
            let name = match enclosing_class(lines, code, i, at) {
                Some(class) if at > 0 => format!("{class}.{name}"),
                _ => name,
            };
            Some(Block {
                name,
                start: block_start(lines, code, i, at),
                end: block_end(lines, code, i, at),
            })
        })
        .collect();
    // a test defined inside another test moves with it
    all.iter()
        .filter(|b| !all.iter().any(|o| o.start < b.start && b.end <= o.end))
        .map(|b| Block {
            name: b.name.clone(),
            start: b.start,
            end: b.end,
        })
        .collect()
}

/// `kept` with `pass` added to every class left without a body.
fn fill_empty_classes(kept: Vec<&str>) -> String {
    let mut out: Vec<String> = Vec::with_capacity(kept.len());
    for (i, line) in kept.iter().enumerate() {
        out.push((*line).to_owned());
        let t = line.trim_start();
        if t.starts_with("class ") && t.trim_end().ends_with(':') {
            let next = kept[i + 1..].iter().find(|l| !l.trim().is_empty());
            if next.is_none_or(|n| indent(n) <= indent(line)) {
                out.push(format!("{}    pass", &line[..indent(line)]));
            }
        }
    }
    out.join("\n") + "\n"
}

/// The lines `keep` accepts, with no class left without a body.
fn render(lines: &[&str], keep: impl Fn(usize) -> bool) -> String {
    fill_empty_classes(
        (0..lines.len())
            .filter(|&i| keep(i))
            .map(|i| lines[i])
            .collect(),
    )
}

/// The visible file, the held-out file and the held-out names. Everything
/// that is not a test (imports, helpers, fixtures, class headers, setUp)
/// is in both.
pub fn split(text: &str, share: f64, seed: &str) -> anyhow::Result<(String, String, Vec<String>)> {
    let lines: Vec<&str> = text.lines().collect();
    let found = blocks(text, &lines);
    if found.len() < 2 {
        anyhow::bail!(
            "{} test function(s): holding out needs at least two, so one stays visible",
            found.len()
        );
    }
    if !(share > 0.0 && share < 1.0) {
        anyhow::bail!("--share must be between 0 and 1");
    }
    // a seeded order by hash: no generator, the same split every time
    let mut order: Vec<&Block> = found.iter().collect();
    order.sort_by_cached_key(|b| {
        digest(
            "holdout",
            &json!({"seed": seed, "name": b.name, "line": b.start}),
        )
    });
    let n = ((share * found.len() as f64).round() as usize).clamp(1, found.len() - 1);
    let held: Vec<&Block> = order[..n].to_vec();
    let is_held = |i: usize| held.iter().any(|b| (b.start..b.end).contains(&i));
    let is_test = |i: usize| found.iter().any(|b| (b.start..b.end).contains(&i));
    let visible = render(&lines, |i| !is_held(i));
    let hidden = render(&lines, |i| !is_test(i) || is_held(i));
    let mut names: Vec<String> = held.iter().map(|b| b.name.clone()).collect();
    names.sort();
    Ok((visible, hidden, names))
}

/// A top-level function's assert statements, each as lines `[start, end)`.
struct Asserts {
    name: String,
    units: Vec<(usize, usize)>,
}

/// The line where the header of the definition at `def_line` ends: the
/// first code line ending in `:` (a signature may span lines).
fn header_end(lines: &[&str], code: &[bool], def_line: usize) -> usize {
    (def_line..lines.len())
        .find(|&i| code[i] && lines[i].trim_end().ends_with(':'))
        .unwrap_or(def_line)
}

fn is_assert(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("assert ") || t.starts_with("assert(")
}

/// The asserts written directly in the body of the function whose lines
/// are `[def_line, end)`. An assert runs to the line before the next code
/// line no deeper than itself; blank lines after it stay out of it.
fn body_asserts(lines: &[&str], code: &[bool], def_line: usize, end: usize) -> Vec<(usize, usize)> {
    let first = header_end(lines, code, def_line) + 1;
    let Some(body) = (first..end).find(|&i| code[i] && !lines[i].trim().is_empty()) else {
        return Vec::new();
    };
    let at = indent(lines[body]);
    let is_boundary = |i: usize| {
        let t = lines[i].trim_start();
        code[i] && !t.is_empty() && !t.starts_with([')', ']', '}']) && indent(lines[i]) <= at
    };
    (first..end)
        .filter(|&i| code[i] && indent(lines[i]) == at && is_assert(lines[i]))
        .map(|i| {
            let mut stop = (i + 1..end).find(|&j| is_boundary(j)).unwrap_or(end);
            while stop > i + 1 && lines[stop - 1].trim().is_empty() {
                stop -= 1;
            }
            (i, stop)
        })
        .collect()
}

/// Top-level functions with at least two asserts in their own body.
fn assert_functions(text: &str, lines: &[&str]) -> Vec<Asserts> {
    let code: Vec<bool> = line_kinds(Lang::Python, text)
        .into_iter()
        .map(|k| k != LineKind::InString)
        .collect();
    let code = &code[..];
    lines
        .iter()
        .enumerate()
        .filter(|(i, l)| code[*i] && indent(l) == 0)
        .filter_map(|(i, l)| {
            let rest = l
                .strip_prefix("async def ")
                .or_else(|| l.strip_prefix("def "))?;
            let name = rest[..rest.find('(')?].to_owned();
            let units = body_asserts(lines, code, i, block_end(lines, code, i, 0));
            Some(Asserts { name, units })
        })
        .filter(|f| f.units.len() >= 2)
        .collect()
}

/// Like [`split`], but the units are the asserts of functions holding at
/// least two. Each such function keeps at least one assert on each side;
/// everything else (imports, helpers, callers) is in both files.
pub fn split_asserts(
    text: &str,
    share: f64,
    seed: &str,
) -> anyhow::Result<(String, String, Vec<String>)> {
    let lines: Vec<&str> = text.lines().collect();
    let found = assert_functions(text, &lines);
    if found.is_empty() {
        anyhow::bail!("no function has two or more assert statements to split; try --unit tests");
    }
    if !(share > 0.0 && share < 1.0) {
        anyhow::bail!("--share must be between 0 and 1");
    }
    let mut held_ids: Vec<String> = Vec::new();
    let mut held: Vec<(usize, usize)> = Vec::new();
    for f in &found {
        // the same seeded order by hash as for tests, within each function
        let mut order: Vec<usize> = (0..f.units.len()).collect();
        order.sort_by_cached_key(|&n| {
            digest(
                "holdout",
                &json!({"seed": seed, "name": f.name, "assert": n + 1}),
            )
        });
        let k = ((share * f.units.len() as f64).round() as usize).clamp(1, f.units.len() - 1);
        let mut chosen = order[..k].to_vec();
        chosen.sort_unstable();
        held_ids.extend(chosen.iter().map(|n| format!("{}:{}", f.name, n + 1)));
        held.extend(chosen.iter().map(|&n| f.units[n]));
    }
    let in_range = |set: &[(usize, usize)], i: usize| set.iter().any(|&(s, e)| (s..e).contains(&i));
    let units: Vec<(usize, usize)> = found.iter().flat_map(|f| f.units.clone()).collect();
    let visible = render(&lines, |i| !in_range(&held, i));
    let hidden = render(&lines, |i| !in_range(&units, i) || in_range(&held, i));
    Ok((visible, hidden, held_ids))
}

/// Whether `path` lies inside the git work tree containing `file`.
fn inside_repo(file: &Path, path: &Path) -> bool {
    let dir = file
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let Ok(top) = emi_git::run(dir, &["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    let top = PathBuf::from(String::from_utf8_lossy(&top).trim());
    let target = path.parent().and_then(|p| std::fs::canonicalize(p).ok());
    target.is_some_and(|t| t.starts_with(&top))
}

/// Both halves must parse as Python before either is written: the split
/// works line by line, and a file it cannot split cleanly is refused, never
/// rewritten into one that no longer runs.
fn check_parses(visible: &str, hidden: &str) -> anyhow::Result<()> {
    let code = "import ast, sys\nfor name, text in zip(('visible', 'held-out'), sys.argv[1:]):\n    try:\n        ast.parse(text)\n    except SyntaxError as e:\n        sys.exit(f'the {name} part would not parse (line {e.lineno}): {e.msg}')\n";
    let spec = emi_exec::Spec::new(&["python3", "-c", code, visible, hidden], ".");
    let o = emi_exec::run(&spec);
    if o.status != emi_exec::Status::Exited || o.exit_code != Some(0) {
        anyhow::bail!(
            "not split, nothing written: {}{}",
            o.stderr.trim(),
            o.launch_error
                .as_deref()
                .map_or(String::new(), |e| format!("python3 could not run: {e}"))
        );
    }
    Ok(())
}

/// The `task draft` flag that carries the held-out file into the contract.
pub fn draft_flag(file: &Path, out: &Path) -> String {
    let dest = file.with_file_name(format!(
        "{}_held_out.py",
        file.file_stem()
            .map_or("test".into(), |s| s.to_string_lossy())
    ));
    format!("--held-out {}={}", dest.display(), out.display())
}

/// `--vary`: the flags that belong to it, checked before anything runs.
fn run_vary(a: &HoldoutArgs) -> anyhow::Result<u8> {
    let Some(reference) = &a.reference else {
        anyhow::bail!(
            "designed variants need a trusted reference implementation for their expected values; pass --reference"
        );
    };
    if a.unit == Unit::Asserts {
        anyhow::bail!("--vary adds cases and --unit asserts splits the file: run them separately");
    }
    if inside_repo(&a.file, reference) {
        anyhow::bail!(
            "--reference {} is inside the repository, where the agent can read it",
            reference.display()
        );
    }
    crate::vary::run(&a.file, reference, &a.out, &a.seed, a.count)
}

pub fn run(a: &HoldoutArgs) -> anyhow::Result<u8> {
    if inside_repo(&a.file, &a.out) {
        anyhow::bail!(
            "--out {} is inside the repository, where the agent can read it",
            a.out.display()
        );
    }
    if a.vary {
        return run_vary(a);
    }
    if a.reference.is_some() {
        anyhow::bail!("--reference is for --vary");
    }
    let text = std::fs::read_to_string(&a.file)?;
    let (visible, hidden, names) = match a.unit {
        Unit::Tests => split(&text, a.share, &a.seed)?,
        Unit::Asserts => split_asserts(&text, a.share, &a.seed)?,
    };
    check_parses(&visible, &hidden)?;
    std::fs::write(&a.out, hidden)?;
    std::fs::write(&a.file, visible)?;
    let mut report = json!({"held_out": names, "visible_file": a.file, "held_out_file": a.out,
        "draft_flag": draft_flag(&a.file, &a.out),
        "next": "commit the visible file as the task baseline before the agent starts"});
    if a.unit != Unit::Tests {
        report["unit"] = json!(a.unit.name());
    }
    println!("{report}");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::{check_parses, split, split_asserts};

    const FILE: &str = "import pytest\nfrom app import add\n\n\ndef helper():\n    return 1\n\n\ndef test_a():\n    assert add(1, 2) == 3\n\n\n@pytest.mark.slow\ndef test_b():\n    assert add(2, 2) == 4\n\n\ndef test_c():\n    assert add(0, 0) == 0\n\n\ndef test_d():\n    assert add(5, 5) == 10\n";

    #[test]
    fn held_out_tests_leave_the_visible_file_and_keep_imports_and_helpers() {
        let (visible, hidden, names) = split(FILE, 0.5, "7").unwrap_or_default();
        assert_eq!(names.len(), 2, "{names:?}");
        for n in &names {
            assert!(
                hidden.contains(&format!("def {n}(")) && !visible.contains(&format!("def {n}("))
            );
        }
        for part in [&visible, &hidden] {
            assert!(
                part.contains("import pytest") && part.contains("def helper():"),
                "{part}"
            );
        }
        assert_eq!(
            visible.matches("def test_").count() + hidden.matches("def test_").count(),
            4
        );
    }

    #[test]
    fn a_decorator_moves_with_its_test() {
        for seed in 0..8 {
            let (visible, hidden, names) = split(FILE, 0.5, &seed.to_string()).unwrap_or_default();
            let (with_b, without_b) = if names.iter().any(|n| n == "test_b") {
                (&hidden, &visible)
            } else {
                (&visible, &hidden)
            };
            assert!(with_b.contains("@pytest.mark.slow\ndef test_b"), "{with_b}");
            assert!(!without_b.contains("@pytest.mark.slow"), "{without_b}");
        }
    }

    #[test]
    fn the_same_seed_gives_the_same_split_and_one_test_always_stays_visible() {
        assert_eq!(split(FILE, 0.5, "s").ok(), split(FILE, 0.5, "s").ok());
        let (visible, _, names) = split(FILE, 0.99, "s").unwrap_or_default();
        assert_eq!(names.len(), 3);
        assert_eq!(visible.matches("def test_").count(), 1);
    }

    #[test]
    fn methods_split_inside_their_classes_and_no_class_is_left_empty() {
        let file = "import unittest\n\n\nclass T(unittest.TestCase):\n    def setUp(self):\n        self.x = 1\n\n    def test_a(self):\n        self.assertEqual(self.x, 1)\n\n    def test_b(self):\n        self.assertTrue(self.x)\n\n\nclass U(unittest.TestCase):\n    def test_c(self):\n        pass\n";
        let (visible, hidden, names) = split(file, 0.5, "1").unwrap_or_default();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(
            names
                .iter()
                .all(|n| n.starts_with("T.") || n.starts_with("U.")),
            "{names:?}"
        );
        for part in [&visible, &hidden] {
            assert!(part.contains("def setUp(self):"), "{part}");
            for class in ["class T(", "class U("] {
                let at = part.find(class).unwrap_or_default();
                let body = part[at..].lines().nth(1).unwrap_or_default();
                assert!(
                    body.starts_with("    "),
                    "class {class} left without a body:\n{part}"
                );
            }
        }
    }

    #[test]
    fn a_decorator_spanning_lines_moves_with_its_test() {
        let file = "class T:\n    @override(\n        X=1,\n    )\n    def test_a(self):\n        pass\n\n    def test_b(self):\n        pass\n";
        for seed in 0..6 {
            let (visible, hidden, names) = split(file, 0.5, &seed.to_string()).unwrap_or_default();
            let with_a = if names.iter().any(|n| n == "T.test_a") {
                &hidden
            } else {
                &visible
            };
            assert!(
                with_a.contains("    @override(\n        X=1,\n    )\n    def test_a"),
                "{with_a}"
            );
            assert_eq!(
                visible.matches("@override").count() + hidden.matches("@override").count(),
                1
            );
        }
    }

    #[test]
    fn a_split_that_would_not_parse_is_refused() {
        assert!(check_parses("x = 1\n", "y = 2\n").is_ok());
        let err = check_parses("x = 1\n", "class A:\n")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("held-out part would not parse"), "{err}");
    }

    #[test]
    fn a_file_with_one_test_cannot_be_split() {
        assert!(split("def test_a():\n    pass\n", 0.5, "0").is_err());
    }

    const LCB: &str = "from solution import add\n\n\ndef check(candidate):\n    assert candidate(1, 2) == 3\n    assert candidate(2, 2) == 4\n    assert candidate(0, 0) == 0\n    assert candidate(5, 5) == 10\n    assert candidate(-1, 1) == 0\n\n\ndef test_add():\n    check(add)\n\n\ntest_add()\n";

    fn asserts_in(part: &str) -> usize {
        part.matches("    assert candidate(").count()
    }

    #[test]
    fn asserts_split_keeps_both_halves_runnable_and_each_with_an_assert() {
        let (visible, hidden, names) = split_asserts(LCB, 0.4, "3").unwrap_or_default();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.iter().all(|n| n.starts_with("check:")), "{names:?}");
        assert_eq!(asserts_in(&visible) + asserts_in(&hidden), 5);
        assert!(asserts_in(&visible) >= 1 && asserts_in(&hidden) >= 1);
        for part in [&visible, &hidden] {
            assert!(part.contains("from solution import add"), "{part}");
            assert!(part.contains("def test_add():\n    check(add)"), "{part}");
            assert!(part.contains("\ntest_add()\n"), "{part}");
        }
        assert!(check_parses(&visible, &hidden).is_ok());
        let held_ids: Vec<usize> = names
            .iter()
            .filter_map(|n| n.rsplit(':').next()?.parse().ok())
            .collect();
        let src: Vec<&str> = LCB.lines().filter(|l| l.contains("assert ")).collect();
        for (i, line) in src.iter().enumerate() {
            let in_hidden = hidden.contains(line);
            assert_eq!(in_hidden, held_ids.contains(&(i + 1)), "{line}");
            assert_eq!(visible.contains(line), !in_hidden, "{line}");
        }
    }

    #[test]
    fn asserts_split_is_repeatable_and_leaves_short_functions_alone() {
        assert_eq!(
            split_asserts(LCB, 0.5, "s").ok(),
            split_asserts(LCB, 0.5, "s").ok()
        );
        let file = format!("{LCB}\n\ndef test_one():\n    assert 1 + 1 == 2\n");
        let (visible, hidden, names) = split_asserts(&file, 0.5, "s").unwrap_or_default();
        assert!(names.iter().all(|n| n.starts_with("check:")), "{names:?}");
        for part in [&visible, &hidden] {
            assert!(
                part.contains("def test_one():\n    assert 1 + 1 == 2\n"),
                "{part}"
            );
        }
        let (visible, _, names) = split_asserts(LCB, 0.99, "s").unwrap_or_default();
        assert_eq!(names.len(), 4);
        assert_eq!(asserts_in(&visible), 1);
    }

    #[test]
    fn a_multi_line_assert_moves_as_one_unit() {
        let file = "def check(f):\n    assert f(\n        1,\n        2,\n    ) == 3, \"sum\"\n\n    assert f(0, 0) == 0\n";
        for seed in 0..6 {
            let (visible, hidden, names) =
                split_asserts(file, 0.5, &seed.to_string()).unwrap_or_default();
            assert_eq!(names.len(), 1, "{names:?}");
            let (with, without) = if names[0] == "check:1" {
                (&hidden, &visible)
            } else {
                (&visible, &hidden)
            };
            assert!(
                with.contains("    assert f(\n        1,\n        2,\n    ) == 3, \"sum\"\n"),
                "{with}"
            );
            assert!(!without.contains("1,"), "{without}");
            assert!(check_parses(&visible, &hidden).is_ok());
        }
    }

    #[test]
    fn a_file_with_no_function_of_two_asserts_is_refused() {
        let err = |text: &str| {
            split_asserts(text, 0.5, "0")
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default()
        };
        assert!(
            err("def test_a():\n    assert 1\n\n\ndef test_b():\n    assert 2\n")
                .contains("--unit tests")
        );
        assert!(err("x = 1\n").contains("--unit tests"));
    }

    #[test]
    fn nested_asserts_are_not_units() {
        let file = "def check(f):\n    for x in (1, 2):\n        assert f(x) == x\n    if f:\n        assert f(3) == 3\n    assert f(4) == 4\n";
        assert!(split_asserts(file, 0.5, "0").is_err());
        let file = format!("{file}    assert f(5) == 5\n");
        let (visible, hidden, names) = split_asserts(&file, 0.5, "0").unwrap_or_default();
        assert_eq!(names.len(), 1, "{names:?}");
        for part in [&visible, &hidden] {
            assert!(
                part.contains("assert f(x) == x") && part.contains("assert f(3) == 3"),
                "{part}"
            );
        }
    }

    #[test]
    fn asserts_in_strings_and_methods_are_not_units() {
        let file = "def check(f):\n    text = \"\"\"\n    assert f(1) == 1\n    assert f(2) == 2\n    \"\"\"\n    assert f(3) == 3\n";
        assert!(split_asserts(file, 0.5, "0").is_err());
        let file = "class T:\n    def test_a(self):\n        assert 1\n        assert 2\n";
        assert!(split_asserts(file, 0.5, "0").is_err());
    }
}
