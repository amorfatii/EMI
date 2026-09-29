//! The report's sections, each built for one question: where the code is
//! and where it changes (the map), what to look at first (hotspots), what
//! the burden is made of (three tiles), what changes together (pairs),
//! and what the rules found. Every chart has a table twin.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::Value;

use super::page::{
    compact, count, esc, fmt_int, interval, num, section, table_view, tile, tile_with,
};
use super::treemap::{self, Tile};

/// One measured file, joined with its history.
pub struct FileRow {
    pub path: String,
    pub sloc: f64,
    /// Erosion, redundancy, coupling: `[measured, upper]`.
    pub dims: [[f64; 2]; 3],
    pub revisions: u32,
    pub authors: u32,
    pub main_share: f64,
    pub ai_share: f64,
}

impl FileRow {
    pub fn total(&self) -> [f64; 2] {
        self.dims
            .iter()
            .fold([0.0, 0.0], |a, d| [a[0] + d[0], a[1] + d[1]])
    }
}

/// `[measured, upper]` of one dimension: an interval's known mass and its
/// unknown lines, or the whole file when the dimension is unknown.
pub fn bounds(c: &Value, dim: &str) -> [f64; 2] {
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

fn arr(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

/// Every measured file with its burden and, when it changed in the
/// window, its history.
pub fn files(data: &Value) -> Vec<FileRow> {
    let hot: BTreeMap<String, Value> = arr(&data["hotspots"])
        .into_iter()
        .map(|h| (h["file"].as_str().unwrap_or("").to_owned(), h))
        .collect();
    arr(&data["measurement"]["components"])
        .iter()
        .map(|c| {
            let path = c["id"].as_str().unwrap_or("").to_owned();
            let h = hot.get(&path).cloned().unwrap_or(Value::Null);
            let u = |k: &str| u32::try_from(h[k].as_u64().unwrap_or(0)).unwrap_or(u32::MAX);
            FileRow {
                sloc: c["sloc"].as_f64().unwrap_or(0.0),
                dims: [bounds(c, "E"), bounds(c, "V"), bounds(c, "C")],
                revisions: u("revisions"),
                authors: u("authors"),
                main_share: h["main_author_share"].as_f64().unwrap_or(0.0),
                ai_share: h["ai_share"].as_f64().unwrap_or(0.0),
                path,
            }
        })
        .collect()
}

fn split_path(path: &str) -> (&str, &str) {
    path.rfind('/')
        .map_or(("", path), |i| (&path[..=i], &path[i + 1..]))
}

fn path_html(path: &str) -> String {
    let (dir, name) = split_path(path);
    format!(r#"<span class="dir">{}</span>{}"#, esc(dir), esc(name))
}

/// The files that take half of all file changes, fewest first.
fn half_of_changes(rows: &[FileRow]) -> Option<(usize, f64)> {
    let mut revs: Vec<u32> = rows.iter().map(|r| r.revisions).collect();
    revs.sort_unstable_by(|a, b| b.cmp(a));
    let total: u64 = revs.iter().map(|r| u64::from(*r)).sum();
    if total == 0 {
        return None;
    }
    let mut acc = 0;
    let n = revs
        .iter()
        .take_while(|r| {
            let before = acc;
            acc += u64::from(**r);
            before * 2 < total
        })
        .count();
    Some((n, 100.0 * n as f64 / rows.len().max(1) as f64))
}

/// The headline tiles.
pub fn tiles(data: &Value, rows: &[FileRow], findings: usize, untested: Option<usize>) -> String {
    let sloc: f64 = rows.iter().map(|r| r.sloc).sum();
    let folders = rows
        .iter()
        .map(|r| r.path.split_once('/').map_or("", |(g, _)| g))
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let window = data["window_days"]
        .as_u64()
        .map_or("in all history".to_owned(), |d| {
            format!("in the last {d} days")
        });
    let (files, touched) = (
        rows.len() as f64,
        rows.iter().filter(|r| r.revisions > 0).count(),
    );
    let lines = if (sloc - 1.0).abs() < 1e-9 {
        "line"
    } else {
        "lines"
    };
    let mut out = String::from(r#"<div class="tiles">"#);
    out += &tile(
        "Code",
        &format!("{} {lines}", compact(sloc)),
        &format!(
            "in {}, {}",
            count(files, "file", "files"),
            count(folders as f64, "folder", "folders")
        ),
    );
    out += &tile(
        "Changes",
        &count(data["commits"].as_f64().unwrap_or(0.0), "commit", "commits"),
        &format!(
            "{window} · {touched} of {} changed",
            count(files, "file", "files")
        ),
    );
    out += &match half_of_changes(rows) {
        Some((n, share)) => tile(
            "Half of all changes land in",
            &count(n as f64, "file", "files"),
            &format!("{share:.0}% of the files"),
        ),
        None => tile(
            "Half of all changes land in",
            "—",
            "no changes in the window",
        ),
    };
    if let Some(n) = untested {
        out += &tile(
            "Functions no test runs",
            &n.to_string(),
            "any change there passes the tests",
        );
    }
    out += &tile(
        "Rule findings",
        &findings.to_string(),
        if findings == 0 {
            "none from the built-in rules"
        } else {
            "from the built-in rules"
        },
    );
    out + "</div>"
}

/// The change classes as one ordered strip, no change first, each step
/// labelled with its range; then the other two encodings.
fn legend(labels: &[String]) -> String {
    let mut out = String::from(
        r#"<div class="legend"><span class="key">Changes in the window<span class="ramp"><span><i style="background:var(--b0)"></i>0</span>"#,
    );
    for (i, l) in labels.iter().enumerate() {
        let _ = write!(
            out,
            r#"<span><i style="background:var(--b{})"></i>{}</span>"#,
            i + 1,
            esc(l)
        );
    }
    out + r#"</span></span><span class="key">Area: lines of code</span><span class="key"><span class="num-badge">1</span>top five hotspots</span></div>"#
}

fn files_table(rows: &[FileRow]) -> String {
    let table: Vec<Vec<(f64, String)>> = rows
        .iter()
        .map(|r| {
            let t = r.total();
            vec![
                (f64::NAN, r.path.clone()),
                (r.sloc, fmt_int(r.sloc)),
                (t[1], interval(t)),
                (r.dims[0][1], interval(r.dims[0])),
                (r.dims[1][1], interval(r.dims[1])),
                (r.dims[2][1], interval(r.dims[2])),
                (f64::from(r.revisions), r.revisions.to_string()),
                (f64::from(r.authors), r.authors.to_string()),
                (r.main_share, format!("{:.0}%", 100.0 * r.main_share)),
                (r.ai_share, format!("{:.0}%", 100.0 * r.ai_share)),
            ]
        })
        .collect();
    table_view(
        "files",
        "All files as a table",
        &[
            "File",
            "Lines",
            "Burden",
            "Erosion",
            "Redundancy",
            "Coupling",
            "Changes",
            "Authors",
            "Main author",
            "AI-attributed",
        ],
        &table,
    )
}

/// The codebase map, its legend, and the files table.
pub fn map(rows: &[FileRow], hot_rank: &BTreeMap<String, usize>) -> String {
    let tiles: Vec<Tile> = rows
        .iter()
        .map(|r| Tile {
            path: r.path.clone(),
            sloc: r.sloc,
            revisions: r.revisions,
            burden: r.total(),
            hotspot: hot_rank.get(&r.path).copied(),
        })
        .collect();
    let body = format!(
        "{}{}{}{}",
        treemap::render(&tiles, treemap::WIDE, "map wide"),
        treemap::render(&tiles, treemap::NARROW, "map narrow"),
        legend(&treemap::legend_labels(&tiles)),
        files_table(rows)
    );
    section(
        "map",
        "Codebase map",
        "Every file, grouped by folder. Area is lines of code; color is how often the file changed in the window. Numbers mark the top five hotspots.",
        &body,
    )
}

fn score(h: &Value) -> [f64; 2] {
    [
        h["score"][0].as_f64().unwrap_or(0.0),
        h["score"][1].as_f64().unwrap_or(0.0),
    ]
}

fn bar(b: [f64; 2], max: f64) -> String {
    let pct = |v: f64| 100.0 * v / max.max(1e-9);
    let known = format!(r#"<i class="k" style="width:{:.2}%"></i>"#, pct(b[0]));
    let possible = if b[1] - b[0] > 1e-9 {
        format!(
            r#"<i class="p" style="width:{:.2}%"></i>"#,
            pct(b[1] - b[0])
        )
    } else {
        String::new()
    };
    format!(r#"<span class="bar">{known}{possible}</span>"#)
}

fn hotspot_row(i: usize, h: &Value, max: f64) -> String {
    let file = h["file"].as_str().unwrap_or("");
    let burden = [
        h["burden"][0].as_f64().unwrap_or(0.0),
        h["burden"][1].as_f64().unwrap_or(0.0),
    ];
    let s = score(h);
    let tip = format!(
        "{file}\n{} × burden {} = score {}\n{} · {} · main author {:.0}% · AI-attributed {:.0}%",
        count(h["revisions"].as_f64().unwrap_or(0.0), "change", "changes"),
        interval(burden),
        interval(s),
        count(h["sloc"].as_f64().unwrap_or(0.0), "line", "lines"),
        count(h["authors"].as_f64().unwrap_or(0.0), "author", "authors"),
        100.0 * h["main_author_share"].as_f64().unwrap_or(0.0),
        100.0 * h["ai_share"].as_f64().unwrap_or(0.0)
    );
    format!(
        r#"<li class="row" tabindex="0" data-tip="{}"><span class="num-badge">{}</span><span class="path">{}</span><span class="r">{}</span><span class="r hide-s">{}</span>{}</li>"#,
        esc(&tip),
        i + 1,
        path_html(file),
        h["revisions"],
        interval(burden),
        bar(s, max)
    )
}

/// The ranked hotspots, and their table.
pub fn hotspots(data: &Value, top: usize) -> String {
    let all = arr(&data["hotspots"]);
    if all.is_empty() {
        let empty = r#"<p class="empty">No measured file changed in the window.</p>"#;
        return section(
            "hotspots",
            "Hotspots",
            "Where change keeps landing in costly code.",
            empty,
        );
    }
    let max = all.iter().map(|h| score(h)[1]).fold(0.0, f64::max);
    let mut body = String::from(
        r#"<div class="hot"><div class="row head"><span>#</span><span>File</span><span class="r">Changes</span><span class="r hide-s">Burden</span><span class="hide-s">Hotspot score</span></div><ol class="rows">"#,
    );
    for (i, h) in all.iter().take(top).enumerate() {
        body += &hotspot_row(i, h, max);
    }
    body += "</ol></div>";
    let rows: Vec<Vec<(f64, String)>> = all
        .iter()
        .map(|h| {
            let s = score(h);
            vec![
                (f64::NAN, h["file"].as_str().unwrap_or("").to_owned()),
                (
                    h["revisions"].as_f64().unwrap_or(0.0),
                    h["revisions"].to_string(),
                ),
                (
                    h["burden"][1].as_f64().unwrap_or(0.0),
                    interval([
                        h["burden"][0].as_f64().unwrap_or(0.0),
                        h["burden"][1].as_f64().unwrap_or(0.0),
                    ]),
                ),
                (s[1], interval(s)),
                (
                    h["authors"].as_f64().unwrap_or(0.0),
                    h["authors"].to_string(),
                ),
            ]
        })
        .collect();
    body += &table_view(
        "hot",
        "All hotspots as a table",
        &["File", "Changes", "Burden", "Score", "Authors"],
        &rows,
    );
    let mut lede = String::from(
        "Where change keeps landing in costly code: changes in the window × burden, highest first.",
    );
    if all.iter().take(top).any(|h| score(h)[1] > score(h)[0]) {
        lede += " The light end of a bar is what unresolved evidence allows.";
    }
    section("hotspots", "Hotspots", &lede, &body)
}

/// The three burden dimensions as tiles, each with its share of the
/// measured total. What unresolved evidence could add is stated in words:
/// it has no share of a total that does not include it.
pub fn burden(rows: &[FileRow], cycles: usize) -> String {
    let names = [
        ("Erosion", "size and branching"),
        ("Redundancy", "repeated code"),
        ("Coupling", "dependencies between files"),
    ];
    let sums: Vec<[f64; 2]> = (0..3)
        .map(|d| {
            rows.iter().fold([0.0, 0.0], |a, r| {
                [a[0] + r.dims[d][0], a[1] + r.dims[d][1]]
            })
        })
        .collect();
    let measured: f64 = sums.iter().map(|s| s[0]).sum();
    let mut body = String::from(r#"<div class="tiles dims">"#);
    for (d, (name, what)) in names.iter().enumerate() {
        let [lo, hi] = sums[d];
        // with nothing measured a share is undefined, not 0%
        let share = (measured > 0.0).then(|| 100.0 * lo / measured);
        let mut meta = (*what).to_owned();
        if let Some(s) = share {
            let _ = write!(meta, " · {s:.0}% of the measured burden");
        }
        if num(hi) != num(lo) {
            let _ = write!(
                meta,
                " · up to {} more from unresolved evidence",
                num(hi - lo)
            );
        }
        if d == 2 && cycles > 0 {
            let _ = write!(
                meta,
                " · {cycles} dependency cycle{}",
                if cycles == 1 { "" } else { "s" }
            );
        }
        let meter = share.map_or(String::new(), |s| {
            format!(r#"<div class="meter" role="img" aria-label="{s:.0}% of the measured burden"><i style="width:{s:.2}%"></i></div>"#)
        });
        body += &tile_with(name, &num(lo), &meta, &meter);
    }
    body += "</div>";
    let lede = if measured > 0.0 {
        "EMI's burden split into its three dimensions and summed over all files, in line-equivalents. Each bar is that dimension's share of the measured total."
    } else {
        "EMI's burden split into its three dimensions and summed over all files, in line-equivalents. No file carries measured burden."
    };
    section("burden", "What the burden is made of", lede, &body)
}

fn top_group(path: &str) -> &str {
    path.split_once('/').map_or("", |(g, _)| g)
}

/// Two paths, their shared folder written once: `dir/a.rs ↔ b.rs`.
fn pair_html(a: &str, b: &str) -> String {
    let ((dir_a, name_a), (dir_b, name_b)) = (split_path(a), split_path(b));
    if dir_a == dir_b {
        format!(
            r#"<span class="dir">{}</span>{} ↔ {}"#,
            esc(dir_a),
            esc(name_a),
            esc(name_b)
        )
    } else {
        format!("{} ↔ {}", path_html(a), path_html(b))
    }
}

fn pair_row(c: &Value) -> String {
    let (a, b) = (c["a"].as_str().unwrap_or(""), c["b"].as_str().unwrap_or(""));
    let degree = c["degree"].as_f64().unwrap_or(0.0);
    let across = if top_group(a) == top_group(b) {
        String::new()
    } else {
        r#"<span class="tag">across folders</span>"#.to_owned()
    };
    let shared = count(
        c["shared"].as_f64().unwrap_or(0.0),
        "shared commit",
        "shared commits",
    );
    let tip = format!("{a}\n↔ {b}\n{shared} · degree {degree:.0}%");
    format!(
        r#"<li class="row" tabindex="0" data-tip="{}"><span class="path">{}{across}</span><span class="r">{}</span><span class="r">{degree:.0}%</span>{}</li>"#,
        esc(&tip),
        pair_html(a, b),
        c["shared"],
        bar([degree.min(100.0), degree.min(100.0)], 100.0)
    )
}

/// Files that change together.
pub fn coupling(data: &Value, top: usize) -> String {
    let pairs = arr(&data["change_coupling"]);
    let lede = "Pairs of files that often change in the same commit. Degree: shared commits ÷ their average number of changes. Pairs across folders often point at a dependency the code does not show.";
    if pairs.is_empty() {
        let empty = r#"<p class="empty">No pair changed together often enough (at least 5 shared commits and 30% degree).</p>"#;
        return section("coupling", "Files that change together", lede, empty);
    }
    let mut body = String::from(
        r#"<div class="pairs"><div class="row head"><span>Files</span><span class="r">Shared</span><span class="r">Degree</span><span class="hide-s"></span></div><ol class="rows">"#,
    );
    for c in pairs.iter().take(top) {
        body += &pair_row(c);
    }
    body += "</ol></div>";
    let rows: Vec<Vec<(f64, String)>> = pairs
        .iter()
        .map(|c| {
            vec![
                (f64::NAN, c["a"].as_str().unwrap_or("").to_owned()),
                (f64::NAN, c["b"].as_str().unwrap_or("").to_owned()),
                (c["shared"].as_f64().unwrap_or(0.0), c["shared"].to_string()),
                (
                    c["degree"].as_f64().unwrap_or(0.0),
                    format!("{:.0}%", c["degree"].as_f64().unwrap_or(0.0)),
                ),
            ]
        })
        .collect();
    body += &table_view(
        "pairs",
        "All pairs as a table",
        &["File", "File", "Shared", "Degree"],
        &rows,
    );
    section("coupling", "Files that change together", lede, &body)
}

/// New findings of the built-in rules, counted per rule, and their table.
pub fn findings(found: &[Value]) -> String {
    let lede = "What the built-in declarative rules flag in the reported tree (emi check).";
    if found.is_empty() {
        return section(
            "findings",
            "Rule findings",
            lede,
            r#"<p class="empty">No findings.</p>"#,
        );
    }
    let mut per_rule: BTreeMap<&str, usize> = BTreeMap::new();
    for f in found {
        *per_rule
            .entry(f["rule"].as_str().unwrap_or(""))
            .or_default() += 1;
    }
    let max = per_rule.values().copied().max().unwrap_or(1) as f64;
    let mut ranked: Vec<(&str, usize)> = per_rule.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let mut body = String::from(
        r#"<div class="rules"><div class="row head"><span>Rule</span><span class="r">Findings</span><span></span></div><ol class="rows">"#,
    );
    for (rule, n) in &ranked {
        let _ = write!(
            body,
            r#"<li class="row"><span class="path">{}</span><span class="r">{n}</span>{}</li>"#,
            esc(rule),
            bar([*n as f64, *n as f64], max)
        );
    }
    body += "</ol></div>";
    let rows: Vec<Vec<(f64, String)>> = found
        .iter()
        .map(|f| {
            vec![
                (f64::NAN, f["severity"].as_str().unwrap_or("").to_owned()),
                (f64::NAN, f["rule"].as_str().unwrap_or("").to_owned()),
                (
                    f64::NAN,
                    format!("{}:{}", f["file"].as_str().unwrap_or(""), f["line"]),
                ),
                (f64::NAN, f["message"].as_str().unwrap_or("").to_owned()),
            ]
        })
        .collect();
    body += &table_view(
        "findings",
        "All findings as a table",
        &["Severity", "Rule", "Where", "Message"],
        &rows,
    );
    section("findings", "Rule findings", lede, &body)
}
