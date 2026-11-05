//! Mutants: small wrong versions of the code (a comparison flipped, a
//! boolean operator or literal swapped, an arithmetic operator changed).
//! Running the tests against one shows whether they would notice that
//! line going wrong, which line coverage alone cannot tell.

use tree_sitter::Node;

use crate::syntax::{LineIndex, parse};

/// One single-token change to a source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mutant {
    /// 1-based line of the changed token.
    pub line: u32,
    /// Byte range of the token.
    pub start: usize,
    pub end: usize,
    pub original: String,
    pub replacement: String,
}

impl Mutant {
    /// `src` with this mutant applied.
    pub fn apply(&self, src: &[u8]) -> Vec<u8> {
        let mut out = src[..self.start].to_vec();
        out.extend_from_slice(self.replacement.as_bytes());
        out.extend_from_slice(&src[self.end..]);
        out
    }

    pub fn describe(&self) -> String {
        format!(
            "line {}: `{}` -> `{}`",
            self.line, self.original, self.replacement
        )
    }
}

/// Operator tokens and what each becomes, applied only inside binary,
/// comparison and boolean expressions (never unary minus, generics or JSX).
const OPERATORS: &[(&str, &str)] = &[
    ("<", ">="),
    (">", "<="),
    ("<=", ">"),
    (">=", "<"),
    ("==", "!="),
    ("!=", "=="),
    ("and", "or"),
    ("or", "and"),
    ("+", "-"),
    ("-", "+"),
    ("*", "/"),
    ("/", "*"),
];

/// Parent node kinds whose operator token may be swapped.
const OPERATOR_PARENTS: &[&str] = &["binary_operator", "comparison_operator", "boolean_operator"];

const LITERALS: &[(&str, &str)] = &[("True", "False"), ("False", "True")];

fn lookup(table: &[(&str, &'static str)], token: &str) -> Option<&'static str> {
    table
        .iter()
        .find(|(from, _)| *from == token)
        .map(|(_, to)| *to)
}

/// The replacement for a leaf token, if it is mutable.
fn replacement(node: Node, token: &str) -> Option<&'static str> {
    let in_operator = node
        .parent()
        .is_some_and(|p| OPERATOR_PARENTS.contains(&p.kind()));
    if !node.is_named() && in_operator {
        return lookup(OPERATORS, token);
    }
    matches!(node.kind(), "true" | "false")
        .then(|| lookup(LITERALS, token))
        .flatten()
}

/// Every mutant of `src`, in source order. A file the grammar cannot parse
/// yields none.
pub fn mutants(src: &[u8]) -> Vec<Mutant> {
    let Some(tree) = parse(src) else {
        return Vec::new();
    };
    let lines = LineIndex::new(src);
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.child_count() == 0 {
            let token = String::from_utf8_lossy(&src[node.start_byte()..node.end_byte()]);
            if let Some(to) = replacement(node, &token) {
                out.push(Mutant {
                    line: lines.line(node.start_byte()),
                    start: node.start_byte(),
                    end: node.end_byte(),
                    original: token.into_owned(),
                    replacement: to.to_owned(),
                });
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    out.sort_by_key(|m| m.start);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn applied(src: &str) -> Vec<String> {
        mutants(src.as_bytes())
            .iter()
            .map(|m| String::from_utf8_lossy(&m.apply(src.as_bytes())).into_owned())
            .collect()
    }

    #[test]
    fn operators_and_literals_but_not_unary_minus() {
        let got = applied("def f(a, b):\n    return a < b and -a or True\n");
        assert!(got.contains(&"def f(a, b):\n    return a >= b and -a or True\n".to_owned()));
        assert!(got.contains(&"def f(a, b):\n    return a < b or -a or True\n".to_owned()));
        assert!(got.contains(&"def f(a, b):\n    return a < b and -a or False\n".to_owned()));
        assert!(!got.iter().any(|g| g.contains("+a")), "{got:?}");
    }

    #[test]
    fn mutants_are_in_source_order_with_lines() {
        let lines: Vec<u32> = mutants(b"x = 1 + 2\ny = x == 3\n")
            .iter()
            .map(|m| m.line)
            .collect();
        assert_eq!(lines, [1, 2]);
    }
}
