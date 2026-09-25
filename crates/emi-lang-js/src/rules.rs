//! Waste and error-masking rules of `js_probe.waste_findings`, reproduced on
//! the same tree-sitter trees. Findings are produced in the oracle's
//! insertion order (main walk, JSDoc pass, single-use pass) and sorted
//! stably by (first line, rule).

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

use emi_ir::pytext::lower_ascii_words;
use regex::Regex;
use tree_sitter::{Node, Tree};

use crate::names::{FUNCTIONS, region_name};
use crate::parse::{push_children, row};

/// Rules whose lines carry V mass; all others are diagnostics.
pub const SCORING_RULES: [&str; 5] = [
    "waste-len-check",
    "waste-bool-return",
    "mask-empty-catch",
    "mask-noop-catch",
    "rep-line",
];

#[derive(Clone, Debug)]
pub struct RuleHit {
    pub rule: &'static str,
    pub lines: Vec<u32>,
}

fn text<'s>(node: Node, src: &'s [u8]) -> Cow<'s, str> {
    String::from_utf8_lossy(&src[node.start_byte()..node.end_byte()])
}

fn node_lines(node: Node) -> Vec<u32> {
    (row(node.start_position().row)..=row(node.end_position().row)).collect()
}

/// Pre-order walk over every node (named and anonymous), like `_walk`.
fn walk(root: Node) -> Vec<Node> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        out.push(node);
        push_children(&mut stack, node);
    }
    out
}

fn children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

fn named_children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn contains(ancestor: Node, node: Node) -> bool {
    let mut current = Some(node);
    while let Some(c) = current {
        if c.id() == ancestor.id() {
            return true;
        }
        current = c.parent();
    }
    false
}

const PATTERN_TYPES: [&str; 7] = [
    "assignment_pattern",
    "object_pattern",
    "pair_pattern",
    "array_pattern",
    "rest_pattern",
    "required_parameter",
    "optional_parameter",
];

/// True when an identifier occurrence binds a name rather than reads one.
pub fn is_binding_position(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let kind = parent.kind();
    if matches!(
        kind,
        "variable_declarator"
            | "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "generator_function"
            | "class_declaration"
    ) && parent
        .child_by_field_name("name")
        .is_some_and(|n| n.id() == node.id())
    {
        return true;
    }
    let field_contains = |field: &str| {
        parent
            .child_by_field_name(field)
            .is_some_and(|f| contains(f, node))
    };
    if (kind == "catch_clause" && field_contains("parameter"))
        || (kind == "for_in_statement" && field_contains("left"))
    {
        return true;
    }
    if matches!(
        kind,
        "import_specifier" | "export_specifier" | "namespace_import" | "namespace_export"
    ) {
        return true;
    }
    let (mut child, mut ancestor) = (node, Some(parent));
    while let Some(a) = ancestor {
        if a.kind() == "formal_parameters" {
            return true;
        }
        if !PATTERN_TYPES.contains(&a.kind()) {
            break;
        }
        if a.kind() == "assignment_pattern"
            && !a
                .child_by_field_name("left")
                .is_some_and(|l| contains(l, child))
        {
            return false;
        }
        child = a;
        ancestor = a.parent();
    }
    false
}

/// Expression returned by a lone `return x;` (bare or in a one-statement block).
fn single_return_expr(node: Node) -> Option<Node> {
    let target = if node.kind() == "statement_block" {
        match named_children(node).as_slice() {
            [only] => *only,
            _ => return None,
        }
    } else {
        node
    };
    if target.kind() != "return_statement" {
        return None;
    }
    match named_children(target).as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

fn single_return<'s>(node: Node, src: &'s [u8]) -> Option<Cow<'s, str>> {
    single_return_expr(node)
        .filter(|e| e.kind() == "identifier")
        .map(|e| text(e, src))
}

fn bool_literal(node: Option<Node>) -> Option<&'static str> {
    match node?.kind() {
        "true" => Some("true"),
        "false" => Some("false"),
        _ => None,
    }
}

struct Rules<'s> {
    src: &'s [u8],
    hits: Vec<RuleHit>,
}

impl<'s> Rules<'s> {
    fn flag(&mut self, node: Node, rule: &'static str) {
        self.hits.push(RuleHit {
            rule,
            lines: node_lines(node),
        });
    }

    fn identity_arrow(&mut self, node: Node) {
        let (Some(params), Some(body)) = (
            node.child_by_field_name("parameters"),
            node.child_by_field_name("body"),
        ) else {
            return;
        };
        if params.kind() != "formal_parameters" {
            return;
        }
        let names: Vec<Node> = children(params)
            .into_iter()
            .filter(|c| c.kind() == "identifier")
            .collect();
        let [name] = names.as_slice() else { return };
        let name = text(*name, self.src);
        let identity = match body.kind() {
            "identifier" => text(body, self.src) == name,
            "statement_block" => single_return(body, self.src).is_some_and(|r| r == name),
            _ => false,
        };
        if identity {
            self.flag(node, "waste-identity-arrow");
        }
    }

    fn len_check(&mut self, node: Node) {
        if node.child_count() != 3 {
            return;
        }
        let kids = children(node);
        if !matches!(
            kids[1].kind(),
            "==" | "!=" | "===" | "!==" | "<" | "<=" | ">" | ">="
        ) {
            return;
        }
        let is_len = |n: Node| {
            n.kind() == "member_expression"
                && n.child_by_field_name("property")
                    .is_some_and(|p| text(p, self.src) == "length")
        };
        let is_zero = |n: Node| n.kind() == "number" && text(n, self.src) == "0";
        if (is_len(kids[0]) && is_zero(kids[2])) || (is_len(kids[2]) && is_zero(kids[0])) {
            self.flag(node, "waste-len-check");
        }
    }

    fn bool_return(&mut self, node: Node) {
        let (first, second) = if node.kind() == "if_statement" {
            let Some(cons) = node.child_by_field_name("consequence") else {
                return;
            };
            let Some(mut alt) = node.child_by_field_name("alternative") else {
                return;
            };
            if alt.kind() == "else_clause" {
                match named_children(alt).last() {
                    Some(inner) => alt = *inner,
                    None => return,
                }
            }
            (
                bool_literal(single_return_expr(cons)),
                bool_literal(single_return_expr(alt)),
            )
        } else {
            (
                bool_literal(node.child_by_field_name("consequence")),
                bool_literal(node.child_by_field_name("alternative")),
            )
        };
        if let (Some(a), Some(b)) = (first, second)
            && a != b
        {
            self.flag(node, "waste-bool-return");
        }
    }

    /// `for (let i = 0; i < arr.length; i++)` whose body only reads `arr[i]`.
    fn for_index(&mut self, node: Node) {
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let Some(init) = node
            .child_by_field_name("initializer")
            .filter(|i| i.kind() == "lexical_declaration")
        else {
            return;
        };
        let declarators: Vec<Node> = children(init)
            .into_iter()
            .filter(|c| c.kind() == "variable_declarator")
            .collect();
        let [decl] = declarators.as_slice() else {
            return;
        };
        let (Some(name), Some(value)) = (
            decl.child_by_field_name("name"),
            decl.child_by_field_name("value"),
        ) else {
            return;
        };
        if name.kind() != "identifier" || value.kind() != "number" || text(value, self.src) != "0" {
            return;
        }
        let ivar = text(name, self.src);
        let Some(arr_text) = self.length_bound(node.child_by_field_name("condition"), &ivar) else {
            return;
        };
        if !self.increments(node.child_by_field_name("increment"), &ivar) {
            return;
        }
        let (mut seen_use, mut clean) = (false, true);
        for sub in walk(body) {
            if sub.kind() != "identifier" || text(sub, self.src) != ivar {
                continue;
            }
            let indexes = sub
                .parent()
                .is_some_and(|p| self.subscript_of(p, &ivar, &arr_text));
            if is_binding_position(sub) || !indexes {
                clean = false;
                break;
            }
            seen_use = true;
        }
        if seen_use && clean {
            self.flag(node, "waste-for-index");
        }
    }

    /// Text of `arr` when `cond` is `ivar < arr.length`.
    fn length_bound(&self, cond: Option<Node>, ivar: &str) -> Option<String> {
        let cond = cond.filter(|c| c.kind() == "binary_expression" && c.child_count() == 3)?;
        let kids = children(cond);
        let ok = kids[1].kind() == "<"
            && kids[0].kind() == "identifier"
            && text(kids[0], self.src) == ivar
            && kids[2].kind() == "member_expression";
        if !ok {
            return None;
        }
        let prop = kids[2].child_by_field_name("property")?;
        let obj = kids[2].child_by_field_name("object")?;
        (text(prop, self.src) == "length").then(|| text(obj, self.src).into_owned())
    }

    fn increments(&self, incr: Option<Node>, ivar: &str) -> bool {
        let Some(incr) = incr.filter(|i| i.kind() == "update_expression") else {
            return false;
        };
        let kids = children(incr);
        kids.iter()
            .any(|c| !c.is_named() && text(*c, self.src) == "++")
            && kids
                .iter()
                .any(|c| c.kind() == "identifier" && text(*c, self.src) == ivar)
    }

    fn subscript_of(&self, node: Node, ivar: &str, arr_text: &str) -> bool {
        if node.kind() != "subscript_expression" {
            return false;
        }
        let (Some(obj), Some(index)) = (
            node.child_by_field_name("object"),
            node.child_by_field_name("index"),
        ) else {
            return false;
        };
        index.kind() == "identifier"
            && text(index, self.src) == ivar
            && text(obj, self.src) == arr_text
    }

    fn empty_block(node: Option<Node>) -> bool {
        node.is_some_and(|b| b.kind() == "statement_block" && named_children(b).is_empty())
    }

    fn empty_catch(&mut self, node: Node) {
        if Self::empty_block(node.child_by_field_name("body")) {
            self.flag(node, "mask-empty-catch");
        }
    }

    fn noop_catch(&mut self, node: Node) {
        let (Some(func), Some(args)) = (
            node.child_by_field_name("function"),
            node.child_by_field_name("arguments"),
        ) else {
            return;
        };
        if func.kind() != "member_expression"
            || !func
                .child_by_field_name("property")
                .is_some_and(|p| text(p, self.src) == "catch")
        {
            return;
        }
        if let [handler] = named_children(args).as_slice()
            && matches!(handler.kind(), "arrow_function" | "function_expression")
            && Self::empty_block(handler.child_by_field_name("body"))
        {
            self.flag(node, "mask-noop-catch");
        }
    }

    fn main_walk(&mut self, root: Node) {
        for node in walk(root) {
            match node.kind() {
                "arrow_function" => self.identity_arrow(node),
                "binary_expression" => self.len_check(node),
                "if_statement" | "ternary_expression" => self.bool_return(node),
                "for_statement" => self.for_index(node),
                "catch_clause" => self.empty_catch(node),
                "call_expression" => self.noop_catch(node),
                _ => {}
            }
        }
    }
}

// ---- JSDoc restatement --------------------------------------------------

const DOC_STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "to", "for", "with", "by", "on", "in", "is", "are", "be",
    "as", "at", "from", "that", "this", "it", "its", "given", "using", "into", "over",
];

static TAG: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^@\w+(?:\s*\{[^}]*\})?\s*\[?[\w.]+\]?\s*-?\s*(.*)").ok());

/// `re.findall(r"[A-Z]?[a-z]+|[A-Z]+(?![a-z])|[a-z]+", text)`, lowercased.
fn camel_words(text: &str) -> BTreeSet<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut words = BTreeSet::new();
    let mut i = 0;
    while i < chars.len() {
        let upper = |k: usize| chars.get(k).is_some_and(char::is_ascii_uppercase);
        let lower = |k: usize| chars.get(k).is_some_and(char::is_ascii_lowercase);
        let start = i;
        let mut j = i + usize::from(upper(i));
        if lower(j) {
            while lower(j) {
                j += 1;
            }
            words.insert(chars[start..j].iter().collect::<String>().to_lowercase());
            i = j;
            continue;
        }
        if upper(i) {
            // `[A-Z]+(?![a-z])`, backtracking so the run is not followed by a lowercase.
            let mut k = i;
            while upper(k) {
                k += 1;
            }
            while k > i && lower(k) {
                k -= 1;
            }
            if k > i {
                words.insert(chars[i..k].iter().collect::<String>().to_lowercase());
                i = k;
                continue;
            }
        }
        i += 1;
    }
    words
}

struct Callable<'t> {
    start_row: usize,
    name: String,
    params: Vec<String>,
    _node: Node<'t>,
}

fn callable<'t>(node: Node<'t>, src: &[u8]) -> Option<Callable<'t>> {
    let (name, formals) = match node.kind() {
        "function_declaration" | "generator_function_declaration" => {
            let named = node
                .child_by_field_name("name")
                .filter(|n| n.kind() == "identifier")?;
            (
                text(named, src).into_owned(),
                node.child_by_field_name("parameters"),
            )
        }
        "method_definition" => (
            region_name(node, src),
            node.child_by_field_name("parameters"),
        ),
        "function_expression" | "generator_function" | "arrow_function" => {
            let parent = node
                .parent()
                .filter(|p| p.kind() == "variable_declarator")?;
            let name = parent
                .child_by_field_name("name")
                .filter(|n| n.kind() == "identifier")?;
            (
                text(name, src).into_owned(),
                node.child_by_field_name("parameters"),
            )
        }
        _ => return None,
    };
    let params = formals.map_or_else(Vec::new, |f| {
        walk(f)
            .into_iter()
            .filter(|s| s.kind() == "identifier" && is_binding_position(*s))
            .map(|s| text(s, src).into_owned())
            .collect()
    });
    Some(Callable {
        start_row: node.start_position().row,
        name,
        params,
        _node: node,
    })
}

/// Cleaned JSDoc content lines, or `None` when the block is exempt.
fn jsdoc_lines(raw: &str) -> Option<Vec<String>> {
    let inner = raw.get(3..).unwrap_or("");
    let inner = inner.strip_suffix("*/").unwrap_or(inner);
    let cleaned: Vec<String> = inner
        .split('\n')
        .map(|ln| {
            let s = emi_ir::pytext::strip(ln);
            match s.strip_prefix('*') {
                Some(rest) => emi_ir::pytext::strip(rest).to_owned(),
                None => s.to_owned(),
            }
        })
        .filter(|s| !s.is_empty())
        .collect();
    let exempt = cleaned.len() > 4
        || cleaned.iter().any(|l| {
            ["@type", "@typedef", "@callback", "@template"]
                .iter()
                .any(|p| l.starts_with(p))
        })
        || cleaned
            .iter()
            .any(|l| l.contains("{@") || l.contains("```") || l.contains(">>>"));
    (!exempt).then_some(cleaned)
}

fn restates(cleaned: &[String], callable: &Callable) -> bool {
    let (mut free, mut descs) = (Vec::new(), Vec::new());
    for ln in cleaned {
        if ln.starts_with('@') {
            let desc = TAG
                .as_ref()
                .and_then(|re| re.captures(ln))
                .and_then(|c| c.get(1))
                .map_or("", |m| m.as_str());
            descs.push(desc.to_owned());
        } else {
            free.push(ln.clone());
        }
    }
    let joined = free
        .iter()
        .chain(descs.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let content: BTreeSet<String> = lower_ascii_words(&joined)
        .into_iter()
        .filter(|w| !DOC_STOPWORDS.contains(&w.as_str()))
        .collect();
    let mut sig = camel_words(&callable.name);
    for p in &callable.params {
        sig.extend(camel_words(p));
    }
    content.is_subset(&sig) && (!content.is_empty() || free.is_empty())
}

fn jsdoc(rules: &mut Rules, root: Node) {
    let (mut comments, mut callables) = (Vec::new(), Vec::new());
    for node in walk(root) {
        if node.kind() == "comment" && text(node, rules.src).starts_with("/**") {
            comments.push(node);
            continue;
        }
        if let Some(c) = callable(node, rules.src) {
            callables.push(c);
        }
    }
    for comment in comments {
        let end_row = comment.end_position().row;
        let Some(target) = callables.iter().find(|c| c.start_row == end_row + 1) else {
            continue;
        };
        let raw = text(comment, rules.src);
        if let Some(cleaned) = jsdoc_lines(&raw)
            && restates(&cleaned, target)
        {
            rules.flag(comment, "doc-restates-signature");
        }
    }
}

// ---- single-use declarator then return ---------------------------------

fn enclosing_function(node: Node) -> Option<Node> {
    let mut current = node.parent();
    while let Some(c) = current {
        if FUNCTIONS.contains(&c.kind()) || c.kind() == "class_declaration" {
            return Some(c);
        }
        current = c.parent();
    }
    None
}

fn read_counts(func: Node, src: &[u8]) -> HashMap<String, u32> {
    let mut counts = HashMap::new();
    for sub in walk(func) {
        if sub.kind() == "identifier" && !is_binding_position(sub) {
            *counts.entry(text(sub, src).into_owned()).or_insert(0) += 1;
        }
    }
    counts
}

fn single_use_return(rules: &mut Rules, root: Node) {
    let mut counts_by_fn: HashMap<usize, HashMap<String, u32>> = HashMap::new();
    for block in walk(root) {
        if block.kind() != "statement_block" {
            continue;
        }
        let Some(func) = enclosing_function(block) else {
            continue;
        };
        let counts = counts_by_fn
            .entry(func.id())
            .or_insert_with(|| read_counts(func, rules.src));
        let mut units = Vec::new();
        for stmt in named_children(block) {
            if matches!(stmt.kind(), "lexical_declaration" | "variable_declaration") {
                units.extend(
                    children(stmt)
                        .into_iter()
                        .filter(|c| c.kind() == "variable_declarator"),
                );
            } else {
                units.push(stmt);
            }
        }
        let mut hits = Vec::new();
        for pair in units.windows(2) {
            let (first, second) = (pair[0], pair[1]);
            if first.kind() != "variable_declarator" || first.child_by_field_name("value").is_none()
            {
                continue;
            }
            let Some(name) = first
                .child_by_field_name("name")
                .filter(|n| n.kind() == "identifier")
            else {
                continue;
            };
            let name = text(name, rules.src);
            if counts.get(name.as_ref()) == Some(&1)
                && single_return(second, rules.src).is_some_and(|r| r == name)
            {
                hits.push(first);
            }
        }
        for first in hits {
            rules.flag(first, "waste-single-use-return");
        }
    }
}

/// All waste/mask findings for one file, in the oracle's order.
pub fn findings(tree: &Tree, src: &[u8]) -> Vec<RuleHit> {
    let root = tree.root_node();
    let mut rules = Rules {
        src,
        hits: Vec::new(),
    };
    rules.main_walk(root);
    jsdoc(&mut rules, root);
    single_use_return(&mut rules, root);
    let mut hits = rules.hits;
    hits.sort_by(|a, b| (a.lines[0], a.rule).cmp(&(b.lines[0], b.rule)));
    hits
}

#[cfg(test)]
mod tests {
    use super::camel_words;

    #[test]
    fn camel_words_match_python_regex() {
        let words = |s: &str| camel_words(s).into_iter().collect::<Vec<_>>();
        assert_eq!(words("getHTTPResponse"), ["get", "http", "response"]);
        assert_eq!(words("parse_url2"), ["parse", "url"]);
        assert_eq!(words("URL"), ["url"]);
        assert_eq!(words("XMLHttpRequest"), ["http", "request", "xml"]);
        assert_eq!(words("ABCdef"), ["ab", "cdef"]);
    }
}
