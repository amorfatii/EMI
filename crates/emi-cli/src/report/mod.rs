//! `emi report`: one self-contained HTML file (nothing is loaded from
//! outside it). Headline tiles; a map of the codebase (folders, size,
//! change frequency); the ranked hotspots; optionally the test evidence of
//! a saved `emi profile`; what the burden is made of; the files that change
//! together; rule findings. Every chart has a table twin, and the data is
//! embedded as JSON.

mod page;
mod readiness;
mod sections;
mod treemap;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::Args;
use emi_diff::Request;
use emi_profile::history::Window;
use serde_json::{Value, json};

use crate::{Language, ProfileArg};
use page::{SCRIPT, STYLE, esc};

#[derive(Args)]
pub struct ReportArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long, default_value = "src")]
    source: String,
    #[arg(long, value_enum, default_value_t = Language::Python)]
    probe: Language,
    #[arg(long, value_enum, default_value_t = ProfileArg::Precise)]
    profile: ProfileArg,
    /// Revision to report on.
    #[arg(long, default_value = "HEAD")]
    head: String,
    /// History window in days (0: all).
    #[arg(long, default_value_t = 365)]
    days: u32,
    /// A saved `emi profile --top 0 --format json` of the same commit: adds
    /// the test evidence section.
    #[arg(long, value_name = "PROFILE")]
    profile_json: Option<PathBuf>,
    #[arg(short, long, default_value = "emi-report.html")]
    output: PathBuf,
}

/// The built-in rules' findings on the reported commit (not on whatever
/// the working tree holds).
fn rule_findings(repo: &Path, sha: &str, source: &str) -> Result<Vec<Value>, String> {
    let scratch = emi_diff::snapshot::TempDir::new("emi-report-")?;
    let tree = scratch.path().join("tree");
    emi_diff::snapshot::materialize(repo, sha, &tree)?;
    let rules = emi_rules::load(&[], true)?;
    let check = emi_rules::Check {
        rules: &rules,
        only: &[],
        exclude_globs: &[],
        today: emi_diff::trajectory::utc_now_iso()[..10].to_owned(),
        baseline: None,
    };
    let findings = emi_rules::check_tree(&tree.join(source), &check)?;
    Ok(findings
        .iter()
        .map(|f| {
            json!({"rule": f.hit.rule, "severity": f.hit.severity, "file": f.hit.file,
                   "line": f.hit.line, "message": f.hit.message, "state": f.state})
        })
        .collect())
}

fn readiness_of(path: Option<&PathBuf>, head: &str) -> Result<Value, String> {
    let Some(path) = path else {
        return Ok(Value::Null);
    };
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("--profile-json {}: {e}", path.display()))?;
    let profile = emi_ledger::strict::loads(&text)?;
    Ok(readiness::from_profile(&profile, head))
}

fn collect(a: &ReportArgs) -> Result<Value, String> {
    let mut req = Request::new(&a.repo, &a.source, &a.head, &a.head, a.probe.into());
    req.profile = a.profile.into();
    req.tolerate_syntax_errors = true;
    let window = Window {
        days: (a.days > 0).then_some(a.days),
        ..Window::default()
    };
    let h = emi_profile::history::history(&req, &a.head, &window)?;
    let source = emi_diff::snapshot::normalize_source(&a.source)?;
    let findings = rule_findings(&a.repo, &h.sha, &source)?;
    let m = &h.measurement;
    Ok(json!({
        "source": source, "head": h.sha, "generated": emi_diff::trajectory::utc_now_iso(),
        "commits": h.commits.len(), "window_days": window.days,
        "measurement": {"measurement_profile": m["measurement_profile"], "components": m["components"],
                        "static_dependency_cycles": m["static_dependency_cycles"]},
        "hotspots": h.hotspots, "change_coupling": h.coupling, "findings": findings,
        "readiness": readiness_of(a.profile_json.as_ref(), &h.sha)?,
    }))
}

/// `2026-09-29T18:58:57.154030+00:00` as `2026-09-29 18:58 UTC`; other
/// text as it is.
fn when(iso: &str) -> String {
    let utc = iso.ends_with("+00:00") || iso.ends_with('Z');
    match (iso.get(..10), iso.get(10..11), iso.get(11..16)) {
        (Some(date), Some("T"), Some(time)) if utc => format!("{date} {time} UTC"),
        _ => iso.to_owned(),
    }
}

/// The reported folder and the commit, abbreviated.
fn scope(data: &Value) -> (String, String) {
    let source = data["source"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(".");
    let head = data["head"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(12)
        .collect();
    (source.to_owned(), head)
}

fn header(data: &Value) -> String {
    let (source, head) = scope(data);
    format!(
        r#"<header><h1>EMI report</h1><p class="scope"><code>{}</code> at commit <code>{}</code> · <span class="nw">generated {}</span></p></header>"#,
        esc(&source),
        esc(&head),
        esc(&when(data["generated"].as_str().unwrap_or("")))
    )
}

const FOOTER: &str = r#"<footer><p><b>Burden</b> is EMI's estimate of maintenance cost in line-equivalents: erosion (size and branching), redundancy (repeated code) and coupling (dependencies between files). Where evidence is missing, EMI shows the range it allows instead of guessing.</p><p><b>Hotspot score</b> is changes in the window × burden. <b>Test evidence</b> comes from <code>emi profile --top 0 --format json -o profile.json</code>, passed as <code>--profile-json profile.json</code>.</p>"#;

fn render(data: &Value) -> String {
    let rows = sections::files(data);
    let found: Vec<Value> = data["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["state"] == "new")
        .cloned()
        .collect();
    let hot_rank: BTreeMap<String, usize> = data["hotspots"]
        .as_array()
        .into_iter()
        .flatten()
        .take(5)
        .enumerate()
        .filter_map(|(i, h)| h["file"].as_str().map(|f| (f.to_owned(), i + 1)))
        .collect();
    let r = &data["readiness"];
    let cycles = data["measurement"]["static_dependency_cycles"]
        .as_array()
        .map_or(0, Vec::len);
    let mut body = header(data);
    body += &sections::tiles(data, &rows, found.len(), readiness::untested(r));
    body += &sections::map(&rows, &hot_rank);
    body += &sections::hotspots(data, 12);
    if !r.is_null() {
        body += &readiness::render(r);
    }
    body += &sections::burden(&rows, cycles);
    body += &sections::coupling(data, 10);
    body += &sections::findings(&found);
    body += FOOTER;
    body += &format!(
        "<p>Generated by emi {}, measured as <code>{}</code>. The data is embedded in this file as JSON (<code>#emi-data</code>).</p></footer>",
        env!("CARGO_PKG_VERSION"),
        esc(data["measurement"]["measurement_profile"]
            .as_str()
            .unwrap_or(""))
    );
    let embedded = serde_json::to_string(data)
        .unwrap_or_default()
        .replace("</", "<\\/");
    let (source, head) = scope(data);
    let short: String = head.chars().take(8).collect();
    let title = esc(&format!("EMI report · {source} · {short}"));
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{title}</title><style>{STYLE}</style></head><body class="viz-root"><main>{body}</main><div id="tip" role="tooltip"></div><script type="application/json" id="emi-data">{embedded}</script><script>{SCRIPT}</script></body></html>
"#
    )
}

pub fn run(a: &ReportArgs) -> anyhow::Result<u8> {
    let data = collect(a).map_err(anyhow::Error::msg)?;
    std::fs::write(&a.output, render(&data))?;
    println!("wrote {}", a.output.display());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({
            "source": "src", "head": "abc", "generated": "now", "commits": 3, "window_days": 365,
            "measurement": {"measurement_profile": "p", "static_dependency_cycles": [],
                "components": [
                    {"id": "</script><b>x.py", "sloc": 10, "burden": {"E": 1.0, "V": 0, "C": null}},
                    {"id": "pkg/y.py", "sloc": 40, "burden": {"E": 4.0, "V": 1.0, "C": {"known_mass": 2.0, "unknown_sloc": 3.0}}}]},
            "hotspots": [{"file": "</script><b>x.py", "revisions": 3, "sloc": 10, "burden": [1.0, 11.0],
                          "score": [3.0, 33.0], "authors": 1, "main_author_share": 1.0, "ai_share": 0.5}],
            "change_coupling": [], "findings": [], "readiness": null})
    }

    #[test]
    fn report_is_self_contained_and_escapes_data() {
        let html = render(&sample());
        assert!(
            !html.contains("http://") && !html.contains("https://"),
            "no external resources"
        );
        assert_eq!(
            html.matches("</script>").count(),
            2,
            "embedded data cannot close the script"
        );
        assert!(
            html.contains("&lt;/script&gt;&lt;b&gt;x.py"),
            "labels are escaped"
        );
        assert!(html.contains("prefers-color-scheme:dark"));
        assert!(
            html.contains(r#"data-theme="dark""#),
            "the theme toggle wins both ways"
        );
    }

    #[test]
    fn every_chart_has_a_table_twin_and_sections_say_when_empty() {
        let html = render(&sample());
        for twin in ["All files as a table", "All hotspots as a table"] {
            assert!(html.contains(twin), "{twin}");
        }
        assert!(html.contains("No pair changed together often enough"));
        assert!(html.contains("No findings."));
        assert!(
            !html.contains("Test evidence</h2>"),
            "no profile, no section"
        );
    }

    #[test]
    fn times_read_in_utc_and_other_text_is_kept() {
        assert_eq!(
            when("2026-09-29T18:58:57.154030+00:00"),
            "2026-09-29 18:58 UTC"
        );
        assert_eq!(when("now"), "now");
    }

    #[test]
    fn narrow_screens_get_their_own_map_and_pairs_name_a_shared_folder_once() {
        let mut data = sample();
        data["change_coupling"] =
            json!([{"a": "pkg/a.py", "b": "pkg/b.py", "shared": 6, "degree": 64.0}]);
        let html = render(&data);
        assert!(html.contains(r#"class="map wide""#) && html.contains(r#"class="map narrow""#));
        assert!(
            html.contains(r#"<span class="dir">pkg/</span>a.py ↔ b.py"#),
            "{html}"
        );
        assert!(html.contains(">64%<"), "degree is written, not only drawn");
    }

    #[test]
    fn with_no_measured_burden_no_share_is_invented() {
        let mut data = sample();
        for c in data["measurement"]["components"]
            .as_array_mut()
            .into_iter()
            .flatten()
        {
            c["burden"] = json!({"E": 0.0, "V": 0.0, "C": 0.0});
        }
        let html = render(&data);
        assert!(
            !html.contains("% of the measured burden"),
            "0 of 0 is not 0%"
        );
        assert!(html.contains("No file carries measured burden"));
    }

    #[test]
    fn a_profile_of_another_commit_is_flagged_not_used() {
        let mut data = sample();
        let profile =
            json!({"head": "def", "functions": [], "summary": {"readiness": {"weak": 1}}});
        data["readiness"] = readiness::from_profile(&profile, "abc");
        let html = render(&data);
        assert!(html.contains("Not shown: the profile is of"), "{html}");
        let fresh = json!({"head": "abc", "summary": {"readiness": {"unexecuted": 2, "strong": 3}},
            "functions": [{"file": "pkg/y.py", "start": 3, "function": "f", "readiness": "unexecuted",
                           "advice": "add tests", "facts": {"revisions": 4, "coverage": {"executed": 0, "executable": 5}, "mutation": null}}]});
        data["readiness"] = readiness::from_profile(&fresh, "abc");
        let html = render(&data);
        assert!(
            html.contains("✕ no test runs it"),
            "status carries an icon and a label"
        );
        assert!(html.contains("Functions no test runs"), "headline tile");
    }
}
