//! Shaken reruns: every check that passed runs again with the trusted
//! Python tests' functions and classes renamed, from a workspace at a
//! different path, under a different hash seed. Honest code cannot tell
//! the difference; code that keys on how it is being tested can, and a
//! check that stops passing needs review.

use std::collections::BTreeMap;
use std::path::Path;

use emi_ledger::pyfmt::digest;
use serde_json::{Value, json};

use super::{RunSetup, run_check};
use crate::contract::Result;
use crate::store::Store;

/// A test definition's name on `line` (`def test_x(`, `async def test_x(`,
/// `class TestX`), if it is one.
fn defined_name(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let rest = t
        .strip_prefix("async def ")
        .or_else(|| t.strip_prefix("def "))
        .filter(|r| r.starts_with("test"))
        .or_else(|| t.strip_prefix("class ").filter(|r| r.starts_with("Test")))?;
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

fn is_word(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// How often `name` occurs in `text` as a whole identifier.
fn occurrences(text: &str, name: &str) -> usize {
    text.match_indices(name)
        .filter(|(i, _)| {
            !is_word(text[..*i].chars().next_back())
                && !is_word(text[i + name.len()..].chars().next())
        })
        .count()
}

/// `text` with each test defined once and named nowhere else (in the file,
/// in `elsewhere`: the other trusted files and the check command) renamed
/// to a name derived from `tag`; the number renamed. A name used anywhere
/// else keeps its name, so nothing that imports or selects it breaks.
pub(super) fn rename(text: &str, tag: &str, elsewhere: &str) -> (String, usize) {
    let names: Vec<&str> = text.lines().filter_map(defined_name).collect();
    let mut out = text.to_owned();
    let mut n = 0;
    for name in names {
        if occurrences(text, name) != 1 || occurrences(elsewhere, name) != 0 {
            continue;
        }
        let fresh = if name.starts_with("Test") {
            format!("TestS{tag}{n}")
        } else {
            format!("test_s{tag}{n}")
        };
        out = out.replacen(name, &fresh, 1);
        n += 1;
    }
    (out, n)
}

/// A test module by name (`test_x.py`, `x_test.py`, `tests.py`): helpers,
/// models and URL modules beside the tests keep every name.
fn test_module(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name == "tests.py"
        || (name.ends_with(".py") && (name.starts_with("test_") || name.ends_with("_test.py")))
}

/// Rename the tests in the workspace's trusted Python test modules, keeping
/// any name another trusted Python file or the command mentions.
fn rename_in(workspace: &Path, files: &[String], tag: &str, command: &str) -> usize {
    let texts: Vec<(&String, String)> = files
        .iter()
        .filter(|f| f.ends_with(".py"))
        .filter_map(|f| Some((f, std::fs::read_to_string(workspace.join(f)).ok()?)))
        .collect();
    let mut renamed = 0;
    for (i, (f, text)) in texts.iter().enumerate() {
        if !test_module(f) {
            continue;
        }
        let others = texts
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, (_, t))| t.as_str());
        let elsewhere: String = std::iter::once(command)
            .chain(others)
            .collect::<Vec<_>>()
            .join("\n");
        let (out, n) = rename(text, tag, &elsewhere);
        if n > 0 && std::fs::write(workspace.join(f), out).is_ok() {
            renamed += n;
        }
    }
    renamed
}

/// The execution policy with a hash seed of its own, unless the contract
/// fixed one.
fn shaken_execution(execution: &Value, tag: &str) -> Value {
    let mut e = execution.clone();
    if e["env"]["PYTHONHASHSEED"].is_null() {
        let seed = u32::from_str_radix(&tag[..6], 16).unwrap_or(1) % 4_294_967_295 + 1;
        if !e["env"].is_object() {
            e["env"] = json!({});
        }
        e["env"]["PYTHONHASHSEED"] = json!(seed.to_string());
    }
    e
}

/// One shaken rerun of one check: `(status, tests renamed)`.
fn rerun(
    store: &Store,
    setup: &RunSetup,
    scratch: &Path,
    (id, def): (&str, &Value),
    (round, candidate_id): (u64, &str),
) -> Result<(String, usize)> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tag = digest(
        "shake",
        &json!({"check": id, "round": round, "nanos": nanos.to_string()}),
    );
    let tag = &tag[tag.len() - 10..];
    // another depth and name than the check's own workspace
    let dir = scratch
        .join(format!("s{}", &tag[..3]))
        .join(format!("w{}", &tag[3..8]));
    std::fs::create_dir_all(dir.parent().unwrap_or(scratch)).map_err(|e| e.to_string())?;
    let mounts = setup.prepare(&dir)?;
    let command = def["command"].to_string();
    let renamed = rename_in(&dir, &setup.trusted_files, &tag[..6], &command);
    let execution = shaken_execution(&setup.contract["execution"], tag);
    let (status, _, mut record) = run_check(id, def, &dir, &execution, mounts);
    let _ = std::fs::remove_dir_all(&dir);
    record["candidate_id"] = json!(candidate_id);
    record["contract_id"] = setup.contract["contract_id"].clone();
    record["shake_round"] = json!(round);
    record["tests_renamed"] = json!(renamed);
    store.append("check_run", &record)?;
    Ok((status, renamed))
}

/// The shaken reruns of every passing check, as part of the verification
/// gate: `pass`, `disagree` (a check failed when shaken) or `unverified`
/// (a rerun could not complete).
pub(super) fn gate(
    store: &Store,
    setup: &RunSetup,
    scratch: &Path,
    statuses: &BTreeMap<String, String>,
    candidate_id: &str,
) -> Result<Value> {
    let rounds = setup.contract["verification"]["shake"]["rounds"]
        .as_u64()
        .unwrap_or(0);
    if rounds == 0 {
        return Ok(json!({"state": "not_required"}));
    }
    let checks = setup.contract["checks"].as_object();
    let passed = checks
        .into_iter()
        .flatten()
        .filter(|(id, _)| statuses.get(*id).is_some_and(|s| s == "passed"));
    let mut changed = Vec::new();
    let mut renamed = 0;
    for (id, def) in passed {
        for round in 0..rounds {
            let (status, n) = rerun(store, setup, scratch, (id, def), (round, candidate_id))?;
            renamed += n;
            if status != "passed" {
                changed.push(json!({"check": id, "round": round, "status": status}));
            }
        }
    }
    let state = if changed.iter().any(|c| c["status"] == "failed") {
        "disagree"
    } else if changed.is_empty() {
        "pass"
    } else {
        "unverified"
    };
    Ok(json!({"state": state, "rounds": rounds, "tests_renamed": renamed, "changed": changed}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tests_named_once_are_renamed_and_names_used_elsewhere_are_kept() {
        let text = "def test_a():\n    pass\n\nasync def test_b():\n    pass\n\nclass TestC:\n    def test_d(self):\n        test_b()\n";
        let (out, n) = rename(text, "abc", "[\"pytest\"]");
        assert_eq!(n, 3, "{out}");
        assert!(
            out.contains("def test_sabc0()")
                && out.contains("class TestSabc")
                && out.contains("test_b()")
        );
        assert!(!out.contains("test_a") && !out.contains("test_d"), "{out}");
    }

    #[test]
    fn a_test_the_command_names_keeps_its_name() {
        let (out, n) = rename(
            "def test_a():\n    pass\n",
            "abc",
            "[\"pytest\",\"t.py::test_a\"]",
        );
        assert_eq!((n, out.as_str()), (0, "def test_a():\n    pass\n"));
    }

    #[test]
    fn a_name_another_file_imports_keeps_its_name_and_helper_modules_are_left_alone() {
        let dir = std::env::temp_dir().join(format!("emi-shake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("tests")).unwrap_or_default();
        let models = "class TestModel:\n    pass\n";
        let tests = "from .models import TestModel\n\ndef test_a():\n    assert TestModel\n";
        std::fs::write(dir.join("tests/models.py"), models).unwrap_or_default();
        std::fs::write(dir.join("tests/test_x.py"), tests).unwrap_or_default();
        let files = vec!["tests/models.py".to_owned(), "tests/test_x.py".to_owned()];
        let n = rename_in(&dir, &files, "abc", "[]");
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_default();
        assert_eq!((n, read("tests/models.py")), (1, models.to_owned()));
        assert!(
            read("tests/test_x.py").contains("def test_sabc0()")
                && read("tests/test_x.py").contains("TestModel")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_matches_are_not_occurrences() {
        assert_eq!(occurrences("test_a test_ab xtest_a test_a", "test_a"), 2);
    }
}
