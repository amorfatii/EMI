//! Grammar selection with the TypeScript fallback of `js_probe` 0.10.

use std::cell::RefCell;
use std::collections::BTreeSet;

use tree_sitter::{Language, Node, Parser, Tree};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Grammar {
    Js,
    Ts,
    Tsx,
}

impl Grammar {
    fn language(self) -> Language {
        match self {
            Grammar::Js => tree_sitter_javascript::LANGUAGE.into(),
            Grammar::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Grammar::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        }
    }
}

thread_local! {
    static PARSERS: RefCell<[Option<Parser>; 3]> = const { RefCell::new([None, None, None]) };
}

fn parse_with(grammar: Grammar, source: &[u8]) -> Option<Tree> {
    PARSERS.with(|cell| {
        let mut parsers = cell.borrow_mut();
        let slot = &mut parsers[grammar as usize];
        if slot.is_none() {
            let mut parser = Parser::new();
            parser.set_language(&grammar.language()).ok()?;
            *slot = Some(parser);
        }
        slot.as_mut()?.parse(source, None)
    })
}

/// 1-based lines touched by ERROR or MISSING nodes.
pub fn error_lines(tree: &Tree) -> BTreeSet<u32> {
    let mut lines = BTreeSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() || node.kind() == "MISSING" {
            lines.extend(row(node.start_position().row)..=row(node.end_position().row));
        }
        push_children(&mut stack, node);
    }
    lines
}

pub fn row(r: usize) -> u32 {
    u32::try_from(r + 1).unwrap_or(u32::MAX)
}

/// Push children in reverse so popping yields source order (pre-order walk).
pub fn push_children<'t>(stack: &mut Vec<Node<'t>>, node: Node<'t>) {
    let mut cursor = node.walk();
    let children: Vec<Node<'t>> = node.children(&mut cursor).collect();
    stack.extend(children.into_iter().rev());
}

pub struct Parsed {
    pub tree: Tree,
    pub fallback: bool,
}

/// Parse by extension; JS-family files with syntax errors retry with the TS
/// grammar (TSX for `.jsx`) and keep the retry only if it has strictly fewer
/// error lines.
pub fn parse(path: &str, source: &[u8]) -> Option<Parsed> {
    let lower = path.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
    let primary = match ext {
        "ts" | "mts" | "cts" => Grammar::Ts,
        "tsx" => Grammar::Tsx,
        _ => Grammar::Js,
    };
    let tree = parse_with(primary, source)?;
    let js_family = matches!(ext, "js" | "jsx" | "mjs" | "cjs");
    if !js_family || !tree.root_node().has_error() {
        return Some(Parsed {
            tree,
            fallback: false,
        });
    }
    let alternative = if ext == "jsx" {
        Grammar::Tsx
    } else {
        Grammar::Ts
    };
    let candidate = parse_with(alternative, source)?;
    if error_lines(&candidate).len() < error_lines(&tree).len() {
        return Some(Parsed {
            tree: candidate,
            fallback: true,
        });
    }
    Some(Parsed {
        tree,
        fallback: false,
    })
}
