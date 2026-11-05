//! `functions::changed_functions` on scratch repositories.

use std::path::{Path, PathBuf};
use std::process::Command;

use emi_python::functions::{ChangeKind, ChangedFunctions, changed_functions};

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn write(repo: &Repo, name: &str, text: &str) {
    let path = repo.0.join(name);
    std::fs::create_dir_all(path.parent().unwrap_or(&repo.0)).unwrap_or_default();
    std::fs::write(path, text).unwrap_or_default();
}

fn commit(repo: &Repo, files: &[(&str, &str)]) -> String {
    for (name, text) in files {
        write(repo, name, text);
    }
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "--allow-empty", "-m", "c"]);
    git(&repo.0, &["rev-parse", "HEAD"])
}

fn repo(name: &str) -> Repo {
    let dir = std::env::temp_dir().join(format!("emi-fns-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_default();
    git(&dir, &["init", "-q", "-b", "main"]);
    Repo(dir)
}

const PY: &str =
    "def f():\n    return 1\n\n\ndef g():\n    a = 1\n    return a\n\n\ndef h():\n    return 3\n";

fn run(r: &Repo, base: &str, head: &str) -> ChangedFunctions {
    changed_functions(&r.0, base, head).unwrap_or_else(|e| panic!("{e}"))
}

fn two(r: &Repo, before: &[(&str, &str)], after: &[(&str, &str)]) -> (String, String) {
    (commit(r, before), commit(r, after))
}

#[test]
fn added_function() {
    let r = repo("added");
    let (b, h) = two(
        &r,
        &[("m.py", PY)],
        &[("m.py", &format!("{PY}\n\ndef k():\n    return 4\n"))],
    );
    let got = run(&r, &b, &h);
    assert_eq!(got.functions.len(), 1);
    let f = &got.functions[0];
    assert_eq!((f.kind, f.name.as_str()), (ChangeKind::Added, "k"));
    assert_eq!((f.old, f.new), (None, Some((14, 15))));
    assert_eq!(f.added_lines, vec![14, 15]);
    assert!(f.removed_lines.is_empty());
}

#[test]
fn edit_in_the_middle_is_modified_only() {
    let r = repo("modified");
    let edited = PY.replace("a = 1", "a = 2");
    let (b, h) = two(&r, &[("m.py", PY)], &[("m.py", &edited)]);
    let got = run(&r, &b, &h);
    assert_eq!(got.functions.len(), 1);
    let f = &got.functions[0];
    assert_eq!((f.kind, f.name.as_str()), (ChangeKind::Modified, "g"));
    assert_eq!((f.old, f.new), (Some((5, 7)), Some((5, 7))));
    assert_eq!(
        (f.added_lines.clone(), f.removed_lines.clone()),
        (vec![6], vec![6])
    );
}

#[test]
fn deleted_function() {
    let r = repo("removed");
    let trimmed = PY.replace("def h():\n    return 3\n", "");
    let (b, h) = two(&r, &[("m.py", PY)], &[("m.py", &trimmed)]);
    let got = run(&r, &b, &h);
    let f = &got.functions[0];
    assert_eq!((f.kind, f.name.as_str()), (ChangeKind::Removed, "h"));
    assert_eq!((f.old, f.new), (Some((10, 11)), None));
    assert_eq!(f.removed_lines, vec![10, 11]);
    assert!(f.added_lines.is_empty());
}

#[test]
fn methods_by_name_and_other_languages_unsupported() {
    let r = repo("langs");
    let py = "class C:\n    def m(self):\n        return 1\n";
    let rs = "fn a() -> i32 {\n    1\n}\n";
    let (b, h) = two(
        &r,
        &[("c.py", py), ("a.rs", rs), ("notes.txt", "x\n")],
        &[
            ("c.py", &py.replace("1", "2")),
            ("a.rs", &rs.replace("1", "2")),
            ("notes.txt", "y\n"),
        ],
    );
    let got = run(&r, &b, &h);
    let names: Vec<(&str, &str)> = got
        .functions
        .iter()
        .map(|f| (f.file.as_str(), f.name.as_str()))
        .collect();
    assert_eq!(names, vec![("c.py", "m")]);
    assert_eq!(
        got.unsupported,
        vec!["a.rs".to_owned(), "notes.txt".to_owned()]
    );
    assert!(got.unparsed.is_empty());
}

// A pure reorder is not special-cased: git reports the moved block as
// removed and added lines, so the moved function is Modified with its span
// on each side, and the function it moved across is untouched.
#[test]
fn reorder_reports_the_moved_function_as_modified() {
    let r = repo("reorder");
    let moved = "def g():\n    a = 1\n    return a\n\n\ndef f():\n    return 1\n\n\ndef h():\n    return 3\n";
    let (b, h) = two(&r, &[("m.py", PY)], &[("m.py", moved)]);
    let got = run(&r, &b, &h);
    let names: Vec<(&str, ChangeKind)> = got
        .functions
        .iter()
        .map(|f| (f.name.as_str(), f.kind))
        .collect();
    assert!(
        names.iter().all(|(_, k)| *k == ChangeKind::Modified),
        "{names:?}"
    );
    assert!(!names.iter().any(|(n, _)| *n == "h"), "{names:?}");
}

#[test]
fn syntax_error_on_head_is_unparsed() {
    let r = repo("broken");
    let (b, h) = two(&r, &[("m.py", PY)], &[("m.py", "def f(:\n    return 1\n")]);
    let got = run(&r, &b, &h);
    assert_eq!(got.unparsed, vec!["m.py".to_owned()]);
    assert!(got.functions.is_empty());
}

#[test]
fn whole_file_added_and_deleted() {
    let r = repo("whole");
    let base = commit(&r, &[("x.py", "def q():\n    return 1\n")]);
    git(&r.0, &["rm", "-q", "x.py"]);
    let head = commit(&r, &[("y.py", "def w():\n    return 2\n")]);
    let got = run(&r, &base, &head);
    let rows: Vec<(&str, &str, ChangeKind)> = got
        .functions
        .iter()
        .map(|f| (f.file.as_str(), f.name.as_str(), f.kind))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("x.py", "q", ChangeKind::Removed),
            ("y.py", "w", ChangeKind::Added)
        ]
    );
}

#[test]
fn json_shape() {
    let r = repo("json");
    let (b, h) = two(
        &r,
        &[("m.py", PY)],
        &[("m.py", &PY.replace("a = 1", "a = 2"))],
    );
    let v = run(&r, &b, &h).to_json();
    assert_eq!(v["functions"][0]["kind"], "modified");
    assert!(v["functions"][0].get("lang").is_none());
    assert_eq!(v["functions"][0]["old"], serde_json::json!([5, 7]));
    assert_eq!(v["unparsed"], serde_json::json!([]));
}
