//! Python adapter. Reproduces the region, SLOC and complexity semantics of
//! the v0.5 `python_probe` (profile `python-probe-0.12`) on ruff's parser.

mod builtins;
mod clones;
mod collect;
mod comments;
pub mod coupling;
pub mod dump;
pub mod exports;
mod lines;
pub mod pyast;
pub mod rules;
mod testvalue;

use std::collections::BTreeSet;

use emi_ir::regions::{self, Candidate, Region};
use emi_ir::{Evidence, Finding, FunctionRecord, RegionRecord, burden, pytext};
use ruff_python_ast::token::{Token, TokenKind};
use ruff_text_size::Ranged;

pub use crate::clones::CloneCandidate;
use crate::collect::Span;
use crate::lines::LineIndex;

/// Measurement profile of this adapter. It reproduces `python-probe-0.12`
/// region/SLOC/CC semantics; the parser pin is part of the identity.
pub const PROFILE: &str = "emi-python-0.1:parity-python-probe-0.12:ruff-parser-0.16.9";

/// Static coupling with value origins and resolved class ancestry; every
/// other dimension is measured as in [`PROFILE`].
pub const PRECISE_PROFILE: &str = "emi-python-0.1:precise-0.1:ruff-parser-0.16.9";

/// Result of measuring one Python file.
#[derive(Debug)]
pub struct FileMeasure {
    pub sloc: u32,
    pub erosion: f64,
    pub functions: Vec<FunctionRecord>,
    pub regions: Vec<RegionRecord>,
    /// Whole-body clone candidates in region order.
    pub clones: Vec<CloneCandidate>,
    /// Waste, masking and verbosity findings in the oracle's order.
    pub findings: Vec<Finding>,
    /// Eligible lines carrying V mass from scored rules and repeated lines.
    pub rule_lines: BTreeSet<u32>,
    /// Top-level declarations this module contributes to the catalog.
    pub catalog: coupling::CatalogPart,
}

#[derive(Debug)]
pub struct ParseFailure {
    pub error: &'static str,
    pub message: String,
}

fn skipped(token: &Token) -> bool {
    matches!(
        token.kind(),
        TokenKind::Comment
            | TokenKind::Newline
            | TokenKind::NonLogicalNewline
            | TokenKind::Indent
            | TokenKind::Dedent
            | TokenKind::EndOfFile
            | TokenKind::Semi
    )
}

/// Containment test for token spans against exclusion spans, as the oracle's
/// `_prepare_spans`/`_in_excluded`: sort by start, keep prefix-max ends.
struct Exclusions {
    starts: Vec<(u32, usize)>,
    max_ends: Vec<(u32, usize)>,
}

impl Exclusions {
    fn new(mut spans: Vec<Span>) -> Self {
        spans.sort_by_key(|s| s.0);
        let mut max_ends = Vec::with_capacity(spans.len());
        let mut current: Option<(u32, usize)> = None;
        for &(_, end) in &spans {
            let next = current.map_or(end, |c| c.max(end));
            max_ends.push(next);
            current = Some(next);
        }
        Exclusions {
            starts: spans.into_iter().map(|s| s.0).collect(),
            max_ends,
        }
    }

    fn contains(&self, start: (u32, usize), end: (u32, usize)) -> bool {
        let idx = self.starts.partition_point(|&s| s <= start);
        idx > 0 && end <= self.max_ends[idx - 1]
    }
}

fn eligible_lines(tokens: &[Token], index: &LineIndex, excluded: Vec<Span>) -> BTreeSet<u32> {
    let exclusions = Exclusions::new(excluded);
    let mut lines = BTreeSet::new();
    for token in tokens.iter().filter(|t| !skipped(t)) {
        let start = index.token_pos(token.start());
        let end = index.token_pos(token.end());
        if !exclusions.contains(start, end) {
            lines.extend(start.0..=end.0);
        }
    }
    lines
}

fn candidates(functions: &[collect::FunctionInfo]) -> Vec<Candidate> {
    functions
        .iter()
        .enumerate()
        .map(|(origin, f)| Candidate {
            key: (
                f.end_line - f.def_line,
                u64::from(f.depth),
                u64::from(f.offset),
            ),
            name: f.name.clone(),
            qname: f.qname.clone(),
            line: f.def_line,
            start: f.start_line,
            end: f.end_line,
            origin,
        })
        .collect()
}

/// Measure one file. `file` is the scope-relative posix path used in records.
pub fn measure(file: &str, source: &str) -> Result<FileMeasure, ParseFailure> {
    let parsed = ruff_python_parser::parse_module(source).map_err(|e| ParseFailure {
        error: "SyntaxError",
        message: e.to_string(),
    })?;
    let index = LineIndex::new(source);
    let collected = collect::collect(parsed.syntax(), parsed.tokens(), &index);
    let eligible = eligible_lines(parsed.tokens(), &index, collected.excluded);
    let regions = regions::assign(candidates(&collected.functions), &eligible, "<module>");
    let cc = regions::complexity(&regions, &collected.events);
    let mut measure = records(file, source, &eligible, &regions, &cc);
    measure.clones = clones::candidates(&regions, &collected.functions, parsed.tokens(), &index);
    measure.catalog = coupling::catalog_part(parsed.syntax());
    if !eligible.is_empty() {
        let hits = rules::findings(parsed.syntax(), parsed.tokens(), &index);
        (measure.findings, measure.rule_lines) =
            verbosity(file, source, &eligible, parsed.tokens(), &index, hits);
    }
    Ok(measure)
}

/// Findings in the oracle's order (comment bloat, commented code, repeated
/// lines, then rule hits) and the eligible lines they put V mass on.
fn verbosity(
    file: &str,
    source: &str,
    eligible: &BTreeSet<u32>,
    tokens: &ruff_python_ast::token::Tokens,
    index: &LineIndex,
    hits: Vec<rules::RuleHit>,
) -> (Vec<Finding>, BTreeSet<u32>) {
    let mut findings = Vec::new();
    let mut rule_lines = BTreeSet::new();
    let ordered = comments::comment_only_lines(source, tokens, index);
    let bloat = comments::comment_bloat(&ordered, eligible);
    if !bloat.flagged.is_empty() {
        let mut f = Finding::new(file, "comment-bloat", bloat.flagged);
        f.comment_lines = Some(bloat.comment_lines);
        f.header_free = Some(bloat.header_free);
        f.counted = Some(bloat.counted);
        f.allowed = Some(bloat.allowed);
        findings.push(f);
    }
    let dead = comments::commented_code(source, &ordered);
    if !dead.is_empty() {
        findings.push(Finding::new(file, "commented-code", dead));
    }
    let repeated = comments::repeated_lines(source, eligible);
    if !repeated.is_empty() {
        rule_lines.extend(repeated.iter().copied());
        findings.push(Finding::new(file, "rep-line", repeated));
    }
    for hit in hits {
        if rules::SCORING_RULES.contains(&hit.rule) {
            rule_lines.extend(hit.lines.iter().filter(|l| eligible.contains(l)).copied());
        }
        findings.push(Finding::new(file, hit.rule, hit.lines));
    }
    (findings, rule_lines)
}

/// `(kind, lineno, col)` of every positioned node in `ast.walk` order, for
/// checking the CPython-shaped walk against CPython itself.
pub fn walk_positions(source: &str) -> Result<Vec<(&'static str, u32, usize)>, ParseFailure> {
    let parsed = ruff_python_parser::parse_module(source).map_err(|e| ParseFailure {
        error: "SyntaxError",
        message: e.to_string(),
    })?;
    let index = LineIndex::new(source);
    let positions = pyast::Positions::new(&index, parsed.tokens());
    let nodes = pyast::bfs(pyast::PyNode::Module(parsed.syntax()));
    let parens: std::collections::HashMap<*const ruff_python_ast::Expr, _> =
        pyast::generator_parens(&nodes).into_iter().collect();
    Ok(nodes
        .into_iter()
        .filter_map(|n| {
            let paren = match n {
                pyast::PyNode::Expr(e) => parens.get(&std::ptr::from_ref(e)).copied(),
                _ => None,
            };
            let (line, col) = positions.lineno_col(n, paren)?;
            let kind = pyast::kind_name(n);
            (!matches!(
                kind,
                "Constant" | "JoinedStr" | "TemplateStr" | "FormattedValue"
            ))
            .then_some((kind, line, col))
        })
        .collect())
}

/// Test-value findings of a test file (`empty-test`, `constant-assert`).
/// Test files are not product source; only these diagnostics are kept.
pub fn measure_test(file: &str, source: &str) -> Result<Vec<Finding>, ParseFailure> {
    let parsed = ruff_python_parser::parse_module(source).map_err(|e| ParseFailure {
        error: "SyntaxError",
        message: e.to_string(),
    })?;
    let index = LineIndex::new(source);
    Ok(testvalue::findings(
        file,
        parsed.syntax(),
        parsed.tokens(),
        &index,
    ))
}

/// Coupling of one module, given scope-wide knowledge from the first pass.
pub struct FileCoupling {
    /// First-party modules this module imports (the import graph edges).
    pub import_deps: BTreeSet<String>,
    /// Per region, in the same order as `FileMeasure::regions`.
    pub regions: Vec<coupling::RegionCoupling>,
}

/// Second pass: resolve imports and per-region dependencies against the
/// project catalog. Re-parses the source; parsing is cheap next to holding
/// every syntax tree of a scope in memory between passes.
pub fn couple(
    source: &str,
    module: &str,
    is_package: bool,
    project: &coupling::Project,
) -> Result<FileCoupling, ParseFailure> {
    let parsed = ruff_python_parser::parse_module(source).map_err(|e| ParseFailure {
        error: "SyntaxError",
        message: e.to_string(),
    })?;
    let index = LineIndex::new(source);
    let collected = collect::collect(parsed.syntax(), parsed.tokens(), &index);
    let eligible = eligible_lines(parsed.tokens(), &index, collected.excluded);
    let regions = regions::assign(candidates(&collected.functions), &eligible, "<module>");
    let imports = coupling::resolve_imports(parsed.syntax(), module, is_package, project);
    let (models, contexts) =
        coupling::class_models(parsed.syntax(), &imports.aliases, module, project);
    let positions = pyast::Positions::new(&index, parsed.tokens());
    let module_env = project
        .precise
        .then(|| coupling::module_env(parsed.syntax(), &imports.aliases, module, project));
    let envs = module_env
        .as_ref()
        .map(|env| coupling::function_envs(parsed.syntax(), env, module, project));
    let ctx = coupling::ModuleContext {
        envs: envs.as_ref(),
        module_env: module_env.as_ref(),
        module,
        imports: &imports,
        project,
        positions: &positions,
        models: &models,
    };
    let per_region = regions
        .iter()
        .map(|region| {
            let (root, class) = match region.origin {
                Some(i) => {
                    let f = &collected.functions[i];
                    let class = contexts
                        .get(&std::ptr::from_ref(f.node))
                        .map(|&m| &models[m]);
                    (pyast::PyNode::Stmt(f.stmt), class)
                }
                None => (pyast::PyNode::Module(parsed.syntax()), None),
            };
            let mut rc = coupling::region_coupling(&ctx, root, &region.lines, class);
            if region.origin.is_none() {
                rc.deps.extend(imports.dependencies.iter().cloned());
            }
            rc
        })
        .collect();
    Ok(FileCoupling {
        import_deps: imports.dependencies.clone(),
        regions: per_region,
    })
}

/// `(def line, exact, normalized)` fingerprint of one function body.
pub type BodyFingerprint = (u32, [u8; 32], [u8; 32]);

/// `(def line, exact, normalized)` body fingerprints of every function,
/// ungated. Used to check fingerprint equivalence against CPython.
pub fn all_fingerprints(source: &str) -> Result<Vec<BodyFingerprint>, ParseFailure> {
    let parsed = ruff_python_parser::parse_module(source).map_err(|e| ParseFailure {
        error: "SyntaxError",
        message: e.to_string(),
    })?;
    let index = LineIndex::new(source);
    let collected = collect::collect(parsed.syntax(), parsed.tokens(), &index);
    Ok(collected
        .functions
        .iter()
        .map(|f| {
            let body = clones::body_without_docstring(&f.node.body);
            (
                f.def_line,
                dump::fingerprint(body, false),
                dump::fingerprint(body, true),
            )
        })
        .collect())
}

fn records(
    file: &str,
    source: &str,
    eligible: &BTreeSet<u32>,
    regions: &[Region],
    cc: &[u32],
) -> FileMeasure {
    let src_lines = pytext::splitlines(source);
    let mut erosion = 0.0;
    let mut functions = Vec::with_capacity(regions.len());
    let mut inventory = Vec::with_capacity(regions.len());
    for (region, &cc) in regions.iter().zip(cc) {
        let sloc = u32::try_from(region.lines.len()).unwrap_or(u32::MAX);
        let mass = burden::erosion_mass(sloc, cc);
        erosion += mass;
        inventory.push(RegionRecord {
            file: file.to_owned(),
            id: region.qname.clone(),
            sloc,
            sha: regions::content_sha(&src_lines, &region.lines),
            erosion: Evidence::Exact(mass),
            in_cycle: None,
            coupling: None,
        });
        functions.push(FunctionRecord {
            file: file.to_owned(),
            function: region.name.clone(),
            line: region.line,
            sloc,
            cc: Some(cc),
            erosion_mass: Some(mass),
            erosion_exact: None,
            lines: None,
            coupling: None,
        });
    }
    FileMeasure {
        sloc: u32::try_from(eligible.len()).unwrap_or(u32::MAX),
        erosion,
        functions,
        regions: inventory,
        clones: Vec::new(),
        findings: Vec::new(),
        rule_lines: BTreeSet::new(),
        catalog: coupling::CatalogPart::default(),
    }
}

#[cfg(test)]
mod tests;
