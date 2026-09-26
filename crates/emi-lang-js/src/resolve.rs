//! Static module resolution of `js_probe` (resolver mode `emi-static-0.10`):
//! the root tsconfig with its `extends` chain, package.json `main` fields,
//! and specifier resolution to a first-party module, external, or unknown.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::paths;
use emi_ir::pyrepr;

pub const CODE_EXTENSIONS: [&str; 8] =
    [".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"];

/// Where a specifier leads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Module(String),
    External,
}

#[derive(Clone, Debug, Default)]
pub struct Resolver {
    pub prefix: Vec<(String, Vec<String>)>,
    pub exact: HashMap<String, Vec<String>>,
    pub base_url: Option<String>,
    /// Scope location relative to the config root; `None` when outside it.
    pub scope_prefix: Option<String>,
    pub maps_usable: bool,
    pub strict_external: bool,
    /// absent | ok | degraded | invalid
    pub status: String,
    pub diagnostics: Vec<(String, String, String)>,
    pub files: Vec<String>,
}

/// Strip `//` and `/* */` comments outside strings, keeping line breaks.
fn strip_comments(text: &str) -> String {
    let c: Vec<char> = text.chars().collect();
    let (mut out, mut i, mut string) = (String::new(), 0, None::<char>);
    while i < c.len() {
        let ch = c[i];
        if let Some(q) = string {
            out.push(ch);
            if ch == '\\' && i + 1 < c.len() {
                out.push(c[i + 1]);
                i += 2;
                continue;
            }
            if ch == q {
                string = None;
            }
        } else if ch == '"' || ch == '\'' {
            string = Some(ch);
            out.push(ch);
        } else if ch == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
            out.push('\n');
            i += 1;
            continue;
        } else if ch == '/' && c.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < c.len() && !(c[i] == '*' && c[i + 1] == '/') {
                if c[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i += 2;
            out.push(' ');
            continue;
        } else {
            out.push(ch);
        }
        i += 1;
    }
    out
}

/// Remove `,` directly before `}` or `]` outside strings.
fn strip_trailing_commas(text: &str) -> String {
    let c: Vec<char> = text.chars().collect();
    let (mut out, mut i, mut string) = (String::new(), 0, None::<char>);
    while i < c.len() {
        let ch = c[i];
        if let Some(q) = string {
            out.push(ch);
            if ch == '\\' && i + 1 < c.len() {
                out.push(c[i + 1]);
                i += 2;
                continue;
            }
            if ch == q {
                string = None;
            }
        } else if ch == '"' || ch == '\'' {
            string = Some(ch);
            out.push(ch);
        } else if ch == ',' {
            let mut j = i + 1;
            while j < c.len() && matches!(c[j], ' ' | '\t' | '\n' | '\r') {
                j += 1;
            }
            if !(j < c.len() && matches!(c[j], '}' | ']')) {
                out.push(ch);
            }
        } else {
            out.push(ch);
        }
        i += 1;
    }
    out
}

enum ConfigRead {
    Absent,
    Unreadable,
    Malformed,
    Ok(serde_json::Map<String, Value>, PathBuf),
}

fn read_config(path: &Path) -> ConfigRead {
    if !path.exists() && path.symlink_metadata().is_err() {
        return ConfigRead::Absent;
    }
    let Ok(raw) = fs::read_to_string(path) else {
        return ConfigRead::Unreadable;
    };
    let text = emi_ir::pytext::universal_newlines(&raw);
    match serde_json::from_str::<Value>(&strip_trailing_commas(&strip_comments(&text))) {
        Ok(Value::Object(map)) => {
            let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            ConfigRead::Ok(map, resolved)
        }
        _ => ConfigRead::Malformed,
    }
}

fn canon(path: &Path) -> String {
    let real = fs::canonicalize(path)
        .unwrap_or_else(|_| std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
    paths::normpath(&real.to_string_lossy())
}

fn is_bare_extends(entry: &str) -> bool {
    !(entry.starts_with('.') || entry.starts_with('/'))
}

/// `PurePosixPath(text).parts`: the anchor, then components without
/// empty or `.` entries (`..` is kept; pathlib never collapses it).
fn pure_parts(text: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = Vec::new();
    if text.starts_with('/') {
        parts.push("/");
    }
    parts.extend(text.split('/').filter(|c| !c.is_empty() && *c != "."));
    parts
}

/// Oracle `_rel`: lexical `relative_to(root)`, else the canonical path.
fn rel_display(path: &str, root: &Path) -> String {
    let root_text = root.to_string_lossy();
    let (p, r) = (pure_parts(path), pure_parts(&root_text));
    if p.len() >= r.len() && p[..r.len()] == r[..] {
        let rest = &p[r.len()..];
        return if rest.is_empty() {
            ".".to_owned()
        } else {
            rest.join("/")
        };
    }
    canon(Path::new(path))
}

/// `PurePosixPath(a, b)` as text.
fn pure_join(a: &str, b: &str) -> String {
    let joined = paths::join(a, b);
    let parts = pure_parts(&joined);
    match parts.split_first() {
        Some((&"/", rest)) => format!("/{}", rest.join("/")),
        _ if parts.is_empty() => ".".to_owned(),
        _ => parts.join("/"),
    }
}

struct Chain<'a> {
    root: &'a Path,
    levels: Vec<(serde_json::Map<String, Value>, String)>,
    degraded: bool,
    files: Vec<String>,
    diagnostics: Vec<(String, String, String)>,
}

impl Chain<'_> {
    fn note(&mut self, code: &str, path: &str, message: String, severe: bool) {
        self.diagnostics
            .push((rel_display(path, self.root), code.to_owned(), message));
        self.degraded |= severe;
    }

    /// Read one `extends` parent, noting why it is skipped when it is.
    fn read_parent(
        &mut self,
        candidate: &str,
        shown: &str,
    ) -> Option<(serde_json::Map<String, Value>, PathBuf)> {
        let (code, state) = match read_config(Path::new(candidate)) {
            ConfigRead::Ok(parent, resolved) => return Some((parent, resolved)),
            ConfigRead::Absent => ("missing-parent", "not found"),
            ConfigRead::Unreadable => ("unreadable-parent", "unreadable"),
            ConfigRead::Malformed => ("malformed-parent", "malformed"),
        };
        let message = format!("extends target {shown} {state}; parent skipped");
        self.note(code, candidate, message, true);
        None
    }

    /// Depth-first `extends` merge in listed order; a true cycle stops.
    fn visit(
        &mut self,
        options: &serde_json::Map<String, Value>,
        owner_dir: &str,
        stack: &mut HashSet<String>,
    ) {
        let entries: Vec<Value> = match options.get("extends") {
            None | Some(Value::Null) => return,
            Some(Value::String(s)) => vec![Value::String(s.clone())],
            Some(Value::Array(a)) => a.clone(),
            Some(other) => {
                let message = format!(
                    "extends must be a string or list, got {}",
                    pyrepr::type_name(other)
                );
                self.note(
                    "invalid-extends",
                    &pure_join(owner_dir, "tsconfig.json"),
                    message,
                    true,
                );
                return;
            }
        };
        for value in entries {
            let Some(entry) = value.as_str().filter(|s| !s.is_empty()) else {
                let message = format!(
                    "extends entries must be nonempty strings, got {}",
                    pyrepr::value(&value)
                );
                self.note(
                    "invalid-extends",
                    &pure_join(owner_dir, "tsconfig.json"),
                    message,
                    true,
                );
                continue;
            };
            let shown = pyrepr::string(entry);
            if is_bare_extends(entry) {
                let message = format!(
                    "extends {shown} is package-style; node_modules lookup is unsupported, parent skipped"
                );
                self.note(
                    "package-extends-unsupported",
                    &pure_join(owner_dir, entry),
                    message,
                    true,
                );
                continue;
            }
            let mut candidate = pure_join(owner_dir, entry);
            if paths::suffix(paths::name(&candidate)).is_empty() {
                candidate.push_str(".json");
            }
            let key = canon(Path::new(&candidate));
            if stack.contains(&key) {
                let message = format!("extends cycle at {shown}; repeated file skipped");
                self.note("circular-extends", &candidate, message, true);
                continue;
            }
            let Some((parent, resolved)) = self.read_parent(&candidate, &shown) else {
                continue;
            };
            self.files.push(candidate.clone());
            let parent_dir = resolved
                .parent()
                .map_or_else(String::new, |p| p.to_string_lossy().into_owned());
            stack.insert(key.clone());
            self.visit(&parent, &parent_dir, stack);
            stack.remove(&key);
            self.levels.push((parent, parent_dir));
        }
    }

    /// Later levels win for `baseUrl` and `paths`; returns (baseUrl as an
    /// absolute path, `paths`, the directory that declared `paths`).
    fn merge(&mut self) -> MergedOptions {
        let mut merged = MergedOptions::default();
        let levels = std::mem::take(&mut self.levels);
        for (options, owner) in &levels {
            let Some(Value::Object(compiler)) = options.get("compilerOptions") else {
                continue;
            };
            let owner_file = pure_join(owner, "tsconfig.json");
            match compiler.get("baseUrl") {
                Some(Value::String(b)) => {
                    merged.base_abs = Some(paths::normpath(&paths::join(owner, b)))
                }
                Some(other) => {
                    let message = format!(
                        "baseUrl must be a string, got {}; option ignored",
                        pyrepr::type_name(other)
                    );
                    self.note("invalid-option", &owner_file, message, true);
                }
                None => {}
            }
            match compiler.get("paths") {
                Some(Value::Object(p)) => {
                    merged.paths = Some(p.clone());
                    merged.owner = Some(owner.clone());
                }
                Some(other) => {
                    let message = format!(
                        "paths must be an object, got {}; option ignored",
                        pyrepr::type_name(other)
                    );
                    self.note("invalid-option", &owner_file, message, true);
                }
                None => {}
            }
        }
        merged
    }
}

#[derive(Default)]
struct MergedOptions {
    base_abs: Option<String>,
    paths: Option<serde_json::Map<String, Value>>,
    owner: Option<String>,
}

fn invalid_root(mut r: Resolver, config_dir: &Path, status: &str) -> Resolver {
    let file = rel_display(
        &pure_join(&config_dir.to_string_lossy(), "tsconfig.json"),
        config_dir,
    );
    let code = if status == "unreadable" {
        "root-unreadable"
    } else {
        "root-malformed"
    };
    let message = format!("tsconfig.json {status}; tsconfig mappings unavailable");
    r.diagnostics.push((file, code.to_owned(), message));
    r.status = "invalid".into();
    r.strict_external = false;
    r
}

/// `_load_tsconfig_paths`: baseUrl and `paths` of the root tsconfig chain.
fn load_tsconfig(config_dir: &Path) -> Resolver {
    let r = Resolver {
        maps_usable: true,
        strict_external: true,
        status: "absent".into(),
        scope_prefix: Some(String::new()),
        ..Resolver::default()
    };
    let root_file = config_dir.join("tsconfig.json");
    let (config, resolved) = match read_config(&root_file) {
        ConfigRead::Absent => return r,
        ConfigRead::Ok(c, p) => (c, p),
        ConfigRead::Unreadable => return invalid_root(r, config_dir, "unreadable"),
        ConfigRead::Malformed => return invalid_root(r, config_dir, "malformed"),
    };
    let root_dir = resolved
        .parent()
        .map_or_else(String::new, |p| p.to_string_lossy().into_owned());
    let mut chain = Chain {
        root: config_dir,
        levels: Vec::new(),
        degraded: false,
        files: vec![root_file.to_string_lossy().into_owned()],
        diagnostics: Vec::new(),
    };
    let mut stack = HashSet::from([canon(&root_file)]);
    chain.visit(&config, &root_dir, &mut stack);
    chain.levels.push((config, root_dir.clone()));
    let merged = chain.merge();
    finish_paths(r, chain, merged, &root_dir, config_dir)
}

fn finish_paths(
    mut r: Resolver,
    mut chain: Chain,
    merged: MergedOptions,
    root_dir: &str,
    config_dir: &Path,
) -> Resolver {
    let anchor = merged
        .base_abs
        .clone()
        .or_else(|| merged.owner.clone())
        .unwrap_or_else(|| root_dir.to_owned());
    r.base_url = merged
        .base_abs
        .as_deref()
        .map(|b| paths::relpath(b, root_dir));
    let owner_file = pure_join(
        &merged
            .owner
            .clone()
            .unwrap_or_else(|| config_dir.to_string_lossy().into_owned()),
        "tsconfig.json",
    );
    for (key, targets) in merged.paths.unwrap_or_default() {
        let Value::Array(targets) = targets else {
            let message = format!(
                "paths entry {} must map to a string array; skipped",
                pyrepr::string(&key)
            );
            chain.note("invalid-option", &owner_file, message, true);
            continue;
        };
        let kept: Vec<&str> = targets.iter().filter_map(Value::as_str).collect();
        if kept.len() != targets.len() {
            let message = format!(
                "paths entry {} has non-string targets; kept {} of {}",
                pyrepr::string(&key),
                kept.len(),
                targets.len()
            );
            chain.note("invalid-option", &owner_file, message, false);
        }
        let rebased: Vec<String> = kept
            .iter()
            .map(|t| paths::relpath(&paths::normpath(&paths::join(&anchor, t)), root_dir))
            .collect();
        match key.strip_suffix('*') {
            Some(p) => r.prefix.push((p.to_owned(), rebased)),
            None => {
                r.exact.insert(key, rebased);
            }
        }
    }
    r.status = if chain.degraded { "degraded" } else { "ok" }.into();
    r.strict_external = !chain.degraded;
    r.files = chain.files;
    r.diagnostics = chain.diagnostics;
    r
}

fn excluded_part(part: &str, excludes: &[String]) -> bool {
    excludes.iter().any(|e| e == part) || part.ends_with(".egg-info")
}

/// Files under `root` (symlinked dirs not followed; excluded names pruned)
/// whose name satisfies `wants`, as sorted relative posix paths.
fn find_files(
    root: &Path,
    excludes: &[String],
    prune: bool,
    wants: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if kind.is_dir() {
                if !(prune && excluded_part(&name, excludes)) {
                    stack.push(path);
                }
            } else if wants(&name) {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                let parent = paths::parent(&rel);
                if parent == "." || !parent.split('/').any(|p| excluded_part(p, excludes)) {
                    out.push(rel);
                }
            }
        }
    }
    out.sort_by(|a, b| a.split('/').cmp(b.split('/')));
    out
}

/// `_config_display_path`: base-relative when inside, else the real path.
fn config_display_path(path: &Path, base: &Path) -> String {
    let (p, b) = (path.to_string_lossy(), base.to_string_lossy());
    let (pp, bp) = (pure_parts(&p), pure_parts(&b));
    if pp.len() >= bp.len() && pp[..bp.len()] == bp[..] {
        let rest = &pp[bp.len()..];
        return if rest.is_empty() {
            ".".to_owned()
        } else {
            rest.join("/")
        };
    }
    fs::canonicalize(path).map_or_else(|_| p.into_owned(), |c| c.to_string_lossy().into_owned())
}

/// `load_resolver_config`: tsconfig at `config_dir`, scope containment, and
/// the nested-config advisory that makes bare misses unknown.
pub fn load(
    scope_root: &Path,
    config_dir: &Path,
    excludes: &[String],
) -> (Resolver, BTreeMap<String, String>) {
    let mut r = load_tsconfig(config_dir);
    let scope = fs::canonicalize(scope_root).unwrap_or_else(|_| scope_root.to_path_buf());
    let config = fs::canonicalize(config_dir).unwrap_or_else(|_| config_dir.to_path_buf());
    match scope.strip_prefix(&config) {
        Ok(rel) => {
            let rel = rel.to_string_lossy().replace('\\', "/");
            r.scope_prefix = Some(if rel.is_empty() || rel == "." {
                String::new()
            } else {
                rel
            });
        }
        Err(_) => {
            r.scope_prefix = None;
            r.maps_usable = false;
            r.strict_external = false;
            r.diagnostics.push((
                config_display_path(scope_root, config_dir),
                "scope-outside-config-root".into(),
                "measured scope is not inside the configuration root; tsconfig mappings disabled, bare specifiers stay unknown".into(),
            ));
            if r.status == "ok" {
                r.status = "degraded".into();
            }
        }
    }
    let chain: HashSet<String> = r.files.iter().map(|f| canon(Path::new(f))).collect();
    let nested = find_files(scope_root, excludes, true, |n| {
        crate::fnmatch_simple(n, "tsconfig", ".json")
    })
    .into_iter()
    .any(|rel| !chain.contains(&canon(&scope_root.join(&rel))));
    if nested && r.status == "absent" {
        r.status = "degraded".into();
        r.strict_external = false;
    }
    (r, package_mains(scope_root, excludes))
}

/// Directory (posix, "." for the root) -> package.json `main`.
fn package_mains(root: &Path, excludes: &[String]) -> BTreeMap<String, String> {
    let mut mains = BTreeMap::new();
    for rel in find_files(root, excludes, false, |n| n == "package.json") {
        let Ok(raw) = fs::read_to_string(root.join(&rel)) else {
            continue;
        };
        let Ok(Value::Object(data)) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if let Some(Value::String(main)) = data.get("main") {
            mains.insert(paths::parent(&rel), main.clone());
        }
    }
    mains
}

/// Everything resolution needs about a scope.
pub struct Scope {
    /// Relative path -> module id.
    pub known_files: HashMap<String, String>,
    pub known_modules: HashSet<String>,
    pub resolver: Resolver,
    pub mains: BTreeMap<String, String>,
}

impl Scope {
    fn match_file(&self, trial: &str, with_mains: bool) -> Option<String> {
        let key = paths::pure(trial);
        if let Some(m) = self.known_files.get(&key) {
            return Some(m.clone());
        }
        let name = paths::name(&key);
        let base = if name.contains('.') {
            key[..key.len() - paths::suffix(name).len()].to_owned()
        } else {
            key.clone()
        };
        for ext in CODE_EXTENSIONS {
            if let Some(m) = self.known_files.get(&format!("{base}{ext}")) {
                return Some(m.clone());
            }
        }
        if with_mains {
            let main = self
                .mains
                .get(&key)
                .filter(|m| !m.is_empty())
                .or_else(|| self.mains.get(key.trim_end_matches('/')));
            if let Some(main) = main {
                let candidate = paths::normpath(&paths::join(&key, main));
                if let Some(hit) = self.match_file(&candidate, false) {
                    return Some(hit);
                }
            }
        }
        CODE_EXTENSIONS
            .iter()
            .find_map(|ext| self.known_files.get(&format!("{base}/index{ext}")).cloned())
    }

    fn scoped(&self, trial: &str) -> Option<String> {
        let prefix = self.resolver.scope_prefix.as_deref()?;
        let text = paths::pure(trial);
        if prefix.is_empty() || prefix == "." {
            return Some(text);
        }
        if text == prefix {
            return Some(".".into());
        }
        text.strip_prefix(&format!("{}/", prefix.trim_end_matches('/')))
            .map(str::to_owned)
    }

    fn try_trial(&self, trial: &str) -> Option<String> {
        self.match_file(&self.scoped(trial)?, true)
    }

    /// `_resolve_specifier`: `None` is unknown (never external).
    pub fn resolve(&self, spec: &str, from_rel: &str) -> Option<Target> {
        if spec.starts_with("./") || spec.starts_with("../") {
            let trial = paths::normpath(&paths::join(&paths::parent(from_rel), spec));
            return self.match_file(&trial, true).map(Target::Module);
        }
        if let Some(rest) = spec.strip_prefix('/') {
            return self
                .match_file(rest.trim_start_matches('/'), true)
                .map(Target::Module);
        }
        let r = &self.resolver;
        if r.maps_usable
            && let Some(targets) = r.exact.get(spec)
        {
            for t in targets {
                for trial in [t.clone(), format!("{t}.js"), paths::normpath(t)] {
                    if let Some(hit) = self.try_trial(&trial) {
                        return Some(Target::Module(hit));
                    }
                }
            }
            return None;
        }
        if r.maps_usable {
            let mut matched: Vec<&(String, Vec<String>)> = r
                .prefix
                .iter()
                .filter(|(p, _)| spec.starts_with(p.as_str()))
                .collect();
            matched.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
            for (prefix, dirs) in &matched {
                let rest = spec[prefix.len()..].trim_start_matches('/');
                for target in dirs {
                    let trial = if target.contains('*') {
                        target.replacen('*', rest, 1)
                    } else {
                        target.clone()
                    };
                    if let Some(hit) = self.try_trial(&trial) {
                        return Some(Target::Module(hit));
                    }
                }
            }
            if !matched.is_empty() {
                return None;
            }
            if let Some(base) = r.base_url.as_deref().filter(|b| !b.is_empty()) {
                let scoped = self.scoped(&paths::pure(&paths::join(base, spec)))?;
                if let Some(hit) = self.match_file(&scoped, true) {
                    return Some(Target::Module(hit));
                }
            }
        }
        r.strict_external.then_some(Target::External)
    }
}
