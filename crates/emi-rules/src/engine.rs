//! Compile rules per grammar and run them over files.

use std::collections::HashMap;

use tree_sitter::{Language as TsLanguage, Parser, Query, QueryCursor, StreamingIterator};

use crate::model::{Language, Rule};

/// The concrete grammar of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Grammar {
    Js,
    Ts,
    Tsx,
    Python,
    Rust,
}

impl Grammar {
    pub fn of(path: &str) -> Option<Grammar> {
        let lower = path.to_lowercase();
        let ext = lower.rsplit_once('.').map(|(_, e)| e)?;
        Some(match ext {
            "js" | "jsx" | "mjs" | "cjs" => Grammar::Js,
            "ts" | "mts" | "cts" => Grammar::Ts,
            "tsx" => Grammar::Tsx,
            "py" => Grammar::Python,
            "rs" => Grammar::Rust,
            _ => return None,
        })
    }

    fn family(self) -> Language {
        match self {
            Grammar::Js => Language::Js,
            Grammar::Ts | Grammar::Tsx => Language::Ts,
            Grammar::Python => Language::Python,
            Grammar::Rust => Language::Rust,
        }
    }

    fn language(self) -> TsLanguage {
        match self {
            Grammar::Js => tree_sitter_javascript::LANGUAGE.into(),
            Grammar::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Grammar::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Grammar::Python => tree_sitter_python::LANGUAGE.into(),
            Grammar::Rust => tree_sitter_rust::LANGUAGE.into(),
        }
    }

    const ALL: [Grammar; 5] = [
        Grammar::Js,
        Grammar::Ts,
        Grammar::Tsx,
        Grammar::Python,
        Grammar::Rust,
    ];
}

/// One rule hit.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Hit {
    pub rule: String,
    pub severity: &'static str,
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub message: String,
    /// Whitespace-normalized matched text (for fingerprints).
    #[serde(skip)]
    pub text: String,
}

pub struct Compiled {
    pub rule: Rule,
    queries: HashMap<Grammar, (Query, u32)>,
}

impl Compiled {
    pub fn applies_to(&self, grammar: Grammar) -> bool {
        self.queries.contains_key(&grammar)
    }
}

/// Compile a rule for every grammar of its languages.
pub fn compile(rule: Rule) -> Result<Compiled, String> {
    let mut queries = HashMap::new();
    for g in Grammar::ALL
        .into_iter()
        .filter(|g| rule.languages.contains(&g.family()))
    {
        let family = format!("{:?}", g.family()).to_lowercase();
        let source = rule.queries.get(&family).unwrap_or(&rule.query);
        let query = Query::new(&g.language(), source)
            .map_err(|e| format!("rule {}: query does not compile for {g:?}: {e}", rule.id))?;
        let capture = query
            .capture_names()
            .iter()
            .position(|n| *n == "match")
            .unwrap_or(0);
        queries.insert(g, (query, u32::try_from(capture).unwrap_or(0)));
    }
    if queries.is_empty() {
        return Err(format!("rule {}: no languages", rule.id));
    }
    Ok(Compiled { rule, queries })
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn render(template: &str, captures: &HashMap<&str, String>) -> String {
    let mut out = template.to_owned();
    for (name, text) in captures {
        out = out.replace(&format!("{{{{{name}}}}}"), text);
    }
    out
}

pub fn parse(grammar: Grammar, source: &str) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    parser.set_language(&grammar.language()).ok()?;
    parser.parse(source, None)
}

/// Run the applicable rules on one parsed file.
pub fn run(
    rules: &[&Compiled],
    grammar: Grammar,
    file: &str,
    source: &str,
    tree: &tree_sitter::Tree,
) -> Vec<Hit> {
    let mut hits = Vec::new();
    for c in rules {
        let Some((query, capture)) = c.queries.get(&grammar) else {
            continue;
        };
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
        while let Some(m) = matches.next() {
            let texts: HashMap<&str, String> = m
                .captures
                .iter()
                .map(|cap| {
                    let name = query.capture_names()[cap.index as usize];
                    (
                        name,
                        cap.node
                            .utf8_text(source.as_bytes())
                            .unwrap_or("")
                            .to_owned(),
                    )
                })
                .collect();
            let node = m
                .captures
                .iter()
                .find(|cap| cap.index == *capture)
                .or(m.captures.first())
                .map(|cap| cap.node);
            let Some(node) = node else { continue };
            let (start, end) = (node.start_position(), node.end_position());
            hits.push(Hit {
                rule: c.rule.id.clone(),
                severity: c.rule.severity.as_str(),
                file: file.to_owned(),
                line: u32::try_from(start.row + 1).unwrap_or(u32::MAX),
                column: u32::try_from(start.column + 1).unwrap_or(u32::MAX),
                end_line: u32::try_from(end.row + 1).unwrap_or(u32::MAX),
                message: render(&c.rule.message, &texts),
                text: normalize(node.utf8_text(source.as_bytes()).unwrap_or("")),
            });
        }
    }
    hits.sort_by(|a, b| (a.line, a.column, &a.rule).cmp(&(b.line, b.column, &b.rule)));
    hits.dedup_by(|a, b| a.line == b.line && a.column == b.column && a.rule == b.rule);
    hits
}
