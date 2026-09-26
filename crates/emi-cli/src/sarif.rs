//! SARIF 2.1.0 export of a measurement, for code-scanning UIs (GitHub,
//! GitLab, IDEs). Results carry stable fingerprints built from content,
//! not line numbers, so moved code keeps its alert identity.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

/// `(id, short description, level)` of the rules EMI reports.
const RULES: [(&str, &str, &str); 18] = [
    (
        "erosion",
        "Function past the size or branching thresholds (erosion mass > 0)",
        "warning",
    ),
    (
        "clone",
        "Duplicated function body (exact or identifier-normalized clone)",
        "warning",
    ),
    (
        "dependency-cycle",
        "Module participates in a static import cycle",
        "warning",
    ),
    ("rep-line", "Long eligible line repeated verbatim", "note"),
    (
        "waste-len-check",
        "Length compared with zero where truthiness suffices",
        "note",
    ),
    (
        "waste-bool-return",
        "if/else returning boolean literals",
        "note",
    ),
    (
        "waste-eq-none-bool",
        "Equality comparison with None/True/False",
        "note",
    ),
    (
        "waste-dict-keys",
        "Iteration or membership through .keys()",
        "note",
    ),
    (
        "waste-for-index",
        "Index loop where direct iteration suffices",
        "note",
    ),
    (
        "waste-identity-comp",
        "Comprehension that copies its input unchanged",
        "note",
    ),
    (
        "waste-identity-arrow",
        "Arrow function that returns its argument",
        "note",
    ),
    (
        "waste-single-use-return",
        "Temporary assigned and immediately returned",
        "note",
    ),
    (
        "waste-type-eq",
        "Exact type comparison instead of isinstance",
        "note",
    ),
    (
        "mask-empty-catch",
        "Empty catch/except handler hides failures",
        "warning",
    ),
    (
        "mask-noop-catch",
        "No-op .catch() handler hides failures",
        "warning",
    ),
    (
        "doc-restates-signature",
        "Documentation restates the signature",
        "note",
    ),
    ("constant-assert", "Test asserts a constant", "warning"),
    ("empty-test", "Test with no assertions or body", "warning"),
];

fn describe(rule: &str) -> (String, &'static str) {
    RULES.iter().find(|r| r.0 == rule).map_or_else(
        || (rule.replace('-', " "), "note"),
        |r| (r.1.to_owned(), r.2),
    )
}

fn fingerprint(parts: &[&str]) -> String {
    emi_ir::pytext::sha256_hex(parts.join("\u{1f}").as_bytes())
}

/// Repository-style URI of a scope file: relative to the working
/// directory when the scope lies under it, else a `file://` URI.
fn uri(root: &Path, file: &str) -> String {
    let full = std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf())
        .join(file);
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|c| std::fs::canonicalize(c).ok());
    match cwd.and_then(|c| full.strip_prefix(&c).ok().map(Path::to_path_buf)) {
        Some(rel) => rel.to_string_lossy().replace('\\', "/"),
        None => format!("file://{}", full.to_string_lossy()),
    }
}

fn location(root: &Path, file: &str, start: u64, end: u64) -> Value {
    json!({"physicalLocation": {"artifactLocation": {"uri": uri(root, file)},
                                "region": {"startLine": start.max(1), "endLine": end.max(start).max(1)}}})
}

struct Results<'a> {
    root: &'a Path,
    out: Vec<Value>,
    used: BTreeMap<String, ()>,
    sources: BTreeMap<String, Vec<String>>,
}

impl Results<'_> {
    /// Trimmed text of the given lines: a fingerprint that survives moves.
    fn text_of(&mut self, file: &str, lines: &[u64]) -> String {
        let root = self.root;
        let src = self.sources.entry(file.to_owned()).or_insert_with(|| {
            std::fs::read_to_string(root.join(file))
                .map(|t| t.lines().map(str::to_owned).collect())
                .unwrap_or_default()
        });
        lines
            .iter()
            .filter_map(|l| {
                usize::try_from(*l)
                    .ok()
                    .and_then(|l| src.get(l.wrapping_sub(1)))
            })
            .map(|t| t.trim())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn push(
        &mut self,
        rule: &str,
        message: String,
        locations: Vec<Value>,
        key: &[&str],
        related: Vec<Value>,
    ) {
        let (_, level) = describe(rule);
        self.used.insert(rule.to_owned(), ());
        let mut r = json!({"ruleId": format!("emi/{rule}"), "level": level, "message": {"text": message},
                           "locations": locations,
                           "partialFingerprints": {"emiContentHash/v1": fingerprint(key)}});
        if !related.is_empty() {
            r["relatedLocations"] = Value::Array(related);
        }
        self.out.push(r);
    }

    fn erosion(&mut self, m: &Value) {
        for f in m["functions"].as_array().into_iter().flatten() {
            let mass = f["erosion_mass"].as_f64().unwrap_or(0.0);
            if mass <= 0.0 {
                continue;
            }
            let (file, name) = (
                f["file"].as_str().unwrap_or(""),
                f["function"].as_str().unwrap_or(""),
            );
            let line = f["line"].as_u64().unwrap_or(1);
            let end = f["lines"]
                .as_array()
                .and_then(|l| l.last())
                .and_then(Value::as_u64)
                .unwrap_or(line);
            let message = format!(
                "{name}: cyclomatic complexity {} over {} lines carries erosion mass {mass:.1} (thresholds: complexity 10, 50 lines)",
                f["cc"], f["sloc"]
            );
            let root = self.root;
            self.push(
                "erosion",
                message,
                vec![location(root, file, line, end)],
                &["erosion", file, name],
                Vec::new(),
            );
        }
    }

    fn clones(&mut self, m: &Value) {
        for (kind, key) in [
            ("exact", "clone_groups"),
            ("normalized", "clone_groups_normalized"),
        ] {
            for group in m[key].as_array().into_iter().flatten() {
                let occ: Vec<(&str, u64, u64)> = group
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|o| {
                        let lines = o["lines"].as_array();
                        let first = lines
                            .and_then(|l| l.first())
                            .and_then(Value::as_u64)
                            .unwrap_or(1);
                        let last = lines
                            .and_then(|l| l.last())
                            .and_then(Value::as_u64)
                            .unwrap_or(first);
                        (o["file"].as_str().unwrap_or(""), first, last)
                    })
                    .collect();
                for (i, (file, start, end)) in occ.iter().enumerate() {
                    let related: Vec<Value> = occ
                        .iter()
                        .enumerate()
                        .filter(|(j, _)| *j != i)
                        .map(|(_, (f, s, e))| location(self.root, f, *s, *e))
                        .collect();
                    let message = format!(
                        "{kind} clone of {} other function bod{}",
                        occ.len() - 1,
                        if occ.len() == 2 { "y" } else { "ies" }
                    );
                    let loc = location(self.root, file, *start, *end);
                    let others: Vec<String> = occ.iter().map(|o| o.0.to_owned()).collect();
                    self.push(
                        "clone",
                        message,
                        vec![loc],
                        &["clone", kind, file, &others.join(",")],
                        related,
                    );
                }
            }
        }
    }

    fn findings(&mut self, m: &Value, key: &str) {
        for f in m[key].as_array().into_iter().flatten() {
            let (file, rule) = (
                f["file"].as_str().unwrap_or(""),
                f["rule"].as_str().unwrap_or("finding"),
            );
            let lines: Vec<u64> = f["lines"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_u64)
                .collect();
            let start = lines.first().copied().unwrap_or(1);
            let (text, _) = describe(rule);
            let content = self.text_of(file, &lines);
            let loc = location(
                self.root,
                file,
                start,
                lines.last().copied().unwrap_or(start),
            );
            self.push(rule, text, vec![loc], &[rule, file, &content], Vec::new());
        }
    }

    fn cycles(&mut self, m: &Value, files_by_module: &BTreeMap<String, String>) {
        for cycle in m["static_dependency_cycles"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let modules: Vec<&str> = cycle
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            for module in &modules {
                let Some(file) = files_by_module.get(*module) else {
                    continue;
                };
                let message = format!(
                    "{module} is in an import cycle of {} modules: {}",
                    modules.len(),
                    modules.join(" -> ")
                );
                let loc = location(self.root, file, 1, 1);
                self.push(
                    "dependency-cycle",
                    message,
                    vec![loc],
                    &["cycle", module, &modules.join(",")],
                    Vec::new(),
                );
            }
        }
    }
}

/// Module name -> file, from the per-file `in_cycle` region records.
fn modules(m: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for c in m["components"].as_array().into_iter().flatten() {
        let Some(file) = c["id"].as_str() else {
            continue;
        };
        let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
        let module = stem
            .strip_suffix("/__init__")
            .unwrap_or(stem)
            .replace('/', ".");
        out.insert(module.clone(), file.to_owned());
        out.insert(stem.to_owned(), file.to_owned());
    }
    out
}

/// SARIF log for one measurement of the scope at `root`.
pub fn log(m: &Value, root: &Path) -> Value {
    let mut r = Results {
        root,
        out: Vec::new(),
        used: BTreeMap::new(),
        sources: BTreeMap::new(),
    };
    r.erosion(m);
    r.clones(m);
    r.findings(m, "verbosity_findings");
    r.findings(m, "test_value_findings");
    r.cycles(m, &modules(m));
    let rules: Vec<Value> = r
        .used
        .keys()
        .map(|id| {
            let (text, level) = describe(id);
            json!({"id": format!("emi/{id}"), "name": id, "shortDescription": {"text": text},
                   "defaultConfiguration": {"level": level}})
        })
        .collect();
    json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {"driver": {"name": "emi", "version": env!("CARGO_PKG_VERSION"),
                                "informationUri": "https://github.com/andrej/emi", "rules": rules}},
            "properties": {"measurementProfile": m["measurement_profile"]},
            "results": r.out,
        }]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding_log(dir: &Path, prefix_lines: usize, line: u64) -> Value {
        let text = format!("{}if (xs.length == 0) {{}}\n", "\n".repeat(prefix_lines));
        std::fs::write(dir.join("a.js"), text).unwrap_or_default();
        let m = json!({"measurement_profile": "p", "functions": [], "clone_groups": [], "clone_groups_normalized": [],
                       "static_dependency_cycles": [], "components": [],
                       "verbosity_findings": [{"file": "a.js", "rule": "waste-len-check", "lines": [line]}]});
        log(&m, dir)
    }

    #[test]
    fn findings_keep_their_fingerprint_when_code_moves() {
        let dir = std::env::temp_dir().join(format!("emi-sarif-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_default();
        let a = finding_log(&dir, 0, 1);
        let b = finding_log(&dir, 3, 4);
        let fp = |v: &Value| v["runs"][0]["results"][0]["partialFingerprints"].clone();
        assert_eq!(fp(&a), fp(&b));
        assert_eq!(
            b["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"]["startLine"],
            4
        );
        assert_eq!(
            a["runs"][0]["tool"]["driver"]["rules"][0]["id"],
            "emi/waste-len-check"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
