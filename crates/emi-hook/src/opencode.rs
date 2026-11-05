//! The opencode adapter (documented in docs/hook.md).
//!
//! The opencode plugin (`plugin.js`) forwards two events as JSON on stdin
//! and applies the JSON answer. opencode cannot block a stop: when a session
//! goes idle the turn is already over. The closest documented equivalent is
//! to send a follow-up prompt to the session, which starts a new turn, so a
//! `repair` answer carries the prompt to send. All decisions stay here.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::json;

use crate::decision::Tone;
use crate::{Env, Reaction};

/// First line of every text the plugin puts into the session. A message that
/// starts with it is not the person speaking, so it must not reset the
/// repair rounds; without this a repair prompt would reset its own count
/// and the loop would never end.
pub const MARKER: &str = "[emi]";

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Event {
    /// The session went idle: the agent finished its turn.
    Idle,
    /// A new message arrived.
    UserMessage,
}

#[derive(Deserialize)]
struct Input {
    session_id: String,
    directory: Option<PathBuf>,
    text: Option<String>,
}

fn marked(text: &str) -> String {
    format!("{MARKER}\n{text}")
}

fn answer(reaction: &Reaction) -> serde_json::Value {
    match reaction {
        // OpenCode's events here are the prompt and the idle session; a
        // tool refusal is Claude Code's, and the laws are restored anyway
        Reaction::Silent | Reaction::Deny { .. } => json!({"action": "none"}),
        Reaction::Block { reason } => json!({"action": "repair", "prompt": marked(reason)}),
        Reaction::Notify { message, tone } => {
            let attention = *tone == Tone::Attention;
            json!({
                "action": "notify",
                "tone": if attention { "attention" } else { "verified" },
                "message": message,
                // Attention text is marked so surfacing it as a queued
                // prompt cannot reset the budget. The frontend decides
                // how to display the receipt without starting a turn.
                "note": attention.then(|| marked(message)),
            })
        }
    }
}

/// Handle one event; the result is the JSON answer for the plugin.
pub fn handle(event: Event, stdin: &str, env: &Result<Env, String>) -> String {
    answer(&react(event, stdin, env)).to_string() + "\n"
}

fn react(event: Event, stdin: &str, env: &Result<Env, String>) -> Reaction {
    let stopping = event == Event::Idle;
    let input: Input = match serde_json::from_str(stdin) {
        Ok(input) => input,
        Err(e) => return crate::failure(&format!("cannot read the plugin input: {e}"), stopping),
    };
    let env = match env {
        Ok(env) => env,
        Err(why) => return crate::failure(why, stopping),
    };
    let cwd = input
        .directory
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));
    match event {
        Event::Idle => crate::stop(env, &input.session_id, &cwd),
        Event::UserMessage if input.text.as_deref().is_some_and(|t| t.starts_with(MARKER)) => {
            Reaction::Silent
        }
        Event::UserMessage => crate::user_message(env, &input.session_id, &cwd),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repair_prompt_carries_the_marker() {
        let v = answer(&Reaction::Block {
            reason: "fix it".into(),
        });
        assert_eq!(v, json!({"action": "repair", "prompt": "[emi]\nfix it"}));
    }

    #[test]
    fn a_verified_message_needs_no_separate_attention_note() {
        let v = answer(&Reaction::Notify {
            message: "ok".into(),
            tone: Tone::Verified,
        });
        assert_eq!(v["note"], serde_json::Value::Null);
        assert_eq!(v["tone"], "verified");
    }

    #[test]
    fn a_message_needing_attention_is_kept_with_the_marker() {
        let v = answer(&Reaction::Notify {
            message: "NOT VERIFIED".into(),
            tone: Tone::Attention,
        });
        assert_eq!(v["note"], "[emi]\nNOT VERIFIED");
        assert_eq!(v["tone"], "attention");
    }
}
