//! The Claude Code adapter (documented in docs/hook.md).
//!
//! Claude Code runs a command hook with the event as JSON on stdin. This
//! adapter answers with the JSON Claude Code documents for the event:
//! `{"decision":"block","reason":…}` keeps the agent working on a Stop
//! event, and `{"systemMessage":…}` shows a warning to the person only.
//! Staying silent is exit 0 with nothing on stdout.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::json;

use crate::{Env, Reaction};

/// How long Claude Code may wait for the Stop hook, in seconds. The done
/// check runs the project's tests, so the 600 second default is too short;
/// the check itself is stopped earlier (`done::DEFAULT_TIMEOUT_SECS`).
pub const STOP_TIMEOUT_SECS: u32 = 900;
/// Claude Code's default for this event is 30 seconds; recording a commit id
/// takes milliseconds, so the default is kept and written down explicitly.
pub const USER_PROMPT_TIMEOUT_SECS: u32 = 30;
/// Checking a path against the laws directory takes milliseconds.
pub const PRE_TOOL_TIMEOUT_SECS: u32 = 10;
/// The tools that write a file at a path the hook can see.
pub const FILE_TOOLS: &str = "Edit|Write|MultiEdit|NotebookEdit";

/// The hook events this adapter handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Event {
    Stop,
    UserPromptSubmit,
    /// Before a tool runs: refuses edits to locked laws.
    PreToolUse,
}

impl Event {
    /// The argument `emi hook claude-code` takes for this event.
    pub fn argument(self) -> &'static str {
        match self {
            Event::Stop => "stop",
            Event::UserPromptSubmit => "user-prompt-submit",
            Event::PreToolUse => "pre-tool-use",
        }
    }

    /// The event's name in Claude Code's settings file.
    pub fn settings_name(self) -> &'static str {
        match self {
            Event::Stop => "Stop",
            Event::UserPromptSubmit => "UserPromptSubmit",
            Event::PreToolUse => "PreToolUse",
        }
    }
}

/// The fields of the hook input this adapter uses; the rest is ignored.
#[derive(Deserialize)]
struct Input {
    session_id: String,
    cwd: Option<PathBuf>,
    #[serde(default)]
    tool_input: serde_json::Value,
}

fn answer(reaction: &Reaction) -> Option<serde_json::Value> {
    match reaction {
        Reaction::Silent => None,
        Reaction::Block { reason } => Some(json!({"decision": "block", "reason": reason})),
        Reaction::Notify { message, .. } => Some(json!({"systemMessage": message})),
        Reaction::Deny { reason } => Some(json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }})),
    }
}

/// Handle one event. Returns the text to print on stdout (empty for
/// silence). A hook that cannot read its input or its environment still
/// exits normally and warns the person: Claude Code treats other failures
/// as non-blocking errors with a terse notice.
pub fn handle(event: Event, stdin: &str, env: &Result<Env, String>) -> String {
    render(&react(event, stdin, env))
}

fn react(event: Event, stdin: &str, env: &Result<Env, String>) -> Reaction {
    let stopping = event == Event::Stop;
    let input: Input = match serde_json::from_str(stdin) {
        Ok(input) => input,
        Err(e) => return crate::failure(&format!("cannot read the hook input: {e}"), stopping),
    };
    let env = match env {
        Ok(env) => env,
        Err(why) => return crate::failure(why, stopping),
    };
    let cwd = input.cwd.unwrap_or_else(|| PathBuf::from("."));
    match event {
        Event::Stop => crate::stop(env, &input.session_id, &cwd),
        Event::UserPromptSubmit => crate::user_message(env, &input.session_id, &cwd),
        Event::PreToolUse => {
            let path = ["file_path", "notebook_path"]
                .iter()
                .find_map(|k| input.tool_input[*k].as_str());
            path.map_or(Reaction::Silent, |p| crate::guard_laws(&cwd, Path::new(p)))
        }
    }
}

fn render(reaction: &Reaction) -> String {
    answer(reaction).map_or_else(String::new, |v| v.to_string() + "\n")
}

#[cfg(test)]
mod tests {
    use crate::decision::Tone;

    use super::*;

    #[test]
    fn silence_prints_nothing() {
        assert_eq!(render(&Reaction::Silent), "");
    }

    #[test]
    fn a_block_uses_the_documented_decision_shape() {
        let text = render(&Reaction::Block {
            reason: "fix it".into(),
        });
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        assert_eq!(v, json!({"decision": "block", "reason": "fix it"}));
    }

    #[test]
    fn a_message_for_the_person_is_a_system_message() {
        let text = render(&Reaction::Notify {
            message: "NOT VERIFIED".into(),
            tone: Tone::Attention,
        });
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        assert_eq!(v, json!({"systemMessage": "NOT VERIFIED"}));
    }
}
