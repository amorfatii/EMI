//! What the hook needs to know about the repository: whether EMI is set up
//! in it, the two config fields the loop uses, where HEAD is, and the
//! identity of the working tree.
//!
//! The done check owns the validation of `.emi/config.json`. The hook reads
//! only `default_branch` and `repair_rounds` and tolerates everything else.

use std::path::{Path, PathBuf};

use serde_json::Value;

const CONFIG: &str = ".emi/config.json";
const DEFAULT_REPAIR_ROUNDS: u32 = 3;

/// The fields of `.emi/config.json` the loop depends on.
#[derive(Debug, PartialEq, Eq)]
pub struct Config {
    pub default_branch: Option<String>,
    pub repair_rounds: u32,
}

/// The directory at or above `cwd` that holds `.emi/config.json`, canonical.
/// `None` means EMI is not set up here and the hook stays silent.
pub fn locate(cwd: &Path) -> Result<Option<PathBuf>, String> {
    let start = std::fs::canonicalize(cwd).map_err(|e| {
        format!(
            "cannot resolve the working directory {}: {e}",
            cwd.display()
        )
    })?;
    Ok(start
        .ancestors()
        .find(|dir| dir.join(CONFIG).is_file())
        .map(Path::to_path_buf))
}

fn optional_string(config: &Value, key: &str) -> Result<Option<String>, String> {
    match config.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(_) => Err(format!("{CONFIG}: {key} must be a non-empty string")),
    }
}

fn repair_rounds(config: &Value) -> Result<u32, String> {
    match config.get("repair_rounds") {
        None | Some(Value::Null) => Ok(DEFAULT_REPAIR_ROUNDS),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| format!("{CONFIG}: repair_rounds must be a non-negative integer")),
    }
}

/// Parse the config text. Fields other than the two read here are ignored.
pub fn parse_config(text: &str) -> Result<Config, String> {
    let config: Value =
        serde_json::from_str(text).map_err(|e| format!("{CONFIG} is not valid JSON: {e}"))?;
    if !config.is_object() {
        return Err(format!("{CONFIG} must be a JSON object"));
    }
    Ok(Config {
        default_branch: optional_string(&config, "default_branch")?,
        repair_rounds: repair_rounds(&config)?,
    })
}

pub fn read_config(root: &Path) -> Result<Config, String> {
    let path = root.join(CONFIG);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse_config(&text)
}

fn git_line(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = emi_git::run(root, args)?;
    Ok(String::from_utf8_lossy(&out).trim().to_owned())
}

/// HEAD as a commit id.
pub fn head(root: &Path) -> Result<String, String> {
    git_line(root, &["rev-parse", "--verify", "HEAD^{commit}"])
}

/// Whether HEAD is the configured default branch. A detached HEAD is not.
pub fn on_default_branch(root: &Path, default_branch: Option<&str>) -> Result<bool, String> {
    let Some(default) = default_branch else {
        return Ok(false);
    };
    let name = git_line(root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    Ok(name == default)
}

/// The identity of the working tree: the tree id of the snapshot the other
/// checks take (tracked and untracked-but-not-ignored files, byte for byte).
pub fn content_id(root: &Path) -> Result<String, String> {
    let view = emi_git::CleanView::of(root)?;
    let commit =
        emi_tree::snapshot::commit_worktree(root, view.path(), &view.path().join("emi-snapshot"))?;
    git_line(view.path(), &["rev-parse", &format!("{commit}^{{tree}}")])
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_apply_when_the_fields_are_absent() {
        let c = parse_config(r#"{"schema":"emi-config-0.1","language":"python"}"#).expect("parse");
        assert_eq!(
            c,
            Config {
                default_branch: None,
                repair_rounds: 3
            }
        );
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        let c = parse_config(
            r#"{"default_branch":"trunk","repair_rounds":5,"tests":{"command":["x"]},"future":[1]}"#,
        )
        .expect("parse");
        assert_eq!(c.default_branch.as_deref(), Some("trunk"));
        assert_eq!(c.repair_rounds, 5);
    }

    #[test]
    fn wrong_types_are_errors_not_defaults() {
        for bad in [
            "[]",
            "nope",
            r#"{"repair_rounds":"3"}"#,
            r#"{"repair_rounds":-1}"#,
            r#"{"repair_rounds":1.5}"#,
            r#"{"default_branch":3}"#,
            r#"{"default_branch":""}"#,
        ] {
            assert!(parse_config(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn zero_rounds_is_allowed() {
        assert_eq!(
            parse_config(r#"{"repair_rounds":0}"#)
                .expect("parse")
                .repair_rounds,
            0
        );
    }

    #[test]
    fn the_config_is_found_from_a_subdirectory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical");
        std::fs::create_dir_all(root.join(".emi")).expect("mkdir");
        std::fs::write(root.join(CONFIG), "{}").expect("write");
        std::fs::create_dir_all(root.join("a/b")).expect("mkdir");
        assert_eq!(locate(&root.join("a/b")), Ok(Some(root.clone())));
        let elsewhere = tempfile::tempdir().expect("tempdir");
        assert_eq!(locate(elsewhere.path()), Ok(None));
    }
}
