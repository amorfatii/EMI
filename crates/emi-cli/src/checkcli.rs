//! `emi check`: run declarative rules (built-in catalog plus your own)
//! with suppressions and baselines.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;

use clap::Args;
use emi_rules::{Check, Finding, Rule};
use serde_json::{Value, json};

#[derive(Args)]
pub struct CheckArgs {
    /// Directory to check (every .py/.js/.jsx/.mjs/.cjs/.ts/.tsx/.mts/.cts file).
    #[arg(default_value = ".")]
    root: PathBuf,
    /// Rule file or directory of *.yml rule files (repeatable).
    #[arg(long = "rules", value_name = "PATH")]
    rules: Vec<PathBuf>,
    /// Do not load the built-in catalog.
    #[arg(long)]
    no_builtin: bool,
    /// Run only this rule (repeatable).
    #[arg(long = "only", value_name = "RULE")]
    only: Vec<String>,
    /// fnmatch glob on root-relative paths to skip (repeatable).
    #[arg(long = "exclude")]
    exclude: Vec<String>,
    /// Findings whose fingerprint is in this baseline file are not new.
    #[arg(long, value_name = "FILE")]
    baseline: Option<PathBuf>,
    /// Write the current findings as a baseline file.
    #[arg(long, value_name = "FILE")]
    write_baseline: Option<PathBuf>,
    /// Exit 2 when a new finding has at least this severity.
    #[arg(long, default_value = "error", value_parser = ["note", "warning", "error", "never"])]
    fail_on: String,
    #[arg(long, default_value = "text", value_parser = ["text", "json", "sarif"])]
    format: String,
    /// Date for suppression expiry (YYYY-MM-DD; default: today, UTC).
    #[arg(long)]
    today: Option<String>,
    /// List the loaded rules and exit.
    #[arg(long)]
    list_rules: bool,
    /// Run every loaded rule's examples and exit (2 on failure).
    #[arg(long)]
    test_rules: bool,
    /// Write the output here instead of stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

fn rank(severity: &str) -> u8 {
    match severity {
        "note" => 1,
        "warning" => 2,
        "error" => 3,
        _ => 4,
    }
}

fn text(findings: &[Finding]) -> String {
    let mut out = String::new();
    for f in findings.iter().filter(|f| f.state == "new") {
        let h = &f.hit;
        out.push_str(&format!(
            "{}:{}:{}: {} [{}] {}\n",
            h.file, h.line, h.column, h.severity, h.rule, h.message
        ));
    }
    let count = |s: &str| findings.iter().filter(|f| f.state == s).count();
    out.push_str(&format!(
        "{} new, {} baseline, {} suppressed\n",
        count("new"),
        count("baseline"),
        count("suppressed")
    ));
    out
}

fn sarif(findings: &[Finding], rules: &[Rule], root: &std::path::Path) -> Value {
    let used: BTreeSet<&str> = findings.iter().map(|f| f.hit.rule.as_str()).collect();
    let mut descriptors: Vec<Value> = rules
        .iter()
        .filter(|r| used.contains(r.id.as_str()))
        .map(|r| {
            json!({"id": r.id, "shortDescription": {"text": r.message.split("{{").next().unwrap_or("").trim()},
                   "fullDescription": {"text": r.description.trim()},
                   "defaultConfiguration": {"level": r.severity.as_str()},
                   "properties": {"tags": r.tags}})
        })
        .collect();
    if used.contains(emi_rules::SUPPRESSION_RULE) {
        descriptors.push(json!({"id": emi_rules::SUPPRESSION_RULE,
                                "shortDescription": {"text": "Invalid or expired emi-ignore suppression"},
                                "defaultConfiguration": {"level": "warning"}}));
    }
    let base = root.to_string_lossy().trim_end_matches('/').to_owned();
    let results: Vec<Value> = findings
        .iter()
        .map(|f| {
            let h = &f.hit;
            let uri = if base.is_empty() || base == "." { h.file.clone() } else { format!("{base}/{}", h.file) };
            let mut r = json!({"ruleId": h.rule, "level": h.severity, "message": {"text": h.message},
                "locations": [{"physicalLocation": {"artifactLocation": {"uri": uri},
                    "region": {"startLine": h.line, "startColumn": h.column, "endLine": h.end_line}}}],
                "partialFingerprints": {"emiRuleHash/v1": f.fingerprint},
                "baselineState": if f.state == "baseline" { "unchanged" } else { "new" }});
            if let Some(reason) = &f.suppression {
                r["suppressions"] = json!([{"kind": "inSource", "justification": reason}]);
            }
            r
        })
        .collect();
    json!({"$schema": "https://json.schemastore.org/sarif-2.1.0.json", "version": "2.1.0",
           "runs": [{"tool": {"driver": {"name": "emi", "version": env!("CARGO_PKG_VERSION"), "rules": descriptors}},
                     "results": results}]})
}

pub fn run(args: &CheckArgs) -> anyhow::Result<u8> {
    let rules = emi_rules::load(&args.rules, !args.no_builtin).map_err(anyhow::Error::msg)?;
    let mut out = std::io::stdout().lock();
    if args.list_rules {
        for r in &rules {
            let langs: Vec<String> = r
                .languages
                .iter()
                .map(|l| format!("{l:?}").to_lowercase())
                .collect();
            writeln!(
                out,
                "{:<24} {:<8} {:<18} {}",
                r.id,
                r.severity.as_str(),
                langs.join(","),
                r.description.trim()
            )?;
        }
        return Ok(0);
    }
    if args.test_rules {
        let failures = emi_rules::self_test(&rules);
        for f in &failures {
            writeln!(out, "FAIL {f}")?;
        }
        writeln!(out, "{} rules, {} failures", rules.len(), failures.len())?;
        return Ok(if failures.is_empty() { 0 } else { 2 });
    }
    let baseline = args
        .baseline
        .as_deref()
        .map(emi_rules::read_baseline)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    let today = args
        .today
        .clone()
        .unwrap_or_else(|| emi_diff::trajectory::utc_now_iso()[..10].to_owned());
    let check = Check {
        rules: &rules,
        only: &args.only,
        exclude_globs: &args.exclude,
        today,
        baseline: baseline.as_ref(),
    };
    let findings = emi_rules::check_tree(&args.root, &check).map_err(anyhow::Error::msg)?;
    if let Some(path) = &args.write_baseline {
        std::fs::write(
            path,
            emi_ledger::pyfmt::dumps_pretty(&emi_rules::baseline(&findings)) + "\n",
        )?;
    }
    let rendered = match args.format.as_str() {
        "json" => serde_json::to_string_pretty(&findings)?,
        "sarif" => serde_json::to_string_pretty(&sarif(&findings, &rules, &args.root))?,
        _ => text(&findings),
    };
    match &args.output {
        Some(path) => std::fs::write(path, rendered)?,
        None => write!(out, "{rendered}")?,
    }
    let threshold = rank(&args.fail_on);
    let failing = findings
        .iter()
        .any(|f| f.state == "new" && rank(f.hit.severity) >= threshold);
    Ok(if failing { 2 } else { 0 })
}
