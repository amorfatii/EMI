//! Commands run against a fresh copy of the profiled commit, isolated like
//! a supervised task's checks: bubblewrap when available, network off
//! unless asked for, the repository hidden (so imports come from the
//! copy, never the live checkout), linked dependency directories mounted
//! back read-only, and the whole process group cleaned up. Coverage runs
//! and mutant runs both go through here.

use std::path::{Path, PathBuf};
use std::time::Duration;

use emi_diff::snapshot;
use emi_exec::{Isolation, Outcome, Spec};

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
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir =
            std::env::temp_dir().join(format!("emi-profile-{tag}-{}-{nanos}", std::process::id()));
        let mut copy = Copy {
            dir,
            linked: Vec::new(),
        };
        snapshot::materialize(self.repo, self.rev, &copy.dir)?;
        copy.linked = snapshot::link_dependencies(self.repo, &copy.dir, self.link)?;
        Ok(copy)
    }

    /// Run `argv` in `copy`, bounded by `timeout`.
    pub fn run(&self, copy: &Copy, argv: &[String], timeout: Duration) -> Outcome {
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let mut spec = Spec::new(&argv, &copy.dir);
        spec.timeout = timeout;
        spec.isolation = Isolation::Best {
            network: self.network,
        };
        spec.hidden = vec![self.repo.to_path_buf()];
        spec.read_only = copy.linked.clone();
        spec.env.extend(self.env.iter().cloned());
        emi_exec::run(&spec)
    }
}

/// The last `n` characters of `s` (for error context in records).
pub fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    chars[chars.len().saturating_sub(n)..].iter().collect()
}
