//! The MCP surface: protocol handshake, errors, and a gate that measures
//! an agent's uncommitted change itself.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use emi_mcp::handle;
use serde_json::{Value, json};

fn call(id: u64, method: &str, params: Value) -> Value {
    handle(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
        .unwrap_or_default()
}

#[test]
fn handshake_lists_tools_and_rejects_unknowns() {
    let init = call(1, "initialize", json!({"protocolVersion": "2025-03-26"}));
    assert_eq!(
        init["result"]["protocolVersion"], "2025-03-26",
        "a supported version is echoed"
    );
    assert_eq!(
        call(2, "initialize", json!({"protocolVersion": "1999-01-01"}))["result"]["protocolVersion"],
        "2025-06-18"
    );
    assert!(handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
    let tools = call(3, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(
        names,
        [
            "emi_gate",
            "emi_explain",
            "emi_hotspots",
            "emi_accept_rewrite",
            "emi_task_verify",
            "emi_task_status",
            "emi_guard",
            "emi_scan"
        ]
    );
    assert_eq!(call(4, "nope", json!({}))["error"]["code"], -32601);
    assert_eq!(
        call(5, "tools/call", json!({"name": "nope"}))["error"]["code"],
        -32602
    );
    let failed = call(
        6,
        "tools/call",
        json!({"name": "emi_gate", "arguments": {"repo": "/nonexistent-emi-repo"}}),
    );
    assert_eq!(
        failed["result"]["isError"], true,
        "tool failures are results, not protocol errors"
    );
}

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) {
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
}

/// A function whose branching and length put it far past the erosion
/// thresholds (cc 10, 50 lines).
fn sprawling() -> String {
    let mut body = String::from("def sprawling(x):\n    total = 0\n");
    for i in 0..60 {
        let _ = writeln!(body, "    if x == {i}:\n        total += {i}");
    }
    body + "    return total\n"
}

#[test]
fn gate_blocks_an_uncommitted_sprawling_function() {
    let r = Repo(std::env::temp_dir().join(format!("emi-mcp-gate-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&r.0);
    std::fs::create_dir_all(r.0.join("src")).unwrap_or_default();
    git(&r.0, &["init", "-q", "-b", "main"]);
    std::fs::write(r.0.join("src/app.py"), "def small(x):\n    return x + 1\n").unwrap_or_default();
    git(&r.0, &["add", "-A"]);
    git(&r.0, &["commit", "-q", "-m", "base"]);
    let args = json!({"repo": r.0, "source": "src", "probe": "python"});
    let clean = call(
        1,
        "tools/call",
        json!({"name": "emi_gate", "arguments": args}),
    );
    let verdict: Value = serde_json::from_str(
        clean["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("{}"),
    )
    .unwrap_or_default();
    assert_eq!(verdict["decision"], "pass", "{verdict}");
    std::fs::write(
        r.0.join("src/app.py"),
        format!("def small(x):\n    return x + 1\n\n\n{}", sprawling()),
    )
    .unwrap_or_default();
    let grown = call(
        2,
        "tools/call",
        json!({"name": "emi_gate", "arguments": args}),
    );
    let verdict: Value = serde_json::from_str(
        grown["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("{}"),
    )
    .unwrap_or_default();
    assert_eq!(verdict["decision"], "block", "{verdict}");
    assert_eq!(verdict["primary"]["id"], "app.py");
    assert_eq!(verdict["primary"]["dimension"], "E");
    assert!(matches!(
        verdict["next_step"].as_str(),
        Some("split" | "shrink")
    ));
}

#[test]
fn guard_reports_a_skipped_test_in_the_worktree() {
    let r = Repo(std::env::temp_dir().join(format!("emi-mcp-guard-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&r.0);
    std::fs::create_dir_all(r.0.join("tests")).unwrap_or_default();
    git(&r.0, &["init", "-q", "-b", "main"]);
    std::fs::write(
        r.0.join("tests/test_a.py"),
        "def test_a():\n    assert True\n",
    )
    .unwrap_or_default();
    git(&r.0, &["add", "-A"]);
    git(&r.0, &["commit", "-q", "-m", "base"]);
    std::fs::write(
        r.0.join("tests/test_a.py"),
        "import pytest\n\n\n@pytest.mark.skip\ndef test_a():\n    assert True\n",
    )
    .unwrap_or_default();
    let out = call(
        1,
        "tools/call",
        json!({"name": "emi_guard", "arguments": {"repo": r.0}}),
    );
    let report: Value =
        serde_json::from_str(out["result"]["content"][0]["text"].as_str().unwrap_or("{}"))
            .unwrap_or_default();
    assert_eq!(report["decision"], "block", "{report}");
    assert_eq!(report["violations"][0]["id"], "tests.skip_added");
    assert_eq!(report["violations"][0]["line"], 4);
}
