//! `emi task` through the binary: generated specs and their verification.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Dir {
    let d = std::env::temp_dir().join(format!("emi-cli-task-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap_or_default();
    Dir(d)
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "git {args:?}");
}

fn emi(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_emi"))
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("emi: {e}"));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn strs(v: &Value) -> Vec<&str> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

#[test]
fn refactor_specs_trust_and_protect_test_runner_configuration() {
    let (repo, authority) = (scratch("rs-repo"), scratch("rs-authority"));
    git(&repo.0, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.0.join("app.py"), "def f(x):\n    return x\n").unwrap_or_default();
    std::fs::write(repo.0.join("pyproject.toml"), "[tool.pytest.ini_options]\n")
        .unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "init"]);
    let repo_arg = repo.0.to_string_lossy().into_owned();
    let (code, out) = emi(&[
        "task",
        "refactor-spec",
        "--repo",
        &repo_arg,
        "--target",
        "app.py",
        "--",
        "python3",
        "-c",
        "import app",
    ]);
    assert_eq!(code, 0, "{out}");
    let spec: Value = serde_json::from_str(&out).unwrap_or_default();
    assert!(strs(&spec["verification"]["trusted"]).contains(&"pyproject.toml"));
    assert!(strs(&spec["envelope"]["protected_paths"]).contains(&"pyproject.toml"));

    let spec_path = authority.0.join("spec.json");
    std::fs::write(&spec_path, &out).unwrap_or_default();
    let auth = authority.0.join("store").to_string_lossy().into_owned();
    let spec_arg = spec_path.to_string_lossy().into_owned();
    let (code, _) = emi(&["task", "create", "--authority", &auth, "--spec", &spec_arg]);
    assert_eq!(code, 0);
    // a candidate that loads a plugin through the runner configuration
    std::fs::write(
        repo.0.join("pyproject.toml"),
        "[tool.pytest.ini_options]\naddopts = \"-p app\"\n",
    )
    .unwrap_or_default();
    let (code, out) = emi(&[
        "task",
        "verify",
        "--authority",
        &auth,
        "--task",
        "refactor-app",
        "--worktree",
    ]);
    assert_eq!(code, 3, "{out}");
    let d: Value = serde_json::from_str(&out).unwrap_or_default();
    assert_eq!(d["decision"], "rejected", "{out}");
    assert_eq!(
        d["gates"]["envelope"]["violations"][0]["path"],
        "pyproject.toml"
    );
}

fn emi_err(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_emi"))
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("emi: {e}"));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn drafts_carry_held_out_tests_and_warn_from_the_profile() {
    let (repo, authority) = (scratch("dr-repo"), scratch("dr-authority"));
    git(&repo.0, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.0.join("src")).unwrap_or_default();
    std::fs::write(repo.0.join("src/pricing.py"), "def f(x):\n    return x\n").unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "init"]);
    let hidden = authority.0.join("hidden_test.py");
    std::fs::write(&hidden, "import sys\n").unwrap_or_default();
    // a profile saved earlier, of another commit
    let profile = authority.0.join("profile.json");
    std::fs::write(
        &profile,
        r#"{"head": "0000000000000000000000000000000000000000", "functions": [
            {"file": "src/pricing.py", "function": "f", "start": 1, "readiness": "weak",
             "advice": "add held-out tests"}]}"#,
    )
    .unwrap_or_default();
    let repo_arg = repo.0.to_string_lossy().into_owned();
    let held = format!("tests/test_hidden.py={}", hidden.display());
    let profile_arg = profile.to_string_lossy().into_owned();
    let (code, out, err) = emi_err(&[
        "task",
        "draft",
        "--repo",
        &repo_arg,
        "--target",
        "src/pricing.py",
        "--objective",
        "f returns its argument doubled",
        "--held-out",
        &held,
        "--profile",
        &profile_arg,
        "--",
        "python3",
        "-c",
        "pass",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("the profile is of"), "{err}");
    assert!(err.contains("src/pricing.py:1 f is weak"), "{err}");
    let spec: Value = serde_json::from_str(&out).unwrap_or_default();
    assert_eq!(spec["task_id"], "change-pricing");
    assert!(spec["verification"]["held_out"]["tests/test_hidden.py"]["file"].is_string());
    assert!(strs(&spec["envelope"]["permitted_paths"]).contains(&"src/*"));

    let spec_path = authority.0.join("spec.json");
    std::fs::write(&spec_path, &out).unwrap_or_default();
    let auth = authority.0.join("store").to_string_lossy().into_owned();
    let spec_arg = spec_path.to_string_lossy().into_owned();
    let (code, _, err) = emi_err(&["task", "create", "--authority", &auth, "--spec", &spec_arg]);
    assert_eq!(code, 0, "the drafted spec freezes: {err}");
}
