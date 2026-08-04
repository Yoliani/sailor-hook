//! Per-agent adapters: each agent's hook payload → the normalized `Event`
//! contract in `events.rs` (`TECH.md` §4).
//!
//! Phase 3 ships all seven agents. Three of them (Claude Code, Codex, Qwen)
//! expose a `PermissionRequest` hook whose stdout is read as a decision, so
//! those get the parked-approval wiring. The other four are events only —
//! Gemini and Kimi have no approval event, Cursor's gate hook fires on every
//! tool call (parking it would block the agent constantly), and OpenCode's
//! `permission.ask` plugin hook is declared in the SDK but never triggered.

pub mod claude_code;
pub mod codex;
pub mod cursor;
pub mod gemini;
pub mod kimi;
pub mod opencode;
pub mod pi;
pub mod qwen;

use crate::events::{Category, Event};
use serde_json::Value;

/// Normalize a raw agent hook payload for `agent` (a `config::Agent` id).
pub fn normalize(agent: &str, payload: &serde_json::Value) -> anyhow::Result<Event> {
    match agent {
        "claude_code" => claude_code::normalize(payload),
        "codex" => codex::normalize(payload),
        "gemini" => gemini::normalize(payload),
        "cursor" => cursor::normalize(payload),
        "kimi" => kimi::normalize(payload),
        "qwen" => qwen::normalize(payload),
        "opencode" => opencode::normalize(payload),
        "pi" => pi::normalize(payload),
        other => anyhow::bail!("no adapter for agent `{other}` yet"),
    }
}

/// Whether this payload is one the phone can actually answer.
///
/// Not every approval-shaped event is decidable: `Notification`-style events
/// tell you permission is being asked for but have no channel to answer on,
/// while `PermissionRequest` decides from the hook's stdout. Only the latter
/// is worth blocking the agent for.
pub fn is_decidable(agent: &str, payload: &serde_json::Value) -> bool {
    match agent {
        "claude_code" => claude_code::is_decidable(payload),
        "codex" => codex::is_decidable(payload),
        "qwen" => qwen::is_decidable(payload),
        _ => false,
    }
}

/// The stdout the agent reads as a decision. Only called for payloads
/// `is_decidable` accepted.
pub fn render_decision(agent: &str, payload: &serde_json::Value, allow: bool) -> Option<Value> {
    match agent {
        "claude_code" => Some(claude_code::render_decision(payload, allow)),
        "codex" => Some(codex::render_decision(payload, allow)),
        "qwen" => Some(qwen::render_decision(payload, allow)),
        _ => None,
    }
}

/// One inbox row per agent session: agents identify sessions with their own
/// ids (Claude Code uses a UUID, others may not), so hash the agent id and
/// its session string into a stable v5 UUID rather than trusting the format.
pub fn session_uuid(agent: &str, agent_session_id: &str) -> uuid::Uuid {
    if let Ok(parsed) = uuid::Uuid::parse_str(agent_session_id) {
        return parsed;
    }
    let name = format!("{agent}:{agent_session_id}");
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, name.as_bytes())
}

/// One line, scannable in a push notification and as the inbox row's title.
/// Shared by the adapters that model tools as a single string.
pub(crate) fn title_for(category: Category, tool: Option<&str>, message: &str) -> String {
    match category {
        Category::ApprovalRequired => match tool {
            Some(t) => format!("Approve: {t}"),
            None if !message.is_empty() => message.lines().next().unwrap_or(message).to_string(),
            None => "Approval required".to_string(),
        },
        Category::TaskComplete => "Task complete".to_string(),
        Category::SessionStarted => "Session started".to_string(),
        Category::ToolRunning => match tool {
            Some(t) => format!("Running {t}"),
            None => "Running tool".to_string(),
        },
        Category::ToolFinished => match tool {
            Some(t) => format!("Finished {t}"),
            None => "Finished tool".to_string(),
        },
    }
}

/// The `hook_event_name` field every payload we model carries, or None.
pub(crate) fn event_name(payload: &Value) -> Option<&str> {
    payload.get("hook_event_name").and_then(Value::as_str)
}

/// Shared session/project/tool/message reads. Every agent puts these on the
/// payload under the same names (Cursor is the exception — see its adapter).
pub(crate) struct Fields<'a> {
    pub session: &'a str,
    pub project: Option<&'a str>,
    pub tool: Option<&'a str>,
    pub message: &'a str,
}

pub(crate) fn common_fields<'a>(payload: &'a Value) -> Fields<'a> {
    Fields {
        session: payload
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        project: payload.get("cwd").and_then(Value::as_str),
        tool: payload.get("tool_name").and_then(Value::as_str),
        message: payload
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_real_uuids() {
        let id = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";
        assert_eq!(session_uuid("claude_code", id).to_string(), id);
    }

    #[test]
    fn hashes_non_uuid_ids_stably() {
        let a = session_uuid("codex", "sess-42");
        let b = session_uuid("codex", "sess-42");
        assert_eq!(a, b);
        assert_ne!(a, session_uuid("gemini", "sess-42"));
    }

    #[test]
    fn every_registered_agent_normalizes_or_errors_cleanly() {
        // Unknown agents stay an error.
        assert!(normalize("nope", &serde_json::json!({})).is_err());
        // Registered agents reject payloads without an event name, rather
        // than panicking or accepting garbage.
        for agent in [
            "claude_code",
            "codex",
            "gemini",
            "cursor",
            "kimi",
            "qwen",
            "opencode",
            "pi",
        ] {
            let err = normalize(agent, &serde_json::json!({ "session_id": "s" }));
            assert!(err.is_err(), "{agent} should reject an eventless payload");
        }
    }
}
