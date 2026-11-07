//! Function spans from the syntax tree.

use tree_sitter::{Node, Parser, Tree};

/// One function definition. Lines are 1-based and inclusive; an end at
/// column 0 belongs to the line before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    /// The name the definition gives (`def name`), without its class.
    pub name: String,
    /// Enclosing class and function names, outermost first (`C`, `C.inner`).
    pub scope: String,
    /// First line, decorators included.
    pub start: u32,
    pub end: u32,
    /// The line the body starts on: past the signature, however many lines
    /// it spans. Equal to `start` for a one-line definition.
    pub body_start: u32,
    /// Byte range of the body, for replacing it.
    pub body: (usize, usize),
    /// Byte range of the entire definition, decorators included.
    pub span: (usize, usize),
}

/// 1-based line of a byte offset, from the source's newline offsets.
pub(crate) struct LineIndex(Vec<usize>);

impl LineIndex {
    pub(crate) fn new(src: &[u8]) -> Self {
        LineIndex(
            src.iter()
                .enumerate()
                .filter(|&(_, &b)| b == b'\n')
                .map(|(i, _)| i)
                .collect(),
        )
    }

    pub(crate) fn line(&self, offset: usize) -> u32 {
        u32::try_from(self.0.partition_point(|&nl| nl < offset) + 1).unwrap_or(u32::MAX)
    }

    fn span(&self, node: Node) -> (u32, u32) {
        let end = node.start_byte().max(node.end_byte().saturating_sub(1));
        (self.line(node.start_byte()), self.line(end))
    }
}

pub(crate) fn parse(src: &[u8]) -> Option<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .ok()?;
    parser.parse(src, None)
}

/// Whether `src` parses without syntax errors.
pub fn parses(src: &[u8]) -> bool {
    parse(src).is_some_and(|t| !t.root_node().has_error())
}

fn text(node: Node, src: &[u8]) -> String {
    String::from_utf8_lossy(&src[node.start_byte()..node.end_byte()]).into_owned()
}

/// The node a definition's span starts at: its decorators, when it has any.
fn outer(def: Node) -> Node {
    def.parent()
        .filter(|p| p.kind() == "decorated_definition")
        .unwrap_or(def)
}

/// Enclosing class and function names around `def`, outermost first.
fn enclosing_scope(def: Node, src: &[u8]) -> String {
    let mut names = Vec::new();
    let mut cur = def.parent();
    while let Some(p) = cur {
        if matches!(p.kind(), "class_definition" | "function_definition")
            && let Some(n) = p.child_by_field_name("name")
        {
            names.push(text(n, src));
        }
        cur = p.parent();
    }
    names.reverse();
    names.join(".")
}

/// Every function definition in `src`, nested ones included, ordered by
/// position. `None` when the file does not parse: its functions are
/// unknown, not absent.
pub fn functions(src: &[u8]) -> Option<Vec<Function>> {
    let tree = parse(src).filter(|t| !t.root_node().has_error())?;
    let lines = LineIndex::new(src);
    let mut found = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_definition"
            && let (Some(name), Some(body)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("body"),
            )
        {
            let outer_node = outer(node);
            let (start, end) = lines.span(outer_node);
            found.push(Function {
                name: text(name, src),
                scope: enclosing_scope(node, src),
                start,
                end,
                body_start: lines.line(body.start_byte()),
                body: (body.start_byte(), body.end_byte()),
                span: (outer_node.start_byte(), outer_node.end_byte()),
            });
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    found.sort_by_key(|f| (f.body.0, f.body.1));
    Some(found)
}

/// Splice the baseline version of the function at `old_start` in `old_src`
/// over the candidate version at `new_start` in `new_src`. Returns `None`
/// if either function cannot be located, the spliced bytes equal `new_src`,
/// or the result does not parse.
pub fn revert_function(
    old_src: &[u8],
    new_src: &[u8],
    old_start: u32,
    new_start: u32,
) -> Option<Vec<u8>> {
    let old_fns = functions(old_src)?;
    let new_fns = functions(new_src)?;
    let old_fn = old_fns.iter().find(|f| f.start == old_start)?;
    let new_fn = new_fns.iter().find(|f| f.start == new_start)?;
    let splice = |old_range: (usize, usize), new_range: (usize, usize)| -> Vec<u8> {
        let mut out = Vec::with_capacity(
            new_src.len() - (new_range.1 - new_range.0) + (old_range.1 - old_range.0),
        );
        out.extend_from_slice(&new_src[..new_range.0]);
        out.extend_from_slice(&old_src[old_range.0..old_range.1]);
        out.extend_from_slice(&new_src[new_range.1..]);
        out
    };
    let by_body = splice(old_fn.body, new_fn.body);
    if by_body != new_src && parses(&by_body) {
        return Some(by_body);
    }
    let by_span = splice(old_fn.span, new_fn.span);
    if by_span != new_src && parses(&by_span) {
        return Some(by_span);
    }
    None
}

fn node_text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    std::str::from_utf8(&src[node.start_byte()..node.end_byte()]).unwrap_or("")
}

fn unquote(s: &str) -> &str {
    let s = s.trim_start_matches(['r', 'b', 'f', 'u', 'R', 'B', 'F', 'U']);
    for quote in ["\"\"\"", "'''", "\"", "'"] {
        if let Some(inner) = s
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    s
}

fn imports_test_runner(node: Node, src: &[u8]) -> bool {
    let is_runner = |m: &str| {
        let root = m.trim().split('.').next().unwrap_or("");
        matches!(root, "pytest" | "hypothesis")
    };
    if node.kind() == "import_from_statement"
        && let Some(m) = node.child_by_field_name("module_name")
        && is_runner(node_text(m, src))
    {
        return true;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "dotted_name" if is_runner(node_text(child, src)) => return true,
            "aliased_import" => {
                if let Some(name) = child.child_by_field_name("name")
                    && is_runner(node_text(name, src))
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn sniffs_sys_modules(node: Node, src: &[u8]) -> bool {
    match node.kind() {
        "comparison_operator" => {
            let mut cursor = node.walk();
            let children: Vec<Node> = node.named_children(&mut cursor).collect();
            let has_sys_modules = children
                .iter()
                .any(|c| node_text(*c, src).trim() == "sys.modules");
            let has_runner_str = children.iter().any(|c| {
                c.kind() == "string"
                    && matches!(unquote(node_text(*c, src)), "pytest" | "hypothesis")
            });
            has_sys_modules && has_runner_str
        }
        "call" => {
            let Some(func) = node.child_by_field_name("function") else {
                return false;
            };
            if node_text(func, src).trim() != "sys.modules.get" {
                return false;
            }
            let Some(args) = node.child_by_field_name("arguments") else {
                return false;
            };
            let mut cursor = args.walk();
            args.named_children(&mut cursor).any(|c| {
                c.kind() == "string"
                    && matches!(unquote(node_text(c, src)), "pytest" | "hypothesis")
            })
        }
        _ => false,
    }
}

fn is_always_equal_def(node: Node, src: &[u8]) -> bool {
    if node.kind() != "function_definition" {
        return false;
    }
    let Some(name) = node.child_by_field_name("name") else {
        return false;
    };
    if node_text(name, src) != "__eq__" {
        return false;
    }
    let Some(body) = node.child_by_field_name("body") else {
        return false;
    };
    let mut cursor = body.walk();
    let stmts: Vec<Node> = body
        .named_children(&mut cursor)
        .filter(|c| c.kind() != "comment")
        .enumerate()
        .filter(|(i, c)| {
            if *i == 0 && c.kind() == "expression_statement" {
                let mut inner = c.walk();
                let mut children = c.named_children(&mut inner);
                if let (Some(first), None) = (children.next(), children.next())
                    && first.kind() == "string"
                {
                    return false;
                }
            }
            true
        })
        .map(|(_, c)| c)
        .collect();
    if stmts.len() != 1 || stmts[0].kind() != "return_statement" {
        return false;
    }
    let mut ret_cursor = stmts[0].walk();
    let mut exprs = stmts[0].named_children(&mut ret_cursor);
    matches!(
        (exprs.next(), exprs.next()),
        (Some(val), None) if val.kind() == "true" || node_text(val, src).trim() == "True"
    )
}

fn node_signal(
    node: Node,
    src: &[u8],
    lines: &LineIndex,
    added_lines: &std::collections::BTreeSet<u32>,
) -> Option<(u32, usize)> {
    let (start, end) = lines.span(node);
    if node.kind() == "function_definition" {
        if is_always_equal_def(node, src) && (start..=end).any(|l| added_lines.contains(&l)) {
            let report_line = (start..=end)
                .find(|l| added_lines.contains(l))
                .unwrap_or(start);
            return Some((report_line, 3));
        }
        return None;
    }
    if !added_lines.contains(&start) {
        return None;
    }
    match node.kind() {
        "comment" => {
            let text = node_text(node, src);
            if text.contains("# type: ignore")
                || text.contains("# noqa")
                || text.contains("pragma: no cover")
            {
                Some((start, 2))
            } else {
                None
            }
        }
        "call" => {
            if let Some(func) = node.child_by_field_name("function") {
                let fname = node_text(func, src).trim();
                if matches!(
                    fname,
                    "eval" | "exec" | "compile" | "builtins.eval" | "builtins.exec"
                ) {
                    return Some((start, 1));
                }
            }
            if sniffs_sys_modules(node, src) {
                Some((start, 0))
            } else {
                None
            }
        }
        "import_statement" | "import_from_statement" => {
            imports_test_runner(node, src).then_some((start, 0))
        }
        "comparison_operator" => sniffs_sys_modules(node, src).then_some((start, 0)),
        "string" => (unquote(node_text(node, src)) == "PYTEST_CURRENT_TEST").then_some((start, 0)),
        _ => None,
    }
}

/// Escape-hatch signals in `src` on lines in `added_lines`:
/// `(1-based line, kind, meaning)` in source order without duplicates.
pub fn ast_signals(
    src: &[u8],
    added_lines: &std::collections::BTreeSet<u32>,
) -> Vec<(u32, &'static str, &'static str)> {
    let Some(tree) = parse(src) else {
        return Vec::new();
    };
    let lines = LineIndex::new(src);
    let mut found: std::collections::BTreeSet<(u32, usize)> = std::collections::BTreeSet::new();
    const KINDS: [(&str, &str); 4] = [
        (
            "test_sniffing",
            "asks whether a test runner is loaded: product code can then behave one way under test",
        ),
        (
            "dynamic_code",
            "runs code built at run time, which no reading of the source covers",
        ),
        ("suppression", "switches a checker off for this line"),
        (
            "always_equal",
            "an __eq__ that returns True whatever it compares: every equality law holds",
        ),
    ];
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if let Some(sig) = node_signal(node, src, &lines, added_lines) {
            found.insert(sig);
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    found
        .into_iter()
        .map(|(line, idx)| (line, KINDS[idx].0, KINDS[idx].1))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_include_decorators_and_bodies_start_past_the_signature() {
        let src = b"@cache\ndef f(\n    a,\n    b,\n):\n    return a\n\ndef g(): return 1\n";
        let fns = functions(src).unwrap_or_default();
        assert_eq!(fns.len(), 2);
        assert_eq!(
            (
                fns[0].name.as_str(),
                fns[0].scope.as_str(),
                fns[0].start,
                fns[0].end,
                fns[0].body_start
            ),
            ("f", "", 1, 6, 6)
        );
        assert_eq!((fns[1].start, fns[1].end, fns[1].body_start), (8, 8, 8));
        assert_eq!(&src[fns[1].body.0..fns[1].body.1], b"return 1");
    }

    #[test]
    fn nested_definitions_are_their_own_functions() {
        let src = b"class A:\n    def m(self):\n        def inner():\n            pass\n        return inner\n";
        let found = functions(src).unwrap_or_default();
        let names: Vec<(&str, &str)> = found
            .iter()
            .map(|f| (f.scope.as_str(), f.name.as_str()))
            .collect();
        assert_eq!(names, [("A", "m"), ("A.m", "inner")]);
    }

    #[test]
    fn revert_function_restores_only_the_target_function() {
        let old_src = b"def f(x):\n    return x + 1\n\ndef g(y):\n    return y * 2\n";
        let new_src = b"def f(x):\n    return x + 10\n\ndef g(y):\n    return y * 20\n";
        let reverted = revert_function(old_src, new_src, 4, 4).unwrap_or_default();
        assert_eq!(
            reverted,
            b"def f(x):\n    return x + 10\n\ndef g(y):\n    return y * 2\n"
        );
    }

    #[test]
    fn a_file_with_a_syntax_error_has_unknown_functions() {
        assert!(functions(b"def f(:\n    pass\n").is_none());
        assert!(!parses(b"def f(:\n"));
        assert!(parses(b"x = 1\n"));
    }

    #[test]
    fn ast_signals_ignore_string_literals_and_catch_multiline_escapes() {
        let src = br#"
msg1 = "never call eval(x) or exec(y)"
msg2 = "use # type: ignore if needed"
msg3 = "PYTEST_CURRENT_TEST in strings is fine"
from pytest import fixture

class Sneaky:
    def __eq__(
        self,
        other: object,
    ) -> bool:
        """Always equal."""
        return True
"#;
        let added: std::collections::BTreeSet<u32> = (1..=14).collect();
        let found: Vec<(u32, &str)> = ast_signals(src, &added)
            .into_iter()
            .map(|(l, k, _)| (l, k))
            .collect();
        assert_eq!(found, vec![(5, "test_sniffing"), (8, "always_equal")]);
    }
}
