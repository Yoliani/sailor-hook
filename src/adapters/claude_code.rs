//! Claude Code hook payload → normalized `Event`.
//!
//! Claude Code runs each configured hook as a command and writes a JSON
//! object to its stdin. The fields we rely on are the ones common to every
//! hook event — `session_id`, `cwd`, `hook_event_name` — plus `tool_name`
//! (tool events) and `message` (Notification). Anything else is ignored, so
//! new fields upstream can't break ingestion.

use crate::adapters::session_uuid;
use crate::events::{Category, Event};
use serde_json::Value;

const AGENT: &str = "claude_code";

pub fn normalize(payload: &Value) -> anyhow::Result<Event> {
    let hook = payload
        .get("hook_event_name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("payload has no hook_event_name"))?;

    let category = category_for(hook)
        .ok_or_else(|| anyhow::anyhow!("unhandled Claude Code hook event `{hook}`"))?;

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
    if category == Category::ApprovalRequired {
        event.pending_action_id = Some(uuid::Uuid::new_v4());
    }
    Ok(event)
}

/// `PermissionRequest` is the one hook whose stdout Claude Code reads as a
/// permission decision. `Notification` looks the same in the inbox but has no
/// answer channel, so blocking on it would stall the agent for nothing.
pub fn is_decidable(payload: &Value) -> bool {
    payload.get("hook_event_name").and_then(Value::as_str) == Some("PermissionRequest")
}

/// The stdout Claude Code parses as the decision. `allow` echoes the tool
/// input back unchanged — this path answers the user's question, it does not
/// rewrite the agent's command.
pub fn render_decision(payload: &Value, allow: bool) -> Value {
    let behavior = if allow { "allow" } else { "deny" };
    let mut decision = serde_json::json!({ "behavior": behavior });
    if allow {
        let input = payload
            .get("tool_input")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        decision["updatedInput"] = input;
    } else {
        decision["message"] = Value::String("Denied from the sailor app.".into());
    }
    serde_json::json!({
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
        "Notification" | "PermissionRequest" => Category::ApprovalRequired,
        "Stop" | "SubagentStop" => Category::TaskComplete,
        _ => return None,
    })
}

/// One line, scannable in a push notification and as the inbox row's title.
fn title_for(category: Category, tool: Option<&str>, message: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_pre_tool_use() {
        let e = normalize(&json!({
            "session_id": "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
            "cwd": "/Users/x/projects/foo",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
        }))
        .unwrap();
        assert_eq!(e.category, Category::ToolRunning);
        assert_eq!(e.title, "Running Bash");
        assert_eq!(e.project.as_deref(), Some("/Users/x/projects/foo"));
        assert_eq!(
            e.session_id.to_string(),
            "3f2504e0-4f89-11d3-9a0c-0305e82c3301"
        );
        assert!(e.pending_action_id.is_none());
    }

    #[test]
    fn permission_request_is_an_approval_with_an_action_id() {
        let e = normalize(&json!({
            "session_id": "s1",
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
        }))
        .unwrap();
        assert_eq!(e.category, Category::ApprovalRequired);
        assert_eq!(e.title, "Approve: Bash");
        assert!(e.pending_action_id.is_some());
    }

    #[test]
    fn notification_without_a_tool_titles_from_its_message() {
        let e = normalize(&json!({
            "session_id": "s1",
            "hook_event_name": "Notification",
            "message": "Claude needs your permission\nto continue",
        }))
        .unwrap();
        assert_eq!(e.title, "Claude needs your permission");
        assert_eq!(e.message, "Claude needs your permission\nto continue");
    }

    #[test]
    fn same_session_id_across_events() {
        let mk = |hook: &str| {
            normalize(&json!({ "session_id": "abc", "hook_event_name": hook })).unwrap()
        };
        assert_eq!(mk("SessionStart").session_id, mk("Stop").session_id);
    }

    #[test]
    fn only_permission_request_is_decidable() {
        assert!(is_decidable(
            &json!({ "hook_event_name": "PermissionRequest" })
        ));
        // Looks like an approval in the inbox, but has no answer channel.
        assert!(!is_decidable(&json!({ "hook_event_name": "Notification" })));
        assert!(!is_decidable(&json!({ "hook_event_name": "PreToolUse" })));
    }

    #[test]
    fn allow_echoes_the_tool_input_unchanged() {
        let payload = json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "tool_input": { "command": "npm test" },
        });
        let out = render_decision(&payload, true);
        let decision = &out["hookSpecificOutput"]["decision"];
        assert_eq!(
            out["hookSpecificOutput"]["hookEventName"],
            "PermissionRequest"
        );
        assert_eq!(decision["behavior"], "allow");
        assert_eq!(decision["updatedInput"]["command"], "npm test");
    }

    #[test]
    fn deny_carries_a_message_and_no_input() {
        let out = render_decision(&json!({ "hook_event_name": "PermissionRequest" }), false);
        let decision = &out["hookSpecificOutput"]["decision"];
        assert_eq!(decision["behavior"], "deny");
        assert!(decision.get("updatedInput").is_none());
        assert!(decision["message"].as_str().is_some());
    }

    #[test]
    fn rejects_payloads_it_cannot_map() {
        assert!(normalize(&json!({ "session_id": "s" })).is_err());
        assert!(normalize(&json!({ "hook_event_name": "PreCompact" })).is_err());
    }
}
