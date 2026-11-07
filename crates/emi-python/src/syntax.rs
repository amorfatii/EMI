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
}
