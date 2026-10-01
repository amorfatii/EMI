//! Registering the hook with a harness, and taking it out again.
//!
//! Claude Code: two entries merged into a settings file next to whatever the
//! person already has there. opencode: one plugin file that EMI owns. Both
//! are idempotent, say what they changed, and refuse to touch a file they
//! cannot parse or that is not theirs.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::claude::{Event, STOP_TIMEOUT_SECS, USER_PROMPT_TIMEOUT_SECS};

const PLUGIN_TEMPLATE: &str = include_str!("plugin.js");
/// The first line of the plugin file: how uninstall knows the file is EMI's.
const PLUGIN_MARK: &str = "// emi-hook: opencode plugin";
const PLUGIN_FILE: &str = "emi.js";

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Harness {
    ClaudeCode,
    Opencode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Scope {
    /// Inside the repository.
    Project,
    /// In the person's own configuration.
    User,
}

/// Where each harness reads its configuration from.
#[derive(Clone, Debug)]
pub struct Dirs {
    pub home: PathBuf,
    /// `$XDG_CONFIG_HOME`, or `~/.config`.
    pub config_home: PathBuf,
}

impl Dirs {
    pub fn from_process() -> Result<Dirs, String> {
        let absolute = |name: &str| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        let home = absolute("HOME").ok_or_else(|| {
            "HOME is not an absolute path, so the user scope has no location".to_owned()
        })?;
        let config_home = absolute("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        Ok(Dirs { home, config_home })
    }
}

pub struct Target<'a> {
    pub harness: Harness,
    pub scope: Scope,
    pub repo: &'a Path,
    pub dirs: &'a Dirs,
    /// How the harness should start emi: an absolute path, or `emi` to use
    /// whatever is on PATH.
    pub emi: &'a str,
}

impl Target<'_> {
    fn file(&self) -> PathBuf {
        match (self.harness, self.scope) {
            (Harness::ClaudeCode, Scope::Project) => self.repo.join(".claude/settings.json"),
            (Harness::ClaudeCode, Scope::User) => self.dirs.home.join(".claude/settings.json"),
            (Harness::Opencode, Scope::Project) => {
                self.repo.join(".opencode/plugins").join(PLUGIN_FILE)
            }
            (Harness::Opencode, Scope::User) => self
                .dirs
                .config_home
                .join("opencode/plugins")
                .join(PLUGIN_FILE),
        }
    }
}

/// Install; the result is what changed, one line per fact.
pub fn install(target: &Target<'_>) -> Result<Vec<String>, String> {
    let file = target.file();
    match target.harness {
        Harness::ClaudeCode => edit_settings(&file, |settings| merge(settings, target.emi)),
        Harness::Opencode => install_plugin(&file, target.emi),
    }
}

/// Uninstall; the result is what changed, one line per fact.
pub fn uninstall(target: &Target<'_>) -> Result<Vec<String>, String> {
    let file = target.file();
    match target.harness {
        Harness::ClaudeCode => edit_settings(&file, remove),
        Harness::Opencode => uninstall_plugin(&file),
    }
}

// ---- Claude Code settings ---------------------------------------------

const EVENTS: [(Event, u32, &str); 2] = [
    (Event::Stop, STOP_TIMEOUT_SECS, "EMI: checking the change"),
    (Event::UserPromptSubmit, USER_PROMPT_TIMEOUT_SECS, ""),
];

fn command_line(emi: &str, event: Event) -> String {
    let exe = shlex::try_quote(emi).map_or_else(|_| emi.to_owned(), |q| q.into_owned());
    format!("{exe} hook claude-code {}", event.argument())
}

/// Whether `handler` is the entry this module writes for `event`, whatever
/// path to emi it holds.
fn is_ours(handler: &Value, event: Event) -> bool {
    let Some(command) = handler.get("command").and_then(Value::as_str) else {
        return false;
    };
    let Some(words) = shlex::split(command) else {
        return false;
    };
    words.len() >= 4
        && words[words.len() - 3] == "hook"
        && words[words.len() - 2] == "claude-code"
        && words[words.len() - 1] == event.argument()
}

fn handler_for(emi: &str, event: Event, timeout: u32, status: &str) -> Value {
    let mut handler = Map::new();
    handler.insert("type".into(), json!("command"));
    handler.insert("command".into(), json!(command_line(emi, event)));
    handler.insert("timeout".into(), json!(timeout));
    if !status.is_empty() {
        handler.insert("statusMessage".into(), json!(status));
    }
    Value::Object(handler)
}

fn object_entry<'a>(parent: &'a mut Value, key: &str, what: &str) -> Result<&'a mut Value, String> {
    let object = parent
        .as_object_mut()
        .ok_or_else(|| format!("{what} is not a JSON object"))?;
    Ok(object.entry(key).or_insert_with(|| Value::Null))
}

/// Every handler of `groups` (the array under one event) as mutable.
fn handlers_of(groups: &mut [Value]) -> Vec<&mut Value> {
    groups
        .iter_mut()
        .filter_map(|g| g.get_mut("hooks").and_then(Value::as_array_mut))
        .flat_map(|hooks| hooks.iter_mut())
        .collect()
}

fn merge_event(
    settings: &mut Value,
    emi: &str,
    (event, timeout, status): (Event, u32, &str),
) -> Result<String, String> {
    let name = event.settings_name();
    let hooks = object_entry(settings, "hooks", "the settings file's hooks entry")?;
    if hooks.is_null() {
        *hooks = json!({});
    }
    let groups = object_entry(hooks, name, "the hooks entry")?;
    if groups.is_null() {
        *groups = json!([]);
    }
    let groups = groups
        .as_array_mut()
        .ok_or_else(|| format!("hooks.{name} is not an array"))?;
    let wanted = command_line(emi, event);
    let mut found = false;
    let mut changed = false;
    for handler in handlers_of(groups)
        .into_iter()
        .filter(|h| is_ours(h, event))
    {
        found = true;
        if handler["command"] != json!(wanted) {
            handler["command"] = json!(wanted);
            changed = true;
        }
    }
    if !found {
        groups.push(json!({"hooks": [handler_for(emi, event, timeout, status)]}));
        return Ok(format!("added the {name} hook"));
    }
    Ok(if changed {
        format!("updated the {name} hook")
    } else {
        format!("the {name} hook was already installed")
    })
}

fn merge(settings: &mut Value, emi: &str) -> Result<Vec<String>, String> {
    EVENTS
        .iter()
        .map(|spec| merge_event(settings, emi, *spec))
        .collect()
}

/// Remove our handlers from the groups of one event, and the groups that
/// held nothing else. Returns how many handlers went.
fn remove_from(groups: &mut Vec<Value>, event: Event) -> usize {
    let mut removed = 0;
    for group in groups.iter_mut() {
        let Some(hooks) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
            continue;
        };
        let before = hooks.len();
        hooks.retain(|h| !is_ours(h, event));
        removed += before - hooks.len();
    }
    // only groups this pass emptied: a group someone else left empty is theirs
    if removed > 0 {
        groups.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|hooks| !hooks.is_empty())
        });
    }
    removed
}

fn remove(settings: &mut Value) -> Result<Vec<String>, String> {
    let Some(hooks) = settings.get_mut("hooks") else {
        return Ok(vec!["no hooks are installed in this file".to_owned()]);
    };
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| "the settings file's hooks entry is not a JSON object".to_owned())?;
    let mut lines = Vec::new();
    for (event, _, _) in EVENTS {
        let name = event.settings_name();
        let Some(groups) = hooks.get_mut(name) else {
            continue;
        };
        let groups = groups
            .as_array_mut()
            .ok_or_else(|| format!("hooks.{name} is not an array"))?;
        if remove_from(groups, event) > 0 {
            lines.push(format!("removed the {name} hook"));
        }
        if groups.is_empty() {
            hooks.remove(name);
        }
    }
    if hooks.is_empty()
        && let Some(object) = settings.as_object_mut()
    {
        object.remove("hooks");
    }
    if lines.is_empty() {
        lines.push("no EMI hooks were installed in this file".to_owned());
    }
    Ok(lines)
}

/// Read the settings file (`{}` when it does not exist), apply `change`, and
/// write it back only when something changed.
fn edit_settings(
    file: &Path,
    change: impl FnOnce(&mut Value) -> Result<Vec<String>, String>,
) -> Result<Vec<String>, String> {
    let original = match std::fs::read_to_string(file) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };
    let mut settings = match &original {
        Some(text) => serde_json::from_str::<Value>(text).map_err(|e| {
            format!(
                "{} is not valid JSON ({e}); it was left unchanged",
                file.display()
            )
        })?,
        None => json!({}),
    };
    if !settings.is_object() {
        return Err(format!(
            "{} is not a JSON object; it was left unchanged",
            file.display()
        ));
    }
    let before = settings.clone();
    let mut lines = change(&mut settings)?;
    if settings != before {
        write_file(
            file,
            &(serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())? + "\n"),
        )?;
        lines.push(format!("wrote {}", file.display()));
    } else {
        lines.push(format!("{} is unchanged", file.display()));
    }
    Ok(lines)
}

/// Create parent directories, write next to the target and rename, so an
/// interrupted write never leaves half a settings file.
fn write_file(file: &Path, text: &str) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("cannot write {}: {e}", file.display());
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    let tmp = file.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, text).map_err(fail)?;
    std::fs::rename(&tmp, file).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        fail(e)
    })
}

// ---- opencode plugin ---------------------------------------------------

fn plugin_text(emi: &str) -> Result<String, String> {
    let argv = serde_json::to_string(&[emi]).map_err(|e| e.to_string())?;
    Ok(PLUGIN_TEMPLATE.replace("__EMI_COMMAND__", &argv))
}

fn is_our_plugin(text: &str) -> bool {
    text.starts_with(PLUGIN_MARK)
}

fn install_plugin(file: &Path, emi: &str) -> Result<Vec<String>, String> {
    let wanted = plugin_text(emi)?;
    let existing = match std::fs::read_to_string(file) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };
    let line = match existing {
        Some(text) if text == wanted => {
            return Ok(vec![format!("{} is already installed", file.display())]);
        }
        Some(text) if !is_our_plugin(&text) => {
            return Err(format!(
                "{} exists and is not an EMI plugin; it was left unchanged",
                file.display()
            ));
        }
        Some(_) => format!("updated {}", file.display()),
        None => format!("wrote {}", file.display()),
    };
    write_file(file, &wanted)?;
    Ok(vec![line])
}

fn uninstall_plugin(file: &Path) -> Result<Vec<String>, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(vec![format!(
                "{} does not exist; nothing to remove",
                file.display()
            )]);
        }
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };
    if !is_our_plugin(&text) {
        return Err(format!(
            "{} is not an EMI plugin; it was left unchanged",
            file.display()
        ));
    }
    std::fs::remove_file(file).map_err(|e| format!("cannot remove {}: {e}", file.display()))?;
    Ok(vec![format!("removed {}", file.display())])
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Dirs) {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("mkdir");
        let dirs = Dirs {
            config_home: home.join(".config"),
            home,
        };
        (dir, dirs)
    }

    fn target<'a>(
        harness: Harness,
        scope: Scope,
        repo: &'a Path,
        dirs: &'a Dirs,
        emi: &'a str,
    ) -> Target<'a> {
        Target {
            harness,
            scope,
            repo,
            dirs,
            emi,
        }
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
    }

    #[test]
    fn the_settings_file_is_created_with_both_hooks() {
        let (dir, dirs) = setup();
        let t = target(
            Harness::ClaudeCode,
            Scope::Project,
            dir.path(),
            &dirs,
            "/bin/emi",
        );
        let lines = install(&t).expect("install");
        assert!(
            lines.iter().any(|l| l == "added the Stop hook"),
            "{lines:?}"
        );
        let v = read_json(&dir.path().join(".claude/settings.json"));
        let stop = &v["hooks"]["Stop"][0]["hooks"][0];
        assert_eq!(stop["type"], "command");
        assert_eq!(stop["command"], "/bin/emi hook claude-code stop");
        assert_eq!(stop["timeout"], 900);
        let prompt = &v["hooks"]["UserPromptSubmit"][0]["hooks"][0];
        assert_eq!(
            prompt["command"],
            "/bin/emi hook claude-code user-prompt-submit"
        );
    }

    #[test]
    fn existing_settings_and_hooks_survive_install_and_uninstall() {
        let (dir, dirs) = setup();
        let file = dir.path().join(".claude/settings.json");
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        let original = json!({
            "model": "opus",
            "permissions": {"allow": ["Bash(ls)"]},
            "hooks": {
                "Stop": [{"hooks": [{"type": "command", "command": "notify-send done"}]}],
                "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "guard.sh"}]}]
            }
        });
        std::fs::write(&file, original.to_string()).expect("write");
        let t = target(
            Harness::ClaudeCode,
            Scope::Project,
            dir.path(),
            &dirs,
            "/bin/emi",
        );
        install(&t).expect("install");
        let merged = read_json(&file);
        assert_eq!(merged["model"], "opus");
        assert_eq!(merged["permissions"], original["permissions"]);
        assert_eq!(
            merged["hooks"]["PreToolUse"],
            original["hooks"]["PreToolUse"]
        );
        assert_eq!(merged["hooks"]["Stop"][0], original["hooks"]["Stop"][0]);
        assert_eq!(merged["hooks"]["Stop"].as_array().expect("array").len(), 2);
        uninstall(&t).expect("uninstall");
        assert_eq!(read_json(&file), original);
    }

    #[test]
    fn install_is_idempotent() {
        let (dir, dirs) = setup();
        let t = target(
            Harness::ClaudeCode,
            Scope::Project,
            dir.path(),
            &dirs,
            "/bin/emi",
        );
        install(&t).expect("first");
        let file = dir.path().join(".claude/settings.json");
        let first = std::fs::read_to_string(&file).expect("read");
        let lines = install(&t).expect("second");
        assert_eq!(std::fs::read_to_string(&file).expect("read"), first);
        assert!(
            lines.iter().any(|l| l.contains("already installed")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("is unchanged")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_new_path_to_emi_updates_the_entry_in_place() {
        let (dir, dirs) = setup();
        install(&target(
            Harness::ClaudeCode,
            Scope::Project,
            dir.path(),
            &dirs,
            "/old/emi",
        ))
        .expect("old");
        let lines = install(&target(
            Harness::ClaudeCode,
            Scope::Project,
            dir.path(),
            &dirs,
            "/new dir/emi",
        ))
        .expect("new");
        assert!(
            lines.iter().any(|l| l == "updated the Stop hook"),
            "{lines:?}"
        );
        let v = read_json(&dir.path().join(".claude/settings.json"));
        assert_eq!(v["hooks"]["Stop"].as_array().expect("array").len(), 1);
        assert_eq!(
            v["hooks"]["Stop"][0]["hooks"][0]["command"],
            "'/new dir/emi' hook claude-code stop"
        );
    }

    #[test]
    fn the_user_scope_writes_under_the_given_home_only() {
        let (dir, dirs) = setup();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        install(&target(
            Harness::ClaudeCode,
            Scope::User,
            &repo,
            &dirs,
            "emi",
        ))
        .expect("install");
        assert!(dirs.home.join(".claude/settings.json").is_file());
        assert!(!repo.join(".claude").exists());
    }

    #[test]
    fn unparseable_settings_are_left_alone() {
        let (dir, dirs) = setup();
        let file = dir.path().join(".claude/settings.json");
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, "{ // comment\n}").expect("write");
        let t = target(
            Harness::ClaudeCode,
            Scope::Project,
            dir.path(),
            &dirs,
            "emi",
        );
        assert!(
            install(&t)
                .expect_err("must refuse")
                .contains("not valid JSON")
        );
        assert_eq!(
            std::fs::read_to_string(&file).expect("read"),
            "{ // comment\n}"
        );
    }

    #[test]
    fn wrongly_shaped_hooks_are_refused() {
        let (dir, dirs) = setup();
        let file = dir.path().join(".claude/settings.json");
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        for bad in [r#"{"hooks": []}"#, r#"{"hooks": {"Stop": {}}}"#, "[]"] {
            std::fs::write(&file, bad).expect("write");
            let t = target(
                Harness::ClaudeCode,
                Scope::Project,
                dir.path(),
                &dirs,
                "emi",
            );
            assert!(install(&t).is_err(), "{bad}");
            assert_eq!(std::fs::read_to_string(&file).expect("read"), bad);
        }
    }

    #[test]
    fn uninstalling_what_was_never_installed_changes_nothing() {
        let (dir, dirs) = setup();
        let t = target(
            Harness::ClaudeCode,
            Scope::Project,
            dir.path(),
            &dirs,
            "emi",
        );
        let lines = uninstall(&t).expect("uninstall");
        assert!(lines.iter().any(|l| l.contains("no hooks")), "{lines:?}");
        assert!(!dir.path().join(".claude").exists());
    }

    #[test]
    fn the_plugin_is_written_and_removed() {
        let (dir, dirs) = setup();
        let t = target(
            Harness::Opencode,
            Scope::Project,
            dir.path(),
            &dirs,
            "/bin/emi",
        );
        install(&t).expect("install");
        let file = dir.path().join(".opencode/plugins/emi.js");
        let text = std::fs::read_to_string(&file).expect("read");
        assert!(text.starts_with(PLUGIN_MARK));
        assert!(text.contains(r#"const EMI = ["/bin/emi"]"#));
        assert!(!text.contains("__EMI_COMMAND__"));
        let again = install(&t).expect("again");
        assert!(again[0].contains("already installed"), "{again:?}");
        uninstall(&t).expect("uninstall");
        assert!(!file.exists());
    }

    #[test]
    fn a_foreign_plugin_file_is_never_overwritten_or_removed() {
        let (dir, dirs) = setup();
        let file = dir.path().join(".opencode/plugins/emi.js");
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, "export const Mine = async () => ({})\n").expect("write");
        let t = target(Harness::Opencode, Scope::Project, dir.path(), &dirs, "emi");
        assert!(install(&t).is_err());
        assert!(uninstall(&t).is_err());
        assert_eq!(
            std::fs::read_to_string(&file).expect("read"),
            "export const Mine = async () => ({})\n"
        );
    }

    #[test]
    fn the_opencode_user_scope_follows_the_config_home() {
        let (dir, mut dirs) = setup();
        dirs.config_home = dir.path().join("xdg");
        install(&target(
            Harness::Opencode,
            Scope::User,
            dir.path(),
            &dirs,
            "emi",
        ))
        .expect("install");
        assert!(dirs.config_home.join("opencode/plugins/emi.js").is_file());
    }
}
