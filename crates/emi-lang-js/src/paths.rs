//! Python `posixpath`/`pathlib` semantics the oracle's resolver relies on.

/// `posixpath.normpath`.
pub fn normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let initial = if path.starts_with("//") && !path.starts_with("///") {
        "//"
    } else if path.starts_with('/') {
        "/"
    } else {
        ""
    };
    let mut parts: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                if !initial.is_empty() && parts.is_empty() {
                    continue;
                }
                if parts.last().is_some_and(|p| *p != "..") {
                    parts.pop();
                } else {
                    parts.push("..");
                }
            }
            c => parts.push(c),
        }
    }
    let joined = parts.join("/");
    let out = format!("{initial}{joined}");
    if out.is_empty() { ".".to_owned() } else { out }
}

/// `posixpath.join(a, b)`.
pub fn join(a: &str, b: &str) -> String {
    if b.starts_with('/') {
        b.to_owned()
    } else if a.is_empty() || a.ends_with('/') {
        format!("{a}{b}")
    } else {
        format!("{a}/{b}")
    }
}

/// `posixpath.relpath(path, start)` for absolute paths.
pub fn relpath(path: &str, start: &str) -> String {
    let (path, start) = (normpath(path), normpath(start));
    let p: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    let s: Vec<&str> = start.split('/').filter(|c| !c.is_empty()).collect();
    let common = p.iter().zip(&s).take_while(|(a, b)| a == b).count();
    let mut out: Vec<&str> = vec![".."; s.len() - common];
    out.extend(&p[common..]);
    if out.is_empty() {
        ".".to_owned()
    } else {
        out.join("/")
    }
}

/// `PurePosixPath(text).as_posix()`: drops empty and `.` components.
pub fn pure(text: &str) -> String {
    let absolute = text.starts_with('/');
    let parts: Vec<&str> = text
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    let body = parts.join("/");
    match (absolute, body.is_empty()) {
        (true, _) => format!("/{body}"),
        (false, true) => ".".to_owned(),
        (false, false) => body,
    }
}

/// `PurePath.name`.
pub fn name(path: &str) -> &str {
    let p = path.trim_end_matches('/');
    let n = p.rsplit('/').next().unwrap_or("");
    if n == "." { "" } else { n }
}

/// `PurePath.suffix` (Python 3.14): from the last dot, unless it leads.
pub fn suffix(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i..],
        _ => "",
    }
}

/// `PurePath.parent.as_posix()` of a relative posix path.
pub fn parent(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_owned(),
        None => ".".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_python() {
        assert_eq!(normpath("./a/../b//c/."), "b/c");
        assert_eq!(normpath("./../x"), "../x");
        assert_eq!(normpath("/a/../../b"), "/b");
        assert_eq!(normpath(""), ".");
        assert_eq!(relpath("/r/src/x", "/r"), "src/x");
        assert_eq!(relpath("/r/lib", "/r/sub"), "../lib");
        assert_eq!(relpath("/r", "/r"), ".");
        assert_eq!(pure("./src//x/"), "src/x");
        assert_eq!(suffix("a.test.js"), ".js");
        assert_eq!(suffix(".eslintrc"), "");
        assert_eq!(suffix("foo."), ".");
        assert_eq!(parent("x.js"), ".");
    }
}
