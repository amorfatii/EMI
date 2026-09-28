//! `emi hotspots`: burden × change frequency, change coupling, authorship
//! and AI attribution over a history window.

use std::io::Write;
use std::path::PathBuf;

use clap::Args;
use emi_diff::Request;
use emi_history::{DEFAULT_AI_PATTERN, Query};
use serde_json::{Value, json};

use crate::{Language, ProfileArg};

#[derive(Args)]
pub struct HotspotsArgs {
    /// Git repository.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Product scope subdirectory.
    #[arg(long, default_value = "src")]
    source: String,
    #[arg(long, value_enum, default_value_t = Language::Python)]
    probe: Language,
    #[arg(long, value_enum, default_value_t = ProfileArg::Graded)]
    profile: ProfileArg,
    /// Revision to measure and read history back from.
    #[arg(long, default_value = "HEAD")]
    head: String,
    /// History window in days before HEAD's commit time (0: all history).
    #[arg(long, default_value_t = 365)]
    days: u32,
    /// Regex (case-insensitive by default) marking AI-attributed commits,
    /// matched against author and Co-authored-by trailers.
    #[arg(long, default_value = DEFAULT_AI_PATTERN)]
    ai_pattern: String,
    /// Rows in the table.
    #[arg(long, default_value_t = 20)]
    top: usize,
    /// Change coupling: minimum shared commits.
    #[arg(long, default_value_t = 5)]
    min_shared: u32,
    /// Change coupling: minimum degree (percent of mean revisions).
    #[arg(long, default_value_t = 30.0)]
    min_degree: f64,
    /// Ignore commits touching more scope files than this.
    #[arg(long, default_value_t = 30)]
    max_changeset: usize,
    /// Record unparseable files under errors instead of aborting.
    #[arg(long)]
    tolerate_syntax_errors: bool,
    /// Human table or machine JSON.
    #[arg(long, default_value = "table", value_parser = ["table", "json"])]
    format: String,
}

fn report(args: &HotspotsArgs) -> Result<Value, String> {
    let probe: emi_scan::Probe = args.probe.into();
    let mut req = Request::new(&args.repo, &args.source, &args.head, &args.head, probe);
    req.profile = args.profile.into();
    req.tolerate_syntax_errors = args.tolerate_syntax_errors;
    let (sha, measurement) = emi_diff::change::measure_ref(&req, &args.head)?;
    let measurement = measurement.ok_or("no measurable source files at HEAD")?;
    let query = Query {
        repo: args.repo.clone(),
        source: emi_diff::snapshot::normalize_source(&args.source)?,
        head: sha.clone(),
        days: (args.days > 0).then_some(args.days),
        ai_pattern: args.ai_pattern.clone(),
    };
    let commits = emi_history::mine(&query)?;
    let histories = emi_history::file_histories(&commits);
    let now = commits.first().map_or(0, |c| c.time);
    let hotspots = emi_history::hotspots::rank(&measurement, &histories, now);
    // Only files present at HEAD: history of deleted files is context, not a target.
    let present: std::collections::HashSet<&str> = measurement["source_sha256"]
        .as_object()
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let coupling: Vec<_> = emi_history::change_coupling(
        &commits,
        args.min_shared,
        args.min_degree,
        args.max_changeset,
    )
    .into_iter()
    .filter(|c| present.contains(c.a.as_str()) && present.contains(c.b.as_str()))
    .collect();
    let count = |o: emi_history::Origin| commits.iter().filter(|c| c.origin == o).count();
    Ok(json!({
        "head": sha,
        "window_days": if args.days > 0 { json!(args.days) } else { Value::Null },
        "measurement_profile": measurement["measurement_profile"],
        "commits": commits.len(),
        "origins": {"human": count(emi_history::Origin::Human), "ai": count(emi_history::Origin::Ai),
                    "bot": count(emi_history::Origin::Bot)},
        "hotspots": hotspots,
        "change_coupling": coupling,
    }))
}

fn table(r: &Value, top: usize) -> String {
    let mut lines = vec![format!(
        "{} commits in window ({} human, {} AI-attributed, {} bot) at {}",
        r["commits"],
        r["origins"]["human"],
        r["origins"]["ai"],
        r["origins"]["bot"],
        r["head"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(12)
            .collect::<String>()
    )];
    lines.push(format!(
        "{:<48} {:>5} {:>6} {:>17} {:>7} {:>5}",
        "hotspot", "revs", "sloc", "burden", "authors", "ai%"
    ));
    for h in r["hotspots"].as_array().into_iter().flatten().take(top) {
        let b = |i: usize| h["burden"][i].as_f64().unwrap_or(0.0);
        lines.push(format!(
            "{:<48} {:>5} {:>6} {:>8.1}–{:<8.1} {:>7} {:>5.0}",
            h["file"].as_str().unwrap_or(""),
            h["revisions"].as_u64().unwrap_or(0),
            h["sloc"].as_u64().unwrap_or(0),
            b(0),
            b(1),
            h["authors"].as_u64().unwrap_or(0),
            100.0 * h["ai_share"].as_f64().unwrap_or(0.0)
        ));
    }
    let pairs: Vec<&Value> = r["change_coupling"]
        .as_array()
        .into_iter()
        .flatten()
        .take(top)
        .collect();
    if !pairs.is_empty() {
        lines.push(String::new());
        lines.push("change coupling (shared commits, degree %)".into());
        for c in pairs {
            lines.push(format!(
                "  {} <-> {}  {} {:.0}%",
                c["a"].as_str().unwrap_or(""),
                c["b"].as_str().unwrap_or(""),
                c["shared"],
                c["degree"].as_f64().unwrap_or(0.0)
            ));
        }
    }
    lines.join("\n") + "\n"
}

pub fn run(args: &HotspotsArgs) -> anyhow::Result<u8> {
    let r = report(args).map_err(anyhow::Error::msg)?;
    let text = if args.format == "json" {
        emi_ledger::pyfmt::dumps_pretty(&r) + "\n"
    } else {
        table(&r, args.top)
    };
    write!(std::io::stdout().lock(), "{text}")?;
    Ok(0)
}
