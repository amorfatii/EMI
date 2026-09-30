//! Places a change may have missed: files that historically change together
//! with the touched files but were not touched this time, and modules that
//! import a touched module. A list to look at, never a score or a gate.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use emi_panel::Lang;
use emi_scan::{OnSyntaxError, Options, Probe};
use serde_json::{Value, json};

use crate::hunks::{self, FileChange, Status};
use crate::snapshot::{self, TempDir, resolve_ref};

/// History window, change-coupling thresholds and list length. The history
/// defaults are the ones `emi hotspots` uses.
#[derive(Clone, Debug)]
pub struct ImpactOptions {
    /// Scope directory whose import graph is read (`""`: the repository).
    pub source: String,
    /// Days of history before the base commit's time (`None`: all).
    pub days: Option<u32>,
    pub min_shared: u32,
    /// Percent of the pair's mean revisions.
    pub min_degree: f64,
    /// Commits touching more files than this are mass edits, not coupling.
    pub max_changeset: usize,
    /// Rows listed per section; totals always count every row.
    pub limit: usize,
}

impl Default for ImpactOptions {
    fn default() -> Self {
        ImpactOptions {
            source: String::new(),
            days: Some(emi_history::DEFAULT_DAYS),
            min_shared: emi_history::DEFAULT_MIN_SHARED,
            min_degree: emi_history::DEFAULT_MIN_DEGREE,
            max_changeset: emi_history::DEFAULT_MAX_CHANGESET,
            limit: 20,
        }
    }
}

/// A file that changed together with a touched file but was not touched.
#[derive(Clone, Debug, PartialEq)]
pub struct Partner {
    pub touched: String,
    pub partner: String,
    pub shared_commits: u32,
    pub degree: f64,
}

/// Change coupling up to the base, or why it cannot be read.
#[derive(Clone, Debug, PartialEq)]
pub enum CoChange {
    Verified {
        /// Commits read (up to and including the base, within the window).
        commits: usize,
        /// Sorted by degree (highest first), then partner, then touched file;
        /// at most `limit` rows.
        missing: Vec<Partner>,
        total: usize,
        /// Touched files with fewer revisions than `min_shared`: no partner
        /// could reach the threshold, which is not the same as having none.
        too_few_revisions: Vec<String>,
    },
    Unverified {
        reason: String,
    },
}

/// A module that imports a touched module but was not touched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependant {
    pub file: String,
    pub imports: String,
}

/// Importers of the touched modules at the base.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependants {
    /// Sorted by file, then imported file; at most `limit` rows.
    pub items: Vec<Dependant>,
    pub total: usize,
    /// Languages whose import graph could not be built, with the reason.
    pub unresolved: Vec<(String, String)>,
    /// Files the scanner could not read: their imports are unknown.
    pub unparsed: Vec<String>,
    /// Languages whose import graph was read.
    pub resolved: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Impact {
    pub co_change: CoChange,
    pub dependants: Dependants,
}

/// The paths a change touches, on both sides of renames.
fn touched(files: &[FileChange]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for f in files {
        out.insert(f.path.clone());
        out.extend(f.old_path.clone());
    }
    out
}

/// Touched paths as they exist at the base (not the files the change adds).
fn touched_at_base(files: &[FileChange]) -> BTreeSet<String> {
    files
        .iter()
        .filter(|f| f.status != Status::Added)
        .map(|f| f.old_path.clone().unwrap_or_else(|| f.path.clone()))
        .collect()
}

/// Missing co-changes and dependants of `base..head`. History and imports
/// are read at the base, so the change is never its own evidence.
pub fn co_change_missing(
    repo: &Path,
    base: &str,
    head: &str,
    opts: &ImpactOptions,
) -> Result<Impact, String> {
    let (base, head) = (resolve_ref(repo, base)?, resolve_ref(repo, head)?);
    let view = emi_git::CleanView::of(repo)?;
    let files = hunks::changes(view.path(), &base, Some(&head))?;
    let touched = touched(&files);
    let co_change = co_change(repo, &base, &touched, &touched_at_base(&files), opts)?;
    let dependants = dependants(repo, &base, &touched, opts)?;
    Ok(Impact {
        co_change,
        dependants,
    })
}

fn is_shallow(repo: &Path) -> Result<bool, String> {
    let out = emi_git::run(repo, &["rev-parse", "--is-shallow-repository"])?;
    Ok(String::from_utf8_lossy(&out).trim() == "true")
}

/// Every path in the base tree.
fn base_paths(repo: &Path, base: &str) -> Result<BTreeSet<String>, String> {
    let out = emi_git::run(
        repo,
        &[
            "ls-tree",
            "-r",
            "-z",
            "--name-only",
            "--end-of-options",
            base,
        ],
    )?;
    Ok(String::from_utf8_lossy(&out)
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect())
}

fn co_change(
    repo: &Path,
    base: &str,
    touched: &BTreeSet<String>,
    at_base: &BTreeSet<String>,
    opts: &ImpactOptions,
) -> Result<CoChange, String> {
    if is_shallow(repo)? {
        return Ok(CoChange::Unverified {
            reason: "shallow clone: the history before the base is truncated".into(),
        });
    }
    let commits = emi_history::mine(&emi_history::Query {
        repo: repo.to_path_buf(),
        source: String::new(),
        head: base.to_owned(),
        days: opts.days,
        ai_pattern: emi_history::DEFAULT_AI_PATTERN.into(),
    })?;
    if commits.len() < opts.min_shared as usize {
        return Ok(CoChange::Unverified {
            reason: format!(
                "{} commit(s) up to the base in the window; change coupling needs at least {}",
                commits.len(),
                opts.min_shared
            ),
        });
    }
    let present = base_paths(repo, base)?;
    let pairs = emi_history::change_coupling(
        &commits,
        opts.min_shared,
        opts.min_degree,
        opts.max_changeset,
    );
    let mut missing: Vec<Partner> = Vec::new();
    for c in &pairs {
        for (t, p) in [(&c.a, &c.b), (&c.b, &c.a)] {
            if touched.contains(t) && !touched.contains(p) && present.contains(p) {
                missing.push(Partner {
                    touched: t.clone(),
                    partner: p.clone(),
                    shared_commits: c.shared,
                    degree: c.degree,
                });
            }
        }
    }
    missing.sort_by(|x, y| {
        y.degree
            .total_cmp(&x.degree)
            .then_with(|| x.partner.cmp(&y.partner))
            .then_with(|| x.touched.cmp(&y.touched))
    });
    let total = missing.len();
    missing.truncate(opts.limit);
    let histories = emi_history::file_histories(&commits);
    let too_few_revisions = at_base
        .iter()
        .filter(|f| histories.get(*f).map_or(0, |h| h.revisions) < opts.min_shared)
        .cloned()
        .collect();
    Ok(CoChange::Verified {
        commits: commits.len(),
        missing,
        total,
        too_few_revisions,
    })
}

fn probe_of(path: &str) -> Option<Probe> {
    Some(match Lang::of_path(path)? {
        Lang::Python => Probe::Python,
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => Probe::Js,
        Lang::Go => Probe::Go,
        Lang::Rust => Probe::Rust,
    })
}

/// Repository path of a scope-relative path.
fn in_repo(source: &str, rel: &str) -> String {
    if source.is_empty() {
        rel.to_owned()
    } else {
        format!("{source}/{rel}")
    }
}

/// The file-level import graph of one language at the scope, repository
/// paths throughout, plus the files that could not be read.
type Graph = (BTreeMap<String, BTreeSet<String>>, Vec<String>);

fn import_graph(dest: &Path, source: &str, probe: Probe) -> Result<Graph, String> {
    let mut options = Options::new(probe);
    options.on_syntax_error = OnSyntaxError::Record;
    if probe == Probe::Js {
        options.config_root = Some(dest.to_path_buf());
    }
    let scope = if source.is_empty() {
        dest.to_path_buf()
    } else {
        dest.join(source)
    };
    let m = emi_scan::scan(&scope, &options).map_err(|e| e.to_string())?;
    let edges = m
        .import_edges
        .into_iter()
        .map(|(file, deps)| {
            let deps = deps.iter().map(|d| in_repo(source, d)).collect();
            (in_repo(source, &file), deps)
        })
        .collect();
    let unparsed = m.errors.iter().map(|e| in_repo(source, &e.file)).collect();
    Ok((edges, unparsed))
}

fn dependants(
    repo: &Path,
    base: &str,
    touched: &BTreeSet<String>,
    opts: &ImpactOptions,
) -> Result<Dependants, String> {
    let source = snapshot::normalize_source(&opts.source)?;
    let within = |p: &&String| source.is_empty() || p.starts_with(&format!("{source}/"));
    let probes: BTreeMap<&str, Probe> = touched
        .iter()
        .filter(within)
        .filter_map(|p| probe_of(p))
        .map(|p| (p.name(), p))
        .collect();
    let mut out = Dependants {
        items: Vec::new(),
        total: 0,
        unresolved: Vec::new(),
        unparsed: Vec::new(),
        resolved: Vec::new(),
    };
    if probes.is_empty() {
        return Ok(out);
    }
    let tmp = TempDir::new("emi-places-")?;
    let dest = tmp.path().join("base");
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    if let Err(e) = snapshot::materialize_scoped(repo, base, &dest, Some(&source)) {
        out.unresolved = probes
            .keys()
            .map(|p| ((*p).to_owned(), e.clone()))
            .collect();
        return Ok(out);
    }
    for (name, probe) in probes {
        match import_graph(&dest, &source, probe) {
            Ok((edges, mut unparsed)) => {
                out.resolved.push(name.to_owned());
                out.unparsed.append(&mut unparsed);
                out.items.extend(importers(&edges, touched));
            }
            Err(e) => out.unresolved.push((name.to_owned(), e)),
        }
    }
    out.items
        .sort_by(|a, b| (&a.file, &a.imports).cmp(&(&b.file, &b.imports)));
    out.total = out.items.len();
    out.items.truncate(opts.limit);
    out.unparsed.sort();
    Ok(out)
}

/// Untouched files that import a touched file.
fn importers(
    edges: &BTreeMap<String, BTreeSet<String>>,
    touched: &BTreeSet<String>,
) -> Vec<Dependant> {
    edges
        .iter()
        .filter(|(file, _)| !touched.contains(*file))
        .flat_map(|(file, deps)| {
            deps.intersection(touched).map(|t| Dependant {
                file: file.clone(),
                imports: t.clone(),
            })
        })
        .collect()
}

impl CoChange {
    fn to_json(&self, opts: &ImpactOptions) -> Value {
        let thresholds = json!({
            "window_days": opts.days,
            "min_shared": opts.min_shared,
            "min_degree": opts.min_degree,
            "max_changeset": opts.max_changeset,
        });
        match self {
            CoChange::Verified {
                commits,
                missing,
                total,
                too_few_revisions,
            } => json!({
                "status": "verified",
                "commits": commits,
                "thresholds": thresholds,
                "missing": missing.iter().map(|p| json!({
                    "touched": p.touched,
                    "partner": p.partner,
                    "shared_commits": p.shared_commits,
                    "degree": p.degree,
                })).collect::<Vec<_>>(),
                "total": total,
                "too_few_revisions": too_few_revisions,
            }),
            CoChange::Unverified { reason } => json!({
                "status": "unverified",
                "reason": reason,
                "thresholds": thresholds,
            }),
        }
    }
}

impl Dependants {
    /// `verified` when every needed import graph was read in full,
    /// `unverified` when none was, `partial` otherwise.
    pub fn status(&self) -> &'static str {
        match (self.resolved.is_empty(), self.unresolved.is_empty()) {
            (true, false) => "unverified",
            (_, true) if self.unparsed.is_empty() => "verified",
            _ => "partial",
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "status": self.status(),
            "dependants": self.items.iter()
                .map(|d| json!({"file": d.file, "imports": d.imports}))
                .collect::<Vec<_>>(),
            "total": self.total,
            "resolved": self.resolved,
            "unresolved": self.unresolved.iter()
                .map(|(lang, reason)| json!({"probe": lang, "reason": reason}))
                .collect::<Vec<_>>(),
            "unparsed": self.unparsed,
        })
    }
}

impl Impact {
    /// Render with the canonical encoder (`emi_ledger::pyfmt::dumps_pretty`).
    pub fn to_json(&self, opts: &ImpactOptions) -> Value {
        json!({
            "co_change": self.co_change.to_json(opts),
            "dependants": self.dependants.to_json(),
            "limit": opts.limit,
        })
    }
}
