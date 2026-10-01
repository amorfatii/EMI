//! Installed native V2 plugin with real EMI and pytest, scripted public
//! harness context. This does not use a model or claim a usefulness result.
#![allow(clippy::expect_used)]
#[path = "support/project.rs"]
mod project;
use std::path::Path;
use std::process::Command;

#[test]
fn installed_v2_adapter_drives_the_real_completion_loop() {
    let p = project::Project::new();
    let install = p.emi(&["hook", "install", "--harness", "opencode"], None);
    assert!(install.status.success(), "{install:?}");
    let out = Command::new("node")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/opencode-v2.mjs"))
        .arg(p.path().join(".opencode/plugins/emi/index.js"))
        .arg(p.path())
        .env("XDG_STATE_HOME", p.state.path())
        .env_remove("EMI_DONE_COMMAND")
        .output()
        .expect("Node is required for the native OpenCode adapter test");
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn the_installed_hook_is_not_counted_as_python_product_code() {
    let p = project::Project::new();
    p.write(".gitignore", ".venv\n__pycache__/\n.pytest_cache/\n");
    let install = p.emi(&["hook", "install", "--harness", "opencode"], None);
    assert!(install.status.success(), "{install:?}");
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(receipt["change"]["status"], "none", "{receipt}");
    assert!(
        receipt["change"]["functions"]
            .as_array()
            .expect("functions")
            .is_empty()
    );
    assert!(
        receipt["change"]["not_measured"]
            .to_string()
            .contains("not Python")
    );
}

// The native application's admission/events cannot be substituted by the
// context fixture: they caught a different completion event and invisible
// non-resuming synthetic notes. No model or account is used in this test.
#[test]
#[ignore = "requires an installed OpenCode V2 executable; run explicitly"]
fn native_harness() {
    let p = project::Project::new();
    let out = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/opencode-live.py"))
        .arg(p.path())
        .arg(env!("CARGO_BIN_EXE_emi"))
        .arg(p.state.path())
        .output()
        .expect("native OpenCode integration test");
    assert!(out.status.success(), "{out:?}");
}
