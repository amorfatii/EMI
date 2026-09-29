//! `emi profile` through the binary: coverage import, sampled mutants and
//! the readiness each function gets from them.

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
    let d = std::env::temp_dir().join(format!("emi-cli-profile-{name}-{}", std::process::id()));
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

const PRICING: &str = "def discount(total, member):\n    if total > 100 and member:\n        return total * 0.9\n    return total\n\n\ndef shipping(weight):\n    if weight < 1:\n        return 5\n    if weight < 10:\n        return 10\n    return 20\n\n\ndef label(code):\n    if code == 1:\n        return \"one\"\n    return \"other\"\n";

// discount is checked, shipping is run but not checked, label never runs
const TESTS: &str = "import sys\nsys.path.insert(0, 'src')\nimport pricing\nassert pricing.discount(200, True) == 180\nassert pricing.discount(50, True) == 50\nassert pricing.discount(200, False) == 200\npricing.shipping(5)\n";

const LCOV: &str = "SF:src/pricing.py\nDA:1,3\nDA:2,3\nDA:3,1\nDA:4,2\nDA:7,1\nDA:8,1\nDA:9,0\nDA:10,1\nDA:11,1\nDA:12,0\nDA:15,0\nDA:16,0\nDA:17,0\nDA:18,0\nend_of_record\n";

fn by_name<'a>(p: &'a Value, name: &str) -> &'a Value {
    p["functions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|f| f["function"] == name)
        .unwrap_or(&Value::Null)
}

#[test]
fn readiness_follows_what_the_tests_run_and_check() {
    let repo = scratch("repo");
    std::fs::create_dir_all(repo.0.join("src")).unwrap_or_default();
    std::fs::create_dir_all(repo.0.join("tests")).unwrap_or_default();
    std::fs::write(repo.0.join("src/pricing.py"), PRICING).unwrap_or_default();
    std::fs::write(repo.0.join("tests/run.py"), TESTS).unwrap_or_default();
    git(&repo.0, &["init", "-q", "-b", "main"]);
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "init"]);
    let lcov = repo.0.join("cov.lcov");
    std::fs::write(&lcov, LCOV).unwrap_or_default();
    let out = Command::new(env!("CARGO_BIN_EXE_emi"))
        .args(["profile", "--source", "src", "--format", "json", "--repo"])
        .arg(&repo.0)
        .arg("--coverage")
        .arg(&lcov)
        .args(["--", "python3", "tests/run.py"])
        .output()
        .unwrap_or_else(|e| panic!("emi: {e}"));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let p: Value = serde_json::from_str(&text).unwrap_or_default();
    assert_eq!(p["mutation"]["baseline"]["passed"], true, "{text}");

    let discount = by_name(&p, "discount");
    assert_eq!(discount["readiness"], "strong", "{discount}");
    assert_eq!(discount["facts"]["coverage"]["executed"], 3);

    let shipping = by_name(&p, "shipping");
    assert_eq!(shipping["readiness"], "weak", "{shipping}");
    assert_eq!(shipping["facts"]["coverage"]["executable"], 5);
    let missed = shipping["facts"]["mutation"]["survivors"].to_string();
    assert!(missed.contains("line 8: `<` -> `>=`"), "{missed}");

    let label = by_name(&p, "label");
    assert_eq!(label["readiness"], "unexecuted", "{label}");
    assert_eq!(label["delegation"], "tests_or_human_review_first");
}

#[test]
fn a_failing_baseline_yields_no_mutant_verdicts() {
    let repo = scratch("fail");
    std::fs::create_dir_all(repo.0.join("src")).unwrap_or_default();
    std::fs::write(repo.0.join("src/pricing.py"), PRICING).unwrap_or_default();
    git(&repo.0, &["init", "-q", "-b", "main"]);
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "init"]);
    let out = Command::new(env!("CARGO_BIN_EXE_emi"))
        .args(["profile", "--source", "src", "--format", "json", "--repo"])
        .arg(&repo.0)
        .args(["--", "python3", "-c", "raise SystemExit(1)"])
        .output()
        .unwrap_or_else(|e| panic!("emi: {e}"));
    let p: Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    assert_eq!(p["mutation"]["baseline"]["passed"], false);
    assert_eq!(p["summary"]["readiness"]["unknown"], 3, "{p}");
}

// A dependency-free coverage tool: runs the tests under a line tracer and
// writes lcov for src/pricing.py (executable lines from its code objects).
const TRACER: &str = r#"import runpy, sys
target = "src/pricing.py"
ran = set()
def local(frame, event, arg):
    if event == "line":
        ran.add(frame.f_lineno)
    return local
def trace(frame, event, arg):
    return local if frame.f_code.co_filename.endswith(target) else None
sys.settrace(trace)
runpy.run_path("tests/run.py")
sys.settrace(None)
def lines(code):
    out = {l for _, _, l in code.co_lines() if l}
    for c in code.co_consts:
        if hasattr(c, "co_lines"):
            out |= lines(c)
    return out
src = open(target).read()
with open("cov.lcov", "w") as f:
    f.write("SF:" + target + "\n")
    for n in sorted(lines(compile(src, target, "exec"))):
        f.write("DA:%d,%d\n" % (n, 1 if n in ran else 0))
    f.write("end_of_record\n")
"#;

fn profile_json(repo: &Path, extra: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_emi"))
        .args(["profile", "--source", "src", "--format", "json", "--repo"])
        .arg(repo)
        .args(extra)
        .output()
        .unwrap_or_else(|e| panic!("emi: {e}"));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_default()
}

#[test]
fn emi_measures_coverage_at_the_head_itself() {
    let repo = scratch("covrun");
    std::fs::create_dir_all(repo.0.join("src")).unwrap_or_default();
    std::fs::create_dir_all(repo.0.join("tests")).unwrap_or_default();
    std::fs::write(repo.0.join("src/pricing.py"), PRICING).unwrap_or_default();
    std::fs::write(repo.0.join("tests/run.py"), TESTS).unwrap_or_default();
    std::fs::write(repo.0.join("tests/cov.py"), TRACER).unwrap_or_default();
    git(&repo.0, &["init", "-q", "-b", "main"]);
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "init"]);
    let p = profile_json(
        &repo.0,
        &[
            "--coverage-command",
            "python3 tests/cov.py",
            "--coverage-output",
            "cov.lcov",
        ],
    );
    assert_eq!(p["coverage"]["source"], "run", "{p}");
    assert_eq!(p["coverage"]["run"]["steps"][0]["exit_code"], 0);
    // label's def line ran on import; no test called it
    assert_eq!(by_name(&p, "label")["readiness"], "unexecuted", "{p}");
    assert_eq!(by_name(&p, "discount")["readiness"], "executed");
    assert!(
        !repo.0.join("cov.lcov").exists(),
        "the run happens in a copy, never the checkout"
    );

    let failed = profile_json(
        &repo.0,
        &[
            "--coverage-command",
            "python3 -c 'raise SystemExit(4)'",
            "--coverage-output",
            "cov.lcov",
        ],
    );
    assert_eq!(
        failed["coverage"]["error"], "coverage step 1 exited 4",
        "{failed}"
    );
    let unknowns = by_name(&failed, "label")["unknowns"].to_string();
    assert!(unknowns.contains("the coverage run failed"), "{unknowns}");
}

fn emi_out(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_emi"))
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("emi: {e}"));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn the_pr_comment_shows_the_evidence_behind_changed_functions() {
    let repo = scratch("prcomment");
    std::fs::create_dir_all(repo.0.join("src")).unwrap_or_default();
    std::fs::create_dir_all(repo.0.join("tests")).unwrap_or_default();
    std::fs::write(repo.0.join("src/pricing.py"), PRICING).unwrap_or_default();
    std::fs::write(repo.0.join("tests/run.py"), TESTS).unwrap_or_default();
    std::fs::write(repo.0.join("tests/cov.py"), TRACER).unwrap_or_default();
    git(&repo.0, &["init", "-q", "-b", "main"]);
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "base"]);
    let stale = repo.0.join("stale.json");
    let r = repo.0.to_string_lossy().into_owned();
    emi_out(&[
        "profile",
        "--repo",
        &r,
        "--source",
        "src",
        "--top",
        "0",
        "--format",
        "json",
        "-o",
        &stale.to_string_lossy(),
    ]);
    std::fs::write(
        repo.0.join("src/pricing.py"),
        PRICING.replace("return 10", "return 12"),
    )
    .unwrap_or_default();
    git(&repo.0, &["commit", "-q", "-am", "change shipping"]);
    let fresh = repo.0.join("fresh.json");
    emi_out(&[
        "profile",
        "--repo",
        &r,
        "--source",
        "src",
        "--top",
        "0",
        "--format",
        "json",
        "-o",
        &fresh.to_string_lossy(),
        "--coverage-command",
        "python3 tests/cov.py",
        "--coverage-output",
        "cov.lcov",
        "--",
        "python3",
        "tests/run.py",
    ]);
    let diff = |profile: &Path| {
        emi_out(&[
            "diff",
            "--repo",
            &r,
            "--source",
            "src",
            "--before",
            "HEAD~1",
            "--after",
            "HEAD",
            "--guardrail",
            "--format",
            "markdown",
            "--profile-json",
            &profile.to_string_lossy(),
        ])
    };
    let md = diff(&fresh);
    assert!(
        md.contains("Test evidence for the changed functions"),
        "{md}"
    );
    assert!(md.contains("`src/pricing.py:7 shipping` | ⚠️ weak"), "{md}");
    assert!(!md.contains("discount"), "only changed functions: {md}");
    // a profile of another commit is flagged, not used
    assert!(diff(&stale).contains("Not shown: the profile is of"));
}
