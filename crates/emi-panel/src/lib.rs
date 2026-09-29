//! Classic per-function metrics as the lab's metric panel (`panel-0.1`,
//! emi-lab `lab/panel/metrics.py`) defines them on tree-sitter trees: the
//! function spans, SLOC, SonarSource cognitive complexity and Halstead
//! volume that the `graded` erosion is built from (study G2).
//!
//! Each function is measured without descending into nested functions,
//! which are measured on their own. The grammars are the workspace pins,
//! the same versions the lab panel ran on; `emi panel` prints these
//! records so the lab can check parity file by file.

use std::collections::HashMap;

use tree_sitter::{Language, Node, Parser};

pub mod explain;
pub mod mutate;

/// Parentheses a logical sequence and a condition may be wrapped in.
const PARENS: [&str; 2] = ["parenthesized_expression", "condition_clause"];
/// Punctuation that is not a Halstead operator.
const DELIMITERS: [&str; 11] = ["(", ")", "[", "]", "{", "}", ",", ";", "\"", "'", "`"];

/// Syntax table of one language (see `lab/panel/lang.py`).
struct Spec {
    functions: &'static [&'static str],
    logical: &'static str,
    bool_ops: &'static [&'static str],
    /// Structures that cost 1 + nesting and deepen nesting.
    nesting_incr: &'static [&'static str],
    /// Structures that deepen nesting at no cost (lambdas, closures).
    nesting_only: &'static [&'static str],
    if_kind: &'static str,
    elif: Option<&'static str>,
    /// `None`: Go, where `else` is the `alternative` field of the if.
    else_kind: Option<&'static str>,
    jumps: &'static [&'static str],
    jump_label: &'static [&'static str],
    goto: &'static [&'static str],
    comments: &'static [&'static str],
    /// Logical operators left out of cognitive sequences (SonarJS).
    cognitive_uncounted: &'static [&'static str],
}

const JS_FUNCTIONS: &[&str] = &[
    "function_declaration",
    "function_expression",
    "function",
    "arrow_function",
    "method_definition",
    "generator_function_declaration",
    "generator_function",
];

const JS: Spec = Spec {
    functions: JS_FUNCTIONS,
    logical: "binary_expression",
    bool_ops: &["&&", "||", "??"],
    nesting_incr: &[
        "if_statement",
        "for_statement",
        "for_in_statement",
        "while_statement",
        "do_statement",
        "catch_clause",
        "ternary_expression",
        "switch_statement",
    ],
    nesting_only: &[],
    if_kind: "if_statement",
    elif: None,
    else_kind: Some("else_clause"),
    jumps: &["break_statement", "continue_statement"],
    jump_label: &["statement_identifier"],
    goto: &[],
    comments: &["comment", "html_comment"],
    cognitive_uncounted: &["||", "??"],
};

const PYTHON: Spec = Spec {
    functions: &["function_definition"],
    logical: "boolean_operator",
    bool_ops: &["and", "or"],
    nesting_incr: &[
        "if_statement",
        "for_statement",
        "while_statement",
        "except_clause",
        "conditional_expression",
        "match_statement",
    ],
    nesting_only: &["lambda"],
    if_kind: "if_statement",
    elif: Some("elif_clause"),
    else_kind: Some("else_clause"),
    jumps: &[],
    jump_label: &[],
    goto: &[],
    comments: &["comment"],
    cognitive_uncounted: &[],
};

const GO: Spec = Spec {
    functions: &["function_declaration", "method_declaration", "func_literal"],
    logical: "binary_expression",
    bool_ops: &["&&", "||"],
    nesting_incr: &[
        "if_statement",
        "for_statement",
        "expression_switch_statement",
        "type_switch_statement",
        "select_statement",
    ],
    nesting_only: &[],
    if_kind: "if_statement",
    elif: None,
    else_kind: None,
    jumps: &["break_statement", "continue_statement"],
    jump_label: &["label_name"],
    goto: &["goto_statement"],
    comments: &["comment"],
    cognitive_uncounted: &[],
};

const RUST: Spec = Spec {
    functions: &["function_item"],
    logical: "binary_expression",
    bool_ops: &["&&", "||"],
    nesting_incr: &[
        "if_expression",
        "for_expression",
        "while_expression",
        "loop_expression",
        "match_expression",
    ],
    nesting_only: &["closure_expression"],
    if_kind: "if_expression",
    elif: None,
    else_kind: Some("else_clause"),
    jumps: &["break_expression", "continue_expression"],
    jump_label: &["label"],
    goto: &[],
    comments: &["line_comment", "block_comment"],
    cognitive_uncounted: &[],
};

/// Grammars the panel measures, chosen by file extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Go,
    Rust,
}

impl Lang {
    /// The panel's extension table (`lab/panel/lang.py` EXTENSIONS, in order).
    pub fn of_path(path: &str) -> Option<Lang> {
        const TABLE: [(&str, Lang); 11] = [
            (".py", Lang::Python),
            (".js", Lang::JavaScript),
            (".mjs", Lang::JavaScript),
            (".cjs", Lang::JavaScript),
            (".jsx", Lang::JavaScript),
            (".ts", Lang::TypeScript),
            (".mts", Lang::TypeScript),
            (".cts", Lang::TypeScript),
            (".tsx", Lang::Tsx),
            (".go", Lang::Go),
            (".rs", Lang::Rust),
        ];
        TABLE
            .iter()
            .find(|(ext, _)| path.ends_with(ext))
            .map(|&(_, lang)| lang)
    }

    fn grammar(self) -> Language {
        match self {
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        }
    }

    fn spec(self) -> &'static Spec {
        match self {
            Lang::Python => &PYTHON,
            Lang::JavaScript | Lang::TypeScript | Lang::Tsx => &JS,
            Lang::Go => &GO,
            Lang::Rust => &RUST,
        }
    }
}

/// One function's panel metrics.
#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    pub name: String,
    /// First and last line (1-based; an end at column 0 belongs to the line before).
    pub start: u32,
    pub end: u32,
    /// Lines of the span that carry at least one non-comment token.
    pub sloc: u32,
    pub cognitive: u32,
    pub halstead_volume: f64,
    /// Where the cognitive complexity comes from, in source order.
    pub increments: Vec<Increment>,
}

/// One cognitive-complexity increment: the structure that costs, where.
#[derive(Clone, Debug, PartialEq)]
pub struct Increment {
    pub line: u32,
    pub cost: u32,
    /// The structure's nesting level (0 at the function's top level).
    pub nesting: u32,
    /// `if`, `loop`, `else if`, `else`, `catch`, `switch`, `ternary`,
    /// `boolean sequence`, `labelled jump`, `goto`, or the node kind.
    pub reason: String,
}

/// 1-based line of a byte offset, from the source's newline offsets.
struct LineIndex(Vec<usize>);

impl LineIndex {
    fn new(src: &[u8]) -> Self {
        LineIndex(
            src.iter()
                .enumerate()
                .filter(|&(_, &b)| b == b'\n')
                .map(|(i, _)| i)
                .collect(),
        )
    }

    fn line(&self, offset: usize) -> u32 {
        u32::try_from(self.0.partition_point(|&nl| nl < offset) + 1).unwrap_or(u32::MAX)
    }

    fn span(&self, node: Node) -> (u32, u32) {
        let end = node.start_byte().max(node.end_byte().saturating_sub(1));
        (self.line(node.start_byte()), self.line(end))
    }
}

fn children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

fn named_children(node: Node) -> Vec<Node> {
    children(node).into_iter().filter(Node::is_named).collect()
}

fn text(node: Node, src: &[u8]) -> String {
    String::from_utf8_lossy(&src[node.start_byte()..node.end_byte()]).into_owned()
}

fn atomic_operand(node: Node) -> bool {
    node.is_named()
        && (node.kind().contains("string")
            || matches!(node.kind(), "template_string" | "char_literal"))
}

/// Lines carrying at least one non-comment token.
fn code_lines(root: Node, spec: &Spec, line: &LineIndex) -> std::collections::HashSet<u32> {
    let mut lines = std::collections::HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if spec.comments.contains(&node.kind()) {
            continue;
        }
        if node.child_count() == 0 || atomic_operand(node) {
            if node.end_byte() > node.start_byte() {
                let (first, last) = line.span(node);
                lines.extend(first..=last);
            }
            continue;
        }
        stack.extend(children(node));
    }
    lines
}

/// One function's cognitive-complexity and Halstead pass.
struct Pass<'a> {
    spec: &'static Spec,
    src: &'a [u8],
    line: &'a LineIndex,
    cognitive: u32,
    increments: Vec<Increment>,
    operators: HashMap<&'static str, u32>,
    operands: HashMap<String, u32>,
}

impl Pass<'_> {
    fn add(&mut self, node: Node, cost: u32, nesting: u32, reason: &str) {
        self.cognitive += cost;
        self.increments.push(Increment {
            line: self.line.line(node.start_byte()),
            cost,
            nesting,
            reason: reason.to_owned(),
        });
    }

    fn operator_of(node: Node<'_>) -> Option<Node<'_>> {
        node.child_by_field_name("operator")
            .or_else(|| children(node).into_iter().find(|c| !c.is_named()))
    }

    fn logical(&self, node: Node) -> bool {
        node.kind() == self.spec.logical
            && Self::operator_of(node).is_some_and(|op| self.spec.bool_ops.contains(&op.kind()))
    }

    fn logical_parent(&self, node: Node) -> bool {
        let mut parent = node.parent();
        while let Some(p) = parent {
            if !PARENS.contains(&p.kind()) {
                break;
            }
            parent = p.parent();
        }
        parent.is_some_and(|p| self.logical(p))
    }

    /// Logical operators of a maximal logical expression, left to right.
    fn sequence(&self, node: Node, out: &mut Vec<&'static str>) {
        if PARENS.contains(&node.kind()) {
            let inner = named_children(node);
            if inner.len() == 1 {
                self.sequence(inner[0], out);
            }
            return;
        }
        if !self.logical(node) {
            return;
        }
        let (mut left, mut right) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        );
        if left.is_none() || right.is_none() {
            let named = named_children(node);
            left = named.first().copied();
            right = named.get(1).copied();
        }
        if let Some(l) = left {
            self.sequence(l, out);
        }
        if let Some(op) = Self::operator_of(node) {
            out.push(op.kind());
        }
        if let Some(r) = right {
            self.sequence(r, out);
        }
    }

    fn token(&mut self, node: Node) {
        if !node.is_named() {
            if !DELIMITERS.contains(&node.kind()) {
                *self.operators.entry(node.kind()).or_default() += 1;
            }
            return;
        }
        *self.operands.entry(text(node, self.src)).or_default() += 1;
    }

    /// The `if` that continues an else-if chain from `node` (Go only).
    fn else_if<'t>(&self, node: Node<'t>) -> Option<Node<'t>> {
        if self.spec.else_kind.is_some() || node.kind() != self.spec.if_kind {
            return None;
        }
        node.child_by_field_name("alternative")
            .filter(|alt| alt.kind() == self.spec.if_kind)
    }

    /// An else clause: `else if` continues the chain at the head's level; a
    /// plain else of an if costs 1.
    fn else_children<'t>(
        &mut self,
        clause: Node<'t>,
        parent_kind: &str,
        level: u32,
        child_level: u32,
    ) -> Vec<(Node<'t>, u32, bool)> {
        let inner: Vec<Node> = named_children(clause)
            .into_iter()
            .filter(|c| !self.spec.comments.contains(&c.kind()))
            .collect();
        if inner.len() == 1 && inner[0].kind() == self.spec.if_kind {
            let chained = parent_kind == self.spec.if_kind;
            return vec![(inner[0], if chained { level } else { child_level }, chained)];
        }
        if parent_kind == self.spec.if_kind {
            self.add(clause, 1, level, "else");
        }
        children(clause)
            .into_iter()
            .map(|c| (c, child_level, false))
            .collect()
    }

    /// Cognitive cost of one inner node; returns its children's nesting level.
    fn score(&mut self, node: Node, level: u32, chained: bool) -> u32 {
        let spec = self.spec;
        let kind = node.kind();
        let mut child_level = level;
        if self.logical(node) && !self.logical_parent(node) {
            let mut ops = Vec::new();
            self.sequence(node, &mut ops);
            let counted: Vec<&str> = ops
                .into_iter()
                .filter(|op| !spec.cognitive_uncounted.contains(op))
                .collect();
            let runs = counted
                .iter()
                .enumerate()
                .filter(|&(i, op)| i == 0 || counted[i - 1] != *op)
                .count();
            if runs > 0 {
                let runs = u32::try_from(runs).unwrap_or(u32::MAX);
                self.add(node, runs, level, "boolean sequence");
            }
        }
        if spec.nesting_incr.contains(&kind) {
            // an else-if continuation costs 1 whatever its depth
            let what = structure(kind);
            if chained {
                self.add(node, 1, level, "else if");
            } else {
                self.add(node, 1 + level, level, what);
            }
            child_level = level + 1;
        } else if spec.nesting_only.contains(&kind) {
            child_level = level + 1;
        }
        if spec.elif == Some(kind) {
            self.add(node, 1, level, "else if");
        }
        if spec.jumps.contains(&kind)
            && children(node)
                .iter()
                .any(|c| spec.jump_label.contains(&c.kind()))
        {
            self.add(node, 1, level, "labelled jump");
        }
        if spec.goto.contains(&kind) {
            self.add(node, 1, level, "goto");
        }
        child_level
    }

    fn run(&mut self, function: Node) {
        let spec = self.spec;
        // (node, cognitive nesting level, else-if continuation)
        let mut stack: Vec<(Node, u32, bool)> = children(function)
            .into_iter()
            .map(|c| (c, 0, false))
            .collect();
        while let Some((node, level, chained)) = stack.pop() {
            let kind = node.kind();
            if spec.functions.contains(&kind) || spec.comments.contains(&kind) {
                continue;
            }
            if node.child_count() == 0 || atomic_operand(node) {
                self.token(node);
                continue;
            }
            let child_level = self.score(node, level, chained);
            let continuation = self.else_if(node);
            let go_alternative = if spec.else_kind.is_none() && kind == spec.if_kind {
                node.child_by_field_name("alternative")
            } else {
                None
            };
            for child in children(node) {
                if continuation.is_some_and(|c| c.id() == child.id()) {
                    stack.push((child, level, true));
                } else if child.kind() == "block"
                    && go_alternative.is_some_and(|a| a.id() == child.id())
                {
                    self.add(child, 1, level, "else"); // Go plain else
                    stack.push((child, child_level, false));
                } else if spec.else_kind == Some(child.kind()) {
                    let items = self.else_children(child, kind, level, child_level);
                    stack.extend(items);
                } else {
                    stack.push((child, child_level, false));
                }
            }
        }
    }

    fn volume(&self) -> f64 {
        let vocabulary = self.operators.len() + self.operands.len();
        let length: u32 =
            self.operators.values().sum::<u32>() + self.operands.values().sum::<u32>();
        if vocabulary > 1 {
            f64::from(length) * (vocabulary as f64).log2()
        } else {
            0.0
        }
    }
}

/// A readable name for a costed structure.
fn structure(kind: &str) -> &'static str {
    match kind {
        k if k.starts_with("if") => "if",
        k if k.starts_with("for")
            || k.starts_with("while")
            || k.starts_with("do")
            || k.starts_with("loop") =>
        {
            "loop"
        }
        "catch_clause" | "except_clause" => "catch",
        k if k.contains("switch") || k.starts_with("match") || k.starts_with("select") => "switch",
        "ternary_expression" | "conditional_expression" => "conditional expression",
        _ => "nesting structure",
    }
}

fn name_of(function: Node, src: &[u8]) -> String {
    if let Some(name) = function.child_by_field_name("name") {
        return text(name, src);
    }
    if let Some(parent) = function.parent()
        && matches!(
            parent.kind(),
            "variable_declarator" | "pair" | "assignment_expression"
        )
        && let Some(target) = parent
            .child_by_field_name("name")
            .or_else(|| parent.child_by_field_name("key"))
            .or_else(|| parent.child_by_field_name("left"))
    {
        return text(target, src);
    }
    "<anonymous>".to_owned()
}

/// Whether `src` parses without syntax errors (tree-sitter error or
/// missing nodes). Metrics of an unparseable file are incomplete.
pub fn parses(lang: Lang, src: &[u8]) -> bool {
    let mut parser = Parser::new();
    parser.set_language(&lang.grammar()).is_ok()
        && parser
            .parse(src, None)
            .is_some_and(|t| !t.root_node().has_error())
}

/// Panel metrics of every function in `src`, ordered by position. Files the
/// grammar cannot load yield no functions.
pub fn functions(lang: Lang, src: &[u8]) -> Vec<Function> {
    let spec = lang.spec();
    let mut parser = Parser::new();
    if parser.set_language(&lang.grammar()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(src, None) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let line = LineIndex::new(src);
    let lines = code_lines(root, spec, &line);
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if spec.functions.contains(&node.kind()) && node.child_by_field_name("body").is_some() {
            found.push(node);
        }
        stack.extend(children(node));
    }
    found.sort_by_key(|n| (n.start_byte(), n.end_byte()));
    found
        .into_iter()
        .map(|fn_node| {
            let mut pass = Pass {
                spec,
                src,
                line: &line,
                cognitive: 0,
                increments: Vec::new(),
                operators: HashMap::new(),
                operands: HashMap::new(),
            };
            pass.run(fn_node);
            let (start, end) = line.span(fn_node);
            let sloc = (start..=end).filter(|l| lines.contains(l)).count();
            Function {
                name: name_of(fn_node, src),
                start,
                end,
                sloc: u32::try_from(sloc).unwrap_or(u32::MAX),
                cognitive: pass.cognitive,
                halstead_volume: pass.volume(),
                increments: {
                    let mut inc = pass.increments;
                    inc.sort_by_key(|i| i.line);
                    inc
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(lang: Lang, src: &str) -> Function {
        functions(lang, src.as_bytes()).remove(0)
    }

    #[test]
    fn python_cognitive_follows_sonar() {
        let f = one(
            Lang::Python,
            "def f(a, b):\n    if a and b or a:\n        for x in a:\n            if x:\n                pass\n    elif b:\n        pass\n    else:\n        pass\n",
        );
        // if +1, `and`/`or` sequence +2, for +2 (nested), inner if +3, elif +1, else +1
        assert_eq!(f.cognitive, 10);
        assert_eq!((f.start, f.end, f.sloc), (1, 9, 9));
        assert!(f.halstead_volume > 0.0);
    }

    #[test]
    fn nested_functions_are_measured_on_their_own() {
        let fns = functions(
            Lang::Python,
            b"def outer():\n    def inner():\n        if x:\n            pass\n    return 1\n",
        );
        assert_eq!(fns.len(), 2);
        assert_eq!(fns[0].cognitive, 0);
        assert_eq!(fns[1].cognitive, 1);
    }

    #[test]
    fn go_else_if_chain_costs_one_each() {
        let f = one(
            Lang::Go,
            "package p\nfunc f(a int) int {\n\tif a > 1 {\n\t\treturn 1\n\t} else if a > 0 {\n\t\treturn 2\n\t} else {\n\t\treturn 3\n\t}\n}\n",
        );
        assert_eq!(f.cognitive, 3);
    }

    #[test]
    fn js_counts_only_and_sequences() {
        // SonarJS: `||` and `??` sequences are not counted
        let f = one(
            Lang::JavaScript,
            "function f(a, b, c) {\n  if (a && b || c) { return 1 }\n}\n",
        );
        assert_eq!(f.cognitive, 2);
    }

    #[test]
    fn extensions_follow_the_panel_table() {
        assert_eq!(Lang::of_path("a/b.tsx"), Some(Lang::Tsx));
        assert_eq!(Lang::of_path("a/b.d.ts"), Some(Lang::TypeScript));
        assert_eq!(Lang::of_path("a/b.cjs"), Some(Lang::JavaScript));
        assert_eq!(Lang::of_path("a/b.md"), None);
    }
}
