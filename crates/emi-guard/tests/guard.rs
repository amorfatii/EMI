//! `emi_guard::guard` end to end on a scratch repository.

use std::path::{Path, PathBuf};
use std::process::Command;

use emi_guard::{Policy, Tier, guard};
use serde_json::json;

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

fn write(r: &Repo, name: &str, text: &str) {
    let path = r.0.join(name);
    std::fs::create_dir_all(path.parent().unwrap_or(&r.0)).unwrap_or_default();
    std::fs::write(path, text).unwrap_or_default();
}

fn commit(r: &Repo) -> String {
    git(&r.0, &["add", "-A"]);
    git(&r.0, &["commit", "-q", "--allow-empty", "-m", "c"]);
    git(&r.0, &["rev-parse", "HEAD"])
}

fn repo(name: &str) -> Repo {
    let dir = std::env::temp_dir().join(format!("emi-guard-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_default();
    git(&dir, &["init", "-q", "-b", "main"]);
    let r = Repo(dir);
    write(&r, "src/app.py", "def f():\n    return 1\n");
    write(
        &r,
        "tests/test_app.py",
        "def test_a():\n    assert f() == 1\n    assert f() > 0\n\n\ndef test_b():\n    assert f()\n",
    );
    write(
        &r,
        "tests/test_old.py",
        "def test_old():\n    assert True\n",
    );
    write(
        &r,
        "pyproject.toml",
        "[project]\nname = \"x\"\n\n[tool.pytest.ini_options]\naddopts = \"-q\"\n",
    );
    write(&r, "poetry.lock", "# lock\n");
    r
}

fn ids(report: &emi_guard::Report) -> Vec<&'static str> {
    report.violations.iter().map(|v| v.id).collect()
}

#[test]
fn clean_change_passes() {
    let r = repo("clean");
    let base = commit(&r);
    write(&r, "src/app.py", "def f():\n    return 2\n");
    let head = commit(&r);
    let report =
        guard(&r.0, &base, Some(&head), &Policy::default()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(report.decision(), "pass", "{:?}", report.violations);
}

#[test]
fn every_core_guardrail_fires() {
    let r = repo("dirty");
    let base = commit(&r);
    write(&r, "src/app.py", "def f():\n    return 2  # noqa\n");
    write(
        &r,
        "tests/test_app.py",
        "import pytest\n\n\n@pytest.mark.skip(reason='later')\ndef test_a():\n    assert f() == 2\n\n\ndef test_b():\n    assert f()\n",
    );
    git(&r.0, &["rm", "-q", "tests/test_old.py"]);
    write(
        &r,
        "pyproject.toml",
        "[project]\nname = \"x\"\n\n[tool.pytest.ini_options]\naddopts = \"-q --deselect tests\"\n",
    );
    write(&r, "poetry.lock", "# lock v2\n");
    write(&r, ".github/workflows/ci.yml", "on: push\n");
    write(&r, "docs/notes.md", "hi\n");
    let head = commit(&r);
    let policy = Policy::from_json(&json!({"permitted_paths": ["src/*", "tests/*", "pyproject.toml", "poetry.lock", ".github/*"]}))
        .unwrap_or_else(|e| panic!("{e}"));
    let report = guard(&r.0, &base, Some(&head), &policy).unwrap_or_else(|e| panic!("{e}"));
    let got = ids(&report);
    for want in [
        "suppress.added",
        "tests.skip_added",
        "tests.fewer_assertions",
        "tests.deleted",
        "tests.config_changed",
        "prohibited.lockfile",
        "prohibited.ci",
        "scope.outside",
    ] {
        assert!(got.contains(&want), "missing {want} in {got:?}");
    }
    assert!(
        !got.contains(&"tests.fewer"),
        "test count unchanged: {got:?}"
    );
    assert_eq!(report.decision(), "block");
    let noqa = report
        .violations
        .iter()
        .find(|v| v.id == "suppress.added")
        .map(|v| (v.tier, v.line));
    assert_eq!(noqa, Some((Tier::Review, Some(2))));
    let outside = report
        .violations
        .iter()
        .find(|v| v.id == "scope.outside")
        .map(|v| v.path.clone());
    assert_eq!(outside.as_deref(), Some("docs/notes.md"));
}

/// S6 false positives: markers in test fixtures or comments, moved or
/// kept on an edited line, and public certificate bundles.
#[test]
fn fixture_text_comments_and_moved_lines_are_not_new_markers() {
    let r = repo("fixture");
    write(
        &r,
        "src/app.py",
        "def f():\n    return 1  # type: ignore\n\n\ndef g():\n    return 2\n",
    );
    let base = commit(&r);
    write(
        &r,
        "src/app.py",
        "def g():\n    return 2\n\n\ndef f():\n    return 1 + 0  # type: ignore\n",
    );
    write(
        &r,
        "build/lib/certifi/cacert.pem",
        "## CA bundle\n-----BEGIN CERTIFICATE-----\nMIIC\n-----END CERTIFICATE-----\n",
    );
    write(
        &r,
        "tests/test_app.py",
        "def test_a(testdir):\n    testdir.makepyfile(\n        \"\"\"\n        import pytest\n        @pytest.mark.skip\n        def test_x(): pass\n        \"\"\"\n    )\n    assert f() == 1\n    assert f() > 0\n\n\n# Case 2: @unittest.skipIf is honoured\ndef test_b():\n    assert f()\n",
    );
    let head = commit(&r);
    let report =
        guard(&r.0, &base, Some(&head), &Policy::default()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(ids(&report), Vec::<&str>::new());
    write(
        &r,
        "tests/test_app.py",
        "import pytest\n\n\ndef test_a():\n    assert f() == 1\n    assert f() > 0\n\n\n@pytest.mark.skip\ndef test_b():\n    assert f()\n",
    );
    let head = commit(&r);
    let report =
        guard(&r.0, &base, Some(&head), &Policy::default()).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        ids(&report).contains(&"tests.skip_added"),
        "{:?}",
        report.violations
    );
    write(
        &r,
        "deploy/server.pem",
        "-----BEGIN CERTIFICATE-----\nMIIC\n-----END CERTIFICATE-----\n-----BEGIN PRIVATE KEY-----\nMIIE\n-----END PRIVATE KEY-----\n",
    );
    let head = commit(&r);
    let report =
        guard(&r.0, &base, Some(&head), &Policy::default()).unwrap_or_else(|e| panic!("{e}"));
    let secrets: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.id == "prohibited.secret")
        .map(|v| v.path.as_str())
        .collect();
    assert_eq!(secrets, vec!["deploy/server.pem"], "a CA bundle is public");
}

#[test]
fn policy_overrides_tiers_and_allows_classes() {
    let r = repo("policy");
    let base = commit(&r);
    write(&r, "poetry.lock", "# lock v2\n");
    write(&r, "src/app.py", "def f():\n    return 1  # type: ignore\n");
    let head = commit(&r);
    let policy =
        Policy::from_json(&json!({"allow_classes": ["lockfile"], "tiers": {"suppress": "off"}}))
            .unwrap_or_else(|e| panic!("{e}"));
    let report = guard(&r.0, &base, Some(&head), &policy).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(ids(&report), vec!["suppress.added"]);
    assert_eq!(
        report.decision(),
        "pass",
        "an 'off' tier reports without affecting the decision"
    );
    assert!(Policy::from_json(&json!({"tiers": {"x": "maybe"}})).is_err());
}

#[test]
fn worktree_and_budgets() {
    let r = repo("worktree");
    let base = commit(&r);
    write(&r, "src/new.py", &"x = 1\n".repeat(30));
    let policy = Policy::from_json(&json!({"max_net_sloc": 20})).unwrap_or_else(|e| panic!("{e}"));
    let report = guard(&r.0, &base, None, &policy).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(ids(&report), vec!["growth.net_sloc"]);
    assert_eq!(report.decision(), "review");
    assert_eq!(report.to_json()["head"], "worktree");
}

#[test]
fn a_split_is_not_weakening_but_a_lint_only_tox_change_is_not_test_config() {
    let r = repo("split");
    write(
        &r,
        "tox.ini",
        "[testenv]\ncommands = pytest\n\n[testenv:lint]\ndeps = flake8\n",
    );
    let base = commit(&r);
    // move test_b into its own file: totals unchanged
    write(
        &r,
        "tests/test_app.py",
        "def test_a():\n    assert f() == 1\n    assert f() > 0\n",
    );
    write(&r, "tests/test_b.py", "def test_b():\n    assert f()\n");
    write(
        &r,
        "tox.ini",
        "[testenv]\ncommands = pytest\n\n[testenv:lint]\ndeps = flake8>=7\n",
    );
    let head = commit(&r);
    let report =
        guard(&r.0, &base, Some(&head), &Policy::default()).unwrap_or_else(|e| panic!("{e}"));
    assert!(report.violations.is_empty(), "{:?}", report.violations);
    // a change to the test environment is test configuration
    write(
        &r,
        "tox.ini",
        "[testenv]\ncommands = pytest -x --deselect tests\n\n[testenv:lint]\ndeps = flake8>=7\n",
    );
    let head2 = commit(&r);
    let report =
        guard(&r.0, &head, Some(&head2), &Policy::default()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(ids(&report), vec!["tests.config_changed"]);
}

#[test]
fn lockfile_changes_need_review_by_default() {
    let r = repo("lock");
    let base = commit(&r);
    write(&r, "poetry.lock", "# lock v2\n");
    let head = commit(&r);
    let report =
        guard(&r.0, &base, Some(&head), &Policy::default()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(report.decision(), "review");
}

const HELPER: &str = "def normalize(record):\n    name = record['name'].strip().lower()\n    email = record['email'].strip().lower()\n    phone = record['phone'].replace('-', '')\n    city = record['city'].title()\n    country = record['country'].upper()\n    return {'name': name, 'email': email, 'phone': phone, 'city': city, 'country': country}\n";

#[test]
fn duplicated_code_is_reviewed_but_moved_code_is_not() {
    let r = repo("dup");
    write(&r, "src/users.py", HELPER);
    let base = commit(&r);
    write(
        &r,
        "src/orders.py",
        &format!(
            "import os\n\n\n{}",
            HELPER.replace("normalize", "normalize_order")
        ),
    );
    let copied = commit(&r);
    let report =
        guard(&r.0, &base, Some(&copied), &Policy::default()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        ids(&report),
        vec!["dup.introduced"],
        "{:?}",
        report.violations
    );
    assert_eq!(report.decision(), "review");
    assert!(report.violations[0].evidence.contains("src/users.py"));

    // moving the helper (old copy deleted) is not duplication
    git(&r.0, &["rm", "-q", "src/orders.py"]);
    let reset = commit(&r);
    git(&r.0, &["mv", "src/users.py", "src/people.py"]);
    write(&r, "src/people.py", &format!("# moved\n{HELPER}"));
    let moved = commit(&r);
    let report =
        guard(&r.0, &reset, Some(&moved), &Policy::default()).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        !ids(&report).contains(&"dup.introduced"),
        "{:?}",
        report.violations
    );
}

/// GV1 round 4: a test file whose text holds a NUL byte past git's
/// binary sniffing window was read as empty, so every test "disappeared".
#[test]
fn test_files_with_nul_bytes_are_counted() {
    let r = repo("nul");
    let base = commit(&r);
    let pad = "# padding\n".repeat(900);
    write(
        &r,
        "tests/test_app.py",
        &format!(
            "{pad}def test_a():\n    assert f() == 1\n    assert f() > 0\n    assert '\0' != 'a'\n\n\ndef test_b():\n    assert f()\n\n\ndef test_c():\n    assert f()\n"
        ),
    );
    let head = commit(&r);
    let report =
        guard(&r.0, &base, Some(&head), &Policy::default()).unwrap_or_else(|e| panic!("{e}"));
    let got = ids(&report);
    assert!(
        !got.contains(&"tests.fewer") && !got.contains(&"tests.fewer_assertions"),
        "{:?}",
        report.violations
    );
}

/// GV1 round 4: deleting a fixture is not deleting tests, and reformatting
/// test configuration or adding a fixture to conftest.py configures nothing.
#[test]
fn fixtures_and_formatting_are_not_test_changes() {
    let r = repo("fixtures");
    write(&r, "tests/fixtures/data.py", "VALUE = {'a': 1}\n");
    write(
        &r,
        ".nycrc",
        "{\n  \"reporter\": [\n    \"json\",\n    \"text\"\n  ]\n}\n",
    );
    write(
        &r,
        "tests/conftest.py",
        "import pytest\n\n\n@pytest.fixture\ndef a():\n    return 1\n",
    );
    let base = commit(&r);
    git(&r.0, &["rm", "-q", "tests/fixtures/data.py"]);
    write(&r, ".nycrc", "{\n  \"reporter\": [\"json\", \"text\"]\n}\n");
    write(
        &r,
        "tests/conftest.py",
        "import pytest\n\n\n@pytest.fixture\ndef a():\n    return 1\n\n\n@pytest.fixture\ndef b():\n    return 2\n",
    );
    let head = commit(&r);
    let report =
        guard(&r.0, &base, Some(&head), &Policy::default()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(ids(&report), Vec::<&str>::new(), "{:?}", report.violations);
    write(&r, ".nycrc", "{\n  \"reporter\": [\"json\"]\n}\n");
    write(
        &r,
        "tests/conftest.py",
        "import pytest\n\ncollect_ignore = ['test_slow.py']\n",
    );
    let head = commit(&r);
    let report =
        guard(&r.0, &base, Some(&head), &Policy::default()).unwrap_or_else(|e| panic!("{e}"));
    let paths: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.id == "tests.config_changed")
        .map(|v| v.path.as_str())
        .collect();
    assert_eq!(
        paths,
        vec![".nycrc", "tests/conftest.py"],
        "{:?}",
        report.violations
    );
}
