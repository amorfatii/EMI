//! CI publishes exactly the action the installed done check returned.
#![allow(clippy::expect_used)]
#[path = "support/project.rs"]
mod project;

use project::Project;
use std::path::Path;
use std::process::Command;

fn publish(p: &Project) -> String {
    let outputs = p.state.path().join("outputs");
    let summary = p.state.path().join("summary");
    std::fs::write(&outputs, "").expect("outputs");
    let bin = Path::new(env!("CARGO_BIN_EXE_emi"))
        .parent()
        .expect("bin directory");
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").expect("PATH"),
    ));
    let out = Command::new("bash")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ci/done-status.sh"))
        .current_dir(p.path())
        .env("BASELINE", "main")
        .env("CANDIDATE", "HEAD")
        .env("XDG_STATE_HOME", p.state.path())
        .env("GITHUB_OUTPUT", &outputs)
        .env("GITHUB_STEP_SUMMARY", summary)
        .env("PATH", std::env::join_paths(paths).expect("PATH"))
        .output()
        .expect("CI publication");
    assert!(out.status.success(), "{out:?}");
    assert!(p.path().join("emi-done.json").is_file());
    assert!(
        std::fs::read_to_string(p.path().join("emi-comment.md"))
            .expect("comment")
            .contains("EMI never merges")
    );
    std::fs::read_to_string(outputs).expect("outputs")
}

#[test]
fn only_stop_publishes_exit_zero_and_review_does_not_pass() {
    let p = Project::new();
    p.write(
        ".gitignore",
        ".venv\n__pycache__/\n.pytest_cache/\nemi-done.json\nemi-comment.md\n",
    );
    p.commit("ignore CI receipts");
    let held = publish(&p);
    assert!(
        held.contains("code=0\naction=stop\nregression=held\nchange=none"),
        "{held}"
    );
    p.write("app.py", project::BROKEN);
    p.commit("broken change");
    let broken = publish(&p);
    assert!(
        broken.contains("code=3\naction=repair\nregression=broken"),
        "{broken}"
    );
    p.write("app.py", project::APP);
    p.write(
        "tests/test_app.py",
        "from app import add\n\ndef test_add():\n    assert add(1, 2) == 3\n",
    );
    p.commit("delete one old test");
    let review = publish(&p);
    assert!(review.contains("code=4\naction=ask_person"), "{review}");
}
