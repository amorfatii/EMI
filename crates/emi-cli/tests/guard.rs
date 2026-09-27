//! `emi guard` through the binary: exit codes and output formats.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
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
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "git {args:?}");
}

fn write(r: &Repo, name: &str, text: &str) {
    let path = r.0.join(name);
    std::fs::create_dir_all(path.parent().unwrap_or(&r.0)).unwrap_or_default();
    std::fs::write(path, text).unwrap_or_default();
}

fn emi(r: &Repo, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_emi"))
        .arg("guard")
        .arg("--repo")
        .arg(&r.0)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("emi: {e}"));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn exit_codes_and_formats() {
    let dir = std::env::temp_dir().join(format!("emi-guard-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_default();
    let r = Repo(dir);
    git(&r.0, &["init", "-q", "-b", "main"]);
    write(&r, "src/app.py", "def f():\n    return 1\n");
    write(&r, "tests/test_app.py", "def test_a():\n    assert True\n");
    git(&r.0, &["add", "-A"]);
    git(&r.0, &["commit", "-q", "-m", "base"]);

    assert_eq!(emi(&r, &["--base", "HEAD"]).0, 0, "no change passes");

    write(&r, "src/app.py", "def f():\n    return 1  # noqa\n");
    assert_eq!(
        emi(&r, &["--base", "HEAD"]).0,
        0,
        "review-only passes by default"
    );
    assert_eq!(emi(&r, &["--base", "HEAD", "--fail-on", "review"]).0, 3);

    write(
        &r,
        "tests/test_app.py",
        "import pytest\n\n@pytest.mark.skip\ndef test_a():\n    assert True\n",
    );
    let (code, out) = emi(&r, &["--base", "HEAD", "--format", "json"]);
    assert_eq!(code, 2);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_default();
    assert_eq!(v["decision"], "block");
    assert_eq!(v["counts"]["tests.skip_added"], 1);
    assert_eq!(v["counts"]["suppress.added"], 1);
    let (_, sarif) = emi(&r, &["--base", "HEAD", "--format", "sarif"]);
    let s: serde_json::Value = serde_json::from_str(&sarif).unwrap_or_default();
    assert_eq!(s["runs"][0]["results"].as_array().map(Vec::len), Some(2));
}
