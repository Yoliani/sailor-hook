//! Gemini CLI hook payload → normalized `Event`.
//!
//! Gemini CLI's hooks (`~/.gemini/settings.json`) share the nested JSON
//! shape but have their own event names (`BeforeTool`, `AfterTool`,
//! `SessionStart`, …) and no `PermissionRequest` at all — its
//! `Notification` (with `notification_type: ToolPermission`) is the closest
//! thing to an approval, and it is observation-only. So Gemini reports
//! events to the inbox but has no approve-from-phone channel.

use crate::adapters::{common_fields, event_name, session_uuid, title_for};
use crate::events::{Category, Event};
use serde_json::Value;

const AGENT: &str = "gemini";

pub fn normalize(payload: &Value) -> anyhow::Result<Event> {
    let hook =
        event_name(payload).ok_or_else(|| anyhow::anyhow!("payload has no hook_event_name"))?;

    let category = category_for(hook)
        .ok_or_else(|| anyhow::anyhow!("unhandled Gemini hook event `{hook}`"))?;

    let f = common_fields(payload);
    let mut event = Event::new(category, AGENT, title_for(category, f.tool, f.message));
    event.session_id = session_uuid(AGENT, f.session);
    event.project = f.project.map(str::to_string);
    event.message = f.message.to_string();
    Ok(event)
}

/// No decision channel: Gemini has no `PermissionRequest` event.
pub fn is_decidable(_payload: &Value) -> bool {
    false
}

fn category_for(hook: &str) -> Option<Category> {
    Some(match hook {
        "SessionStart" => Category::SessionStarted,
        "BeforeTool" => Category::ToolRunning,
        "AfterTool" => Category::ToolFinished,
        "Notification" => Category::ApprovalRequired,
        "SessionEnd" => Category::TaskComplete,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(hook: &str) -> Value {
        json!({
            "session_id": "gs-1",
            "cwd": "/proj",
            "hook_event_name": hook,
        })
    }

    #[test]
    fn maps_gemini_event_names() {
        assert_eq!(
            normalize(&payload("BeforeTool")).unwrap().category,
            Category::ToolRunning
        );
        assert_eq!(
            normalize(&payload("AfterTool")).unwrap().category,
            Category::ToolFinished
        );
        assert_eq!(
            normalize(&payload("SessionStart")).unwrap().category,
            Category::SessionStarted
        );
        assert_eq!(
            normalize(&payload("SessionEnd")).unwrap().category,
            Category::TaskComplete
        );
    }

    #[test]
    fn notification_is_an_observation_approval() {
        let mut p = payload("Notification");
        p["notification_type"] = json!("ToolPermission");
        p["message"] = json!("Gemini wants to run: make test");
        let e = normalize(&p).unwrap();
        assert_eq!(e.category, Category::ApprovalRequired);
        // No answer channel: no pending id, and never decidable.
        assert!(e.pending_action_id.is_none());
        assert!(!is_decidable(&p));
    }

    #[test]
    fn unknown_events_are_rejected() {
        assert!(normalize(&payload("BeforeModel")).is_err());
        assert!(normalize(&payload("PreCompress")).is_err());
    }
}
