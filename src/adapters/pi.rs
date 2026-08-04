//! pi agent payload → normalized `Event`.
//!
//! pi has no hooks file; its extension system is the hook surface, so
//! `sailor-hook install --agent pi` writes a pi extension (the same file
//! this repo carries at `.pi/extensions/sailor-hooks.ts`) that forwards a
//! small payload contract to the shim. The adapter maps that contract:
//!
//! | hook_event_name | category |
//! |---|---|
//! | `session_start` | session_started |
//! | `agent_start` | tool_running ("Working") |
//! | `tool_execution_start` | tool_running |
//! | `tool_execution_end` | tool_finished |
//! | `agent_end` / `agent_settled` | task_complete |
//!
//! pi's hooks are observers — there is no decision channel, so nothing here
//! parks. `agent_settled` carries `context_usage` (from
//! `ctx.getContextUsage()` / the model's context window — one of the few
//! real context sources in the agent landscape), which becomes
//! `context_remaining` and feeds the rings.

use crate::adapters::{session_uuid, title_for};
use crate::events::{Category, Event};
use serde_json::Value;

const AGENT: &str = "pi";

pub fn normalize(payload: &Value) -> anyhow::Result<Event> {
    let hook = payload
        .get("hook_event_name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("payload has no hook_event_name"))?;

    let category =
        category_for(hook).ok_or_else(|| anyhow::anyhow!("unhandled pi event `{hook}`"))?;

    let session = payload
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool = payload.get("tool_name").and_then(Value::as_str);
    // `agent_start` has no tool to title from; say what it means instead.
    let title = if hook == "agent_start" {
        "Working".to_string()
    } else {
        title_for(category, tool, "")
    };

    let mut event = Event::new(category, AGENT, title);
    event.session_id = session_uuid(AGENT, session);
    event.project = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::to_string);
    if hook == "agent_settled" {
        // Same contract as Qwen's Stop hook: a fraction of the window used.
        if let Some(used) = payload.get("context_usage").and_then(Value::as_f64) {
            event.context_remaining = Some((1.0 - used).clamp(0.0, 1.0) as f32);
        }
    }
    Ok(event)
}

/// No decision channel: pi's hooks are observers only.
pub fn is_decidable(_payload: &Value) -> bool {
    false
}

fn category_for(hook: &str) -> Option<Category> {
    Some(match hook {
        "session_start" => Category::SessionStarted,
        "agent_start" | "tool_execution_start" => Category::ToolRunning,
        "tool_execution_end" => Category::ToolFinished,
        "agent_end" | "agent_settled" => Category::TaskComplete,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(hook: &str) -> Value {
        json!({
            "hook_event_name": hook,
            "session_id": "pi-sess-1",
            "cwd": "/proj",
        })
    }

    #[test]
    fn maps_the_extension_contract() {
        assert_eq!(
            normalize(&payload("session_start")).unwrap().category,
            Category::SessionStarted
        );
        assert_eq!(
            normalize(&payload("tool_execution_start"))
                .unwrap()
                .category,
            Category::ToolRunning
        );
        assert_eq!(
            normalize(&payload("tool_execution_end")).unwrap().category,
            Category::ToolFinished
        );
        assert_eq!(
            normalize(&payload("agent_end")).unwrap().category,
            Category::TaskComplete
        );
    }

    #[test]
    fn agent_start_titles_as_working() {
        let e = normalize(&payload("agent_start")).unwrap();
        assert_eq!(e.title, "Working");
        // Tool events title from the tool name.
        let mut p = payload("tool_execution_start");
        p["tool_name"] = json!("bash");
        assert_eq!(normalize(&p).unwrap().title, "Running bash");
    }

    #[test]
    fn settled_carries_context_usage_into_remaining() {
        let mut p = payload("agent_settled");
        p["context_usage"] = json!(0.7);
        let e = normalize(&p).unwrap();
        assert_eq!(e.context_remaining, Some(0.3));
        // Absent usage leaves it None.
        assert_eq!(
            normalize(&payload("agent_settled"))
                .unwrap()
                .context_remaining,
            None
        );
    }

    #[test]
    fn same_session_across_events_and_never_decidable() {
        let a = normalize(&payload("session_start")).unwrap();
        let b = normalize(&payload("agent_settled")).unwrap();
        assert_eq!(a.session_id, b.session_id);
        assert!(!is_decidable(&payload("agent_end")));
    }
}
