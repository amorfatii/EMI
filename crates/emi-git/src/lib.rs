//! The one way EMI runs git.
//!
//! EMI reads repositories whose working tree, `.git/config`,
//! `.git/info/attributes` and refs an agent under supervision can write.
//! Git lets those run programs (filters, textconv drivers, fsmonitor,
//! hooks) and change what a read returns (a `-diff` attribute makes a test
//! file "binary", a replace ref swaps an object). Every git call EMI makes
//! therefore goes through [`command`], which switches off what can be
//! switched off per call, and reads that compare revisions go through a
//! [`CleanView`]: a scratch bare repository that borrows the objects and
//! nothing else, so no configuration, attribute, hook or replace ref of
//! the agent's repository applies.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub type Result<T> = std::result::Result<T, String>;

/// git's well-known empty tree: attributes are read from it, i.e. none.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Per-call configuration that no repository or user file may change.
const OVERRIDES: &[&str] = &[
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.untrackedCache=false",
    "-c",
    "core.attributesFile=/dev/null",
];

/// A `git -C repo` command with EMI's hardening applied: no system
/// configuration, no replace objects, attributes from the empty tree, no
/// fsmonitor or hooks. Callers add `--no-textconv --no-ext-diff` to
/// commands that produce diffs.
pub fn command(repo: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        .args(OVERRIDES)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_ATTR_SOURCE", EMPTY_TREE)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .stdin(Stdio::null());
    cmd
}

fn failure(args: &[&str], stderr: &[u8]) -> String {
    let detail = String::from_utf8_lossy(&stderr[..stderr.len().min(300)])
        .trim()
        .to_owned();
    format!(
        "git {} failed: {detail}",
        args.iter().take(3).copied().collect::<Vec<_>>().join(" ")
    )
}

/// Run `git args` in `repo`; stdout, or the failure.
pub fn run(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    run_with(repo, args, &[], None)
}

/// [`run`] with extra environment variables and optional stdin bytes.
pub fn run_with(
    repo: &Path,
    args: &[&str],
    env: &[(&str, &Path)],
    input: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let mut cmd = command(repo);
    cmd.args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if input.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "EMI requires a git binary on PATH".to_owned(),
        _ => format!("git {} failed: {e}", args.first().copied().unwrap_or("")),
    })?;
    if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
        let bytes = bytes.to_vec();
        std::thread::spawn(move || stdin.write_all(&bytes));
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("git {} failed: {e}", args.first().copied().unwrap_or("")))?;
    if out.status.success() {
        Ok(out.stdout)
    } else if out.stderr.is_empty() {
        Err(failure(args, &out.stdout))
    } else {
        Err(failure(args, &out.stderr))
    }
}

/// A scratch bare repository whose objects are the source repository's
/// (through `objects/info/alternates`) and whose configuration, attributes,
/// hooks and refs are empty. Objects written into it (a worktree snapshot)
/// stay out of the source repository. Removed when dropped.
pub struct CleanView {
    dir: PathBuf,
}

impl CleanView {
    /// A clean view of `repo`'s objects, created under the system temp
    /// directory.
    pub fn of(repo: &Path) -> Result<CleanView> {
        let common = run(
            repo,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        let common = PathBuf::from(String::from_utf8_lossy(&common).trim());
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let view = CleanView {
            dir: std::env::temp_dir().join(format!("emi-git-view-{}-{nanos}", std::process::id())),
        };
        let dir = view.dir.to_string_lossy().into_owned();
        run(
            Path::new("."),
            &["init", "--quiet", "--bare", "--template=", &dir],
        )?;
        let objects = common.join("objects");
        let alternates = view.dir.join("objects").join("info");
        std::fs::create_dir_all(&alternates).map_err(|e| format!("clean view: {e}"))?;
        std::fs::write(
            alternates.join("alternates"),
            format!("{}\n", objects.display()),
        )
        .map_err(|e| format!("clean view: {e}"))?;
        // a shallow clone's boundary, so history reads stop where it does
        if let Ok(shallow) = std::fs::read(common.join("shallow")) {
            std::fs::write(view.dir.join("shallow"), shallow)
                .map_err(|e| format!("clean view: {e}"))?;
        }
        Ok(view)
    }

    /// The view's git directory, for `git -C`.
    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for CleanView {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        out.trim().to_owned()
    }

    #[test]
    fn the_view_ignores_the_repositorys_attributes_and_drivers() {
        let repo = Dir(std::env::temp_dir().join(format!("emi-git-test-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&repo.0);
        std::fs::create_dir_all(repo.0.join("tests")).unwrap_or_default();
        sh(&repo.0, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.0.join("tests/t.py"), "def test_a():\n    pass\n").unwrap_or_default();
        sh(&repo.0, &["add", "-A"]);
        sh(&repo.0, &["commit", "-q", "-m", "a"]);
        std::fs::write(
            repo.0.join("tests/t.py"),
            "@skip\ndef test_a():\n    pass\n",
        )
        .unwrap_or_default();
        sh(&repo.0, &["commit", "-q", "-am", "b"]);
        // the repository marks tests binary and routes them through a driver
        std::fs::write(
            repo.0.join(".git/info/attributes"),
            "tests/* -diff\n*.py diff=hide\n",
        )
        .unwrap_or_default();
        sh(&repo.0, &["config", "diff.hide.textconv", "false"]);
        let view = CleanView::of(&repo.0).unwrap_or_else(|e| panic!("{e}"));
        let diff = run(
            view.path(),
            &[
                "diff",
                "--no-textconv",
                "--no-ext-diff",
                "-U0",
                &sh(&repo.0, &["rev-parse", "HEAD~1"]),
                &sh(&repo.0, &["rev-parse", "HEAD"]),
            ],
        )
        .unwrap_or_default();
        let diff = String::from_utf8_lossy(&diff);
        assert!(diff.contains("+@skip"), "{diff}");
        assert!(!diff.contains("Binary"), "{diff}");
    }
}
