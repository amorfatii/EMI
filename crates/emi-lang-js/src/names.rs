//! Region naming of `js_probe`: bare region names, decorator-aware start
//! rows, and outermost-first qualified ids.

use tree_sitter::Node;

pub const FUNCTIONS: [&str; 6] = [
    "function_declaration",
    "generator_function_declaration",
    "function_expression",
    "generator_function",
    "arrow_function",
    "method_definition",
];

const FUNCTION_VALUES: [&str; 4] = [
    "function_expression",
    "generator_function",
    "arrow_function",
    "class",
];

fn text<'s>(node: Node, source: &'s [u8]) -> std::borrow::Cow<'s, str> {
    String::from_utf8_lossy(&source[node.start_byte()..node.end_byte()])
}

/// First identifier-like child, else `<anonymous>`.
pub fn region_name(node: Node, source: &[u8]) -> String {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| {
        matches!(
            c.kind(),
            "identifier"
                | "property_identifier"
                | "private_property_identifier"
                | "shorthand_property_identifier"
        )
    });
    found.map_or_else(
        || "<anonymous>".to_owned(),
        |c| text(c, source).into_owned(),
    )
}

/// First 0-based row of a function, extended over an immediately preceding
/// run of decorator siblings (the TypeScript grammar's placement).
pub fn start_row(func: Node) -> usize {
    let mut start = func.start_position().row;
    let Some(parent) = func.parent() else {
        return start;
    };
    let mut cursor = parent.walk();
    let preceding: Vec<Node> = parent
        .children(&mut cursor)
        .take_while(|c| c.id() != func.id())
        .collect();
    for child in preceding.iter().rev() {
        if child.kind() != "decorator" {
            break;
        }
        start = start.min(child.start_position().row);
    }
    start
}

/// Contents of a plain string literal (no template holes), as `_string_of`.
fn string_value(node: Node, source: &[u8]) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    let fragments = children
        .iter()
        .filter(|c| c.kind() == "string_fragment")
        .count();
    let counted = children
        .iter()
        .filter(|c| c.is_named() || c.kind() == "string_fragment")
        .count();
    if fragments != counted {
        return None;
    }
    let (s, e) = (node.start_byte() + 1, node.end_byte().saturating_sub(1));
    Some(String::from_utf8_lossy(source.get(s..e).unwrap_or_default()).into_owned())
}

fn pair_key(node: Node, source: &[u8]) -> Option<String> {
    let key = node.child_by_field_name("key")?;
    match key.kind() {
        "property_identifier" => Some(text(key, source).into_owned()),
        "string" => string_value(key, source),
        _ => None,
    }
}

fn member_target(node: Node, source: &[u8]) -> Option<String> {
    (node.kind() == "member_expression").then(|| {
        text(node, source)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    })
}

fn identifier_field(node: Node, field: &str, source: &[u8]) -> Option<String> {
    node.child_by_field_name(field)
        .filter(|n| n.kind() == "identifier")
        .map(|n| text(n, source).into_owned())
}

/// The name one ancestor contributes to a qualified id, if any.
fn contribution(current: Node, child: Option<Node>, source: &[u8]) -> Option<String> {
    let anon = || "<anonymous>".to_owned();
    match current.kind() {
        "method_definition" => Some(region_name(current, source)),
        "function_declaration" | "generator_function_declaration" | "class_declaration" => {
            Some(identifier_field(current, "name", source).unwrap_or_else(anon))
        }
        kind if FUNCTION_VALUES.contains(&kind) => {
            let parent = current.parent();
            match parent.map(|p| p.kind()) {
                Some("assignment_expression" | "pair") => None,
                Some("variable_declarator") => Some(
                    parent
                        .and_then(|p| identifier_field(p, "name", source))
                        .unwrap_or_else(anon),
                ),
                _ => Some(anon()),
            }
        }
        "assignment_expression" => current
            .child_by_field_name("left")
            .and_then(|l| member_target(l, source)),
        "pair" => Some(pair_key(current, source).unwrap_or_else(anon)),
        "variable_declarator" => {
            let value = current.child_by_field_name("value");
            let names_itself = matches!((value, child), (Some(v), Some(c))
                if v.id() == c.id() && FUNCTION_VALUES.contains(&v.kind()));
            if names_itself {
                None
            } else {
                identifier_field(current, "name", source)
            }
        }
        _ => None,
    }
}

/// Dotted scope name, outermost first; `<module>` for the program root.
pub fn qualified_name(node: Node, source: &[u8]) -> String {
    if node.kind() == "program" {
        return "<module>".to_owned();
    }
    let mut parts = Vec::new();
    let mut current = Some(node);
    let mut child: Option<Node> = None;
    while let Some(cur) = current {
        if cur.kind() == "program" {
            break;
        }
        if let Some(part) = contribution(cur, child, source) {
            parts.push(part);
        }
        child = Some(cur);
        current = cur.parent();
    }
    parts.reverse();
    if parts.is_empty() {
        "<anonymous>".to_owned()
    } else {
        parts.join(".")
    }
}
