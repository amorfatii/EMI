//! `emi hook` through the binary, with a stand-in for `emi done`: the loop
//! the harness drives, from the first message to a stop that is allowed.

#![allow(clippy::expect_used)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const SESSION: &str = "session-1";

/// A repository with EMI configured, a stand-in done check, and a private
/// state directory, all removed when dropped.
struct Fixture {
    repo: tempfile::TempDir,
    state: tempfile::TempDir,
    standin: tempfile::TempDir,
    home: tempfile::TempDir,
    edits: u32,
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

fn receipt(action: &str) -> Value {
    json!({
        "schema": "emi-done-receipt-0.1",
        "repo": "/r",
        "baseline": {"sha": "0123456789abcdef", "source": "session"},
        "candidate": {"id": "worktree", "content_id": "t"},
        "assurance": {"setting": "cooperative", "isolation": "unsandboxed (stand-in)"},
        "regression": {"status": "held", "tests_run": 7, "failed": [], "missing": [], "reasons": []},
        "changed_old_tests": [],
        "change": {"status": "unproven", "functions": [], "proving_tests": [],
                   "redundant_tests": [], "unknown_tests": [], "not_measured": []},
        "action": action,
        "repair": [],
        "not_verified": ["SENTINEL-NOT-VERIFIED-LINE"]
    })
}

fn repair_receipt(line: &str) -> Value {
    let mut r = receipt("repair");
    r["regression"]["status"] = json!("broken");
    r["repair"] = json!([line]);
    r
}

impl Fixture {
    fn new(config: &Value) -> Fixture {
        let f = Fixture {
            repo: tempfile::tempdir().expect("tempdir"),
            state: tempfile::tempdir().expect("tempdir"),
            standin: tempfile::tempdir().expect("tempdir"),
            home: tempfile::tempdir().expect("tempdir"),
            edits: 0,
        };
        git(f.repo.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(f.repo.path().join("app.py"), "x = 1\n").expect("write");
        std::fs::create_dir_all(f.repo.path().join(".emi")).expect("mkdir");
        std::fs::write(f.repo.path().join(".emi/config.json"), config.to_string()).expect("write");
        git(f.repo.path(), &["add", "-A"]);
        git(f.repo.path(), &["commit", "-q", "-m", "base"]);
        let script = "#!/bin/sh\n\
            dir=\"$1\"; shift\n\
            echo \"$*\" >> \"$dir/calls\"\n\
            cat \"$dir/stdout\"\n\
            echo \"$(cat \"$dir/stderr\")\" >&2\n\
            exit \"$(cat \"$dir/exit\")\"\n";
        std::fs::write(f.standin.path().join("done.sh"), script).expect("write");
        f.set_done(&receipt("stop"), 0);
        f
    }

    fn configured() -> Fixture {
        Fixture::new(&json!({"schema": "emi-config-0.1", "default_branch": "main"}))
    }

    fn set_done(&self, receipt: &Value, exit: i32) {
        self.set_raw(&receipt.to_string(), exit);
    }

    fn set_raw(&self, stdout: &str, exit: i32) {
        let dir = self.standin.path();
        std::fs::write(dir.join("stdout"), stdout).expect("write");
        std::fs::write(dir.join("stderr"), "").expect("write");
        std::fs::write(dir.join("exit"), exit.to_string()).expect("write");
    }

    /// Change the working tree, as an agent editing a file does.
    fn edit(&mut self) {
        self.edits += 1;
        let text = format!("x = {}\n", self.edits + 1);
        std::fs::write(self.repo.path().join("app.py"), text).expect("write");
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.standin.path().join("calls"))
            .map(|t| t.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    fn command(&self, args: &[&str]) -> Command {
        let done = format!(
            "sh {} {}",
            self.standin.path().join("done.sh").display(),
            self.standin.path().display()
        );
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_emi"));
        cmd.args(args)
            .current_dir(self.repo.path())
            .env("HOME", self.home.path())
            .env("XDG_STATE_HOME", self.state.path())
            .env_remove("XDG_CONFIG_HOME")
            .env("EMI_DONE_COMMAND", done);
        cmd
    }

    fn feed(&self, args: &[&str], input: &Value) -> (i32, String) {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn emi");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.to_string().as_bytes())
            .expect("write stdin");
        let out = child.wait_with_output().expect("wait");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    fn claude(&self, event: &str, hook_event: &str, active: bool) -> (i32, String) {
        let input = json!({
            "session_id": SESSION,
            "cwd": self.repo.path(),
            "hook_event_name": hook_event,
            "stop_hook_active": active,
        });
        self.feed(&["hook", "claude-code", event], &input)
    }

    fn stop(&self) -> Answer {
        self.stop_with(false)
    }

    fn stop_with(&self, active: bool) -> Answer {
        let (code, out) = self.claude("stop", "Stop", active);
        assert_eq!(code, 0, "the hook itself always exits 0; stdout: {out}");
        Answer::parse(&out)
    }

    fn user_message(&self) -> Answer {
        let (code, out) = self.claude("user-prompt-submit", "UserPromptSubmit", false);
        assert_eq!(code, 0);
        Answer::parse(&out)
    }

    fn head(&self) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(self.repo.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn state_files(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut pending = vec![self.state.path().to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).expect("read_dir") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    found.push(path);
                }
            }
        }
        found
    }
}

/// What the hook printed: nothing, or one JSON object.
#[derive(Debug)]
enum Answer {
    Silent,
    Block(String),
    Message(String),
}

impl Answer {
    fn parse(stdout: &str) -> Answer {
        if stdout.trim().is_empty() {
            return Answer::Silent;
        }
        let v: Value = serde_json::from_str(stdout).expect("stdout is one JSON object");
        if v["decision"] == "block" {
            return Answer::Block(v["reason"].as_str().expect("reason").to_owned());
        }
        Answer::Message(
            v["systemMessage"]
                .as_str()
                .expect("systemMessage")
                .to_owned(),
        )
    }

    fn block(&self) -> &str {
        match self {
            Answer::Block(reason) => reason,
            other => panic!("expected a block, got {other:?}"),
        }
    }

    fn message(&self) -> &str {
        match self {
            Answer::Message(text) => text,
            other => panic!("expected a message for the person, got {other:?}"),
        }
    }
}

#[test]
fn a_repository_without_config_is_left_alone() {
    let f = Fixture::configured();
    std::fs::remove_dir_all(f.repo.path().join(".emi")).expect("rm");
    f.user_message();
    assert!(matches!(f.stop(), Answer::Silent));
    assert!(f.calls().is_empty());
    assert!(
        f.state_files().is_empty(),
        "no state for a repository EMI is not set up in"
    );
}

#[test]
fn an_unchanged_tree_is_checked_once() {
    let f = Fixture::configured();
    f.user_message();
    assert!(f.stop().message().contains("Regression evidence: held"));
    assert_eq!(f.calls().len(), 1);
    assert!(matches!(f.stop(), Answer::Silent));
    assert!(matches!(f.stop(), Answer::Silent));
    assert_eq!(f.calls().len(), 1, "the check does not run again");
}

#[test]
fn a_changed_tree_is_checked_again() {
    let mut f = Fixture::configured();
    f.user_message();
    f.stop();
    f.edit();
    f.stop();
    assert_eq!(f.calls().len(), 2);
}

#[test]
fn repair_with_rounds_left_blocks_with_only_the_repair_lines() {
    let f = Fixture::configured();
    f.user_message();
    f.set_done(
        &repair_receipt("tests/test_a.py::test_x fails: expected 1"),
        3,
    );
    let answer = f.stop();
    let reason = answer.block();
    assert!(
        reason.contains("tests/test_a.py::test_x fails: expected 1"),
        "{reason}"
    );
    assert!(reason.contains("repair round 1 of 3"), "{reason}");
    assert!(!reason.contains("SENTINEL-NOT-VERIFIED-LINE"), "{reason}");
    assert!(!reason.contains("Regression evidence"), "{reason}");
    assert!(reason.lines().count() <= 3, "a few short lines: {reason}");
}

#[test]
fn stopping_again_on_the_same_tree_after_a_block_checks_again() {
    let f = Fixture::configured();
    f.user_message();
    f.set_done(&repair_receipt("fix it"), 3);
    f.stop().block();
    f.stop().block();
    assert_eq!(f.calls().len(), 2, "a block is never remembered as checked");
}

#[test]
fn three_repairs_then_the_fourth_stop_is_allowed_as_not_verified() {
    let mut f = Fixture::configured();
    f.user_message();
    f.set_done(&repair_receipt("tests/test_a.py::test_x fails"), 3);
    for round in 1..=3 {
        f.edit();
        let answer = f.stop_with(round > 1);
        assert!(
            answer
                .block()
                .contains(&format!("repair round {round} of 3"))
        );
    }
    f.edit();
    let answer = f.stop_with(true);
    let message = answer.message();
    assert_eq!(message.lines().next(), Some("NOT VERIFIED"), "{message}");
    assert!(
        message.contains("tests/test_a.py::test_x fails"),
        "{message}"
    );
    assert_eq!(f.calls().len(), 4);
    assert!(
        matches!(f.stop_with(true), Answer::Silent),
        "the person has been told once"
    );
}

#[test]
fn the_configured_round_limit_is_used() {
    let mut f = Fixture::new(&json!({"default_branch": "main", "repair_rounds": 1}));
    f.user_message();
    f.set_done(&repair_receipt("fix it"), 3);
    f.edit();
    f.stop().block();
    f.edit();
    assert!(f.stop_with(true).message().starts_with("NOT VERIFIED"));
}

#[test]
fn a_user_message_resets_the_round_count() {
    let mut f = Fixture::configured();
    f.user_message();
    f.set_done(&repair_receipt("fix it"), 3);
    for _ in 0..3 {
        f.edit();
        f.stop().block();
    }
    f.edit();
    assert!(f.stop().message().starts_with("NOT VERIFIED"));
    f.user_message();
    f.edit();
    assert!(f.stop().block().contains("repair round 1 of 3"));
}

#[test]
fn ask_person_is_allowed_with_the_approval_arguments() {
    let f = Fixture::configured();
    f.user_message();
    let mut r = receipt("ask_person");
    r["changed_old_tests"] = json!([{
        "item": "old-test:tests/test_a.py", "path": "tests/test_a.py",
        "kind": "modified", "restored_result": "passes", "approved": false
    }]);
    f.set_done(&r, 4);
    let answer = f.stop();
    let message = answer.message();
    assert!(
        message.contains("--approve old-test:tests/test_a.py"),
        "{message}"
    );
    assert!(message.contains("emi done --repo"), "{message}");
    assert!(message.contains("--baseline-source session"), "{message}");
    assert!(!message.starts_with("NOT VERIFIED"), "{message}");
}

#[test]
fn incomplete_is_allowed_and_never_looks_like_a_pass() {
    let f = Fixture::configured();
    f.user_message();
    let mut r = receipt("incomplete");
    r["regression"]["status"] = json!("unknown");
    r["regression"]["reasons"] = json!(["the test runner is not installed"]);
    f.set_done(&r, 5);
    let answer = f.stop();
    let message = answer.message();
    assert!(message.starts_with("EMI INCOMPLETE"), "{message}");
    assert!(
        message.contains("the test runner is not installed"),
        "{message}"
    );
}

#[test]
fn a_garbage_receipt_is_incomplete() {
    let f = Fixture::configured();
    f.user_message();
    f.set_raw("this is not a receipt", 0);
    let answer = f.stop();
    let message = answer.message();
    assert!(message.starts_with("EMI INCOMPLETE"), "{message}");
    assert!(message.contains("cannot be trusted"), "{message}");
}

#[test]
fn a_receipt_of_an_unknown_schema_is_incomplete() {
    let f = Fixture::configured();
    f.user_message();
    let mut r = receipt("stop");
    r["schema"] = json!("emi-done-receipt-2.0");
    f.set_done(&r, 0);
    assert!(f.stop().message().starts_with("EMI INCOMPLETE"));
}

#[test]
fn an_unexpected_exit_code_is_incomplete_even_with_a_stop_receipt() {
    let f = Fixture::configured();
    f.user_message();
    f.set_done(&receipt("stop"), 1);
    assert!(f.stop().message().starts_with("EMI INCOMPLETE"));
}

#[test]
fn a_usage_error_exit_is_incomplete() {
    let f = Fixture::configured();
    f.user_message();
    f.set_raw("", 2);
    assert!(f.stop().message().starts_with("EMI INCOMPLETE"));
}

#[test]
fn a_missing_done_command_is_incomplete() {
    let f = Fixture::configured();
    f.user_message();
    let mut cmd = f.command(&["hook", "claude-code", "stop"]);
    cmd.env("EMI_DONE_COMMAND", "/nonexistent/emi-done");
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn");
    let input = json!({"session_id": SESSION, "cwd": f.repo.path()});
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.to_string().as_bytes())
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        Answer::parse(&text).message().starts_with("EMI INCOMPLETE"),
        "{text}"
    );
}

#[test]
fn on_the_default_branch_the_session_baseline_is_passed() {
    let f = Fixture::configured();
    let head = f.head();
    f.user_message();
    f.stop();
    let calls = f.calls();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].contains(&format!("--baseline {head} --baseline-source session")),
        "{calls:?}"
    );
    assert!(calls[0].ends_with("--format json"), "{calls:?}");
}

#[test]
fn the_session_baseline_is_the_head_of_the_first_message_only() {
    let mut f = Fixture::configured();
    let first = f.head();
    f.user_message();
    f.edit();
    git(f.repo.path(), &["commit", "-q", "-am", "agent commit"]);
    f.user_message();
    f.stop();
    assert!(
        f.calls()[0].contains(&format!("--baseline {first} ")),
        "{:?}",
        f.calls()
    );
}

#[test]
fn off_the_default_branch_done_resolves_the_baseline_itself() {
    let f = Fixture::configured();
    git(f.repo.path(), &["checkout", "-q", "-b", "feature"]);
    f.user_message();
    f.stop();
    let calls = f.calls();
    assert!(!calls[0].contains("--baseline"), "{calls:?}");
}

#[test]
fn without_a_session_baseline_on_the_default_branch_the_check_is_incomplete() {
    let f = Fixture::configured();
    let answer = f.stop();
    let message = answer.message();
    assert!(message.starts_with("EMI INCOMPLETE"), "{message}");
    assert!(message.contains("first message"), "{message}");
    assert!(f.calls().is_empty());
}

#[test]
fn corrupt_state_is_reported_and_never_blocks() {
    let mut f = Fixture::configured();
    f.user_message();
    let files = f.state_files();
    assert_eq!(files.len(), 1);
    std::fs::write(&files[0], "{ truncated").expect("write");
    f.set_done(&repair_receipt("fix it"), 3);
    f.edit();
    let answer = f.stop_with(true);
    let message = answer.message();
    assert!(message.starts_with("EMI INCOMPLETE"), "{message}");
    assert!(
        message.contains(&files[0].display().to_string()),
        "{message}"
    );
    assert!(
        f.calls().is_empty(),
        "no check is spent when the count cannot be kept"
    );
    assert_eq!(
        std::fs::read_to_string(&files[0]).expect("read"),
        "{ truncated",
        "not silently reset"
    );
}

#[test]
fn state_lives_under_the_state_directory_and_not_in_the_repository() {
    let f = Fixture::configured();
    f.user_message();
    let files = f.state_files();
    assert_eq!(files.len(), 1);
    assert!(files[0].starts_with(f.state.path().join("emi")));
    let status = Command::new("git")
        .arg("-C")
        .arg(f.repo.path())
        .args(["status", "--porcelain"])
        .output()
        .expect("git");
    assert_eq!(
        String::from_utf8_lossy(&status.stdout),
        "",
        "the repository is untouched"
    );
}

#[test]
fn unreadable_hook_input_warns_instead_of_failing_silently() {
    let f = Fixture::configured();
    let (code, out) = f.feed(&["hook", "claude-code", "stop"], &json!({"no": "session"}));
    assert_eq!(code, 0);
    assert!(
        Answer::parse(&out)
            .message()
            .contains("cannot read the hook input")
    );
}

#[test]
fn a_failed_head_lookup_on_the_first_message_is_reported() {
    let f = Fixture::configured();
    std::fs::remove_dir_all(f.repo.path().join(".git")).expect("rm");
    let message = f.user_message();
    assert!(message.message().contains("EMI hook error"), "{message:?}");
}

#[test]
fn opencode_idle_answers_with_a_repair_prompt_the_plugin_sends() {
    let f = Fixture::configured();
    f.set_done(&repair_receipt("tests/test_a.py::test_x fails"), 3);
    let input = json!({"session_id": SESSION, "directory": f.repo.path(), "text": "add a feature"});
    let (code, _) = f.feed(&["hook", "opencode", "user-message"], &input);
    assert_eq!(code, 0);
    let (code, out) = f.feed(&["hook", "opencode", "idle"], &input);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["action"], "repair");
    let prompt = v["prompt"].as_str().expect("prompt");
    assert!(prompt.starts_with("[emi]\n"), "{prompt}");
    assert!(prompt.contains("tests/test_a.py::test_x fails"), "{prompt}");
}

#[test]
fn opencode_does_not_reset_the_rounds_on_its_own_messages() {
    let mut f = Fixture::configured();
    f.set_done(&repair_receipt("fix it"), 3);
    let person =
        json!({"session_id": SESSION, "directory": f.repo.path(), "text": "add a feature"});
    let own = json!({"session_id": SESSION, "directory": f.repo.path(), "text": "[emi]\nfix it"});
    let idle = json!({"session_id": SESSION, "directory": f.repo.path()});
    f.feed(&["hook", "opencode", "user-message"], &person);
    for _ in 0..3 {
        f.edit();
        let (_, out) = f.feed(&["hook", "opencode", "idle"], &idle);
        assert!(out.contains("\"repair\""), "{out}");
        f.feed(&["hook", "opencode", "user-message"], &own);
    }
    f.edit();
    let (_, out) = f.feed(&["hook", "opencode", "idle"], &idle);
    let v: Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["action"], "notify", "{out}");
    assert!(
        v["message"]
            .as_str()
            .expect("message")
            .starts_with("NOT VERIFIED")
    );
    f.feed(&["hook", "opencode", "user-message"], &person);
    f.edit();
    let (_, out) = f.feed(&["hook", "opencode", "idle"], &idle);
    assert!(
        out.contains("\"repair\""),
        "a message from the person resets: {out}"
    );
}

#[test]
fn install_and_uninstall_through_the_command_line() {
    let f = Fixture::configured();
    let run = |args: &[&str]| {
        let out = f.command(args).output().expect("emi");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let said = run(&[
        "hook",
        "install",
        "--harness",
        "claude-code",
        "--emi",
        "emi",
    ]);
    assert!(said.contains("added the Stop hook"), "{said}");
    let settings = f.repo.path().join(".claude/settings.json");
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).expect("read")).expect("json");
    assert_eq!(
        v["hooks"]["Stop"][0]["hooks"][0]["command"],
        "emi hook claude-code stop"
    );
    let again = run(&[
        "hook",
        "install",
        "--harness",
        "claude-code",
        "--emi",
        "emi",
    ]);
    assert!(again.contains("already installed"), "{again}");
    run(&["hook", "uninstall", "--harness", "claude-code"]);
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).expect("read")).expect("json");
    assert_eq!(v, json!({}));

    let said = run(&[
        "hook",
        "install",
        "--harness",
        "opencode",
        "--scope",
        "user",
    ]);
    let plugin = f.home.path().join(".config/opencode/plugins/emi/index.js");
    assert!(said.contains(&plugin.display().to_string()), "{said}");
    assert!(plugin.is_file());
    run(&[
        "hook",
        "uninstall",
        "--harness",
        "opencode",
        "--scope",
        "user",
    ]);
    assert!(!plugin.exists());
}
