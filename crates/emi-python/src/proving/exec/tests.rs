// a test that cannot set itself up must fail loudly, not pass
#![allow(clippy::expect_used)]

use emi_exec::Receipt;

use super::*;

#[test]
fn controller_coverage_inputs_never_follow_candidate_links() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("target");
    std::fs::write(&target, "original").expect("target");
    let link = dir.path().join("plugin.py");
    std::os::unix::fs::symlink(&target, &link).expect("link");
    assert!(write_new(&link, b"replacement").is_err());
    assert_eq!(std::fs::read_to_string(target).expect("read"), "original");
}

const LIMIT: Duration = Duration::from_secs(30);

fn outcome(status: Status, exit_code: Option<i32>, clean: bool) -> Outcome {
    Outcome {
        status,
        exit_code,
        signal: exit_code.is_none().then_some(9),
        duration_ms: 12,
        stdout: String::new(),
        stderr: String::new(),
        stdout_bytes: 0,
        stderr_bytes: 0,
        launch_error: (status == Status::NotLaunched).then(|| "no such file".to_owned()),
        receipt: Receipt {
            verified_clean: clean,
            ..Receipt::default()
        },
    }
}

#[test]
fn only_a_clean_exit_has_an_exit_code() {
    assert_eq!(
        exit_of(&outcome(Status::Exited, Some(1), true), LIMIT),
        RunExit::Code(1)
    );
    let failed = [
        outcome(Status::Exited, Some(0), false),
        outcome(Status::Exited, None, true),
        outcome(Status::TimedOut, None, true),
        outcome(Status::NotLaunched, None, true),
    ];
    let words = [
        "left processes",
        "signal",
        "timed out",
        "could not start: no such file",
    ];
    for (o, word) in failed.iter().zip(words) {
        match exit_of(o, LIMIT) {
            RunExit::Failed(why) => assert!(why.contains(word), "{why}"),
            RunExit::Code(c) => panic!("exit code {c} for {:?}", o.status),
        }
    }
}

#[test]
fn how_a_run_ended_does_not_vary_between_runs() {
    // two timeouts that took different times read the same
    let mut slow = outcome(Status::TimedOut, None, true);
    let mut slower = slow.clone();
    slow.duration_ms = 30_001;
    slower.duration_ms = 30_907;
    assert_eq!(exit_of(&slow, LIMIT), exit_of(&slower, LIMIT));
    assert_eq!(
        exit_of(&slow, LIMIT),
        RunExit::Failed("timed out (limit 30 s)".into())
    );
}

#[test]
fn error_lines_name_what_failed_without_paths_or_timings() {
    let dir = Path::new("/scratch/emi-profile-candidate-4242-99");
    let out = "==== ERRORS ====\n_____ ERROR collecting tests/test_x.py _____\nImportError while importing test module '/scratch/emi-profile-candidate-4242-99/tests/test_x.py'.\ntests/test_x.py:1: in <module>\nE   ModuleNotFoundError: No module named 'nothere'\nE   second line\nE\nE   third /scratch/emi-profile-candidate-4242-99/x.py\nE   fourth\n!!! Interrupted: 1 error during collection !!!\n1 error in 0.13s\n";
    assert_eq!(
        error_lines(out, dir),
        "second line; third <tree>/x.py; fourth"
    );
    assert_eq!(error_lines("1 error in 0.13s\n", dir), "");
    assert_eq!(with(String::new()), "");
    assert_eq!(with("x".into()), ": x");
}

#[test]
fn a_report_that_is_missing_a_link_or_too_large_is_not_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(read_report(dir.path(), "none.xml").is_err());
    std::fs::create_dir(dir.path().join("a_dir")).expect("dir");
    assert!(read_report(dir.path(), "a_dir").is_err());
    #[cfg(unix)]
    {
        let real = dir.path().join("real.xml");
        std::fs::write(
            &real,
            r#"<testsuites><testsuite tests="0" failures="0" errors="0" skipped="0"/></testsuites>"#,
        )
        .expect("write");
        std::os::unix::fs::symlink(&real, dir.path().join("link.xml")).expect("symlink");
        assert!(read_report(dir.path(), "real.xml").is_ok());
        let linked = read_report(dir.path(), "link.xml");
        assert!(linked.is_err_and(|e| e.contains("not a regular file")));
    }
}
