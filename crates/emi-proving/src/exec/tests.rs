// a test that cannot set itself up must fail loudly, not pass
#![allow(clippy::expect_used)]

use emi_exec::Receipt;

use super::*;

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
        exit_of(&outcome(Status::Exited, Some(1), true)),
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
        match exit_of(o) {
            RunExit::Failed(why) => assert!(why.contains(word), "{why}"),
            RunExit::Code(c) => panic!("exit code {c} for {:?}", o.status),
        }
    }
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
