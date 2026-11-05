//! `.emi/config.json` (`emi-config-0.1`): what the done check needs to know
//! about a repository. A misspelt key or a value of the wrong type is an
//! error, never ignored: a key read as its default could switch a check off
//! without a word.

use std::path::Path;

use emi_tree::snapshot;
use serde_json::{Map, Value, json};

use crate::Error;

pub const SCHEMA: &str = "emi-config-0.1";
/// Where the configuration lives, relative to the repository root.
pub const PATH: &str = ".emi/config.json";
pub const DEFAULT_REPAIR_ROUNDS: u64 = 3;
/// Rounds beyond this are a typing mistake, not a policy.
const MAX_REPAIR_ROUNDS: u64 = 1000;

const KEYS: &[&str] = &[
    "schema",
    "language",
    "source",
    "default_branch",
    "tests",
    "link",
    "python_path",
    "repair_rounds",
    "on_unproven",
    "on_redundant",
];
const REQUIRED: &[&str] = &["schema", "language", "source", "default_branch", "tests"];

/// What unexercised functions do to the action: stated (`report`, the
/// default), sent back (`repair`), or left to a person (`ask_person`).
pub use emi_kernel::decide::OnFinding as OnUnproven;

/// What redundant tests do to the action: stated (the default; a test
/// that passes before and after may still guard behaviour the change must
/// keep), or sent back to be made to fail on the old code.
pub type OnRedundant = OnUnproven;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tests {
    /// The test command as an argv list, never a shell line.
    pub command: Vec<String>,
    /// Globs of the test files, matched against repository-relative paths.
    pub globs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub language: String,
    pub source: String,
    pub default_branch: String,
    pub tests: Tests,
    /// Ignored dependency directories the test command needs (a virtualenv).
    pub link: Vec<String>,
    /// Directories the tests import the project's own code from, relative
    /// to the repository root (`src` for a src layout). They go first on
    /// the import path of every run, so the tests import the code being
    /// checked and never a copy the environment has installed.
    pub python_path: Vec<String>,
    pub repair_rounds: u64,
    pub on_unproven: OnUnproven,
    pub on_redundant: OnRedundant,
}

fn bad(at: &str, what: &str) -> Error {
    Error::Config(format!("{PATH}: {at} {what}"))
}

fn nonempty_string<'a>(v: &'a Value, at: &str) -> Result<&'a str, Error> {
    match v.as_str() {
        Some(s) if !s.trim().is_empty() => Ok(s),
        _ => Err(bad(at, "must be a nonempty string")),
    }
}

fn string_list(v: &Value, at: &str, nonempty: bool) -> Result<Vec<String>, Error> {
    let items = v
        .as_array()
        .ok_or_else(|| bad(at, "must be an array of strings"))?;
    if nonempty && items.is_empty() {
        return Err(bad(at, "must not be empty"));
    }
    items
        .iter()
        .map(|x| nonempty_string(x, at).map(str::to_owned))
        .collect()
}

/// A repository-relative path, `.` for the root.
fn relative(v: &Value, at: &str) -> Result<String, Error> {
    let s = nonempty_string(v, at)?;
    let rel = snapshot::normalize_source(s)
        .map_err(|e| bad(at, &format!("must stay inside the repository ({e})")))?;
    Ok(if rel.is_empty() { ".".to_owned() } else { rel })
}

fn only_keys(obj: &Map<String, Value>, known: &[&str], at: &str) -> Result<(), Error> {
    match obj.keys().find(|k| !known.contains(&k.as_str())) {
        Some(k) => Err(Error::Config(format!("{PATH}: unknown key {at}{k}"))),
        None => Ok(()),
    }
}

fn tests(v: &Value) -> Result<Tests, Error> {
    let obj = v
        .as_object()
        .ok_or_else(|| bad("tests", "must be an object"))?;
    only_keys(obj, &["command", "globs"], "tests.")?;
    let field = |k: &str| {
        obj.get(k)
            .ok_or_else(|| bad("tests", &format!("needs {k}")))
    };
    Ok(Tests {
        command: string_list(field("command")?, "tests.command", true)?,
        globs: string_list(field("globs")?, "tests.globs", true)?,
    })
}

fn rounds(v: Option<&Value>) -> Result<u64, Error> {
    match v {
        None => Ok(DEFAULT_REPAIR_ROUNDS),
        Some(x) => x
            .as_u64()
            .filter(|n| *n <= MAX_REPAIR_ROUNDS)
            .ok_or_else(|| {
                bad(
                    "repair_rounds",
                    &format!("must be an integer from 0 to {MAX_REPAIR_ROUNDS}"),
                )
            }),
    }
}

/// `report` (also when absent), `repair` or `ask_person`, for a finding
/// the proposer could address.
fn report_or_repair(v: Option<&Value>, key: &str) -> Result<OnUnproven, Error> {
    match v.map(|x| x.as_str().and_then(OnUnproven::from_name)) {
        None => Ok(OnUnproven::Report),
        Some(Some(treatment)) => Ok(treatment),
        Some(None) => Err(bad(key, "must be report, repair or ask_person")),
    }
}

fn language(v: &Value) -> Result<String, Error> {
    match v.as_str() {
        Some("python") => Ok("python".to_owned()),
        Some(other) => Err(Error::Config(format!(
            "{PATH}: language {other} is unsupported for now (only python)"
        ))),
        None => Err(bad("language", "must be a string")),
    }
}

impl Config {
    /// Parse and validate configuration text.
    pub fn parse(text: &str) -> Result<Config, Error> {
        let value =
            emi_kernel::strict::loads(text).map_err(|e| Error::Config(format!("{PATH}: {e}")))?;
        Config::from_value(&value)
    }

    pub fn from_value(v: &Value) -> Result<Config, Error> {
        let obj = v
            .as_object()
            .ok_or_else(|| Error::Config(format!("{PATH}: must be a JSON object")))?;
        only_keys(obj, KEYS, "")?;
        if let Some(k) = REQUIRED.iter().find(|k| !obj.contains_key(**k)) {
            return Err(Error::Config(format!("{PATH}: missing key {k}")));
        }
        if obj["schema"].as_str() != Some(SCHEMA) {
            return Err(bad("schema", &format!("must be {SCHEMA}")));
        }
        Ok(Config {
            language: language(&obj["language"])?,
            source: relative(&obj["source"], "source")?,
            default_branch: branch(&obj["default_branch"])?,
            tests: tests(&obj["tests"])?,
            link: obj
                .get("link")
                .map_or(Ok(Vec::new()), |v| paths(v, "link"))?,
            python_path: obj
                .get("python_path")
                .map_or(Ok(Vec::new()), |v| paths(v, "python_path"))?,
            repair_rounds: rounds(obj.get("repair_rounds"))?,
            on_unproven: report_or_repair(obj.get("on_unproven"), "on_unproven")?,
            on_redundant: report_or_repair(obj.get("on_redundant"), "on_redundant")?,
        })
    }

    /// The configuration as written to disk (plan.md, interface 1).
    pub fn to_value(&self) -> Value {
        let mut v = json!({
            "schema": SCHEMA,
            "language": self.language,
            "source": self.source,
            "default_branch": self.default_branch,
            "tests": {"command": self.tests.command, "globs": self.tests.globs},
            "link": self.link,
            "repair_rounds": self.repair_rounds,
            "on_unproven": self.on_unproven.as_str(),
        });
        // written only when set, so a configuration without it reads back
        // as it was written
        if !self.python_path.is_empty() {
            v["python_path"] = json!(self.python_path);
        }
        if self.on_redundant == OnRedundant::Repair {
            v["on_redundant"] = json!(self.on_redundant.as_str());
        }
        v
    }

    pub fn to_text(&self) -> String {
        emi_kernel::json::dumps_pretty(&self.to_value()) + "\n"
    }
}

fn branch(v: &Value) -> Result<String, Error> {
    let s = nonempty_string(v, "default_branch")?;
    if s.starts_with('-') || s.chars().any(char::is_whitespace) {
        return Err(bad("default_branch", "is not a branch name"));
    }
    Ok(s.to_owned())
}

fn paths(v: &Value, key: &str) -> Result<Vec<String>, Error> {
    let items = v
        .as_array()
        .ok_or_else(|| bad(key, "must be an array of relative paths"))?;
    items.iter().map(|x| relative(x, key)).collect()
}

/// The configuration the working tree holds.
pub fn read_file(repo: &Path) -> Result<Config, Error> {
    let path = repo.join(PATH);
    let text = std::fs::read_to_string(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => Error::Config(format!(
            "{PATH} does not exist in {}; run `emi init` first",
            repo.display()
        )),
        _ => Error::Config(format!("{}: {e}", path.display())),
    })?;
    Config::parse(&text)
}

/// The configuration a revision holds, read as a git blob (never from the
/// working tree). `Ok(None)` means the revision has no such file; any
/// other failure to read it is an error.
pub fn read_at(repo: &Path, rev: &str) -> Result<Option<Config>, Error> {
    let listed = emi_git::run(repo, &["ls-tree", "--name-only", rev, "--", PATH])
        .map_err(|e| Error::Config(format!("cannot look for {PATH} at {rev}: {e}")))?;
    if listed.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let blob = emi_git::run(repo, &["cat-file", "blob", &format!("{rev}:{PATH}")])
        .map_err(|e| Error::Config(format!("cannot read {PATH} at {rev}: {e}")))?;
    let text = String::from_utf8(blob)
        .map_err(|_| Error::Config(format!("{PATH} at {rev} is not UTF-8")))?;
    Config::parse(&text).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{
      "schema": "emi-config-0.1", "language": "python", "source": "src",
      "default_branch": "main",
      "tests": {"command": ["python", "-m", "pytest", "-q"], "globs": ["tests/*"]},
      "link": [".venv"], "repair_rounds": 2, "on_unproven": "repair"
    }"#;

    fn with(edit: impl FnOnce(&mut Value)) -> Result<Config, Error> {
        let mut v: Value = serde_json::from_str(GOOD).unwrap_or_default();
        edit(&mut v);
        Config::from_value(&v)
    }

    fn message(r: Result<Config, Error>) -> String {
        match r {
            Err(Error::Config(m)) => m,
            other => format!("expected a config error, got {other:?}"),
        }
    }

    #[test]
    fn a_valid_file_round_trips() {
        let c = Config::parse(GOOD).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.repair_rounds, 2);
        assert_eq!(c.on_unproven, OnUnproven::Repair);
        assert_eq!(
            Config::from_value(&c.to_value()).as_ref().ok(),
            Some(&c),
            "what init writes must read back"
        );
    }

    #[test]
    fn optional_keys_take_their_defaults() {
        let c = with(|v| {
            for k in ["link", "repair_rounds", "on_unproven"] {
                v.as_object_mut().map(|m| m.remove(k));
            }
        })
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.repair_rounds, 3);
        assert_eq!(c.on_unproven, OnUnproven::Report);
        assert!(c.link.is_empty());
    }

    /// Set the member at a JSON pointer to the JSON `to`; `-` removes it.
    fn patch(v: &mut Value, pointer: &str, to: &str) {
        let (parent, key) = pointer.rsplit_once('/').unwrap_or(("", pointer));
        let Some(Value::Object(map)) = v.pointer_mut(parent) else {
            panic!("no object at {parent}");
        };
        if to == "-" {
            map.remove(key);
        } else {
            map.insert(key.to_owned(), serde_json::from_str(to).unwrap_or_default());
        }
    }

    // (what is wrong, where, the new JSON value or `-`, what the error says)
    const INVALID: &[(&str, &str, &str, &str)] = &[
        ("top key", "/repair_round", "3", "unknown key repair_round"),
        (
            "tests key",
            "/tests/cmd",
            "[\"x\"]",
            "unknown key tests.cmd",
        ),
        ("schema", "/schema", "\"emi-config-9\"", "schema must be"),
        ("language", "/language", "\"rust\"", "unsupported for now"),
        (
            "language type",
            "/language",
            "3",
            "language must be a string",
        ),
        (
            "source escapes",
            "/source",
            "\"../x\"",
            "source must stay inside",
        ),
        (
            "source type",
            "/source",
            "[\"src\"]",
            "source must be a nonempty",
        ),
        (
            "branch blank",
            "/default_branch",
            "\" \"",
            "default_branch must be",
        ),
        (
            "branch option",
            "/default_branch",
            "\"-x\"",
            "not a branch name",
        ),
        (
            "command string",
            "/tests/command",
            "\"pytest\"",
            "command must be an array",
        ),
        (
            "command empty",
            "/tests/command",
            "[]",
            "command must not be empty",
        ),
        (
            "command item",
            "/tests/command",
            "[\"a\", 1]",
            "command must be a nonempty",
        ),
        ("globs missing", "/tests/globs", "-", "tests needs globs"),
        (
            "link absolute",
            "/link",
            "[\"/etc\"]",
            "link must stay inside",
        ),
        ("link type", "/link", "\".venv\"", "link must be an array"),
        (
            "rounds string",
            "/repair_rounds",
            "\"3\"",
            "must be an integer",
        ),
        (
            "rounds negative",
            "/repair_rounds",
            "-1",
            "must be an integer",
        ),
        (
            "rounds float",
            "/repair_rounds",
            "2.5",
            "must be an integer",
        ),
        (
            "unproven",
            "/on_unproven",
            "\"ignore\"",
            "must be report, repair or ask_person",
        ),
        ("missing key", "/source", "-", "missing key source"),
    ];

    #[test]
    fn a_table_of_invalid_files_is_refused_with_the_reason() {
        for (name, pointer, to, want) in INVALID {
            let got = message(with(|v| patch(v, pointer, to)));
            assert!(got.contains(want), "{name}: {got}");
        }
    }

    #[test]
    fn text_that_is_not_one_json_object_is_refused() {
        for text in ["", "[]", "{", r#"{"schema": "a", "schema": "b"}"#] {
            assert!(Config::parse(text).is_err(), "{text:?}");
        }
    }
}
