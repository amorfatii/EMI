//! Stand-ins: trivial bodies a function could have that a law worth the
//! name must reject. A law that still holds when `cancel(order)` just
//! returns `order` says nothing about cancelling; one that holds when the
//! function raises says nothing at all. Each stand-in is the source file
//! with one function's body replaced, signature and decorators kept.

use tree_sitter::Node;

use crate::syntax::{Function, parse};

/// One trivial version of a function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StandIn {
    /// What the stand-in does, for a person: "returns its first argument".
    pub label: String,
    /// The whole file with the function's body replaced.
    pub source: Vec<u8>,
}

fn text(node: Node, src: &[u8]) -> String {
    String::from_utf8_lossy(&src[node.start_byte()..node.end_byte()]).into_owned()
}

/// The definition whose body is `body` (byte range).
fn definition(root: Node, body: (usize, usize)) -> Option<Node> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_definition"
            && node
                .child_by_field_name("body")
                .is_some_and(|b| (b.start_byte(), b.end_byte()) == body)
        {
            return Some(node);
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    None
}

/// The name a parameter binds, when it binds one positionally.
fn parameter_name(p: Node, src: &[u8]) -> Option<String> {
    match p.kind() {
        "identifier" => Some(text(p, src)),
        "typed_parameter" => {
            let mut cursor = p.walk();
            let first = p.named_children(&mut cursor).next()?;
            (first.kind() == "identifier").then(|| text(first, src))
        }
        "default_parameter" | "typed_default_parameter" => {
            p.child_by_field_name("name").map(|n| text(n, src))
        }
        _ => None,
    }
}

/// The first parameter that is not the receiver (`self`, `cls`).
fn first_argument(def: Node, src: &[u8]) -> Option<String> {
    let params = def.child_by_field_name("parameters")?;
    let mut cursor = params.walk();
    let names: Vec<String> = params
        .named_children(&mut cursor)
        .filter_map(|p| parameter_name(p, src))
        .collect();
    names.into_iter().find(|n| n != "self" && n != "cls")
}

/// The value a return annotation's type defaults to, for the plain types.
fn annotation_default(def: Node, src: &[u8]) -> Option<&'static str> {
    let annotation = text(def.child_by_field_name("return_type")?, src);
    let head = annotation.split('[').next().unwrap_or("").trim();
    match head {
        "int" => Some("0"),
        "float" => Some("0.0"),
        "str" => Some("\"\""),
        "bytes" => Some("b\"\""),
        "bool" => Some("False"),
        "list" | "List" | "Sequence" => Some("[]"),
        "dict" | "Dict" | "Mapping" => Some("{}"),
        "set" | "Set" | "frozenset" => Some("set()"),
        "tuple" | "Tuple" => Some("()"),
        _ => None,
    }
}

/// The stand-ins for `f` in `src`, in a fixed order. A function whose
/// definition cannot be found again (the file changed) has none.
pub fn stand_ins(src: &[u8], f: &Function) -> Vec<StandIn> {
    let Some(tree) = parse(src) else {
        return Vec::new();
    };
    let Some(def) = definition(tree.root_node(), f.body) else {
        return Vec::new();
    };
    let mut bodies: Vec<(String, String)> = Vec::new();
    if let Some(arg) = first_argument(def, src) {
        bodies.push((
            format!("returns its first argument (`{arg}`) unchanged"),
            format!("return {arg}"),
        ));
    }
    bodies.push(("returns None".to_owned(), "return None".to_owned()));
    if let Some(value) = annotation_default(def, src) {
        bodies.push((format!("returns `{value}`"), format!("return {value}")));
    }
    bodies.push((
        "raises NotImplementedError".to_owned(),
        "raise NotImplementedError".to_owned(),
    ));
    bodies
        .into_iter()
        .map(|(label, body)| {
            let mut out = src[..f.body.0].to_vec();
            out.extend_from_slice(body.as_bytes());
            out.extend_from_slice(&src[f.body.1..]);
            StandIn { label, source: out }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::functions;

    fn of(src: &str, name: &str) -> Vec<(String, String)> {
        let fns = functions(src.as_bytes()).unwrap_or_default();
        let f = fns
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no {name}"));
        stand_ins(src.as_bytes(), f)
            .into_iter()
            .map(|s| (s.label, String::from_utf8_lossy(&s.source).into_owned()))
            .collect()
    }

    #[test]
    fn a_function_gets_identity_none_default_and_raise() {
        let src = "def cancel(order, *, now=None) -> int:\n    \"\"\"Doc.\"\"\"\n    order.state = 'x'\n    return order\n\nOTHER = 1\n";
        let got = of(src, "cancel");
        let labels: Vec<&str> = got.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            labels,
            [
                "returns its first argument (`order`) unchanged",
                "returns None",
                "returns `0`",
                "raises NotImplementedError"
            ]
        );
        assert_eq!(
            got[0].1,
            "def cancel(order, *, now=None) -> int:\n    return order\n\nOTHER = 1\n"
        );
        for (_, file) in &got {
            assert!(crate::syntax::parses(file.as_bytes()), "{file}");
        }
    }

    #[test]
    fn methods_skip_the_receiver_and_nested_code_keeps_its_indentation() {
        let src = "class Order:\n    @staticmethod\n    def make(): return 1\n\n    def total(self, tax: float = 0.0):\n        if tax:\n            return 1\n        return 0\n";
        let total = of(src, "total");
        assert_eq!(total[0].0, "returns its first argument (`tax`) unchanged");
        assert!(
            total[0]
                .1
                .contains("    def total(self, tax: float = 0.0):\n        return tax\n")
        );
        let make = of(src, "make");
        assert_eq!(make[0].0, "returns None", "no argument to return");
        assert!(make[0].1.contains("def make(): return None\n"));
        for (_, file) in total.iter().chain(&make) {
            assert!(crate::syntax::parses(file.as_bytes()), "{file}");
        }
    }
}
