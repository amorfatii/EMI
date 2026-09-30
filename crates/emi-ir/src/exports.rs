//! Names a file makes visible to other modules, as the adapters report them.

/// One exported name. `signature` is the declared parameter (and return)
/// text of a function, `None` for anything that has no parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    pub name: String,
    pub signature: Option<String>,
}

impl Export {
    pub fn new(name: impl Into<String>, signature: Option<String>) -> Self {
        Export {
            name: name.into(),
            signature,
        }
    }
}

/// Signature text with layout removed: no space inside brackets or before
/// a comma, one after it, and no trailing comma before a closing bracket,
/// so reformatting a parameter list does not read as a signature change.
pub fn signature(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut space = false;
    for c in raw.chars() {
        match c {
            c if c.is_whitespace() => space = true,
            ',' => {
                out.push(',');
                space = true;
            }
            ')' | ']' => {
                if out.ends_with(',') {
                    out.pop();
                }
                out.push(c);
                space = false;
            }
            _ => {
                if space && !out.is_empty() && !out.ends_with(['(', '[']) {
                    out.push(' ');
                }
                out.push(c);
                space = false;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_not_a_signature_change() {
        assert_eq!(
            signature("(\n    a,\n    b: int = 1,\n)"),
            "(a, b: int = 1)"
        );
        assert_eq!(signature("(a,b)"), "(a, b)");
        assert_eq!(signature("( a ,  b ) -> int"), "(a, b) -> int");
        assert_eq!(signature("(xs: list[ int , ])"), "(xs: list[int])");
    }
}
