//! Herdr's native agent-pane state, read from `herdr agent list`.
//!
//! Herdr is the one multiplexer that tracks agent lifecycle itself: its
//! sidebar shows each agent pane as blocked / working / done / idle, and
//! `herdr --session <name> agent list` returns that state keyed by
//! `pane_id`. The inbox rows from hooks are hook-derived (a `tool_running`
//! event only appears while a tool runs); this module lets the daemon
//! overlay Herdr's own view of each pane onto the rows, so the phone shows
//! the same state Herdr's sidebar shows. See `TECH.md` §2.3.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Command;

/// The pane states Herdr reports. `Idle` is the resting state; anything
/// Herdr reports that we don't model is skipped (a row keeps its last state
/// rather than dropping to unknown).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Blocked,
    Working,
    Done,
    Idle,
}

impl AgentState {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "blocked" => AgentState::Blocked,
            "working" => AgentState::Working,
            "done" => AgentState::Done,
            "idle" => AgentState::Idle,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            AgentState::Blocked => "blocked",
            AgentState::Working => "working",
            AgentState::Done => "done",
            AgentState::Idle => "idle",
        }
    }
}

/// `pane_id` → state for one herdr session.
pub type AgentStates = HashMap<String, AgentState>;

/// Query one herdr session's agent states. The CLI resolves `--session
/// <name>` against the server's sessions (verified against herdr 0.7). A
/// missing herdr binary or a dead server yields an empty map — the poller
/// treats that as "nothing to report", never as an error worth surfacing.
pub fn list_states(session: &str) -> AgentStates {
    let out = Command::new("herdr")
        .args(["--session", session, "agent", "list"])
        .env("PATH", extra_path())
        .output();
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            tracing::debug!("herdr list failed to spawn: {e}");
            return AgentStates::new();
        }
    };
    if !out.status.success() {
        tracing::debug!(
            "herdr list exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        return AgentStates::new();
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed = parse_agent_list(&stdout);
    if parsed.is_empty() {
        tracing::debug!("herdr list parsed 0 agents from {} bytes", stdout.len());
    }
    parsed
}

/// Parse `herdr agent list` output (wrapped `{result:{agents:[…]}}` or a
/// bare array) into pane states. Real schema (herdr 0.7): each agent has
/// `pane_id` and `agent_status`; unknown or missing fields are skipped.
pub fn parse_agent_list(output: &str) -> AgentStates {
    let mut states = AgentStates::new();
    for item in unwrap_json_list(output, "agents") {
        let Some(pane) = item.get("pane_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(state) = item
            .get("agent_status")
            .and_then(|v| v.as_str())
            .and_then(AgentState::parse)
        else {
            continue;
        };
        states.insert(pane.to_string(), state);
    }
    states
}

/// The same PATH widening the app applies over SSH: macOS non-interactive
/// shells strip Homebrew and user bins, so `herdr` in `/opt/homebrew/bin`
/// would be invisible to a spawned child.
fn extra_path() -> String {
    let mut dirs = vec![
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".to_string(),
        "$HOME/.local/bin".to_string(),
        "$HOME/bin".to_string(),
    ];
    if let Ok(existing) = std::env::var("PATH") {
        dirs.push(existing);
    }
    dirs.join(":")
}

/// Pull `{result: { <key>: [...] }}` (herdr CLI) or a bare array of records
/// as owned values — small payloads, and it keeps the parser free of
/// lifetime plumbing.
fn unwrap_json_list(output: &str, key: &str) -> Vec<serde_json::Value> {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(output) else {
        return Vec::new();
    };
    match parsed {
        serde_json::Value::Array(arr) => arr,
        serde_json::Value::Object(obj) => {
            if let Some(serde_json::Value::Array(arr)) = obj.get(key) {
                arr.clone()
            } else if let Some(serde_json::Value::Object(inner)) = obj.get("result") {
                match inner.get(key) {
                    Some(serde_json::Value::Array(arr)) => arr.clone(),
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_agent_list_shape() {
        let output = r#"{"id":"cli:agent:list","result":{"agents":[
            {"agent":"claude","pane_id":"w1:p1","agent_status":"blocked"},
            {"agent":"pi","pane_id":"w7:pJ","agent_status":"working"},
            {"agent":"pi","pane_id":"w7:pF","agent_status":"idle"},
            {"agent":"pi","pane_id":"w5:p4","agent_status":"done"}
        ]}}"#;
        let states = parse_agent_list(output);
        assert_eq!(states.get("w1:p1"), Some(&AgentState::Blocked));
        assert_eq!(states.get("w7:pJ"), Some(&AgentState::Working));
        assert_eq!(states.get("w7:pF"), Some(&AgentState::Idle));
        assert_eq!(states.get("w5:p4"), Some(&AgentState::Done));
        assert_eq!(states.len(), 4);
    }

    #[test]
    fn tolerates_a_bare_array_and_unknown_states() {
        let output = r#"[
            {"pane_id":"w1:p1","agent_status":"working"},
            {"pane_id":"w1:p2","agent_status":"something-else"},
            {"pane_id":"w1:p3","agent_status":"done"},
            {"pane_id":"w1:p4"}
        ]"#;
        let states = parse_agent_list(output);
        assert_eq!(states.get("w1:p1"), Some(&AgentState::Working));
        // Unknown state and missing state are both skipped.
        assert_eq!(states.len(), 2);
    }

    #[test]
    fn garbage_and_missing_fields_degrade_to_empty() {
        assert!(parse_agent_list("not json").is_empty());
        assert!(parse_agent_list("[]").is_empty());
        assert!(parse_agent_list(r#"{"result":{}}"#).is_empty());
        assert!(parse_agent_list(r#"{"result":{"agents":[{}]}}"#).is_empty());
    }

    #[test]
    fn parse_roundtrip() {
        for s in ["blocked", "working", "done", "idle"] {
            let st = AgentState::parse(s).unwrap();
            assert_eq!(st.as_str(), s);
        }
        assert!(AgentState::parse("hung").is_none());
    }
}
