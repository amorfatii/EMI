//! A file's public items: `pub` items at the top level and in `pub` inline
//! modules, and `pub` methods of inherent impls, named as region ids are
//! (`module.Type.method`). Restricted visibility (`pub(crate)`) is not
//! public; `pub use` re-exports and trait impls are not listed.

use emi_ir::exports::{Export, signature};
use emi_lang_common::{children, text};
use tree_sitter::Node;

use crate::is_test_item;

const NAMED: [&str; 10] = [
    "function_item",
    "function_signature_item",
    "struct_item",
    "enum_item",
    "union_item",
    "trait_item",
    "type_item",
    "const_item",
    "static_item",
    "mod_item",
];

fn is_pub(item: Node, src: &[u8]) -> bool {
    children(item)
        .iter()
        .any(|c| c.kind() == "visibility_modifier" && text(*c, src) == "pub")
}

fn function_signature(item: Node, src: &[u8]) -> Option<String> {
    let params = item.child_by_field_name("parameters")?;
    let mut raw = text(params, src).to_owned();
    if let Some(ret) = item.child_by_field_name("return_type") {
        raw.push_str(" -> ");
        raw.push_str(text(ret, src));
    }
    Some(signature(&raw))
}

/// `pub` methods of an inherent impl, as `Type.method`.
fn impl_methods(item: Node, src: &[u8], prefix: &str, out: &mut Vec<Export>) {
    let (Some(ty), Some(body)) = (
        item.child_by_field_name("type"),
        item.child_by_field_name("body"),
    ) else {
        return;
    };
    let ty = text(ty, src);
    let ty = ty.split('<').next().unwrap_or(ty).trim();
    for f in children(body) {
        if f.kind() == "function_item"
            && is_pub(f, src)
            && let Some(name) = f.child_by_field_name("name")
        {
            let name = format!("{prefix}{ty}.{}", text(name, src));
            out.push(Export::new(name, function_signature(f, src)));
        }
    }
}

fn items(scope: Node, src: &[u8], prefix: &str, out: &mut Vec<Export>) {
    for item in children(scope) {
        if is_test_item(item, src) {
            continue;
        }
        if item.kind() == "impl_item" && item.child_by_field_name("trait").is_none() {
            impl_methods(item, src, prefix, out);
            continue;
        }
        if !NAMED.contains(&item.kind()) || !is_pub(item, src) {
            continue;
        }
        let Some(name) = item.child_by_field_name("name") else {
            continue;
        };
        let name = format!("{prefix}{}", text(name, src));
        out.push(Export::new(name.clone(), function_signature(item, src)));
        if let Some(body) = item.child_by_field_name("body")
            && item.kind() == "mod_item"
        {
            items(body, src, &format!("{name}."), out);
        }
    }
}

/// Public items of a file in source order, each function with its parameter
/// and return type text. `None` when the file does not parse cleanly.
pub fn exports(source: &str) -> Option<Vec<Export>> {
    let tree = crate::parse(source).filter(|t| !t.root_node().has_error())?;
    let mut out = Vec::new();
    items(tree.root_node(), source.as_bytes(), "", &mut out);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_items_only() {
        let src = "pub fn f(a: u32,) -> u32 { a }\nfn g() {}\npub(crate) fn h() {}\n\
                   pub struct S;\nimpl S { pub fn new() -> S { S } fn private(&self) {} }\n\
                   impl Clone for S { fn clone(&self) -> S { S } }\n\
                   pub mod m { pub const N: u32 = 1; }\n#[cfg(test)]\npub mod tests {}\n";
        let got: Vec<(String, Option<String>)> = exports(src)
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.name, e.signature))
            .collect();
        assert_eq!(
            got,
            vec![
                ("f".into(), Some("(a: u32) -> u32".into())),
                ("S".into(), None),
                ("S.new".into(), Some("() -> S".into())),
                ("m".into(), None),
                ("m.N".into(), None),
            ]
        );
        assert_eq!(exports("pub fn f( {"), None);
    }
}
