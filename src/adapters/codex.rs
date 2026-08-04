//! Codex (OpenAI CLI) hook payload → normalized `Event`.
//!
//! Codex's hook system shares Claude Code's schema family (the codex-rs
//! hooks module was built against the same wire format): same
//! `hook_event_name`/`session_id`/`cwd`/`tool_name` fields, same nested
//! `hooks.json` shape, and a `PermissionRequest` whose stdout is read as a
//! decision. See `developers.openai.com/codex/hooks`.

use crate::adapters::{common_fields, event_name, session_uuid, title_for};
use crate::events::{Category, Event};
use serde_json::{json, Value};

const AGENT: &str = "codex";

pub fn normalize(payload: &Value) -> anyhow::Result<Event> {
    let hook =
        event_name(payload).ok_or_else(|| anyhow::anyhow!("payload has no hook_event_name"))?;

    let category =
        category_for(hook).ok_or_else(|| anyhow::anyhow!("unhandled Codex hook event `{hook}`"))?;

    let f = common_fields(payload);
    let mut event = Event::new(category, AGENT, title_for(category, f.tool, f.message));
    event.session_id = session_uuid(AGENT, f.session);
    event.project = f.project.map(str::to_string);
    event.message = f.message.to_string();
    if category == Category::ApprovalRequired {
        // Codex's PermissionRequest gets answered from stdout; the id the
        // app resolves is ours.
        event.pending_action_id = Some(uuid::Uuid::new_v4());
    }
    Ok(event)
}

/// Codex's `PermissionRequest` is the one event whose stdout is read as a
/// decision — identical to Claude Code's.
pub fn is_decidable(payload: &Value) -> bool {
    event_name(payload) == Some("PermissionRequest")
}

/// The stdout Codex parses as the decision. Codex's `PermissionRequest`
/// accepts `decision.behavior` and a `message`; it explicitly reserves
/// `updatedInput` for the future, so `allow` carries nothing else.
pub fn render_decision(payload: &Value, allow: bool) -> Value {
    let behavior = if allow { "allow" } else { "deny" };
    let mut decision = json!({ "behavior": behavior });
    if !allow {
        decision["message"] = Value::String("Denied from the sailor app.".into());
    }
    json!({
        "hookSpecificOutput": {
            "hookEventName": payload
                .get("hook_event_name")
                .and_then(Value::as_str)
                .unwrap_or("PermissionRequest"),
            "decision": decision,
        }
    })
}

fn category_for(hook: &str) -> Option<Category> {
    Some(match hook {
        "SessionStart" => Category::SessionStarted,
        "PreToolUse" => Category::ToolRunning,
        "PostToolUse" => Category::ToolFinished,
        "PermissionRequest" => Category::ApprovalRequired,
        "Stop" | "SubagentStop" | "SessionEnd" => Category::TaskComplete,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(hook: &str) -> Value {
        json!({
            "session_id": "thr_123",
            "cwd": "/workspace/proj",
            "hook_event_name": hook,
        })
    }

    #[test]
    fn maps_tool_and_lifecycle_events() {
        let e = normalize(&payload("PreToolUse")).unwrap();
        assert_eq!(e.category, Category::ToolRunning);
        assert_eq!(e.title, "Running tool");
        assert_eq!(e.project.as_deref(), Some("/workspace/proj"));

        let e = normalize(&payload("SessionEnd")).unwrap();
        assert_eq!(e.category, Category::TaskComplete);
    }

    #[test]
    fn same_session_id_across_events() {
        let a = normalize(&payload("SessionStart")).unwrap();
        let b = normalize(&payload("Stop")).unwrap();
        assert_eq!(a.session_id, b.session_id);
    }

    #[test]
    fn permission_request_is_decidable_and_renders() {
        assert!(is_decidable(&payload("PermissionRequest")));
        assert!(!is_decidable(&payload("PreToolUse")));

        let e = normalize(&payload("PermissionRequest")).unwrap();
        assert_eq!(e.category, Category::ApprovalRequired);
        assert!(e.pending_action_id.is_some());

        let allow = render_decision(&payload("PermissionRequest"), true);
        assert_eq!(allow["hookSpecificOutput"]["decision"]["behavior"], "allow");
        assert!(allow["hookSpecificOutput"]["decision"]
            .get("updatedInput")
            .is_none());

        let deny = render_decision(&payload("PermissionRequest"), false);
        assert_eq!(deny["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert!(deny["hookSpecificOutput"]["decision"]["message"]
            .as_str()
            .is_some());
    }
}
