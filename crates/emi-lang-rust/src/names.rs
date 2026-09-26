//! Region names and qualified ids.

use tree_sitter::Node;

fn text(node: Node, src: &[u8]) -> String {
    String::from_utf8_lossy(&src[node.start_byte()..node.end_byte()]).into_owned()
}

pub fn function_name(f: Node, src: &[u8]) -> String {
    f.child_by_field_name("name")
        .map_or_else(|| "<anonymous>".into(), |n| text(n, src))
}

/// Display name of a region item: `f`, `Foo`, `impl Foo`, `impl Trait for Foo`.
pub fn item_name(item: Node, src: &[u8]) -> String {
    if item.kind() == "impl_item" {
        let ty = item
            .child_by_field_name("type")
            .map_or_else(String::new, |t| type_name(t, src));
        return match item.child_by_field_name("trait") {
            Some(tr) => format!("impl {} for {ty}", type_name(tr, src)),
            None => format!("impl {ty}"),
        };
    }
    function_name(item, src)
}

/// Base type name of an impl's type (`Foo<T>` -> `Foo`, `&mut Foo` -> `Foo`).
fn type_name(t: Node, src: &[u8]) -> String {
    let full = text(t, src);
    let trimmed = full
        .trim_start_matches('&')
        .trim_start_matches("mut ")
        .trim();
    let base = trimmed.split('<').next().unwrap_or(trimmed).trim();
    base.rsplit("::").next().unwrap_or(base).to_owned()
}

/// `mod.Type.method`, `<Type as Trait>.method`, `outer.inner` for nested fns.
pub fn qualified_name(f: Node, src: &[u8]) -> String {
    let mut parts = vec![item_name(f, src)];
    let mut current = f.parent();
    while let Some(n) = current {
        match n.kind() {
            "impl_item" => {
                let ty = n
                    .child_by_field_name("type")
                    .map_or_else(String::new, |t| type_name(t, src));
                match n.child_by_field_name("trait") {
                    Some(tr) => parts.push(format!("<{ty} as {}>", type_name(tr, src))),
                    None => parts.push(ty),
                }
            }
            "trait_item" | "mod_item" | "function_item" => {
                if let Some(name) = n.child_by_field_name("name") {
                    parts.push(text(name, src));
                }
            }
            _ => {}
        }
        current = n.parent();
    }
    parts.reverse();
    parts.join(".")
}
