//! Print `file<TAB>kind<TAB>lineno<TAB>col` for every positioned node in
//! `ast.walk` order (tools/walk_check.py compares this with CPython).

use std::path::Path;

fn visit(root: &Path, dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            visit(root, &path);
        } else if path.extension().is_some_and(|e| e == "py") {
            let Ok(source) = std::fs::read_to_string(&path) else { continue };
            let Ok(nodes) = emi_lang_python::walk_positions(&source) else { continue };
            let rel = path.strip_prefix(root).unwrap_or(&path).display().to_string();
            for (kind, line, col) in nodes {
                println!("{rel}\t{kind}\t{line}\t{col}");
            }
        }
    }
}

fn main() {
    let root = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    visit(Path::new(&root), Path::new(&root));
}
