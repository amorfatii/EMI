//! `emi report`: one self-contained HTML file (no external resources):
//! summary, burden per dimension, most burdened files, a hotspot map,
//! change coupling, rule findings, and the data embedded as JSON.

use std::fmt::Write as _;
use std::path::PathBuf;

use clap::Args;
use emi_diff::Request;
use serde_json::{Value, json};

use crate::{Language, ProfileArg};

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
    #[arg(short, long, default_value = "emi-report.html")]
    output: PathBuf,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn bounds(c: &Value, dim: &str) -> [f64; 2] {
    let sloc = c["sloc"].as_f64().unwrap_or(0.0);
    match &c["burden"][dim] {
        Value::Null => [0.0, sloc],
        Value::Object(o) => {
            let k = o.get("known_mass").and_then(Value::as_f64).unwrap_or(0.0);
            [
                k,
                k + o.get("unknown_sloc").and_then(Value::as_f64).unwrap_or(0.0),
            ]
        }
        v => [v.as_f64().unwrap_or(0.0); 2],
    }
}

/// Horizontal bars: known part solid, unknown width hatched.
fn bars(rows: &[(String, [f64; 2])], width: f64) -> String {
    let max = rows.iter().map(|r| r.1[1]).fold(1e-9, f64::max);
    let h = 22.0;
    let mut svg = format!(
        r#"<svg viewBox="0 0 {width} {}" class="chart" role="img">"#,
        rows.len() as f64 * h + 4.0
    );
    for (i, (label, [lo, hi])) in rows.iter().enumerate() {
        let y = i as f64 * h + 2.0;
        let (x0, w) = (230.0, width - 300.0);
        let _ = write!(
            svg,
            r#"<text x="225" y="{}" class="lbl">{}</text><rect x="{x0}" y="{y}" width="{:.1}" height="16" class="known"/><rect x="{:.1}" y="{y}" width="{:.1}" height="16" class="unknown"/><text x="{:.1}" y="{}" class="val">{}</text>"#,
            y + 12.0,
            esc(label),
            w * lo / max,
            x0 + w * lo / max,
            w * (hi - lo) / max,
            x0 + w * hi / max + 5.0,
            y + 12.0,
            if (hi - lo).abs() < 0.05 {
                format!("{lo:.1}")
            } else {
                format!("{lo:.1}–{hi:.1}")
            }
        );
    }
    svg + "</svg>"
}

/// Hotspot map: revisions (x, log) against burden (y, log).
fn scatter(hotspots: &[Value]) -> String {
    let (w, h, pad) = (720.0, 360.0, 48.0);
    let lx = |v: f64| (v.max(1.0)).ln();
    let max_x = hotspots
        .iter()
        .filter_map(|p| p["revisions"].as_f64())
        .map(lx)
        .fold(1.0, f64::max);
    let max_y = hotspots
        .iter()
        .filter_map(|p| p["burden"][1].as_f64())
        .map(|b| lx(b + 1.0))
        .fold(1.0, f64::max);
    let mut svg = format!(
        r#"<svg viewBox="0 0 {w} {h}" class="chart" role="img"><line x1="{pad}" y1="{}" x2="{w}" y2="{}" class="axis"/><line x1="{pad}" y1="0" x2="{pad}" y2="{}" class="axis"/><text x="{}" y="{}" class="lbl">revisions →</text><text x="{}" y="14" class="lbl2">burden ↑</text>"#,
        h - pad,
        h - pad,
        h - pad,
        w - 10.0,
        h - 18.0,
        pad + 6.0
    );
    for (i, p) in hotspots.iter().enumerate() {
        let x = pad + (w - pad - 10.0) * lx(p["revisions"].as_f64().unwrap_or(1.0)) / max_x;
        let y =
            (h - pad) - (h - pad - 10.0) * lx(p["burden"][1].as_f64().unwrap_or(0.0) + 1.0) / max_y;
        let r = 3.0 + (p["sloc"].as_f64().unwrap_or(0.0)).sqrt() / 4.0;
        let class = if i < 5 { "dot hot" } else { "dot" };
        let _ = write!(
            svg,
            r#"<circle cx="{x:.1}" cy="{y:.1}" r="{r:.1}" class="{class}"><title>{} · {} revisions · burden {:.1}</title></circle>"#,
            esc(p["file"].as_str().unwrap_or("")),
            p["revisions"],
            p["burden"][1].as_f64().unwrap_or(0.0)
        );
        if i < 5 {
            // Label on the side with room: right of the dot, else left.
            let (lx, anchor) = if x > w * 0.75 {
                (x - r - 3.0, "end")
            } else {
                (x + r + 3.0, "start")
            };
            let _ = write!(
                svg,
                r#"<text x="{lx:.1}" y="{:.1}" class="val" text-anchor="{anchor}">{}</text>"#,
                y + 4.0,
                esc(p["file"].as_str().unwrap_or(""))
            );
        }
    }
    svg + "</svg>"
}

fn table(id: &str, head: &[&str], rows: &[Vec<String>]) -> String {
    let mut t = format!(r#"<table id="{id}" class="sortable"><thead><tr>"#);
    for h in head {
        let _ = write!(t, "<th>{}</th>", esc(h));
    }
    t.push_str("</tr></thead><tbody>");
    if rows.is_empty() {
        let _ = write!(
            t,
            r#"<tr><td colspan="{}" class="muted">None.</td></tr>"#,
            head.len()
        );
    }
    for r in rows {
        t.push_str("<tr>");
        for c in r {
            let _ = write!(t, "<td>{}</td>", esc(c));
        }
        t.push_str("</tr>");
    }
    t + "</tbody></table>"
}

const STYLE: &str = r"
:root{--bg:#fff;--fg:#1d2330;--muted:#5d6678;--card:#f5f7fa;--line:#d8dee8;--known:#3b6fd8;--unknown:#b9c9ee;--hot:#d9483b}
@media (prefers-color-scheme:dark){:root{--bg:#10141b;--fg:#e4e8ef;--muted:#98a2b3;--card:#171d27;--line:#2b3445;--known:#6e9bff;--unknown:#34466e;--hot:#ff7a6b}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,sans-serif}
main{max-width:1100px;margin:0 auto;padding:24px 16px}h1{font-size:24px;margin:0 0 4px}h2{font-size:18px;margin:32px 0 8px}
.muted{color:var(--muted)}.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(160px,1fr));gap:12px;margin:16px 0}
.card{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:12px}.card b{display:block;font-size:22px}
.chart{width:100%;height:auto}.lbl{fill:var(--fg);font-size:12px;text-anchor:end}.lbl2{fill:var(--fg);font-size:12px}.val{fill:var(--muted);font-size:11px}
.known{fill:var(--known)}.unknown{fill:var(--unknown)}.axis{stroke:var(--line)}.dot{fill:var(--known);opacity:.55}.dot.hot{fill:var(--hot);opacity:.9}
table{width:100%;border-collapse:collapse;font-size:13px}th,td{text-align:left;padding:5px 8px;border-bottom:1px solid var(--line)}
th{cursor:pointer;user-select:none;position:sticky;top:0;background:var(--bg)}.wrap{max-height:420px;overflow:auto;border:1px solid var(--line);border-radius:8px}
input{width:100%;padding:6px 8px;margin:6px 0;border:1px solid var(--line);border-radius:6px;background:var(--card);color:var(--fg)}
";

const SCRIPT: &str = r"
document.querySelectorAll('table.sortable th').forEach((th,i)=>th.addEventListener('click',()=>{
 const tb=th.closest('table').tBodies[0];const rows=[...tb.rows];const asc=th.dataset.asc!=='1';th.dataset.asc=asc?'1':'0';
 rows.sort((a,b)=>{const x=a.cells[i].textContent,y=b.cells[i].textContent;const nx=parseFloat(x),ny=parseFloat(y);
  const c=(!isNaN(nx)&&!isNaN(ny))?nx-ny:x.localeCompare(y);return asc?c:-c});rows.forEach(r=>tb.appendChild(r))}));
document.querySelectorAll('input[data-filter]').forEach(inp=>inp.addEventListener('input',()=>{
 const q=inp.value.toLowerCase();document.querySelectorAll('#'+inp.dataset.filter+' tbody tr').forEach(r=>{r.style.display=r.textContent.toLowerCase().includes(q)?'':'none'})}));
";

fn total(c: &Value) -> [f64; 2] {
    ["E", "V", "C"].iter().fold([0.0, 0.0], |a, d| {
        let x = bounds(c, d);
        [a[0] + x[0], a[1] + x[1]]
    })
}

fn dimension_rows(comps: &[Value]) -> Vec<(String, [f64; 2])> {
    [
        ("E erosion", "E"),
        ("V redundancy", "V"),
        ("C coupling", "C"),
    ]
    .iter()
    .map(|(name, d)| {
        let b = comps.iter().fold([0.0, 0.0], |a, c| {
            let x = bounds(c, d);
            [a[0] + x[0], a[1] + x[1]]
        });
        ((*name).to_owned(), b)
    })
    .collect()
}

fn file_rows(comps: &[Value]) -> Vec<(String, [f64; 2])> {
    let mut files: Vec<(String, [f64; 2])> = comps
        .iter()
        .map(|c| (c["id"].as_str().unwrap_or("").to_owned(), total(c)))
        .collect();
    files.sort_by(|a, b| (b.1[0] + b.1[1]).total_cmp(&(a.1[0] + a.1[1])));
    files.truncate(15);
    files
}

fn pct(v: &Value) -> String {
    format!("{:.0}", 100.0 * v.as_f64().unwrap_or(0.0))
}

fn hotspot_rows(hotspots: &[Value]) -> Vec<Vec<String>> {
    hotspots
        .iter()
        .take(50)
        .map(|h| {
            vec![
                h["file"].as_str().unwrap_or("").into(),
                h["revisions"].to_string(),
                h["sloc"].to_string(),
                format!("{:.1}", h["burden"][1].as_f64().unwrap_or(0.0)),
                h["authors"].to_string(),
                pct(&h["main_author_share"]),
                pct(&h["ai_share"]),
            ]
        })
        .collect()
}

fn coupling_rows(data: &Value) -> Vec<Vec<String>> {
    data["change_coupling"]
        .as_array()
        .into_iter()
        .flatten()
        .take(50)
        .map(|c| {
            vec![
                c["a"].as_str().unwrap_or("").into(),
                c["b"].as_str().unwrap_or("").into(),
                c["shared"].to_string(),
                format!("{:.0}", c["degree"].as_f64().unwrap_or(0.0)),
            ]
        })
        .collect()
}

fn finding_rows(data: &Value) -> Vec<Vec<String>> {
    data["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["state"] == "new")
        .map(|f| {
            vec![
                f["severity"].as_str().unwrap_or("").into(),
                f["rule"].as_str().unwrap_or("").into(),
                format!("{}:{}", f["file"].as_str().unwrap_or(""), f["line"]),
                f["message"].as_str().unwrap_or("").into(),
            ]
        })
        .collect()
}

fn render(data: &Value) -> String {
    let m = &data["measurement"];
    let comps = m["components"].as_array().cloned().unwrap_or_default();
    let sloc: f64 = comps.iter().filter_map(|c| c["sloc"].as_f64()).sum();
    let dims = dimension_rows(&comps);
    let files = file_rows(&comps);
    let hotspots = data["hotspots"].as_array().cloned().unwrap_or_default();
    let hot_rows = hotspot_rows(&hotspots);
    let coupling_rows = coupling_rows(data);
    let finding_rows = finding_rows(data);
    let unknown_c = dims[2].1[1] - dims[2].1[0];
    let embedded = serde_json::to_string(data)
        .unwrap_or_default()
        .replace("</", "<\\/");
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>EMI report</title><style>{STYLE}</style></head><body><main>
<h1>EMI report</h1><div class="muted">{scope} · {head} · {profile} · generated {when}</div>
<div class="cards"><div class="card"><b>{nfiles}</b>files</div><div class="card"><b>{sloc:.0}</b>SLOC</div><div class="card"><b>{commits}</b>commits in window</div><div class="card"><b>{unknown:.0}%</b>coupling unknown</div><div class="card"><b>{nfind}</b>rule findings</div></div>
<h2>Burden by dimension</h2><p class="muted">Solid: measured mass. Light: the range missing evidence allows.</p>{dimchart}
<h2>Most burdened files</h2>{filechart}
<h2>Hotspots</h2><p class="muted">Burden × change frequency (log scales; dot size ~ SLOC). Top five named.</p>{scatter}
<input data-filter="hot" placeholder="Filter hotspots"><div class="wrap">{hot}</div>
<h2>Change coupling</h2><p class="muted">Files that change together (shared commits, degree = shared ÷ mean revisions).</p><div class="wrap">{coupling}</div>
<h2>Rule findings</h2><input data-filter="findings" placeholder="Filter findings"><div class="wrap">{findings}</div>
<script type="application/json" id="emi-data">{embedded}</script><script>{SCRIPT}</script></main></body></html>
"#,
        scope = esc(data["source"].as_str().unwrap_or("")),
        head = esc(&data["head"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(12)
            .collect::<String>()),
        profile = esc(m["measurement_profile"].as_str().unwrap_or("")),
        when = esc(data["generated"].as_str().unwrap_or("")),
        nfiles = comps.len(),
        commits = data["commits"],
        unknown = if sloc > 0.0 {
            100.0 * unknown_c / sloc
        } else {
            0.0
        },
        nfind = finding_rows.len(),
        dimchart = bars(&dims, 900.0),
        filechart = bars(&files, 900.0),
        scatter = scatter(&hotspots),
        hot = table(
            "hot",
            &[
                "file",
                "revisions",
                "SLOC",
                "burden",
                "authors",
                "main author %",
                "AI %"
            ],
            &hot_rows
        ),
        coupling = table(
            "coupling",
            &["file", "file", "shared", "degree %"],
            &coupling_rows
        ),
        findings = table(
            "findings",
            &["severity", "rule", "location", "message"],
            &finding_rows
        ),
    )
}

fn collect(a: &ReportArgs) -> Result<Value, String> {
    let probe = if matches!(a.probe, Language::Js) {
        emi_scan::Probe::Js
    } else {
        emi_scan::Probe::Python
    };
    let mut req = Request::new(&a.repo, &a.source, &a.head, &a.head, probe);
    req.profile = a.profile.into();
    req.tolerate_syntax_errors = true;
    let (sha, m) = emi_diff::change::measure_ref(&req, &a.head)?;
    let m = m.ok_or("no measurable source files")?;
    let source = emi_diff::snapshot::normalize_source(&a.source)?;
    let commits = emi_history::mine(&emi_history::Query {
        repo: a.repo.clone(),
        source: source.clone(),
        head: sha.clone(),
        days: (a.days > 0).then_some(a.days),
        ai_pattern: emi_history::DEFAULT_AI_PATTERN.into(),
    })?;
    let histories = emi_history::file_histories(&commits);
    let now = commits.first().map_or(0, |c| c.time);
    let rules = emi_rules::load(&[], true)?;
    let check = emi_rules::Check {
        rules: &rules,
        only: &[],
        exclude_globs: &[],
        today: emi_diff::trajectory::utc_now_iso()[..10].to_owned(),
        baseline: None,
    };
    let findings = emi_rules::check_tree(&a.repo.join(&source), &check)?;
    Ok(json!({
        "source": source, "head": sha, "generated": emi_diff::trajectory::utc_now_iso(),
        "commits": commits.len(),
        "measurement": {"measurement_profile": m["measurement_profile"], "components": m["components"],
                        "static_dependency_cycles": m["static_dependency_cycles"]},
        "hotspots": emi_history::hotspots::rank(&m, &histories, now),
        "change_coupling": emi_history::change_coupling(&commits, 5, 30.0, 30)
            .into_iter()
            .filter(|c| m["source_sha256"].get(&c.a).is_some() && m["source_sha256"].get(&c.b).is_some())
            .collect::<Vec<_>>(),
        "findings": findings.iter().map(|f| json!({"rule": f.hit.rule, "severity": f.hit.severity,
            "file": f.hit.file, "line": f.hit.line, "message": f.hit.message, "state": f.state})).collect::<Vec<_>>(),
    }))
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

    #[test]
    fn report_is_self_contained_and_escapes_data() {
        let data = json!({
            "source": "src", "head": "abc", "generated": "now", "commits": 3,
            "measurement": {"measurement_profile": "p", "static_dependency_cycles": [],
                "components": [{"id": "</script><b>x.py", "sloc": 10, "burden": {"E": 1.0, "V": 0, "C": null}}]},
            "hotspots": [{"file": "</script><b>x.py", "revisions": 3, "sloc": 10, "burden": [1.0, 11.0],
                          "authors": 1, "main_author_share": 1.0, "ai_share": 0.5}],
            "change_coupling": [], "findings": []});
        let html = render(&data);
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
    }
}
