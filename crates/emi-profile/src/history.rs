//! Measured burden joined with change history over a window: the part
//! `emi hotspots`, the MCP hotspots tool and `emi profile` share.

use emi_diff::Request;
use emi_history::hotspots::Hotspot;
use emi_history::{Commit, Coupling, DEFAULT_AI_PATTERN, Query};
use serde_json::Value;

use crate::Result;

/// Which history to read and what counts as change coupling.
#[derive(Clone, Debug)]
pub struct Window {
    /// Days before the head commit's time (`None`: all history).
    pub days: Option<u32>,
    /// Regex marking AI-attributed commits.
    pub ai_pattern: String,
    /// Change coupling: minimum shared commits.
    pub min_shared: u32,
    /// Change coupling: minimum degree (percent of mean revisions).
    pub min_degree: f64,
    /// Commits touching more scope files than this are ignored.
    pub max_changeset: usize,
}

impl Default for Window {
    fn default() -> Self {
        Window {
            days: Some(emi_history::DEFAULT_DAYS),
            ai_pattern: DEFAULT_AI_PATTERN.into(),
            min_shared: emi_history::DEFAULT_MIN_SHARED,
            min_degree: emi_history::DEFAULT_MIN_DEGREE,
            max_changeset: emi_history::DEFAULT_MAX_CHANGESET,
        }
    }
}

/// The head's measurement and the history read back from it.
pub struct History {
    pub sha: String,
    pub measurement: Value,
    pub commits: Vec<Commit>,
    pub hotspots: Vec<Hotspot>,
    /// Pairs of files present at the head that change together.
    pub coupling: Vec<Coupling>,
}

/// Measure `head` under `req` and rank its files by burden × revisions.
pub fn history(req: &Request, head: &str, w: &Window) -> Result<History> {
    let (sha, measurement) = emi_diff::change::measure_ref(req, head)?;
    let measurement = measurement.ok_or("no measurable source files at the head")?;
    let query = Query {
        repo: req.repo.clone(),
        source: emi_diff::snapshot::normalize_source(&req.source)?,
        head: sha.clone(),
        days: w.days,
        ai_pattern: w.ai_pattern.clone(),
    };
    let commits = emi_history::mine(&query)?;
    let histories = emi_history::file_histories(&commits);
    let now = commits.first().map_or(0, |c| c.time);
    let hotspots = emi_history::hotspots::rank(&measurement, &histories, now);
    // history of deleted files is context, not a target
    let present: std::collections::HashSet<&str> = measurement["source_sha256"]
        .as_object()
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let coupling: Vec<Coupling> =
        emi_history::change_coupling(&commits, w.min_shared, w.min_degree, w.max_changeset)
            .into_iter()
            .filter(|c| present.contains(c.a.as_str()) && present.contains(c.b.as_str()))
            .collect();
    Ok(History {
        sha,
        measurement,
        commits,
        hotspots,
        coupling,
    })
}
