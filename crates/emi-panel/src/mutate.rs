//! Mutants: small wrong versions of the code (a comparison flipped, a
//! boolean operator or literal swapped, an arithmetic operator changed).
//! Running the tests against one shows whether they would notice that
//! line going wrong, which line coverage alone cannot tell.

use tree_sitter::{Node, Parser};

use crate::{Lang, LineIndex};

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
    ("===", "!=="),
    ("!==", "==="),
    ("&&", "||"),
    ("||", "&&"),
    ("and", "or"),
    ("or", "and"),
    ("+", "-"),
    ("-", "+"),
    ("*", "/"),
    ("/", "*"),
];

/// Parent node kinds whose operator token may be swapped.
const OPERATOR_PARENTS: &[&str] = &[
    "binary_operator",
    "comparison_operator",
    "boolean_operator",
    "binary_expression",
];

const LITERALS: &[(&str, &str)] = &[
    ("True", "False"),
    ("False", "True"),
    ("true", "false"),
    ("false", "true"),
];

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
pub fn mutants(lang: Lang, src: &[u8]) -> Vec<Mutant> {
    let mut parser = Parser::new();
    if parser.set_language(&lang.grammar()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(src, None) else {
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

    fn applied(lang: Lang, src: &str) -> Vec<String> {
        mutants(lang, src.as_bytes())
            .iter()
            .map(|m| String::from_utf8_lossy(&m.apply(src.as_bytes())).into_owned())
            .collect()
    }

    #[test]
    fn python_operators_and_literals_but_not_unary_minus() {
        let src = "def f(a, b):\n    if a < b and -a:\n        return a + 1\n    return True\n";
        let got = applied(Lang::Python, src);
        assert_eq!(
            got,
            vec![
                src.replace("a < b", "a >= b"),
                src.replace(" and ", " or "),
                src.replace("a + 1", "a - 1"),
                src.replace("True", "False"),
            ]
        );
        let lines: Vec<u32> = mutants(Lang::Python, src.as_bytes())
            .iter()
            .map(|m| m.line)
            .collect();
        assert_eq!(lines, vec![2, 2, 3, 4]);
    }

    #[test]
    fn javascript_strict_equality_and_logic() {
        let src = "function f(a, b) { return a === b || !a; }\n";
        let got = applied(Lang::JavaScript, src);
        assert_eq!(
            got,
            vec![src.replace("===", "!=="), src.replace("||", "&&")]
        );
    }

    #[test]
    fn rust_generics_are_not_comparisons() {
        let src = "fn f(v: Vec<u8>, n: usize) -> bool { v.len() > n || false }\n";
        let got = applied(Lang::Rust, src);
        assert_eq!(
            got,
            vec![
                src.replace("v.len() > n", "v.len() <= n"),
                src.replace("||", "&&"),
                src.replace("false", "true"),
            ]
        );
    }

    #[test]
    fn describe_names_line_and_tokens() {
        let m = &mutants(
            Lang::Go,
            b"package p\n\nfunc f(a int) bool {\n\treturn a == 1\n}\n",
        )[0];
        assert_eq!(m.describe(), "line 4: `==` -> `!=`");
    }
}
