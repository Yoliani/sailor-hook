//! Cursor hook payload → normalized `Event`.
//!
//! Cursor's native hooks (`~/.cursor/hooks.json`) use camelCase event names
//! and a flat config shape, and its payloads carry a `conversation_id`
//! (stable across turns) instead of a `session_id`. Its gate hook
//! (`preToolUse`) can deny a tool call, but it fires on *every* tool use —
//! parking it would block the agent constantly — so Cursor is events-only:
//! the inbox shows what the agent is doing, and there is no approve-from-
//! phone channel.

use crate::adapters::{event_name, session_uuid, title_for};
use crate::events::{Category, Event};
use serde_json::Value;

const AGENT: &str = "cursor";

pub fn normalize(payload: &Value) -> anyhow::Result<Event> {
    let hook =
        event_name(payload).ok_or_else(|| anyhow::anyhow!("payload has no hook_event_name"))?;

    let category = category_for(hook)
        .ok_or_else(|| anyhow::anyhow!("unhandled Cursor hook event `{hook}`"))?;

    // Cursor has no `session_id`; `conversation_id` is the stable session
    // key across turns, so that is what one inbox row is bound to.
    let session = payload
        .get("conversation_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool = payload.get("tool_name").and_then(Value::as_str);
    let message = payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let mut event = Event::new(category, AGENT, title_for(category, tool, message));
    event.session_id = session_uuid(AGENT, session);
    event.project = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::to_string);
    event.message = message.to_string();
    Ok(event)
}

/// No decision channel: nothing Cursor emits can be answered from stdout.
pub fn is_decidable(_payload: &Value) -> bool {
    false
}

fn category_for(hook: &str) -> Option<Category> {
    Some(match hook {
        "sessionStart" => Category::SessionStarted,
        "preToolUse" => Category::ToolRunning,
        "postToolUse" | "postToolUseFailure" => Category::ToolFinished,
        "stop" | "sessionEnd" => Category::TaskComplete,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(hook: &str) -> Value {
        json!({
            "conversation_id": "conv-456",
            "cwd": "/proj",
            "hook_event_name": hook,
            "tool_name": "Shell",
            "tool_input": { "command": "npm test" },
        })
    }

    #[test]
    fn maps_cursor_event_names() {
        assert_eq!(
            normalize(&payload("preToolUse")).unwrap().category,
            Category::ToolRunning
        );
        assert_eq!(
            normalize(&payload("postToolUseFailure")).unwrap().category,
            Category::ToolFinished
        );
        assert_eq!(
            normalize(&payload("sessionStart")).unwrap().category,
            Category::SessionStarted
        );
        assert_eq!(
            normalize(&payload("stop")).unwrap().category,
            Category::TaskComplete
        );
    }

    #[test]
    fn conversation_id_binds_the_session() {
        let a = normalize(&payload("sessionStart")).unwrap();
        let b = normalize(&payload("stop")).unwrap();
        assert_eq!(a.session_id, b.session_id);
        // A different conversation is a different row.
        let mut other = payload("sessionStart");
        other["conversation_id"] = json!("conv-999");
        assert_ne!(a.session_id, normalize(&other).unwrap().session_id);
    }

    #[test]
    fn shell_tools_title_from_the_cursor_tool_name() {
        let e = normalize(&payload("preToolUse")).unwrap();
        assert_eq!(e.title, "Running Shell");
    }

    #[test]
    fn never_decidable() {
        assert!(!is_decidable(&payload("preToolUse")));
    }
}
