//! `surface::surface_changes` and `impact::co_change_missing` on scratch
//! repositories.

use std::path::{Path, PathBuf};
use std::process::Command;

use emi_diff::impact::{CoChange, Dependant, ImpactOptions, Partner, co_change_missing};
use emi_diff::surface::{Surface, SurfaceKind, surface_changes};

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn write(repo: &Repo, name: &str, text: &str) {
    let path = repo.0.join(name);
    std::fs::create_dir_all(path.parent().unwrap_or(&repo.0)).unwrap_or_default();
    std::fs::write(path, text).unwrap_or_default();
}

fn commit(repo: &Repo, files: &[(&str, &str)]) -> String {
    for (name, text) in files {
        write(repo, name, text);
    }
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "--allow-empty", "-m", "c"]);
    git(&repo.0, &["rev-parse", "HEAD"])
}

fn repo(name: &str) -> Repo {
    let dir = std::env::temp_dir().join(format!("emi-places-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_default();
    git(&dir, &["init", "-q", "-b", "main"]);
    Repo(dir)
}

fn surface(r: &Repo, base: &str, head: &str) -> Surface {
    surface_changes(&r.0, base, head).unwrap_or_else(|e| panic!("{e}"))
}

/// `(file, name, kind)` of every surface change.
fn kinds(s: &Surface) -> Vec<(String, String, SurfaceKind)> {
    s.changes
        .iter()
        .map(|c| (c.file.clone(), c.name.clone(), c.kind))
        .collect()
}

fn row(file: &str, name: &str, kind: SurfaceKind) -> (String, String, SurfaceKind) {
    (file.into(), name.into(), kind)
}

#[test]
fn python_removed_and_changed_public_functions() {
    let r = repo("py");
    let base = commit(
        &r,
        &[(
            "api.py",
            "def f(a):\n    return a\n\n\ndef g(a):\n    return a\n",
        )],
    );
    let head = commit(
        &r,
        &[(
            "api.py",
            "def g(a, b=1):\n    return a\n\n\ndef _private():\n    return 0\n",
        )],
    );
    let s = surface(&r, &base, &head);
    assert_eq!(
        kinds(&s),
        vec![
            row("api.py", "f", SurfaceKind::Removed),
            row("api.py", "g", SurfaceKind::SignatureChanged),
        ]
    );
    let g = &s.changes[1];
    assert_eq!(g.old_signature.as_deref(), Some("(a)"));
    assert_eq!(g.new_signature.as_deref(), Some("(a, b=1)"));
    assert!(s.unparsed.is_empty() && s.boundaries.is_empty());
}

#[test]
fn python_all_decides_what_is_public() {
    let r = repo("all");
    let base = commit(
        &r,
        &[(
            "m.py",
            "__all__ = ['f']\n\ndef f():\n    pass\n\ndef g(a):\n    pass\n",
        )],
    );
    // g is not in __all__: neither its new signature nor h is public
    let head = commit(
        &r,
        &[(
            "m.py",
            "__all__ = ['f']\n\ndef f():\n    pass\n\ndef g(a, b):\n    pass\n\ndef h():\n    pass\n",
        )],
    );
    assert!(kinds(&surface(&r, &base, &head)).is_empty());
    let exported = commit(
        &r,
        &[(
            "m.py",
            "__all__ = ['f', 'h']\n\ndef f():\n    pass\n\ndef g(a, b):\n    pass\n\ndef h():\n    pass\n",
        )],
    );
    assert_eq!(
        kinds(&surface(&r, &head, &exported)),
        vec![row("m.py", "h", SurfaceKind::Added)]
    );
    // an __all__ built at run time cannot be read: unknown, not unchanged
    let dynamic = commit(
        &r,
        &[("m.py", "__all__ = ['f'] + extra\n\ndef f():\n    pass\n")],
    );
    let s = surface(&r, &exported, &dynamic);
    assert!(s.changes.is_empty());
    assert_eq!(s.unparsed, ["m.py"]);
}

#[test]
fn go_rust_and_js_exports_added_and_removed() {
    let r = repo("native");
    let base = commit(
        &r,
        &[
            ("p/p.go", "package p\n\nfunc Old() {}\n\nfunc keep() {}\n"),
            ("src/lib.rs", "pub fn old() {}\nfn private() {}\n"),
            ("web/m.ts", "export function old(a: number) {}\n"),
        ],
    );
    let head = commit(
        &r,
        &[
            (
                "p/p.go",
                "package p\n\nfunc New(a int) error { return nil }\n\nfunc keep() {}\n\nfunc hidden() {}\n",
            ),
            (
                "src/lib.rs",
                "pub fn new(a: u8) {}\nfn private() {}\nfn more() {}\n",
            ),
            (
                "web/m.ts",
                "export function fresh() {}\nfunction local() {}\n",
            ),
        ],
    );
    let s = surface(&r, &base, &head);
    assert_eq!(
        kinds(&s),
        vec![
            row("p/p.go", "New", SurfaceKind::Added),
            row("p/p.go", "Old", SurfaceKind::Removed),
            row("src/lib.rs", "new", SurfaceKind::Added),
            row("src/lib.rs", "old", SurfaceKind::Removed),
            row("web/m.ts", "fresh", SurfaceKind::Added),
            row("web/m.ts", "old", SurfaceKind::Removed),
        ]
    );
    assert_eq!(s.changes[0].new_signature.as_deref(), Some("(a int) error"));
}

#[test]
fn boundary_touches_are_labelled() {
    let r = repo("bound");
    let base = commit(&r, &[("README.md", "x\n")]);
    let head = commit(
        &r,
        &[
            ("app/migrations/0002_x.py", "X = 1\n"),
            ("api.proto", "syntax = \"proto3\";\n"),
        ],
    );
    let s = surface(&r, &base, &head);
    let got: Vec<(&str, &str)> = s
        .boundaries
        .iter()
        .map(|b| (b.file.as_str(), b.label))
        .collect();
    assert_eq!(
        got,
        [
            ("api.proto", "protobuf"),
            ("app/migrations/0002_x.py", "migration")
        ]
    );
    assert_eq!(s.unsupported, ["api.proto"]);
}

fn impact(r: &Repo, base: &str, head: &str) -> emi_diff::impact::Impact {
    co_change_missing(&r.0, base, head, &ImpactOptions::default()).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn co_changed_partner_missing_from_the_change() {
    let r = repo("cochange");
    commit(&r, &[("d.py", "D = 0\n")]);
    let mut base = String::new();
    for i in 0..5 {
        let (a, b) = (format!("A = {i}\n"), format!("B = {i}\n"));
        base = commit(&r, &[("a.py", &a), ("b.py", &b)]);
    }
    // after the base, a.py changes with d.py six times (d.py ends where it
    // began): that history is the change's own and must not count
    for i in 0..6 {
        let d = if i % 2 == 0 { "D = 1\n" } else { "D = 0\n" };
        commit(&r, &[("a.py", &format!("A = {}\n", 10 + i)), ("d.py", d)]);
    }
    let head = git(&r.0, &["rev-parse", "HEAD"]);
    let found = impact(&r, &base, &head);
    let CoChange::Verified {
        commits,
        missing,
        total,
        too_few_revisions,
    } = found.co_change
    else {
        panic!("unverified: {:?}", found.co_change)
    };
    assert_eq!(commits, 6);
    assert_eq!(total, 1);
    assert_eq!(
        missing,
        [Partner {
            touched: "a.py".into(),
            partner: "b.py".into(),
            shared_commits: 5,
            degree: 100.0,
        }]
    );
    assert!(too_few_revisions.is_empty());
}

#[test]
fn too_little_history_is_unverified() {
    let r = repo("young");
    let base = commit(&r, &[("a.py", "A = 1\n")]);
    let head = commit(&r, &[("a.py", "A = 2\n")]);
    let CoChange::Unverified { reason } = impact(&r, &base, &head).co_change else {
        panic!("a one-commit history cannot show coupling")
    };
    assert!(reason.contains("1 commit"), "{reason}");
}

#[test]
fn shallow_clone_is_unverified() {
    let r = repo("deep");
    for i in 0..6 {
        commit(
            &r,
            &[
                ("a.py", &format!("A = {i}\n")),
                ("b.py", &format!("B = {i}\n")),
            ],
        );
    }
    let shallow = repo("shallow");
    let _ = std::fs::remove_dir_all(&shallow.0);
    let url = format!("file://{}", r.0.display());
    let parent = shallow.0.parent().unwrap_or(&shallow.0);
    let target = shallow.0.to_string_lossy().into_owned();
    git(parent, &["clone", "-q", "--depth", "2", &url, &target]);
    let CoChange::Unverified { reason } = impact(&shallow, "HEAD~1", "HEAD").co_change else {
        panic!("a shallow clone's history is truncated")
    };
    assert!(reason.contains("shallow"), "{reason}");
}

#[test]
fn importers_of_a_touched_module() {
    let r = repo("deps");
    let base = commit(
        &r,
        &[
            ("pkg/__init__.py", ""),
            ("pkg/a.py", "def f():\n    return 1\n"),
            (
                "pkg/c.py",
                "from pkg.a import f\n\n\ndef g():\n    return f()\n",
            ),
            ("pkg/e.py", "def h():\n    return 2\n"),
        ],
    );
    let head = commit(&r, &[("pkg/a.py", "def f():\n    return 3\n")]);
    let d = impact(&r, &base, &head).dependants;
    assert_eq!(d.status(), "verified");
    assert_eq!(
        d.items,
        [Dependant {
            file: "pkg/c.py".into(),
            imports: "pkg/a.py".into(),
        }]
    );
    assert_eq!(d.total, 1);
}
