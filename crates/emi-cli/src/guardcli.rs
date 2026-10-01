//! `emi guard`: verifiable guardrails on a change (see `emi-guard`).
//!
//! Exit codes: 0 pass (or below `--fail-on`), 2 a blocking violation,
//! 3 review-only violations with `--fail-on review`.

use std::io::Write;
use std::path::PathBuf;

use clap::Args;
use emi_diff::snapshot::resolve_ref;
use emi_guard::{Policy, Report, Tier, guard};
use serde_json::{Value, json};

#[derive(Args)]
pub struct GuardArgs {
    /// Repository (any directory inside it).
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Baseline ref the change is judged against.
    #[arg(long)]
    base: String,
    /// Candidate ref; omit to judge the working tree (untracked files included).
    #[arg(long)]
    head: Option<String>,
    /// Guardrail policy JSON (scope globs, allowed classes, tiers, budgets).
    #[arg(long, value_name = "FILE")]
    policy: Option<PathBuf>,
    /// Lowest tier that makes the exit code non-zero.
    #[arg(long, default_value = "block", value_parser = ["block", "review", "never"])]
    fail_on: String,
    #[arg(long, default_value = "text", value_parser = ["text", "json", "sarif", "markdown"])]
    format: String,
    /// Write the output here instead of stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

fn level(tier: Tier) -> &'static str {
    match tier {
        Tier::Block => "error",
        Tier::Review => "warning",
        Tier::Off => "note",
    }
}

fn sarif(report: &Report) -> Value {
    let mut ids: Vec<&str> = report.violations.iter().map(|v| v.id).collect();
    ids.sort_unstable();
    ids.dedup();
    let rules: Vec<Value> = ids.iter().map(|id| json!({"id": id, "name": id})).collect();
    let results: Vec<Value> = report
        .violations
        .iter()
        .map(|v| {
            let mut region = json!({});
            if let Some(line) = v.line {
                region = json!({"startLine": line});
            }
            json!({"ruleId": v.id, "level": level(v.tier), "message": {"text": v.evidence},
                   "locations": [{"physicalLocation": {"artifactLocation": {"uri": v.path}, "region": region}}]})
        })
        .collect();
    json!({"$schema": "https://json.schemastore.org/sarif-2.1.0.json", "version": "2.1.0",
           "runs": [{"tool": {"driver": {"name": "emi-guard", "informationUri": "https://github.com/amorfatii/EMI",
                                          "rules": rules}}, "results": results}]})
}

fn location(v: &emi_guard::Violation) -> String {
    v.line
        .map_or_else(|| v.path.clone(), |l| format!("{}:{l}", v.path))
}

fn text(report: &Report) -> String {
    let mut out = format!("{} ({} files changed)\n", report.decision(), report.files);
    for v in &report.violations {
        out.push_str(&format!(
            "  [{}] {} {}  {}\n",
            v.tier.as_str(),
            v.id,
            location(v),
            v.evidence
        ));
    }
    out
}

fn markdown(report: &Report) -> String {
    let mut out = format!("### emi guard: **{}**\n\n", report.decision());
    if report.violations.is_empty() {
        out.push_str("No guardrail violations.\n");
        return out;
    }
    out.push_str("| Tier | Guardrail | Where | Evidence |\n|---|---|---|---|\n");
    for v in &report.violations {
        let evidence = v.evidence.replace('|', "\\|");
        out.push_str(&format!(
            "| {} | `{}` | `{}` | {evidence} |\n",
            v.tier.as_str(),
            v.id,
            location(v)
        ));
    }
    out
}

fn exit_code(report: &Report, fail_on: &str) -> u8 {
    match (report.decision(), fail_on) {
        ("block", "block" | "review") => 2,
        ("review", "review") => 3,
        _ => 0,
    }
}

pub fn run(args: &GuardArgs) -> anyhow::Result<u8> {
    let policy = match &args.policy {
        Some(path) => {
            let text = std::fs::read_to_string(path)?;
            let value: Value = serde_json::from_str(&text)?;
            Policy::from_json(&value).map_err(anyhow::Error::msg)?
        }
        None => Policy::default(),
    };
    let base = resolve_ref(&args.repo, &args.base).map_err(anyhow::Error::msg)?;
    let head = match &args.head {
        Some(h) => Some(resolve_ref(&args.repo, h).map_err(anyhow::Error::msg)?),
        None => None,
    };
    let report = guard(&args.repo, &base, head.as_deref(), &policy).map_err(anyhow::Error::msg)?;
    let rendered = match args.format.as_str() {
        "json" => serde_json::to_string_pretty(&report.to_json())?,
        "sarif" => serde_json::to_string_pretty(&sarif(&report))?,
        "markdown" => markdown(&report),
        _ => text(&report),
    };
    match &args.output {
        Some(path) => std::fs::write(path, format!("{}\n", rendered.trim_end()))?,
        None => writeln!(std::io::stdout().lock(), "{}", rendered.trim_end())?,
    }
    Ok(exit_code(&report, &args.fail_on))
}
