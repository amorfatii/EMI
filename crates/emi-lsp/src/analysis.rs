//! Per-document analysis: rule diagnostics, erosion hints, and code
//! lenses with the burden delta against a base revision.

use emi_rules::engine::Compiled;
use serde_json::{Value, json};

/// One measured function of a document.
#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    pub name: String,
    pub line: u32,
    pub sloc: u32,
    pub cc: Option<u32>,
    pub mass: f64,
}

/// Functions of a Python or JS/TS source (`None` if it does not parse or
/// the language is not supported).
pub fn functions(rel: &str, text: &str) -> Option<Vec<Function>> {
    let lower = rel.to_lowercase();
    let records = if lower.ends_with(".py") {
        emi_lang_python::measure(rel, text).ok()?.functions
    } else if [".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"]
        .iter()
        .any(|e| lower.ends_with(e))
    {
        emi_lang_js::measure(rel, text)?.functions
    } else {
        return None;
    };
    Some(
        records
            .into_iter()
            .filter(|f| !f.function.starts_with("<module"))
            .map(|f| Function {
                name: f.function,
                line: f.line,
                sloc: f.sloc,
                cc: f.cc,
                mass: f.erosion_mass.unwrap_or(0.0),
            })
            .collect(),
    )
}

/// UTF-16 column of a 1-based byte column on `line_text`.
fn utf16_col(line_text: &str, byte_col: u32) -> u32 {
    let byte = usize::try_from(byte_col.saturating_sub(1))
        .unwrap_or(0)
        .min(line_text.len());
    let prefix = line_text.get(..byte).unwrap_or(line_text);
    u32::try_from(prefix.encode_utf16().count()).unwrap_or(0)
}

fn line_range(lines: &[&str], line: u32, byte_col: u32, end_line: u32) -> Value {
    let idx = |l: u32| usize::try_from(l.saturating_sub(1)).unwrap_or(0);
    let text = lines.get(idx(line)).copied().unwrap_or("");
    let end_text = lines.get(idx(end_line)).copied().unwrap_or(text);
    let end_char = u32::try_from(end_text.encode_utf16().count()).unwrap_or(0);
    json!({"start": {"line": line.saturating_sub(1), "character": utf16_col(text, byte_col)},
           "end": {"line": end_line.saturating_sub(1), "character": end_char}})
}

fn severity(s: &str) -> u8 {
    match s {
        "error" => 1,
        "warning" => 2,
        _ => 3,
    }
}

/// LSP diagnostics: rule findings (suppressed ones omitted) and erosion
/// hints on functions past the thresholds.
pub fn diagnostics(rules: &[Compiled], rel: &str, text: &str, today: &str) -> Vec<Value> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<Value> = emi_rules::check_source(rules, rel, text, today)
        .into_iter()
        .filter(|(_, v)| *v == emi_rules::suppress::Verdict::Active)
        .map(|(h, _)| {
            json!({"range": line_range(&lines, h.line, h.column, h.line), "severity": severity(h.severity),
                   "source": "emi", "code": h.rule, "message": h.message})
        })
        .collect();
    for f in functions(rel, text)
        .unwrap_or_default()
        .into_iter()
        .filter(|f| f.mass > 0.0)
    {
        out.push(json!({
            "range": line_range(&lines, f.line, 1, f.line), "severity": 4, "source": "emi", "code": "erosion",
            "message": format!("{}: complexity {} over {} lines carries erosion mass {:.1} (thresholds: complexity 10, 50 lines)",
                f.name, f.cc.map_or("?".into(), |c| c.to_string()), f.sloc, f.mass)}));
    }
    out
}

/// Match functions to the base version by name and occurrence order.
fn base_masses(current: &[Function], base: Option<&[Function]>) -> Vec<Option<f64>> {
    let Some(base) = base else {
        return vec![None; current.len()];
    };
    let mut seen = std::collections::HashMap::new();
    current
        .iter()
        .map(|f| {
            let k = seen.entry(f.name.as_str()).or_insert(0usize);
            let hit = base
                .iter()
                .filter(|b| b.name == f.name)
                .nth(*k)
                .map(|b| b.mass);
            *k += 1;
            hit
        })
        .collect()
}

/// Code lenses for non-trivial or changed functions: erosion, complexity,
/// size, and the erosion delta since the base revision.
pub fn lenses(rel: &str, text: &str, base_text: Option<&str>) -> Vec<Value> {
    let Some(current) = functions(rel, text) else {
        return Vec::new();
    };
    let base = base_text.and_then(|b| functions(rel, b));
    let deltas = base_masses(&current, base.as_deref());
    current
        .iter()
        .zip(deltas)
        .filter_map(|(f, before)| {
            let delta = match (base.is_some(), before) {
                (false, _) => None,
                (true, None) => Some("new".to_owned()),
                (true, Some(b)) if (f.mass - b).abs() >= 0.05 => Some(format!("Δ {:+.1}", f.mass - b)),
                (true, Some(_)) => None,
            };
            let notable = f.mass > 0.0 || f.cc.is_some_and(|c| c >= 5) || f.sloc >= 20 || delta.is_some();
            notable.then(|| {
                let mut title = format!("E {:.1}", f.mass);
                if let Some(d) = &delta {
                    title.push_str(&format!(" ({d})"));
                }
                title.push_str(&format!(" · cc {} · {} lines", f.cc.map_or("?".into(), |c| c.to_string()), f.sloc));
                let line = f.line.saturating_sub(1);
                json!({"range": {"start": {"line": line, "character": 0}, "end": {"line": line, "character": 0}},
                       "command": {"title": title, "command": ""}})
            })
        })
        .collect()
}
