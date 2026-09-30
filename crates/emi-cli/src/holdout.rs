//! `emi task holdout`: split a Python test file into the part the agent
//! sees and held-out test functions it never does. Code that special-cases
//! the visible tests meets cases it has not seen; the held-out file goes to
//! `task draft --held-out` and is written into the workspace only at verify.

use std::path::{Path, PathBuf};

use clap::Args;
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
    /// Share of the file's top-level test functions to hold out.
    #[arg(long, default_value_t = 0.3)]
    share: f64,
    /// Selects which tests are held out (the same seed, the same split).
    #[arg(long, default_value = "0")]
    seed: String,
}

/// A top-level test function: its lines `[start, end)`, decorators included.
struct Block {
    name: String,
    start: usize,
    end: usize,
}

fn top_level_test(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("async def ")
        .or_else(|| line.strip_prefix("def "))?;
    let end = rest.find('(')?;
    rest.starts_with("test").then(|| rest[..end].to_owned())
}

/// Where the block starting at `def_line` begins: its decorators.
fn block_start(lines: &[&str], def_line: usize) -> usize {
    let mut start = def_line;
    while start > 0 && lines[start - 1].starts_with('@') {
        start -= 1;
    }
    start
}

/// Where the block starting at `def_line` ends: the next line at column 0
/// that is not blank or a comment (trailing blank lines stay with it).
fn block_end(lines: &[&str], def_line: usize) -> usize {
    (def_line + 1..lines.len())
        .find(|&i| {
            let l = lines[i];
            !l.is_empty() && !l.starts_with([' ', '\t', '#', ')', ']', '}'])
        })
        .unwrap_or(lines.len())
}

fn blocks(lines: &[&str]) -> Vec<Block> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            top_level_test(l).map(|name| Block {
                name,
                start: block_start(lines, i),
                end: block_end(lines, i),
            })
        })
        .collect()
}

/// The visible file, the held-out file and the held-out names. Everything
/// that is not a top-level test (imports, helpers, fixtures) is in both.
pub fn split(text: &str, share: f64, seed: &str) -> anyhow::Result<(String, String, Vec<String>)> {
    let lines: Vec<&str> = text.lines().collect();
    let found = blocks(&lines);
    if found.len() < 2 {
        anyhow::bail!(
            "{} top-level test function(s): holding out needs at least two, so one stays visible",
            found.len()
        );
    }
    if !(share > 0.0 && share < 1.0) {
        anyhow::bail!("--share must be between 0 and 1");
    }
    // a seeded order by hash: no generator, the same split every time
    let mut order: Vec<&Block> = found.iter().collect();
    order.sort_by_cached_key(|b| digest("holdout", &json!({"seed": seed, "name": b.name})));
    let n = ((share * found.len() as f64).round() as usize).clamp(1, found.len() - 1);
    let held: Vec<&Block> = order[..n].to_vec();
    let is_held = |i: usize| held.iter().any(|b| (b.start..b.end).contains(&i));
    let is_test = |i: usize| found.iter().any(|b| (b.start..b.end).contains(&i));
    let pick = |keep: &dyn Fn(usize) -> bool| -> String {
        let kept: Vec<&str> = (0..lines.len())
            .filter(|&i| keep(i))
            .map(|i| lines[i])
            .collect();
        kept.join("\n") + "\n"
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

pub fn run(a: &HoldoutArgs) -> anyhow::Result<u8> {
    if inside_repo(&a.file, &a.out) {
        anyhow::bail!(
            "--out {} is inside the repository, where the agent can read it",
            a.out.display()
        );
    }
    let text = std::fs::read_to_string(&a.file)?;
    let (visible, hidden, names) = split(&text, a.share, &a.seed)?;
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
    use super::split;

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
    fn a_file_with_one_test_cannot_be_split() {
        assert!(split("def test_a():\n    pass\n", 0.5, "0").is_err());
    }
}
