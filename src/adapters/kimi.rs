//! Kimi Code hook payload → normalized `Event`.
//!
//! Kimi Code's hooks (`~/.kimi-code/config.toml`, a flat `[[hooks]]` array)
//! pass `{ hook_event_name, session_id, cwd, … }` on stdin — the same base
//! fields as Claude Code. Its `PermissionRequest` event exists but is
//! observation-only (moonshotai/kimi-cli#2154 asks for a decision channel
//! that doesn't exist yet), so Kimi reports to the inbox but nothing parks.

use crate::adapters::{common_fields, event_name, session_uuid, title_for};
use crate::events::{Category, Event};
use serde_json::Value;

const AGENT: &str = "kimi";

pub fn normalize(payload: &Value) -> anyhow::Result<Event> {
    let hook =
        event_name(payload).ok_or_else(|| anyhow::anyhow!("payload has no hook_event_name"))?;

    let category =
        category_for(hook).ok_or_else(|| anyhow::anyhow!("unhandled Kimi hook event `{hook}`"))?;

    let f = common_fields(payload);
    let mut event = Event::new(category, AGENT, title_for(category, f.tool, f.message));
    event.session_id = session_uuid(AGENT, f.session);
    event.project = f.project.map(str::to_string);
    event.message = f.message.to_string();
    Ok(event)
}

/// No decision channel: Kimi's `PermissionRequest` is observation-only.
pub fn is_decidable(_payload: &Value) -> bool {
    false
}

fn category_for(hook: &str) -> Option<Category> {
    Some(match hook {
        "SessionStart" => Category::SessionStarted,
        "PreToolUse" => Category::ToolRunning,
        "PostToolUse" | "PostToolUseFailure" => Category::ToolFinished,
        "Notification" | "PermissionRequest" => Category::ApprovalRequired,
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
            "session_id": "session_abc",
            "cwd": "/proj",
            "hook_event_name": hook,
        })
    }

    #[test]
    fn maps_kimi_events() {
        assert_eq!(
            normalize(&payload("PreToolUse")).unwrap().category,
            Category::ToolRunning
        );
        assert_eq!(
            normalize(&payload("PostToolUseFailure")).unwrap().category,
            Category::ToolFinished
        );
        assert_eq!(
            normalize(&payload("SessionStart")).unwrap().category,
            Category::SessionStarted
        );
        assert_eq!(
            normalize(&payload("Stop")).unwrap().category,
            Category::TaskComplete
        );
    }

    #[test]
    fn permission_request_is_observation_only() {
        // Kimi fires PermissionRequest just before the approval UI, but its
        // stdout is not read as a decision.
        let e = normalize(&payload("PermissionRequest")).unwrap();
        assert_eq!(e.category, Category::ApprovalRequired);
        assert!(e.pending_action_id.is_none());
        assert!(!is_decidable(&payload("PermissionRequest")));
    }
}
