//! Agent event schema — the normalized 5-category contract between agents
//! and the sailor app. See `TECH.md` §4.
//!
//! Phase 0: just the types. Phase 3 wires ingestion (Unix socket) and the
//! per-agent adapters that normalize each agent's hook output into this.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    ApprovalRequired,
    TaskComplete,
    SessionStarted,
    ToolRunning,
    ToolFinished,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageWindow {
    pub label: String,
    /// 0.0–1.0 fraction used.
    pub used: f32,
    pub resets_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub category: Category,
    pub source: String,
    pub session_id: Uuid,
    pub host_id: String,
    pub project: Option<String>,
    pub title: String,
    #[serde(default)]
    pub message: String,
    pub event_id: Uuid,
    // approval_required only:
    pub pending_action_id: Option<Uuid>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    // optional usage snapshot:
    #[serde(default)]
    pub usage: Vec<UsageWindow>,
    pub context_remaining: Option<f32>,
}

impl Event {
    pub fn new(category: Category, source: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            category,
            source: source.into(),
            session_id: Uuid::new_v4(),
            host_id: String::new(),
            project: None,
            title: title.into(),
            message: String::new(),
            event_id: Uuid::new_v4(),
            pending_action_id: None,
            expires_at: None,
            usage: Vec::new(),
            context_remaining: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_serializes_snake_case() {
        let s = serde_json::to_string(&Category::ApprovalRequired).unwrap();
        assert_eq!(s, "\"approval_required\"");
        let s = serde_json::to_string(&Category::TaskComplete).unwrap();
        assert_eq!(s, "\"task_complete\"");
    }

    #[test]
    fn event_roundtrip() {
        let e = Event::new(Category::TaskComplete, "claude_code", "done");
        let json = serde_json::to_string(&e).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back.category, Category::TaskComplete);
        assert_eq!(back.source, "claude_code");
    }
}
