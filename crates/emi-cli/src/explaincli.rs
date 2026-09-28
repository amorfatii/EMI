//! `emi explain`: per-function graded erosion, what drives it, and the
//! small structural steps that lower it (the refactoring counterpart of
//! `emi scan --profile graded`).

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Args;
use emi_panel::Lang;
use emi_panel::explain::{Explained, explain};
use serde_json::{Value, json};

#[derive(Args)]
pub struct ExplainArgs {
    /// Source files or directories (vendored, build and VCS directories are skipped).
    #[arg(required = true)]
    paths: Vec<PathBuf>,
    /// Functions to show, highest graded erosion first (0: all).
    #[arg(long, default_value_t = 10)]
    top: usize,
    /// Only functions with this name.
    #[arg(long)]
    function: Option<String>,
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    format: String,
}

const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "vendor",
    "third_party",
    "target",
    "venv",
    ".venv",
    "__pycache__",
    "dist",
    "build",
    ".tox",
];

fn files(path: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    if path.is_file() {
        if Lang::of_path(&path.to_string_lossy()).is_some() {
            out.push(path.to_path_buf());
        }
        return Ok(());
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
        .with_context(|| format!("reading {}", path.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for p in entries {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if p.is_dir() && (SKIP_DIRS.contains(&name.as_str()) || name.starts_with('.')) {
            continue;
        }
        files(&p, out)?;
    }
    Ok(())
}

fn record(file: &str, e: &Explained) -> Value {
    let f = &e.function;
    json!({
        "file": file, "function": f.name, "start": f.start, "end": f.end, "sloc": f.sloc,
        "cognitive": f.cognitive, "halstead_volume": f.halstead_volume,
        "graded": {"total": e.mass.total, "complexity": e.mass.cognitive_part, "size": e.mass.volume_part},
        "drivers": {"structures": e.drivers.structures, "nesting": e.drivers.nesting,
                    "boolean_sequences": e.drivers.boolean_sequences,
                    "else_branches": e.drivers.else_branches, "jumps": e.drivers.jumps},
        "steps": e.steps.iter().map(|s| json!({"kind": s.kind, "lines": s.lines, "advice": s.advice,
                                                 "saves_cognitive": s.saves})).collect::<Vec<_>>(),
        "graded_after_steps": e.mass_after_steps,
        "cognitive_after_steps": e.cognitive_after_steps,
        "saturated": e.saturated,
    })
}

fn text(r: &Value) -> String {
    let d = &r["drivers"];
    let mut parts = vec![format!("{} structures", d["structures"])];
    for (key, label) in [
        ("nesting", "nesting"),
        ("boolean_sequences", "boolean"),
        ("else_branches", "else"),
        ("jumps", "jumps"),
    ] {
        if d[key].as_u64().unwrap_or(0) > 0 {
            parts.push(format!("{} {label}", d[key]));
        }
    }
    let g = &r["graded"];
    let mut out = format!(
        "{}:{} {}  graded {:.1} (complexity {:.1}, size {:.1})  cognitive {} = {}  sloc {}\n",
        r["file"].as_str().unwrap_or(""),
        r["start"],
        r["function"].as_str().unwrap_or(""),
        g["total"].as_f64().unwrap_or(0.0),
        g["complexity"].as_f64().unwrap_or(0.0),
        g["size"].as_f64().unwrap_or(0.0),
        r["cognitive"],
        parts.join(" + "),
        r["sloc"]
    );
    let steps = r["steps"].as_array().cloned().unwrap_or_default();
    for (i, s) in steps.iter().enumerate() {
        let lines: Vec<String> = s["lines"]
            .as_array()
            .into_iter()
            .flatten()
            .map(ToString::to_string)
            .collect();
        out += &format!(
            "  {}. {} (line {}; -{} cognitive): {}\n",
            i + 1,
            s["kind"].as_str().unwrap_or(""),
            lines.join(", "),
            s["saves_cognitive"],
            s["advice"].as_str().unwrap_or("")
        );
    }
    if !steps.is_empty() {
        out += &format!(
            "  after these steps: cognitive {} -> {}, graded {:.1} -> {:.1}{}\n",
            r["cognitive"],
            r["cognitive_after_steps"],
            g["total"].as_f64().unwrap_or(0.0),
            r["graded_after_steps"].as_f64().unwrap_or(0.0),
            if r["saturated"].as_bool().unwrap_or(false) {
                " (complexity part saturated: graded ranks this function but moves little until cognitive \
                 density falls below ~0.2 per line; track cognitive complexity)"
            } else {
                ""
            }
        );
    }
    out
}

pub fn run(args: &ExplainArgs) -> anyhow::Result<u8> {
    let mut paths = Vec::new();
    for p in &args.paths {
        files(p, &mut paths)?;
    }
    let mut rows = Vec::new();
    for path in &paths {
        let name = path.to_string_lossy().into_owned();
        let Some(lang) = Lang::of_path(&name) else {
            continue;
        };
        let src = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        for e in explain(lang, &src) {
            if args.function.as_ref().is_none_or(|n| *n == e.function.name) {
                rows.push(record(&name, &e));
            }
        }
    }
    rows.sort_by(|a, b| {
        b["graded"]["total"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&a["graded"]["total"].as_f64().unwrap_or(0.0))
    });
    if args.top > 0 {
        rows.truncate(args.top);
    }
    if args.format == "json" {
        println!("{}", Value::Array(rows));
    } else {
        for r in &rows {
            print!("{}", text(r));
        }
        if rows.is_empty() {
            println!("no functions found");
        } else {
            println!(
                "\ngraded = complexity + size. Graded picks what to refactor (it ranks bug-prone functions: \
                      emi-lab G2); cognitive complexity measures the refactor's progress. Splitting a function evenly \
                      changes neither."
            );
        }
    }
    Ok(0)
}
