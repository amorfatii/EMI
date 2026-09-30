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

#[test]
fn every_kind_of_generated_spec_freezes() {
    // the contract refuses unknown keys, so whatever the generators write
    // must be something it reads
    let (repo, authority) = (scratch("kinds-repo"), scratch("kinds-authority"));
    git(&repo.0, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.0.join("app.py"), "def f(x):\n    return x\n").unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "init"]);
    let hidden = authority.0.join("hidden_test.py");
    std::fs::write(&hidden, "import app\n").unwrap_or_default();
    let repo_arg = repo.0.to_string_lossy().into_owned();
    let held = format!("tests/test_hidden.py={}", hidden.display());
    let pytest = ["--", "python3", "-m", "pytest", "-q"];
    let draft = [
        &["task", "draft", "--repo", &repo_arg, "--target", "app.py"][..],
        &[
            "--objective",
            "f doubles",
            "--held-out",
            &held,
            "--task-id",
            "d",
        ],
        &pytest,
    ]
    .concat();
    let refactor = [
        &[
            "task",
            "refactor-spec",
            "--repo",
            &repo_arg,
            "--target",
            "app.py",
        ][..],
        &["--task-id", "r"],
        &pytest,
    ]
    .concat();
    let auth = authority.0.join("store").to_string_lossy().into_owned();
    for (n, args) in [draft, refactor].iter().enumerate() {
        let (code, out, err) = emi_err(args);
        assert_eq!(code, 0, "{err}");
        let spec: Value = serde_json::from_str(&out).unwrap_or_default();
        assert!(spec["verification"]["inventory"].is_object(), "{out}");
        let spec_path = authority.0.join(format!("spec-{n}.json"));
        std::fs::write(&spec_path, &out).unwrap_or_default();
        let spec_arg = spec_path.to_string_lossy().into_owned();
        let (code, _, err) =
            emi_err(&["task", "create", "--authority", &auth, "--spec", &spec_arg]);
        assert_eq!(code, 0, "{args:?} freezes: {err}");
    }
}

const LCB_TEST: &str = "from solution import add\n\n\ndef check(candidate):\n    assert candidate(1, 2) == 3\n    assert candidate(2, 2) == 4\n    assert candidate(0, 0) == 0\n    assert candidate(5, 5) == 10\n    assert candidate(7, 1) == 8\n\n\ndef test_add():\n    check(add)\n\n\ntest_add()\n";

fn verdict(authority: &str, task: &str) -> Value {
    let (_, out, err) = emi_err(&[
        "task",
        "verify",
        "--authority",
        authority,
        "--task",
        task,
        "--worktree",
    ]);
    serde_json::from_str(&out).unwrap_or_else(|_| panic!("verify: {out}{err}"))
}

#[test]
fn held_out_asserts_reject_code_that_hard_codes_the_visible_cases() {
    let (repo, authority) = (scratch("ho-repo"), scratch("ho-authority"));
    git(&repo.0, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.0.join("tests")).unwrap_or_default();
    std::fs::write(
        repo.0.join("solution.py"),
        "def add(a, b):\n    raise NotImplementedError\n",
    )
    .unwrap_or_default();
    let test = repo.0.join("tests/test_add.py");
    std::fs::write(&test, LCB_TEST).unwrap_or_default();
    let (repo_arg, test_arg) = (
        repo.0.to_string_lossy().into_owned(),
        test.to_string_lossy().into_owned(),
    );
    let held = authority.0.join("held.py");
    let held_arg = held.to_string_lossy().into_owned();
    let (code, out, err) = emi_err(&[
        "task", "holdout", "--file", &test_arg, "--out", &held_arg, "--unit", "asserts", "--share",
        "0.4", "--seed", "4",
    ]);
    assert_eq!(code, 0, "{err}");
    let split: Value = serde_json::from_str(&out).unwrap_or_default();
    assert_eq!(split["unit"], "asserts", "{out}");
    assert_eq!(strs(&split["held_out"]).len(), 2, "{out}");
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "baseline"]);

    let run_both = "import runpy\nfor f in ('tests/test_add.py', 'tests/test_add_held_out.py'):\n    runpy.run_path(f)\n";
    let flag = format!("tests/test_add_held_out.py={}", held.display());
    let (code, spec, err) = emi_err(&[
        "task",
        "draft",
        "--repo",
        &repo_arg,
        "--target",
        "solution.py",
        "--objective",
        "add adds its arguments",
        "--held-out",
        &flag,
        "--task-id",
        "add",
        "--",
        "python3",
        "-c",
        run_both,
    ]);
    assert_eq!(code, 0, "{err}");
    // the sandbox is not under test, and not every machine has one
    let mut spec: Value = serde_json::from_str(&spec).unwrap_or_default();
    spec["execution"]["isolation"] = "none".into();
    let spec_path = authority.0.join("spec.json");
    std::fs::write(&spec_path, spec.to_string()).unwrap_or_default();
    let auth = authority.0.join("store").to_string_lossy().into_owned();
    let spec_arg = spec_path.to_string_lossy().into_owned();
    let (code, _, err) = emi_err(&["task", "create", "--authority", &auth, "--spec", &spec_arg]);
    assert_eq!(code, 0, "{err}");

    // hard-codes exactly the cases the agent can still see
    let visible = std::fs::read_to_string(&test).unwrap_or_default();
    let table: Vec<String> = visible
        .lines()
        .filter_map(|l| l.trim().strip_prefix("assert candidate("))
        .filter_map(|l| l.split_once(") == "))
        .map(|(args, want)| format!("({args}): {want}"))
        .collect();
    assert_eq!(table.len(), 3, "{visible}");
    let lookup = format!(
        "def add(a, b):\n    return {{{}}}[(a, b)]\n",
        table.join(", ")
    );
    std::fs::write(repo.0.join("solution.py"), lookup).unwrap_or_default();
    let d = verdict(&auth, "add");
    assert_eq!(d["decision"], "rejected", "{d}");

    std::fs::write(
        repo.0.join("solution.py"),
        "def add(a, b):\n    return a + b\n",
    )
    .unwrap_or_default();
    let d = verdict(&auth, "add");
    assert_eq!(d["decision"], "accepted", "{d}");
}

#[test]
fn designed_variants_need_a_reference_outside_the_repository() {
    let (repo, authority) = (scratch("vr-repo"), scratch("vr-authority"));
    git(&repo.0, &["init", "-q", "-b", "main"]);
    let test = repo.0.join("test_add.py");
    std::fs::write(&test, LCB_TEST).unwrap_or_default();
    let (test_arg, out) = (
        test.to_string_lossy().into_owned(),
        authority.0.join("held.py").to_string_lossy().into_owned(),
    );
    let (code, _, err) = emi_err(&[
        "task", "holdout", "--file", &test_arg, "--out", &out, "--vary",
    ]);
    assert_ne!(code, 0);
    assert!(err.contains("pass --reference"), "{err}");
    let inside = repo.0.join("reference.py");
    std::fs::write(&inside, "def add(a, b):\n    return a + b\n").unwrap_or_default();
    let inside_arg = inside.to_string_lossy().into_owned();
    let (code, _, err) = emi_err(&[
        "task",
        "holdout",
        "--file",
        &test_arg,
        "--out",
        &out,
        "--vary",
        "--reference",
        &inside_arg,
    ]);
    assert_ne!(code, 0);
    assert!(err.contains("is inside the repository"), "{err}");

    // the visible file is unchanged by a run that writes variants
    let reference = authority.0.join("reference.py");
    std::fs::write(&reference, "def add(a, b):\n    return a + b\n").unwrap_or_default();
    let ref_arg = reference.to_string_lossy().into_owned();
    let (code, stdout, err) = emi_err(&[
        "task",
        "holdout",
        "--file",
        &test_arg,
        "--out",
        &out,
        "--vary",
        "--reference",
        &ref_arg,
        "--count",
        "5",
    ]);
    assert_eq!(code, 0, "{err}");
    let r: Value = serde_json::from_str(&stdout).unwrap_or_default();
    assert_eq!(r["variants_written"], 5, "{stdout}");
    assert!(r["dropped"]["duplicate"].is_u64(), "{stdout}");
    assert_eq!(std::fs::read_to_string(&test).unwrap_or_default(), LCB_TEST);
    let held = std::fs::read_to_string(&out).unwrap_or_default();
    assert_eq!(held.matches("def test_emi_variant_").count(), 5, "{held}");
}
