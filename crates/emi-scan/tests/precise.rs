//! The precise profile: static coupling through value origins. Each case
//! pins both directions — what it now resolves, and what must stay
//! unknown because a first-party target cannot be ruled out.

use std::path::{Path, PathBuf};

use emi_scan::{Options, Probe, Profile, scan};

struct Tree(PathBuf);

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tree(name: &str, files: &[(&str, &str)]) -> Tree {
    let root = std::env::temp_dir().join(format!("emi-precise-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (rel, text) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap_or(&root)).unwrap_or_default();
        std::fs::write(path, text).unwrap_or_default();
    }
    Tree(root)
}

type Row = (String, bool, Vec<String>, Vec<String>);

/// `(function, unknown, deps, unresolved reasons)` under a profile.
fn coupling(root: &Path, probe: Probe, profile: Profile) -> Vec<Row> {
    let mut options = Options::new(probe);
    options.profile = profile;
    let Ok(m) = scan(root, &options) else {
        panic!("scan failed");
    };
    m.functions
        .iter()
        .filter_map(|f| {
            let c = f.coupling.as_ref()?;
            let reasons = c
                .unresolved_calls
                .iter()
                .map(|u| u.reason.clone())
                .collect();
            Some((
                f.function.clone(),
                c.coupling_unknown,
                c.resolved_dependencies.clone(),
                reasons,
            ))
        })
        .collect()
}

fn get<'a>(rows: &'a [Row], name: &str) -> &'a Row {
    rows.iter()
        .find(|r| r.0 == name)
        .unwrap_or_else(|| panic!("no function {name} in {rows:?}"))
}

const JS: &[(&str, &str)] = &[
    (
        "util.js",
        "export function helper(x) { return x; }\nexport const K = 1;\n",
    ),
    (
        "main.js",
        r#"import { helper } from './util.js';
import EventEmitter from 'node:events';
const H = require('./util.js').helper;
const debug = require('debug')('app');

export function params(str, items) {
  debug('x');
  return str.indexOf(';') + items.slice(1).length;
}

export function closure(list) {
  function local(v) { return v.trim(); }
  return list.map((item) => local(item) + H(item));
}

export class Box extends EventEmitter {
  run() {
    return [1].map(
      () => this.emit('go'));
  }
}

export function dynamic(name) {
  return require(name).go() + eval('1');
}

export function undeclared() {
  return notDefinedAnywhere();
}
"#,
    ),
];

#[test]
fn js_parity_marks_receivers_unknown_precise_resolves_them() {
    let t = tree("js", JS);
    let parity = coupling(&t.0, Probe::Js, Profile::Parity);
    let precise = coupling(&t.0, Probe::Js, Profile::Precise);
    assert!(
        get(&parity, "params").1,
        "parity: parameter receivers are unknown"
    );
    assert!(
        !get(&precise, "params").1,
        "precise: str.indexOf is dispatch"
    );
    let callbacks: Vec<&Row> = precise.iter().filter(|r| r.0 == "<anonymous>").collect();
    assert!(
        callbacks
            .iter()
            .any(|r| r.2 == vec!["util".to_owned()] && !r.1),
        "closure sees H = require('./util.js').helper: {precise:?}"
    );
    assert!(
        callbacks.iter().all(|r| !r.1),
        "this in an arrow is the Box instance: {precise:?}"
    );
    let dynamic = get(&precise, "dynamic");
    assert!(dynamic.1, "non-literal require and eval stay unknown");
    assert!(
        dynamic
            .3
            .contains(&"require of non-literal specifier".to_owned())
    );
    assert!(dynamic.3.contains(&"dynamic callable lookup".to_owned()));
    assert!(
        get(&precise, "undeclared").1,
        "an undeclared global could be first-party"
    );
}

const PY: &[(&str, &str)] = &[
    ("pkg/__init__.py", ""),
    (
        "pkg/base.py",
        "class Base:\n    def ping(self):\n        return 1\n",
    ),
    (
        "pkg/app.py",
        r#"import logging
from pkg.base import Base
from pkg import base as base_mod
from somewhere import Mixin

logger = logging.getLogger(__name__)


def params(request, items):
    logger.info("x")
    return request.db.query([i.strip() for i in items])


class Child(Base):
    def __init__(self, request):
        self.request = request

    def run(self):
        self.request.db.query(1)
        return self.ping()


class External(Mixin):
    def run(self):
        return self.from_package()


class Computed(make_base()):
    def run(self):
        return self.whatever()


class Lazy:
    def __getattr__(self, name):
        return name

    def run(self):
        return self.missing()


def outer(xs):
    def helper(x):
        return base_mod.Base().ping() + x.bit_length()
    return [helper(x).bit_length() for x in xs]


def dynamic(obj, name):
    return getattr(obj, name)()
"#,
    ),
];

#[test]
fn python_precise_resolves_origins_and_keeps_true_unknowns() {
    let t = tree("py", PY);
    let root = t.0.join("pkg");
    let parity = coupling(&root, Probe::Python, Profile::Parity);
    let precise = coupling(&root, Probe::Python, Profile::Precise);
    assert!(get(&parity, "params").1);
    assert!(
        !get(&precise, "params").1,
        "parameters, comprehension items and module loggers are known"
    );
    let inherited = precise
        .iter()
        .find(|r| r.0 == "run" && r.2 == vec!["pkg.base".to_owned()]);
    assert!(
        inherited.is_some_and(|r| !r.1),
        "self.ping() inherits from the first-party base: {precise:?}"
    );
    let unknown_self = precise.iter().filter(|r| r.0 == "run" && r.1).count();
    assert_eq!(
        unknown_self, 2,
        "a computed base and __getattr__ keep self calls unknown: {precise:?}"
    );
    let package_base = precise
        .iter()
        .filter(|r| r.0 == "run" && !r.1 && r.2.is_empty())
        .count();
    assert_eq!(
        package_base, 1,
        "a package base class names no first-party ancestor: {precise:?}"
    );
    let helper = get(&precise, "helper");
    assert_eq!(
        helper.2,
        vec!["pkg.base".to_owned()],
        "the package prefix makes pkg.base first-party"
    );
    assert!(!helper.1 && !get(&precise, "outer").1);
    assert!(get(&precise, "dynamic").1, "getattr stays a dynamic lookup");
}
