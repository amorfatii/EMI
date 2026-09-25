//! Print `file<TAB>def line<TAB>exact<TAB>normalized` body fingerprints for
//! every `.py` file under a directory (tools/clone_partition.py compares the
//! resulting partitions with CPython's `ast.dump`).

use std::path::Path;

fn visit(root: &Path, dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            visit(root, &path);
        } else if path.extension().is_some_and(|e| e == "py") {
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(fps) = emi_lang_python::all_fingerprints(&source) else {
                continue;
            };
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            for (line, exact, norm) in fps {
                let hex = |b: [u8; 32]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
                println!("{rel}\t{line}\t{}\t{}", hex(exact), hex(norm));
            }
        }
    }
}

fn main() {
    let root = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    visit(Path::new(&root), Path::new(&root));
}
