//! OpenCode plugin payload → normalized `Event`.
//!
//! OpenCode has no hooks file; `install` writes a plugin that forwards a
//! small payload contract to the shim. The adapter maps that contract:
//!
//! | plugin hook_event_name | category |
//! |---|---|
//! | `session.created` | session_started |
//! | `session.idle` | task_complete |
//! | `tool.execute.before` | tool_running |
//! | `tool.execute.after` | tool_finished |
//! | `permission.asked` / `permission.v2.asked` | approval_required (observation) |
//!
//! `permission.ask` — the plugin hook that could actually decide — is
//! declared in the SDK but never triggered (anomalyco/opencode#7006,
//! #9229), so approvals appear in the inbox without an answer channel.

use crate::adapters::{session_uuid, title_for};
use crate::events::{Category, Event};
use serde_json::Value;

const AGENT: &str = "opencode";

pub fn normalize(payload: &Value) -> anyhow::Result<Event> {
    let hook = payload
        .get("hook_event_name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("payload has no hook_event_name"))?;

    let category =
        category_for(hook).ok_or_else(|| anyhow::anyhow!("unhandled OpenCode event `{hook}`"))?;

    let session = payload
        .get("session_id")
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

/// No decision channel: `permission.ask` is declared in the SDK but never
/// triggered, so nothing OpenCode emits can be answered from stdout.
pub fn is_decidable(_payload: &Value) -> bool {
    false
}

fn category_for(hook: &str) -> Option<Category> {
    Some(match hook {
        "session.created" => Category::SessionStarted,
        "session.idle" => Category::TaskComplete,
        "tool.execute.before" => Category::ToolRunning,
        "tool.execute.after" => Category::ToolFinished,
        "permission.asked" | "permission.v2.asked" => Category::ApprovalRequired,
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
            "session_id": "ses_abc",
            "cwd": "/proj",
            "tool_name": "bash",
        })
    }

    #[test]
    fn maps_the_plugin_contract() {
        assert_eq!(
            normalize(&payload("session.created")).unwrap().category,
            Category::SessionStarted
        );
        assert_eq!(
            normalize(&payload("session.idle")).unwrap().category,
            Category::TaskComplete
        );
        assert_eq!(
            normalize(&payload("tool.execute.before")).unwrap().category,
            Category::ToolRunning
        );
        assert_eq!(
            normalize(&payload("tool.execute.after")).unwrap().category,
            Category::ToolFinished
        );
    }

    #[test]
    fn permission_asked_is_an_observation_approval() {
        let mut p = payload("permission.v2.asked");
        p["message"] = json!("Bash: npm publish");
        let e = normalize(&p).unwrap();
        assert_eq!(e.category, Category::ApprovalRequired);
        assert!(e.pending_action_id.is_none());
        assert!(!is_decidable(&p));
        assert_eq!(e.title, "Approve: bash");
    }

    #[test]
    fn same_session_across_events() {
        let a = normalize(&payload("session.created")).unwrap();
        let b = normalize(&payload("session.idle")).unwrap();
        assert_eq!(a.session_id, b.session_id);
    }
}
