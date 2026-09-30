//! `emi task holdout`: split a Python test file into the part the agent
//! sees and held-out test functions it never does. Code that special-cases
//! the visible tests meets cases it has not seen; the held-out file goes to
//! `task draft --held-out` and is written into the workspace only at verify.

use std::path::{Path, PathBuf};

use clap::Args;
use emi_guard::patterns::{Lang, LineKind, line_kinds};
use emi_ledger::pyfmt::digest;
use serde_json::json;

#[derive(Args)]
pub struct HoldoutArgs {
    /// The test file to split (rewritten in place without the held-out tests).
    #[arg(long)]
    file: PathBuf,
    /// Where the held-out tests go: outside the repository, where the agent
    /// cannot read them.
    #[arg(long)]
    out: PathBuf,
    /// Share of the file's test functions and methods to hold out.
    #[arg(long, default_value_t = 0.3)]
    share: f64,
    /// Selects which tests are held out (the same seed, the same split).
    #[arg(long, default_value = "0")]
    seed: String,
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
    let pick = |keep: &dyn Fn(usize) -> bool| -> String {
        fill_empty_classes(
            (0..lines.len())
                .filter(|&i| keep(i))
                .map(|i| lines[i])
                .collect(),
        )
    };
    let visible = pick(&|i| !is_held(i));
    let hidden = pick(&|i| !is_test(i) || is_held(i));
    let mut names: Vec<String> = held.iter().map(|b| b.name.clone()).collect();
    names.sort();
    Ok((visible, hidden, names))
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

pub fn run(a: &HoldoutArgs) -> anyhow::Result<u8> {
    if inside_repo(&a.file, &a.out) {
        anyhow::bail!(
            "--out {} is inside the repository, where the agent can read it",
            a.out.display()
        );
    }
    let text = std::fs::read_to_string(&a.file)?;
    let (visible, hidden, names) = split(&text, a.share, &a.seed)?;
    check_parses(&visible, &hidden)?;
    std::fs::write(&a.out, hidden)?;
    std::fs::write(&a.file, visible)?;
    let dest = a.file.with_file_name(format!(
        "{}_held_out.py",
        a.file
            .file_stem()
            .map_or("test".into(), |s| s.to_string_lossy())
    ));
    println!(
        "{}",
        json!({"held_out": names, "visible_file": a.file, "held_out_file": a.out,
               "draft_flag": format!("--held-out {}={}", dest.display(), a.out.display()),
               "next": "commit the visible file as the task baseline before the agent starts"})
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::{check_parses, split};

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
}
