//! Commands run against a fresh copy of the profiled commit, isolated like
//! a supervised task's checks: bubblewrap when available, network off
//! unless asked for, the repository hidden (so imports come from the
//! copy, never the live checkout), linked dependency directories mounted
//! back read-only, and the whole process group cleaned up. Coverage runs
//! and mutant runs both go through here.

use std::path::{Path, PathBuf};
use std::time::Duration;

use emi_exec::{Isolation, Outcome, Spec};
use emi_tree::snapshot;

use crate::Result;

/// Where and how commands run.
#[derive(Clone, Copy)]
pub struct Runner<'a> {
    pub repo: &'a Path,
    /// The commit every copy is made from.
    pub rev: &'a str,
    /// Ignored dependency directories linked into each copy.
    pub link: &'a [String],
    pub env: &'a [(String, String)],
    /// Upper bound for one command.
    pub timeout: Duration,
    pub network: bool,
}

/// A fresh copy of the commit, removed when dropped.
pub struct Copy {
    pub dir: PathBuf,
    /// The regular files the commit holds, sorted.
    pub files: Vec<String>,
    /// The symbolic links the commit holds, sorted. Links to dependency
    /// directories made by the runner are not among them.
    pub symlinks: Vec<String>,
    linked: Vec<PathBuf>,
}

impl Drop for Copy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Runner<'_> {
    /// Materialize the commit into a new scratch directory (`tag` keeps
    /// concurrent copies apart).
    pub fn copy(&self, tag: &str) -> Result<Copy> {
        self.copy_of(tag, self.repo, self.rev)
    }

    /// [`Runner::copy`] of `rev` read from `objects`: a repository that
    /// holds the commit but is not the one being profiled (a clean view
    /// that also holds a snapshot of the working tree). Dependency links
    /// and hidden paths still come from the runner's own repository.
    pub fn copy_of(&self, tag: &str, objects: &Path, rev: &str) -> Result<Copy> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir =
            std::env::temp_dir().join(format!("emi-profile-{tag}-{}-{nanos}", std::process::id()));
        let mut copy = Copy {
            dir,
            files: Vec::new(),
            symlinks: Vec::new(),
            linked: Vec::new(),
        };
        let inventory = snapshot::materialize(objects, rev, &copy.dir)?;
        copy.files = inventory.files;
        copy.symlinks = inventory.symlinks;
        copy.linked = snapshot::link_dependencies(self.repo, &copy.dir, self.link)?;
        crate::pins::install(&copy.dir)?;
        Ok(copy)
    }

    /// Run `argv` in `copy`, bounded by `timeout`.
    pub fn run(&self, copy: &Copy, argv: &[String], timeout: Duration) -> Outcome {
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let mut spec = Spec::new(&argv, &copy.dir);
        // a listing or a report with one line per test outgrows the
        // default limit on a large suite
        spec.output_limit = 32 << 20;
        spec.timeout = timeout;
        spec.isolation = Isolation::Best {
            network: self.network,
        };
        spec.hidden = vec![self.repo.to_path_buf()];
        spec.read_only = copy.linked.clone();
        spec.env.extend(self.env.iter().cloned());
        with_plugin_path(&mut spec.env);
        emi_exec::run(&spec)
    }
}

/// Make the pins plugin importable in every run of a copy: its directory
/// goes last on `PYTHONPATH`, after whatever the caller put there.
fn with_plugin_path(env: &mut Vec<(String, String)>) {
    let dir = emi_kernel::pins::PLUGIN_DIR;
    let last = env.iter().rposition(|(k, _)| k == "PYTHONPATH");
    match last {
        Some(i) if env[i].1.split(':').any(|p| p == dir) => {}
        Some(i) => env[i].1 = format!("{}:{dir}", env[i].1),
        None => env.push(("PYTHONPATH".to_owned(), dir.to_owned())),
    }
}

/// The last `n` characters of `s` (for error context in records).
pub fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    chars[chars.len().saturating_sub(n)..].iter().collect()
}
