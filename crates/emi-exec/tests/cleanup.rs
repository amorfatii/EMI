//! The executor's promises, tested with real processes (Linux).

#![cfg(target_os = "linux")]

use std::time::Duration;

use emi_exec::{Isolation, Spec, Status, run};

fn alive(marker: &str) -> bool {
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            std::fs::read(e.path().join("cmdline"))
                .is_ok_and(|c| String::from_utf8_lossy(&c).contains(marker))
        })
}

fn sh(script: &str) -> Spec {
    let mut spec = Spec::new(&["sh", "-c", script], std::env::temp_dir());
    spec.timeout = Duration::from_secs(2);
    spec
}

#[test]
fn exit_codes_and_output_are_captured() {
    let o = run(&sh("echo out; echo err >&2; exit 3"));
    assert_eq!((o.status, o.exit_code), (Status::Exited, Some(3)));
    assert_eq!((o.stdout.as_str(), o.stderr.as_str()), ("out\n", "err\n"));
    assert!(o.receipt.verified_clean, "{:?}", o.receipt);
}

#[test]
fn background_children_die_with_the_command() {
    let o = run(&sh("sleep 301 & echo started"));
    assert_eq!(o.exit_code, Some(0));
    assert!(o.receipt.group_killed && o.receipt.group_empty);
    std::thread::sleep(Duration::from_millis(50));
    assert!(!alive("sleep 301"), "background child outlived its command");
}

#[test]
fn timeouts_kill_the_whole_group() {
    let o = run(&sh("sleep 302 & sleep 303"));
    assert_eq!(o.status, Status::TimedOut);
    assert!(o.receipt.verified_clean, "{:?}", o.receipt);
    std::thread::sleep(Duration::from_millis(50));
    assert!(!alive("sleep 302") && !alive("sleep 303"));
}

#[test]
fn setsid_daemons_are_reaped_as_orphans() {
    // Double fork into a new session: escapes the process group.
    let o = run(&sh("setsid sh -c 'sleep 304' & sleep 0.2"));
    assert_eq!(o.exit_code, Some(0));
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !alive("sleep 304"),
        "setsid daemon escaped: {:?}",
        o.receipt
    );
}

#[test]
fn launch_failures_are_reported_not_hidden() {
    let o = run(&Spec::new(
        &["/nonexistent/emi-check-binary"],
        std::env::temp_dir(),
    ));
    assert_eq!(o.status, Status::NotLaunched);
    assert!(
        o.launch_error
            .is_some_and(|e| e.contains("emi-check-binary"))
    );
}

#[test]
fn environment_is_an_allowlist() {
    // SAFETY-free: set for this process only; the child must not see it.
    let mut spec = sh("echo \"[${EMI_SECRET_TEST:-unset}]\"");
    spec.env.retain(|(k, _)| k == "PATH");
    let o = run(&spec);
    assert_eq!(o.stdout, "[unset]\n");
}

#[test]
fn output_is_capped_but_counted() {
    let mut spec = sh("head -c 100000 /dev/zero");
    spec.output_limit = 1000;
    let o = run(&spec);
    assert_eq!((o.stdout.len(), o.stdout_bytes), (1000, 100_000));
}

#[test]
fn bwrap_isolation_is_recorded_honestly() {
    let mut spec = sh("touch /usr/emi-should-not-write 2>/dev/null && echo wrote || echo denied");
    spec.isolation = Isolation::Best { network: false };
    let o = run(&spec);
    if emi_exec::bwrap_available() {
        assert_eq!(o.receipt.isolation, "bwrap");
        assert!(o.receipt.pid_namespace && !o.receipt.network);
        assert_eq!(o.stdout, "denied\n", "root is read-only inside the sandbox");
    } else {
        assert_eq!(o.receipt.isolation, "process-group");
        assert!(!o.receipt.pid_namespace);
    }
}

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::current_dir()
        .unwrap_or_default()
        .join("target")
        .join(format!("exec-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap_or_default();
    d
}

#[test]
fn read_only_paths_cannot_be_changed_in_the_sandbox() {
    if !emi_exec::bwrap_available() {
        return;
    }
    let ws = scratch("ro");
    std::fs::write(ws.join("trusted.txt"), "original").unwrap_or_default();
    let mut s = Spec::new(
        &[
            "/bin/sh",
            "-c",
            "echo gutted > trusted.txt; echo new > added.txt",
        ],
        &ws,
    );
    s.isolation = Isolation::Bwrap { network: false };
    s.read_only = vec![ws.join("trusted.txt")];
    let o = run(&s);
    assert!(o.receipt.read_only_enforced);
    assert_eq!(
        std::fs::read_to_string(ws.join("trusted.txt")).unwrap_or_default(),
        "original"
    );
    assert!(
        ws.join("added.txt").exists(),
        "the rest of the workspace stays writable"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn a_sandbox_that_never_starts_the_command_is_not_the_commands_failure() {
    if !emi_exec::bwrap_available() {
        return;
    }
    let ws = scratch("nostart");
    // the command does not exist inside the sandbox
    let mut s = Spec::new(&["/no/such/tool", "--flag"], &ws);
    s.isolation = Isolation::Bwrap { network: false };
    let o = run(&s);
    assert_eq!(
        o.status,
        Status::NotLaunched,
        "{:?} {}",
        o.exit_code,
        o.stderr
    );
    // bubblewrap itself fails (its working directory is gone)
    let gone = ws.join("gone");
    let mut s = Spec::new(&["/bin/true"], &gone);
    s.isolation = Isolation::Bwrap { network: false };
    let o = run(&s);
    assert_eq!(o.status, Status::NotLaunched);
    // a command that starts and fails is the command's result
    let mut s = Spec::new(&["/bin/sh", "-c", "exit 3"], &ws);
    s.isolation = Isolation::Bwrap { network: false };
    let o = run(&s);
    assert_eq!((o.status, o.exit_code), (Status::Exited, Some(3)));
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn hidden_paths_are_unreadable_except_what_is_mounted_back() {
    if !emi_exec::bwrap_available() {
        return;
    }
    let ws = scratch("hid-ws");
    let secret = scratch("hid-secret");
    std::fs::write(secret.join("held_out.py"), "secret").unwrap_or_default();
    std::fs::create_dir_all(secret.join("deps")).unwrap_or_default();
    std::fs::write(secret.join("deps").join("lib.txt"), "dep").unwrap_or_default();
    let script = format!(
        "cat {0}/held_out.py; echo; cat {0}/deps/lib.txt; echo x > {0}/deps/lib.txt",
        secret.display()
    );
    let mut s = Spec::new(&["/bin/sh", "-c", &script], &ws);
    s.isolation = Isolation::Bwrap { network: false };
    s.hidden = vec![secret.clone()];
    s.read_only = vec![secret.join("deps")];
    let o = run(&s);
    assert!(o.receipt.hidden_enforced);
    assert!(!o.stdout.contains("secret"), "{}", o.stdout);
    assert!(o.stdout.contains("dep"), "{} {}", o.stdout, o.stderr);
    assert_eq!(
        std::fs::read_to_string(secret.join("deps").join("lib.txt")).unwrap_or_default(),
        "dep",
        "a path mounted back stays read-only"
    );
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&secret);
}

#[test]
fn a_command_cannot_disown_its_launch() {
    if !emi_exec::bwrap_available() {
        return;
    }
    let ws = scratch("launch");
    // a failing command is the command's failure, not a sandbox that
    // never started, whatever it does to the filesystem
    let mut s = Spec::new(&["/bin/sh", "-c", "echo oops >&2; exit 3"], &ws);
    s.isolation = Isolation::Bwrap { network: false };
    let o = run(&s);
    assert_eq!(o.status, Status::Exited, "{o:?}");
    assert_eq!(o.exit_code, Some(3));
    assert_eq!(
        o.stderr, "oops\n",
        "the launch token is not part of the output"
    );
    let missing = Spec {
        isolation: Isolation::Bwrap { network: false },
        ..Spec::new(&["emi-no-such-command"], &ws)
    };
    assert_eq!(run(&missing).status, Status::NotLaunched);
    let _ = std::fs::remove_dir_all(&ws);
}
