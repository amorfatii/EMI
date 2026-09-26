//! JavaScript/TypeScript scope assembly, following `js_probe.collect`.

use std::fs;
use std::path::Path;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use emi_ir::clones::Grouper;
use emi_ir::graph::strongly_connected;
use emi_ir::{Burden, Component, FileError, Measurement, UnresolvedCall, pytext};
use emi_lang_js::FileMeasure;
use emi_lang_js::resolve::{self, Scope};

use crate::coupling::{RegionEvidence, apply};
use rayon::prelude::*;

use crate::walk::{self, ScopeFile};
use crate::{OnSyntaxError, Options, ScanError, is_symlink, truncate};

pub const DEFAULT_EXCLUDES: [&str; 14] = [
    "__pycache__",
    ".git",
    ".hg",
    ".svn",
    ".tox",
    ".venv",
    "venv",
    "node_modules",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    "dist",
    "build",
    "coverage",
];

const CODE_EXTENSIONS: [&str; 8] = [".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"];

fn is_code(name: &str) -> bool {
    let lower = name.to_lowercase();
    CODE_EXTENSIONS.iter().any(|e| lower.ends_with(e))
}

enum Outcome {
    Symlink,
    Unreadable(&'static str, String),
    Parsed {
        sha: String,
        measure: FileMeasure,
        source: String,
    },
    Unparsed,
}

fn measure_file(f: &ScopeFile) -> Outcome {
    if is_symlink(&f.path) {
        return Outcome::Symlink;
    }
    let raw = match fs::read(&f.path) {
        Ok(raw) => raw,
        Err(e) => return Outcome::Unreadable("OSError", truncate(&crate::os_error(&e, &f.path))),
    };
    let text = match String::from_utf8(raw) {
        Ok(text) => text,
        Err(e) => {
            let message = pytext::utf8_error(e.as_bytes(), &e.utf8_error());
            return Outcome::Unreadable("UnicodeDecodeError", truncate(&message));
        }
    };
    let source = pytext::universal_newlines(&text);
    let sha = pytext::sha256_hex(source.as_bytes());
    match emi_lang_js::measure(&f.rel, &source) {
        Some(measure) => Outcome::Parsed {
            sha,
            measure,
            source,
        },
        None => Outcome::Unparsed,
    }
}

fn file_error(file: &str, error: &str, message: String, lines: Option<Vec<u32>>) -> FileError {
    FileError {
        file: file.to_owned(),
        stage: None,
        error: error.to_owned(),
        message: Some(message),
        error_lines: lines,
        sha256: None,
    }
}

fn select(
    root: &Path,
    options: &Options,
    excludes: &[String],
    out: &mut Measurement,
) -> Vec<ScopeFile> {
    let (candidates, _pruned, walk_errors) = walk::walk(root, excludes, is_code);
    out.errors.extend(
        walk_errors
            .into_iter()
            .map(|e| file_error(&e.rel, "ScanError", truncate(&e.message), None)),
    );
    candidates
        .into_iter()
        .filter(|f| fs::metadata(&f.path).is_ok_and(|m| m.is_file()))
        .filter(|f| walk::globs_keep(&f.rel, &options.exclude_globs, &options.include_globs))
        .collect()
}

/// A parsed file waiting for the coupling pass.
struct Parsed {
    rel: String,
    measure: FileMeasure,
    source: String,
}

/// Record syntax errors: abort in raise mode, keep the affected lines otherwise.
fn syntax_errors(
    out: &mut Measurement,
    rel: &str,
    measure: &FileMeasure,
    raise: bool,
) -> Result<(), ScanError> {
    if measure.error_lines.is_empty() {
        return Ok(());
    }
    let rows: Vec<u32> = measure.error_lines.iter().copied().collect();
    if raise {
        return Err(ScanError::Syntax {
            file: rel.to_owned(),
            message: format!("rows {rows:?}"),
        });
    }
    let message = "best-effort measurement; regions on error lines are unknown".to_owned();
    out.errors
        .push(file_error(rel, "SyntaxError", message, Some(rows)));
    Ok(())
}

/// Resolution knowledge of the scope: every parsed file is first-party.
fn scope_of(
    parsed: &[Parsed],
    resolver: resolve::Resolver,
    mains: BTreeMap<String, String>,
) -> Scope {
    let known_files: HashMap<String, String> = parsed
        .iter()
        .map(|p| (p.rel.clone(), emi_lang_js::module_name(&p.rel)))
        .collect();
    let known_modules = known_files.values().cloned().collect();
    Scope {
        known_files,
        known_modules,
        resolver,
        mains,
    }
}

/// `_config_error_entries`: every resolver diagnostic but the nested-config
/// advisory fails collection (missing evidence, not clean measurement).
fn config_errors(resolver: &resolve::Resolver) -> impl Iterator<Item = FileError> + '_ {
    resolver
        .diagnostics
        .iter()
        .filter(|(_, code, _)| code != "nested-config-unsupported")
        .map(|(file, code, message)| {
            file_error(
                file,
                "ConfigError",
                truncate(&format!("{code}: {message}")),
                None,
            )
        })
}

/// Pinned by the oracle's resolver contract (`js_probe.RESOLVER_MODE`).
const RESOLVER_MODE: &str = "emi-static-0.10";

/// `_measurement_options`: the extraction contract compared across revisions.
fn measurement_options(
    options: &Options,
    excludes: &[String],
    resolver: &resolve::Resolver,
) -> serde_json::Map<String, serde_json::Value> {
    let value = serde_json::json!({
        "adapter": emi_lang_js::PROFILE,
        "engine": crate::ENGINE,
        "parsers": {"tree-sitter": "0.26.2", "tree-sitter-javascript": "0.25.0",
                    "tree-sitter-typescript": "0.23.2"},
        "resolver_mode": RESOLVER_MODE,
        "on_syntax_error": crate::syntax_policy(options),
        "exclude_dirs": crate::sorted(excludes),
        "exclude_globs": crate::sorted(&options.exclude_globs),
        "include_globs": crate::sorted(&options.include_globs),
        "scope_prefix": resolver.scope_prefix.clone().unwrap_or_default(),
    });
    match value {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}

fn evidence(fc: &emi_lang_js::FileCoupling) -> Vec<RegionEvidence> {
    fc.regions
        .iter()
        .map(|rc| RegionEvidence {
            deps: rc.deps.iter().cloned().collect(),
            unresolved: rc
                .unresolved
                .iter()
                .map(|u| UnresolvedCall {
                    line: Some(u.line),
                    column: Some(u.column),
                    reason: u.reason.to_owned(),
                })
                .collect(),
            local_callees: rc.local_callees.clone(),
        })
        .collect()
}

/// Couple every parsed file, find cycles, and assemble records in file order.
fn assemble(out: &mut Measurement, parsed: Vec<Parsed>, scope: &Scope) -> Result<(), ScanError> {
    let couplings = parsed
        .par_iter()
        .map(|p| {
            emi_lang_js::couple(&p.rel, &p.source, scope).ok_or_else(|| ScanError::Syntax {
                file: p.rel.clone(),
                message: "parser unavailable".into(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let graph: BTreeMap<String, BTreeSet<String>> = parsed
        .iter()
        .zip(&couplings)
        .map(|(p, c)| (emi_lang_js::module_name(&p.rel), c.graph.clone()))
        .collect();
    out.static_dependency_cycles = strongly_connected(&graph);
    let cycle_modules: HashSet<&String> = out.static_dependency_cycles.iter().flatten().collect();
    let cycle_modules: HashSet<String> = cycle_modules.into_iter().cloned().collect();
    let (mut exact, mut normalized) = (Grouper::default(), Grouper::default());
    let mut rule_lines: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    for (p, fc) in parsed.into_iter().zip(&couplings) {
        let mut m = p.measure;
        for c in &m.clones {
            exact.add(c.exact, &p.rel, &c.body_lines);
            normalized.add(c.normalized, &p.rel, &c.body_lines);
        }
        out.verbosity_findings.append(&mut m.findings);
        rule_lines.insert(p.rel.clone(), std::mem::take(&mut m.rule_lines));
        if m.sloc == 0 {
            continue;
        }
        let in_cycle = cycle_modules.contains(&emi_lang_js::module_name(&p.rel));
        let c = apply(&mut m.functions, &mut m.regions, &evidence(fc), in_cycle);
        out.components.push(Component {
            id: p.rel.clone(),
            sloc: m.sloc,
            burden: Burden {
                erosion: m.erosion,
                redundancy: None,
                coupling: Some(c),
            },
        });
        out.functions.append(&mut m.functions);
        out.regions.append(&mut m.regions);
    }
    let mut clone_lines = BTreeMap::new();
    out.clone_groups = exact.finish(&mut clone_lines);
    out.clone_groups_normalized = normalized.finish(&mut clone_lines);
    let empty = BTreeSet::new();
    for c in &mut out.components {
        let clones = clone_lines.get(&c.id).unwrap_or(&empty);
        let rules = rule_lines.get(&c.id).unwrap_or(&empty);
        // Unlike the Python adapter, JS redundancy is not capped at size.
        c.burden.redundancy = Some(u32::try_from(clones.union(rules).count()).unwrap_or(u32::MAX));
    }
    Ok(())
}

/// Read and parse every selected file; failures become errors (or abort in
/// raise mode). Returns the parsed files and the grammar fallbacks.
fn read_all(
    selected: &[ScopeFile],
    raise: bool,
    out: &mut Measurement,
) -> Result<(Vec<Parsed>, Vec<String>), ScanError> {
    let outcomes: Vec<Outcome> = selected.par_iter().map(measure_file).collect();
    let (mut fallbacks, mut parsed) = (Vec::new(), Vec::new());
    for (f, outcome) in selected.iter().zip(outcomes) {
        let rel = f.rel.clone();
        match outcome {
            Outcome::Symlink if raise => return Err(ScanError::Symlink(rel)),
            Outcome::Unreadable(_, message) if raise => {
                return Err(ScanError::Read { file: rel, message });
            }
            Outcome::Symlink => {
                let message = "symlinked source is unsupported".to_owned();
                out.errors
                    .push(file_error(&rel, "SymlinkError", message, None));
            }
            Outcome::Unreadable(name, message) => {
                out.errors.push(file_error(&rel, name, message, None))
            }
            Outcome::Unparsed => {
                return Err(ScanError::Syntax {
                    file: rel,
                    message: "parser unavailable".into(),
                });
            }
            Outcome::Parsed {
                sha,
                measure,
                source,
            } => {
                out.source_sha256.insert(rel.clone(), sha);
                if measure.fallback {
                    fallbacks.push(rel.clone());
                }
                syntax_errors(out, &rel, &measure, raise)?;
                parsed.push(Parsed {
                    rel,
                    measure,
                    source,
                });
            }
        }
    }
    Ok((parsed, fallbacks))
}

pub fn scan(root: &Path, options: &Options) -> Result<Measurement, ScanError> {
    let mut out = Measurement {
        measurement_profile: emi_lang_js::PROFILE.to_owned(),
        ..Measurement::default()
    };
    let excludes = options
        .exclude_dirs
        .clone()
        .unwrap_or_else(|| DEFAULT_EXCLUDES.iter().map(|s| (*s).to_owned()).collect());
    // Configuration comes first, as in the oracle: its failures are errors
    // of this collection whether or not any source parses.
    let config_dir = options.config_root.as_deref().unwrap_or(root);
    let (resolver, mains) = resolve::load(root, config_dir, &excludes);
    out.errors.extend(config_errors(&resolver));
    out.measurement_options = Some(measurement_options(options, &excludes, &resolver));
    let selected = select(root, options, &excludes, &mut out);
    if selected.is_empty() {
        return Err(ScanError::empty("no JavaScript/TypeScript source files"));
    }
    let raise = options.on_syntax_error == OnSyntaxError::Raise;
    let (parsed, mut fallbacks) = read_all(&selected, raise, &mut out)?;
    let all_failed = parsed.is_empty();
    if all_failed && (raise || out.errors.is_empty()) {
        return Err(ScanError::unmeasurable(
            "no measurable JavaScript/TypeScript source files",
        ));
    }
    if !all_failed {
        let scope = scope_of(&parsed, resolver, mains);
        assemble(&mut out, parsed, &scope)?;
        if out.components.is_empty() {
            return Err(ScanError::unmeasurable(
                "no measurable JavaScript/TypeScript source files",
            ));
        }
    }
    out.errors
        .sort_by(|a, b| (&a.file, &a.error, &a.message).cmp(&(&b.file, &b.error, &b.message)));
    fallbacks.sort();
    out.fallbacks = Some(fallbacks);
    Ok(out)
}
