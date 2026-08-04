//! Qwen Code hook payload → normalized `Event`.
//!
//! Qwen Code's hook system (`.qwen/settings.json`) uses the same nested JSON
//! shape and the same field names as Claude Code — `hook_event_name`,
//! `session_id`, `cwd`, `tool_name` — and its `PermissionRequest` decides
//! from stdout with the same `decision.behavior` JSON. The differences that
//! matter here: hook `timeout` values are milliseconds (handled at install),
//! and Qwen's `Stop` event carries a `context_usage` ratio, which is one of
//! the few places in the whole agent landscape that actually feeds the
//! usage/context rings.

use crate::adapters::{common_fields, event_name, session_uuid, title_for};
use crate::events::{Category, Event};
use serde_json::{json, Value};

const AGENT: &str = "qwen";

pub fn normalize(payload: &Value) -> anyhow::Result<Event> {
    let hook =
        event_name(payload).ok_or_else(|| anyhow::anyhow!("payload has no hook_event_name"))?;

    let category =
        category_for(hook).ok_or_else(|| anyhow::anyhow!("unhandled Qwen hook event `{hook}`"))?;

    let f = common_fields(payload);
    let mut event = Event::new(category, AGENT, title_for(category, f.tool, f.message));
    event.session_id = session_uuid(AGENT, f.session);
    event.project = f.project.map(str::to_string);
    event.message = f.message.to_string();
    if category == Category::ApprovalRequired {
        event.pending_action_id = Some(uuid::Uuid::new_v4());
    }
    if hook == "Stop" {
        // Qwen's Stop hook reports `context_usage` (fraction of the window
        // used) — the one hook payload in the supported set that carries
        // context, so this is where the rings get their data.
        if let Some(used) = payload.get("context_usage").and_then(Value::as_f64) {
            event.context_remaining = Some((1.0 - used).clamp(0.0, 1.0) as f32);
        }
    }
    Ok(event)
}

/// Qwen's `PermissionRequest` decides from stdout, like Claude Code's.
pub fn is_decidable(payload: &Value) -> bool {
    event_name(payload) == Some("PermissionRequest")
}

/// The stdout Qwen parses as the decision. Same `decision.behavior` shape;
/// Qwen accepts `updatedInput` on allow, so the tool input echoes back
/// unchanged.
pub fn render_decision(payload: &Value, allow: bool) -> Value {
    let behavior = if allow { "allow" } else { "deny" };
    let mut decision = json!({ "behavior": behavior });
    if allow {
        let input = payload.get("tool_input").cloned().unwrap_or(json!({}));
        decision["updatedInput"] = input;
    } else {
        decision["message"] = Value::String("Denied from the sailor app.".into());
    }
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": decision,
        }
    })
}

fn category_for(hook: &str) -> Option<Category> {
    Some(match hook {
        "SessionStart" => Category::SessionStarted,
        "PreToolUse" => Category::ToolRunning,
        "PostToolUse" | "PostToolUseFailure" => Category::ToolFinished,
        "PermissionRequest" | "Notification" => Category::ApprovalRequired,
        "Stop" | "SessionEnd" => Category::TaskComplete,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(hook: &str) -> Value {
        json!({
            "session_id": "s1",
            "cwd": "/proj",
            "hook_event_name": hook,
        })
    }

    #[test]
    fn maps_events_and_parks_permission_requests() {
        assert_eq!(
            normalize(&payload("PreToolUse")).unwrap().category,
            Category::ToolRunning
        );
        assert_eq!(
            normalize(&payload("PostToolUseFailure")).unwrap().category,
            Category::ToolFinished
        );
        let approval = normalize(&payload("PermissionRequest")).unwrap();
        assert_eq!(approval.category, Category::ApprovalRequired);
        assert!(approval.pending_action_id.is_some());
        // Notification looks the same but is observation-only.
        assert!(is_decidable(&payload("PermissionRequest")));
        assert!(!is_decidable(&payload("Notification")));
    }

    #[test]
    fn stop_fills_context_remaining_from_context_usage() {
        let mut p = payload("Stop");
        p["context_usage"] = json!(0.82);
        let e = normalize(&p).unwrap();
        assert_eq!(e.category, Category::TaskComplete);
        assert_eq!(e.context_remaining, Some(0.18));

        // Absent or nonsense usage leaves the field None.
        let e = normalize(&payload("Stop")).unwrap();
        assert_eq!(e.context_remaining, None);
        let mut p = payload("Stop");
        p["context_usage"] = json!("nope");
        assert_eq!(normalize(&p).unwrap().context_remaining, None);
    }

    #[test]
    fn allow_echoes_tool_input() {
        let p = json!({
            "hook_event_name": "PermissionRequest",
            "tool_input": { "command": "make test" },
        });
        let out = render_decision(&p, true);
        assert_eq!(out["hookSpecificOutput"]["decision"]["behavior"], "allow");
        assert_eq!(
            out["hookSpecificOutput"]["decision"]["updatedInput"]["command"],
            "make test"
        );
    }
}
