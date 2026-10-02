//! `emi-exec`: run a check command so that nothing it starts outlives it
//! (audit F-01), and say exactly how that was verified.
//!
//! - argv only, never a shell; environment is an explicit allowlist;
//! - the command leads its own process group, and the whole group is
//!   killed on every exit path (normal exit, timeout, launch failure);
//! - on Linux the executor is a child subreaper, so double-forked
//!   orphans reparent to it and are killed and reaped too;
//! - with bubblewrap the command also runs in its own PID namespace (all
//!   descendants die with it), read-only outside the workspace, optionally
//!   without network;
//! - the returned [`Receipt`] records what was actually enforced and
//!   whether the group was verified empty afterwards. It never claims an
//!   isolation that was not applied.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::Serialize;

/// How much to isolate the command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Isolation {
    /// Process group + subreaper only.
    None,
    /// bubblewrap: PID namespace, read-only root, writable workspace.
    Bwrap { network: bool },
    /// bubblewrap when available, else `None` (recorded in the receipt).
    Best { network: bool },
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    /// The complete environment (nothing is inherited).
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
    /// Bytes kept per stream; the rest is counted and dropped.
    pub output_limit: usize,
    pub isolation: Isolation,
    /// Paths inside the workspace mounted read-only over it (bubblewrap
    /// only): trusted tests the command must not change.
    pub read_only: Vec<PathBuf>,
    /// Host paths replaced by an empty directory in the sandbox (bubblewrap
    /// only): the root is mounted read-only, so without this the command
    /// could still read them. `read_only` paths inside them stay visible.
    pub hidden: Vec<PathBuf>,
}

impl Spec {
    pub fn new(argv: &[&str], cwd: impl Into<PathBuf>) -> Self {
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into());
        Spec {
            argv: argv.iter().map(|s| (*s).to_owned()).collect(),
            cwd: cwd.into(),
            env: vec![("PATH".into(), path), ("LANG".into(), "C.UTF-8".into())],
            timeout: Duration::from_secs(120),
            output_limit: 1 << 20,
            isolation: Isolation::None,
            read_only: Vec::new(),
            hidden: Vec::new(),
        }
    }
}

/// How cleanup was enforced and verified.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Receipt {
    pub isolation: String,
    pub pid_namespace: bool,
    pub network: bool,
    pub process_group: Option<i32>,
    pub group_killed: bool,
    /// No process was left in the group afterwards (signal-0 probe).
    pub group_empty: bool,
    pub subreaper: bool,
    /// Reparented descendants found and killed after the command ended.
    pub orphans_killed: Vec<i32>,
    /// Every check passed: the command left nothing running.
    pub verified_clean: bool,
    /// `read_only` paths were mounted read-only (bubblewrap only).
    pub read_only_enforced: bool,
    /// A crash of the command is not handed to the system's crash handler
    /// (the core limit its processes inherit is one byte).
    pub cores_quiet: bool,
    /// `hidden` paths were replaced by empty directories (bubblewrap only).
    pub hidden_enforced: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Exited,
    TimedOut,
    /// Could not start (missing executable, bad cwd): an environment fault.
    NotLaunched,
}

#[derive(Clone, Debug, Serialize)]
pub struct Outcome {
    pub status: Status,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub launch_error: Option<String>,
    pub receipt: Receipt,
}

/// Whether bubblewrap can run here (installed and permitted).
pub fn bwrap_available() -> bool {
    Command::new("bwrap")
        .args([
            "--ro-bind",
            "/",
            "/",
            "--unshare-pid",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "true",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn resolve_isolation(isolation: &Isolation) -> Option<bool> {
    match isolation {
        Isolation::None => None,
        Isolation::Bwrap { network } => Some(*network),
        Isolation::Best { network } => bwrap_available().then_some(*network),
    }
}

/// The sandbox's launcher: it proves the sandbox came up and found the
/// command by writing the token (argument 0) as the first line of stderr,
/// then replaces itself with the command. Bytes already written cannot be
/// taken back, so the command cannot make its own failure look like a
/// sandbox that never started (a marker file could be deleted). The
/// command's argv is passed through as arguments, never interpreted.
const LAUNCHER: &str =
    r#"command -v "$1" >/dev/null 2>&1 || exit 127; printf '%s\n' "$0" >&2; exec "$@""#;

/// Starts bubblewrap with the file named by its first argument open as
/// descriptor 9, where `--seccomp 9` reads the filter from. The shell opens
/// it because a descriptor this process opens is closed on exec.
const WITH_FILTER: &str = r#"f="$1"; shift; exec bwrap --seccomp 9 "$@" 9<"$f""#;

/// A seccomp filter that makes changing the core-size limit fail with
/// `EPERM` and allows everything else.
///
/// The one-byte core limit ([`platform::quiet_cores`]) keeps a crash away
/// from the system's crash handler only while it stays one byte. Programs
/// that crash on purpose lower it to zero first, meaning well (CPython's
/// own `faulthandler` test helpers do), and with zero the handler is
/// called again: a test suite that aborts child interpreters deliberately
/// then fills the desktop with crash reports. Inside the sandbox the limit
/// therefore cannot be changed. Reading it is allowed.
///
/// The program is classic BPF over `struct seccomp_data`, written out by
/// hand for the architectures listed; elsewhere there is no filter.
fn core_limit_filter() -> Option<Vec<u8>> {
    // (audit architecture, setrlimit, prlimit64)
    let (arch, setrlimit, prlimit64): (u32, u32, u32) = if cfg!(target_arch = "x86_64") {
        (0xC000_003E, 160, 302)
    } else if cfg!(target_arch = "aarch64") {
        (0xC000_00B7, 164, 261)
    } else {
        return None;
    };
    const LOAD: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
    const EQUAL: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
    const RETURN: u16 = 0x06; // BPF_RET | BPF_K
    const ALLOW: u32 = 0x7fff_0000;
    const EPERM: u32 = 0x0005_0000 | 1;
    const RLIMIT_CORE: u32 = 4;
    // offsets in seccomp_data: nr 0, arch 4, args from 16, eight bytes each
    let program: [(u16, u8, u8, u32); 16] = [
        (LOAD, 0, 0, 4),
        (EQUAL, 0, 12, arch),
        (LOAD, 0, 0, 0),
        (EQUAL, 2, 0, setrlimit),
        (EQUAL, 3, 0, prlimit64),
        (RETURN, 0, 0, ALLOW),
        // setrlimit(resource, new)
        (LOAD, 0, 0, 16),
        (EQUAL, 7, 6, RLIMIT_CORE),
        // prlimit64(pid, resource, new, old): only a call that sets
        (LOAD, 0, 0, 24),
        (EQUAL, 0, 4, RLIMIT_CORE),
        (LOAD, 0, 0, 32),
        (EQUAL, 0, 3, 0),
        (LOAD, 0, 0, 36),
        (EQUAL, 0, 1, 0),
        (RETURN, 0, 0, ALLOW),
        (RETURN, 0, 0, EPERM),
    ];
    let mut bytes = Vec::with_capacity(program.len() * 8);
    for (code, jt, jf, k) in program {
        bytes.extend_from_slice(&code.to_ne_bytes());
        bytes.extend_from_slice(&[jt, jf]);
        bytes.extend_from_slice(&k.to_ne_bytes());
    }
    Some(bytes)
}

/// The filter in a file bubblewrap can be given, written once per process.
/// `None` where there is no filter or it could not be written: the sandbox
/// then runs without it, as before.
fn filter_file() -> Option<&'static Path> {
    static FILE: OnceLock<Option<PathBuf>> = OnceLock::new();
    FILE.get_or_init(|| {
        let bytes = core_limit_filter()?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!("emi-seccomp-{}-{nanos}", std::process::id()));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, &bytes))
            .ok()?;
        Some(path)
    })
    .as_deref()
}

/// bubblewrap, behind the shell that hands it the filter when there is one.
fn bwrap_command() -> Command {
    match filter_file() {
        Some(filter) => {
            let mut c = Command::new("/bin/sh");
            c.args(["-c", WITH_FILTER, "emi-bwrap"]).arg(filter);
            c
        }
        None => Command::new("bwrap"),
    }
}

fn command(spec: &Spec, bwrap: Option<bool>, token: Option<&str>) -> Command {
    let mut cmd = match bwrap {
        None => {
            let mut c = Command::new(&spec.argv[0]);
            c.args(&spec.argv[1..]);
            c
        }
        Some(network) => {
            let cwd = spec.cwd.to_string_lossy().into_owned();
            let mut c = bwrap_command();
            c.args([
                "--die-with-parent",
                "--unshare-pid",
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
            ]);
            c.args(["--tmpfs", "/tmp"]);
            // the sockets of the system's and the session's services live
            // under /run: with them visible a command can ask the host's
            // service manager to change units (a test suite did, sixty
            // times, each one an authentication prompt on the desktop)
            c.args(["--tmpfs", "/run"]);
            if network {
                // name resolution, where the resolver's files are under /run
                c.args([
                    "--ro-bind-try",
                    "/run/systemd/resolve",
                    "/run/systemd/resolve",
                ]);
            }
            for h in spec.hidden.iter().filter(|h| h.exists()) {
                c.arg("--tmpfs").arg(h);
            }
            c.args(["--bind", &cwd, &cwd]);
            for ro in &spec.read_only {
                if ro.exists() {
                    c.arg("--ro-bind").arg(ro).arg(ro);
                }
            }
            c.args(["--chdir", &cwd]);
            if !network {
                c.arg("--unshare-net");
            }
            c.arg("--");
            match token {
                Some(t) => {
                    c.args(["/bin/sh", "-c", LAUNCHER, t]).args(&spec.argv);
                }
                None => {
                    c.args(&spec.argv);
                }
            }
            c
        }
    };
    cmd.current_dir(&spec.cwd)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)));
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

/// Read a stream to the end, keeping at most `limit` bytes.
fn drain(
    mut r: impl Read + Send + 'static,
    limit: usize,
) -> std::thread::JoinHandle<(Vec<u8>, u64)> {
    std::thread::spawn(move || {
        let (mut kept, mut total, mut buf) = (Vec::new(), 0u64, [0u8; 8192]);
        while let Ok(n) = r.read(&mut buf) {
            if n == 0 {
                break;
            }
            total += n as u64;
            let room = limit.saturating_sub(kept.len());
            kept.extend_from_slice(&buf[..n.min(room)]);
        }
        (kept, total)
    })
}

fn not_launched(error: String, mut receipt: Receipt) -> Outcome {
    receipt.group_empty = true;
    receipt.verified_clean = true;
    Outcome {
        status: Status::NotLaunched,
        exit_code: None,
        signal: None,
        duration_ms: 0,
        stdout: String::new(),
        stderr: String::new(),
        stdout_bytes: 0,
        stderr_bytes: 0,
        launch_error: Some(error),
        receipt,
    }
}

/// Wait for the command; past the deadline, kill its group.
fn wait(
    child: &mut std::process::Child,
    pgid: Option<i32>,
    timeout: Duration,
    start: Instant,
) -> (Option<std::process::ExitStatus>, bool) {
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return (Some(s), false),
            Ok(None) if start.elapsed() >= timeout => {
                if let Some(g) = pgid {
                    platform::kill_group(g);
                }
                let _ = child.kill();
                return (child.wait().ok(), true);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => return (None, false),
        }
    }
}

/// A token for one launch: unique per run, so no earlier output matches.
fn launch_token() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("emi-sandbox-ready-{}-{nanos}", std::process::id())
}

/// Start the command and leave it running after this process ends: its own
/// process group, no terminal, no environment but `spec.env`. This is for
/// work whose result is picked up later from what it writes (a warmed
/// cache), never for a run whose outcome matters now: nothing waits for
/// it, and `timeout`, `output_limit` and `isolation` do not apply, so the
/// command must bound itself. Returns its process id.
pub fn spawn_detached(spec: &Spec) -> Result<u32, String> {
    use std::process::{Command, Stdio};
    let (program, args) = spec
        .argv
        .split_first()
        .ok_or_else(|| "nothing to start: the command is empty".to_owned())?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn()
        .map(|child| child.id())
        .map_err(|e| format!("{program}: {e}"))
}

/// Run the command; never returns before its group is gone.
pub fn run(spec: &Spec) -> Outcome {
    let start = Instant::now();
    let subreaper = platform::enable_subreaper();
    let bwrap = resolve_isolation(&spec.isolation);
    let mut receipt = Receipt {
        isolation: if bwrap.is_some() {
            "bwrap".into()
        } else {
            "process-group".into()
        },
        pid_namespace: bwrap.is_some(),
        network: bwrap.is_none_or(|n| n),
        subreaper,
        cores_quiet: platform::quiet_cores(),
        ..Receipt::default()
    };
    if spec.argv.is_empty() {
        return not_launched("empty argv".into(), receipt);
    }
    receipt.read_only_enforced = bwrap.is_some() && !spec.read_only.is_empty();
    receipt.hidden_enforced = bwrap.is_some() && !spec.hidden.is_empty();
    // readiness: the launcher writes this token once the sandbox is up
    let token = bwrap.map(|_| launch_token());
    let mut child = match command(spec, bwrap, token.as_deref()).spawn() {
        Ok(c) => c,
        Err(e) => return not_launched(format!("{}: {e}", spec.argv[0]), receipt),
    };
    let pgid = i32::try_from(child.id()).ok();
    let started = pgid.and_then(platform::start_time);
    receipt.process_group = pgid;
    let out = child.stdout.take().map(|s| drain(s, spec.output_limit));
    let err = child.stderr.take().map(|s| drain(s, spec.output_limit));
    let (status, timed_out) = wait(&mut child, pgid, spec.timeout, start);
    // Every exit path: the group goes, then reparented orphans.
    if let Some(g) = pgid {
        receipt.group_killed = platform::kill_group(g);
        receipt.group_empty = platform::group_empty(g);
    }
    receipt.orphans_killed = if subreaper {
        platform::reap_orphans(pgid, started)
    } else {
        Vec::new()
    };
    receipt.verified_clean = receipt.group_empty && (subreaper || receipt.pid_namespace);
    let (stdout, stdout_bytes) = out.and_then(|h| h.join().ok()).unwrap_or_default();
    let (mut stderr, mut stderr_bytes) = err.and_then(|h| h.join().ok()).unwrap_or_default();
    let (exit_code, signal) = status.map_or((None, None), platform::exit_parts);
    let launched = match &token {
        None => true,
        Some(t) => {
            let line = format!("{t}\n");
            let found = stderr.starts_with(line.as_bytes());
            if found {
                stderr.drain(..line.len());
                stderr_bytes = stderr_bytes.saturating_sub(line.len() as u64);
            }
            found
        }
    };
    if !launched && !timed_out {
        // the sandbox never came up, or the command does not exist: an
        // environment fault, not the candidate's result
        let why = String::from_utf8_lossy(&stderr)
            .trim()
            .chars()
            .take(300)
            .collect::<String>();
        return not_launched(format!("sandbox did not start the command: {why}"), receipt);
    }
    Outcome {
        status: if timed_out {
            Status::TimedOut
        } else {
            Status::Exited
        },
        exit_code,
        signal,
        duration_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        stdout_bytes,
        stderr_bytes,
        launch_error: None,
        receipt,
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::sync::OnceLock;

    use rustix::process::{
        Pid, Resource, Rlimit, Signal, WaitOptions, getpid, getrlimit, kill_process,
        kill_process_group, setrlimit, test_kill_process_group, waitpid,
    };

    pub fn enable_subreaper() -> bool {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| rustix::process::set_child_subreaper(Some(getpid())).is_ok())
    }

    /// Keep the commands' crashes out of the system's crash reports. Test
    /// suites crash child processes on purpose, and each one would
    /// otherwise be recorded by the crash handler and shown to the person
    /// as a crashed program. The kernel skips a piped crash handler only
    /// when the core limit is exactly one byte (zero still calls it), so
    /// this process sets its own soft limit to one, once, and every command
    /// it starts inherits that. The hard limit is left alone.
    pub fn quiet_cores() -> bool {
        static DONE: OnceLock<bool> = OnceLock::new();
        *DONE.get_or_init(|| {
            let now = getrlimit(Resource::Core);
            // a hard limit of zero cannot be raised to one: nothing to do
            if now.maximum == Some(0) {
                return false;
            }
            setrlimit(
                Resource::Core,
                Rlimit {
                    current: Some(1),
                    maximum: now.maximum,
                },
            )
            .is_ok()
        })
    }

    pub fn kill_group(pgid: i32) -> bool {
        Pid::from_raw(pgid).is_some_and(|p| {
            matches!(
                kill_process_group(p, Signal::KILL),
                Ok(()) | Err(rustix::io::Errno::SRCH)
            )
        })
    }

    /// Processes reparented to us that belong to the run led by `pgid`.
    fn ours(pgid: i32) -> Vec<(i32, i32, i32, u64)> {
        let me = getpid().as_raw_nonzero().get();
        std::fs::read_dir("/proc")
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let pid: i32 = name.parse().ok()?;
                let (ppid, pgrp, sid, start) = ids(&name)?;
                (ppid == me && pid != pgid).then_some((pid, pgrp, sid, start))
            })
            .collect()
    }

    /// Reap one specific child (never `waitpid(-1)`: that would steal
    /// other runs' exit statuses).
    fn reap(pid: i32) {
        if let Some(p) = Pid::from_raw(pid) {
            let _ = waitpid(Some(p), WaitOptions::NOHANG);
        }
    }

    pub fn group_empty(pgid: i32) -> bool {
        let Some(p) = Pid::from_raw(pgid) else {
            return true;
        };
        // Killed members linger as zombies until their parent (us, as
        // subreaper, once reparented) reaps them: reap ours, then probe.
        for _ in 0..400 {
            for (pid, pgrp, _, _) in ours(pgid) {
                if pgrp == pgid {
                    reap(pid);
                }
            }
            if test_kill_process_group(p) == Err(rustix::io::Errno::SRCH) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        false
    }

    /// `(ppid, pgrp, session, starttime)` from `/proc/<pid>/stat`.
    fn ids(pid: &str) -> Option<(i32, i32, i32, u64)> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // pid (comm) state ppid pgrp session ...; comm may contain ')'.
        let fields: Vec<&str> = stat[stat.rfind(')')? + 2..].split(' ').collect();
        Some((
            fields.get(1)?.parse().ok()?,
            fields.get(2)?.parse().ok()?,
            fields.get(3)?.parse().ok()?,
            fields.get(19)?.parse().ok()?,
        ))
    }

    /// Start time (clock ticks since boot) of a process.
    pub fn start_time(pid: i32) -> Option<u64> {
        ids(&pid.to_string()).map(|i| i.3)
    }

    /// Kill and reap descendants of this run that were reparented to us:
    /// members of its process group, or processes that left our session
    /// (`setsid` daemons) after the run started. Direct children of other
    /// threads stay in our session, so concurrent runs are untouched.
    pub fn reap_orphans(pgid: Option<i32>, started: Option<u64>) -> Vec<i32> {
        let Some(pgid) = pgid else { return Vec::new() };
        let session = rustix::process::getsid(None).map_or(0, |s| s.as_raw_nonzero().get());
        let mut killed = Vec::new();
        for _ in 0..5 {
            let orphans: Vec<i32> = ours(pgid)
                .into_iter()
                .filter(|(_, pgrp, sid, start)| {
                    *pgrp == pgid || (*sid != session && started.is_some_and(|s| *start >= s))
                })
                .map(|o| o.0)
                .collect();
            if orphans.is_empty() {
                break;
            }
            for pid in orphans {
                if let Some(p) = Pid::from_raw(pid) {
                    let _ = kill_process(p, Signal::KILL);
                    let _ = waitpid(Some(p), WaitOptions::empty());
                    killed.push(pid);
                }
            }
        }
        killed.sort_unstable();
        killed.dedup();
        killed
    }

    pub fn exit_parts(s: std::process::ExitStatus) -> (Option<i32>, Option<i32>) {
        use std::os::unix::process::ExitStatusExt;
        (s.code(), s.signal())
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    pub fn enable_subreaper() -> bool {
        false
    }
    pub fn quiet_cores() -> bool {
        false
    }
    pub fn kill_group(_pgid: i32) -> bool {
        false
    }
    pub fn group_empty(_pgid: i32) -> bool {
        false
    }
    pub fn reap_orphans(_pgid: Option<i32>, _started: Option<u64>) -> Vec<i32> {
        Vec::new()
    }
    pub fn start_time(_pid: i32) -> Option<u64> {
        None
    }
    pub fn exit_parts(s: std::process::ExitStatus) -> (Option<i32>, Option<i32>) {
        (s.code(), None)
    }
}
