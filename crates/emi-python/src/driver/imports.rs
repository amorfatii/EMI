//! Whether the tests would import the code being checked: a project's
//! own top-level names, looked up by the Python that runs the tests.

use std::collections::BTreeSet;

use emi_driver::Run;

use crate::proving::pytest::Command;

/// Most names the import check looks up.
const MAX_OWN_NAMES: usize = 30;

/// Where each of the project's own top-level names would be imported from,
/// seen by the Python that runs the tests, in the directory the tests run
/// in. A name found outside that directory is an installed copy (an
/// editable install of another checkout, a package in site-packages): the
/// tests would then exercise that copy, not the candidate. A name that
/// cannot be found is left to the tests themselves to report.
const IMPORT_ORIGINS: &str = r#"
import importlib.util, os, sys
here = os.path.realpath(os.getcwd()) + os.sep
foreign = []
for name in sys.argv[1:]:
    if name in getattr(sys, "stdlib_module_names", ()):
        continue
    try:
        spec = importlib.util.find_spec(name)
    except Exception:
        spec = None
    if spec is None:
        continue
    origin = spec.origin
    if origin is None and spec.submodule_search_locations:
        origin = list(spec.submodule_search_locations)[0]
    if origin and not (os.path.realpath(origin) + os.sep).startswith(here):
        foreign.append(f"{name} from {origin}")
if foreign:
    print("the tests would not import the code being checked: " + "; ".join(foreign))
    sys.exit(3)
print("own code is imported from the checked copy")
"#;

/// Names that are files of a repository, not code its tests import.
const NOT_OWN: &[&str] = &["setup", "conftest", "noxfile", "tests", "test", "docs"];

fn identifier(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The top-level name `path` gives under import root `root`: a package
/// (`root/pkg/__init__.py`) anywhere, a module (`root/mod.py`) only under
/// a root other than the repository's (a module at the repository root may
/// share its name with anything).
fn own_name<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    let rest = if root == "." {
        path
    } else {
        path.strip_prefix(root)?.strip_prefix('/')?
    };
    let name = match rest.split_once('/') {
        Some((pkg, "__init__.py")) => pkg,
        None if root != "." => rest.strip_suffix(".py")?,
        _ => return None,
    };
    (identifier(name) && !NOT_OWN.contains(&name)).then_some(name)
}

/// The project's own top-level importable names among `files` (the
/// baseline's, so that the same baseline and configuration always give
/// the same check).
fn own_names(run: &Run, files: &[String]) -> Vec<String> {
    let mut roots: Vec<&str> = run.roots.iter().map(String::as_str).collect();
    roots.extend([run.source, "."]);
    let is_test = |p: &str| {
        run.test_globs
            .iter()
            .any(|g| emi_kernel::glob::fnmatch(p, g))
    };
    let mut names: BTreeSet<String> = BTreeSet::new();
    for path in files.iter().filter(|p| !is_test(p)) {
        names.extend(
            roots
                .iter()
                .filter_map(|r| own_name(path, r))
                .map(str::to_owned),
        );
    }
    names.into_iter().take(MAX_OWN_NAMES).collect()
}

/// The import check for `run`, when its command is pytest and the project
/// has names of its own.
pub fn check(run: &Run, files: &[String]) -> Option<Vec<String>> {
    let pytest = Command::parse(run.command).ok()?;
    let own = own_names(run, files);
    if own.is_empty() {
        return None;
    }
    let mut argv = vec![
        pytest.interpreter().to_owned(),
        "-c".to_owned(),
        IMPORT_ORIGINS.to_owned(),
    ];
    argv.extend(own);
    Some(argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn own_names_are_the_packages_and_src_modules_of_the_baseline() {
        let files: Vec<String> = [
            "src/app/__init__.py",
            "src/tool.py",
            "pkg/__init__.py",
            "top.py",
            "setup.py",
            "tests/__init__.py",
            "tests/test_a.py",
            "docs/conf.py",
        ]
        .map(str::to_owned)
        .to_vec();
        let (roots, globs) = (["src".to_owned()], ["tests/*".to_owned()]);
        let run = Run {
            repo: Path::new("."),
            view: Path::new("."),
            baseline: "b",
            candidate: "c",
            command: &[],
            test_globs: &globs,
            source: "src",
            link: &[],
            pins: Vec::new(),
            roots: &roots,
            cache: None,
        };
        // a module at the repository root is not looked up, nor are tests
        assert_eq!(own_names(&run, &files), ["app", "pkg", "tool"]);
    }
}
