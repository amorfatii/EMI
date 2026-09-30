//! `emi places`: where else a change may need a look. Public names and
//! schema boundaries it changes, files that usually change with the touched
//! ones, and modules importing them. A list; gates nothing.

use std::io::Write;
use std::path::PathBuf;

use clap::Args;
use emi_diff::impact::{CoChange, Impact, ImpactOptions, co_change_missing};
use emi_diff::surface::{Surface, surface_changes};
use serde_json::json;

#[derive(Args)]
pub struct PlacesArgs {
    /// Git repository.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Base ref (commit, branch, tag).
    #[arg(long)]
    base: String,
    /// Changed ref (commit, branch, tag).
    #[arg(long)]
    head: String,
    /// Scope directory whose import graph is read for dependants.
    #[arg(long, default_value = ".")]
    source: String,
    /// History window in days before the base commit's time (0: all history).
    #[arg(long, default_value_t = emi_history::DEFAULT_DAYS)]
    days: u32,
    /// Change coupling: minimum shared commits.
    #[arg(long, default_value_t = emi_history::DEFAULT_MIN_SHARED)]
    min_shared: u32,
    /// Change coupling: minimum degree (percent of mean revisions).
    #[arg(long, default_value_t = emi_history::DEFAULT_MIN_DEGREE)]
    min_degree: f64,
    /// Ignore commits touching more files than this.
    #[arg(long, default_value_t = emi_history::DEFAULT_MAX_CHANGESET)]
    max_changeset: usize,
    /// Rows listed per section (totals count every row).
    #[arg(long, default_value_t = 20)]
    limit: usize,
    #[arg(long, default_value = "json", value_parser = ["json", "text"])]
    format: String,
}

fn surface_text(s: &Surface, out: &mut Vec<String>) {
    out.push("Public surface".into());
    for c in &s.changes {
        let sig = match (&c.old_signature, &c.new_signature) {
            (Some(o), Some(n)) if o != n => format!(" {o} => {n}"),
            (_, Some(s)) | (Some(s), None) => format!(" {s}"),
            (None, None) => String::new(),
        };
        out.push(format!("  {} {}:{}{sig}", c.kind.as_str(), c.file, c.name));
    }
    for b in &s.boundaries {
        out.push(format!("  boundary {} {}", b.label, b.file));
    }
    for (label, files) in [("unparsed", &s.unparsed), ("unsupported", &s.unsupported)] {
        out.extend(files.iter().map(|f| format!("  {label} {f}")));
    }
}

fn more(out: &mut Vec<String>, shown: usize, total: usize) {
    if total > shown {
        out.push(format!("  ... and {} more", total - shown));
    }
}

fn impact_text(i: &Impact, out: &mut Vec<String>) {
    match &i.co_change {
        CoChange::Verified {
            commits,
            missing,
            total,
            too_few_revisions,
        } => {
            out.push(format!(
                "Usually changed together, not touched ({commits} commits read)"
            ));
            for p in missing {
                out.push(format!(
                    "  {} with {}: {} shared commits, degree {:.0}%",
                    p.partner, p.touched, p.shared_commits, p.degree
                ));
            }
            more(out, missing.len(), *total);
            for f in too_few_revisions {
                out.push(format!("  too few revisions to judge {f}"));
            }
        }
        CoChange::Unverified { reason } => {
            out.push(format!("Usually changed together: unverified ({reason})"));
        }
    }
    let d = &i.dependants;
    out.push(format!("Importers not touched ({})", d.status()));
    for item in &d.items {
        out.push(format!("  {} imports {}", item.file, item.imports));
    }
    more(out, d.items.len(), d.total);
    for (probe, reason) in &d.unresolved {
        out.push(format!("  unresolved {probe}: {reason}"));
    }
    out.extend(d.unparsed.iter().map(|f| format!("  unparsed {f}")));
}

pub fn run(args: &PlacesArgs) -> anyhow::Result<u8> {
    let opts = ImpactOptions {
        source: args.source.clone(),
        days: (args.days > 0).then_some(args.days),
        min_shared: args.min_shared,
        min_degree: args.min_degree,
        max_changeset: args.max_changeset,
        limit: args.limit,
    };
    let surface =
        surface_changes(&args.repo, &args.base, &args.head).map_err(anyhow::Error::msg)?;
    let impact =
        co_change_missing(&args.repo, &args.base, &args.head, &opts).map_err(anyhow::Error::msg)?;
    let text = if args.format == "json" {
        let doc = json!({"surface": surface.to_json(), "impact": impact.to_json(&opts)});
        emi_ledger::pyfmt::dumps_pretty(&doc) + "\n"
    } else {
        let mut lines = Vec::new();
        surface_text(&surface, &mut lines);
        impact_text(&impact, &mut lines);
        lines.join("\n") + "\n"
    };
    write!(std::io::stdout().lock(), "{text}")?;
    Ok(0)
}
