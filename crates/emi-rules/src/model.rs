//! Rule definitions as written in YAML.

use serde::Deserialize;

/// Grammar family a rule applies to (by file extension).
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// `.js .jsx .mjs .cjs`
    Js,
    /// `.ts .mts .cts` and `.tsx`
    Ts,
    /// `.py`
    Python,
    /// `.rs`
    Rust,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Note,
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Note => "note",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

/// Which files a rule looks at, relative to test detection.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Tests {
    /// Every file.
    #[default]
    Any,
    /// Test files only (`.only`, skipped tests).
    Only,
    /// Product files only.
    Exclude,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Examples {
    /// Snippets that must produce at least one hit.
    #[serde(default, rename = "match")]
    pub matching: Vec<String>,
    /// Snippets that must produce none.
    #[serde(default)]
    pub no_match: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub languages: Vec<Language>,
    pub severity: Severity,
    /// Message; `{{capture}}` is replaced by the capture's text.
    pub message: String,
    /// Tree-sitter query; the `@match` capture (else the first) locates it.
    pub query: String,
    /// Per-language replacements for `query` (grammars name nodes
    /// differently, e.g. Rust's `line_comment`).
    #[serde(default)]
    pub queries: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tests: Tests,
    /// fnmatch globs on scope-relative paths.
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub examples: Examples,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleFile {
    pub rules: Vec<Rule>,
}
