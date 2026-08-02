//! Per-agent adapters: each agent's hook payload → the normalized `Event`
//! contract in `events.rs` (`TECH.md` §4).
//!
//! Phase 3 ships Claude Code first; Codex/Gemini/Cursor/OpenCode follow the
//! same shape (parse the agent's JSON, map its event name onto one of the
//! five categories, derive a stable session id).

pub mod claude_code;

use crate::events::Event;
use serde_json::Value;

/// Normalize a raw agent hook payload for `agent` (a `config::Agent` id).
pub fn normalize(agent: &str, payload: &serde_json::Value) -> anyhow::Result<Event> {
    match agent {
        "claude_code" => claude_code::normalize(payload),
        other => anyhow::bail!("no adapter for agent `{other}` yet"),
    }
}

/// Whether this payload is one the phone can actually answer.
///
/// Not every approval-shaped event is decidable: Claude Code's `Notification`
/// tells you permission is being asked for but has no channel to answer on,
/// while `PermissionRequest` decides from the hook's stdout. Only the latter
/// is worth blocking the agent for.
pub fn is_decidable(agent: &str, payload: &serde_json::Value) -> bool {
    match agent {
        "claude_code" => claude_code::is_decidable(payload),
        _ => false,
    }
}

/// The stdout the agent reads as a decision. Only called for payloads
/// `is_decidable` accepted.
pub fn render_decision(agent: &str, payload: &serde_json::Value, allow: bool) -> Option<Value> {
    match agent {
        "claude_code" => Some(claude_code::render_decision(payload, allow)),
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
    fn unknown_agent_is_an_error() {
        assert!(normalize("nope", &serde_json::json!({})).is_err());
    }
}
