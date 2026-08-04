//! The inbox: agent events collapsed into one row per session.
//!
//! `TECH.md` §4 fixes the rules — one row per `sessionId`, later events
//! update that row in place, `approval_required` pins to the top, rows group
//! by `hostId` + `project`, and a row that has been idle for ~3h is archived.
//! Rows live in memory: the host's agents are the source of truth, so a
//! daemon restart re-populating from live sessions is the right recovery.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::context::Context;
use crate::events::{Category, Event, UsageWindow};
use crate::herdr::AgentState;

/// How long a row stays in the inbox after its last event.
const ARCHIVE_AFTER_HOURS: i64 = 3;

/// Ring size for the WebSocket fan-out. A phone that falls this far behind
/// re-reads the full row list on reconnect, so dropping is safe.
const BROADCAST_CAPACITY: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub session_id: Uuid,
    pub host_id: String,
    pub source: String,
    pub project: Option<String>,
    pub category: Category,
    pub title: String,
    pub message: String,
    pub updated_at: DateTime<Utc>,
    pub pending_action_id: Option<Uuid>,
    pub expires_at: Option<DateTime<Utc>>,
    pub usage: Vec<UsageWindow>,
    pub context_remaining: Option<f32>,
    pub terminal: Option<Context>,
    /// Herdr's *native* view of the pane this row's agent runs in — set by
    /// the daemon's herdr poller, not by hook events. `None` for rows whose
    /// agent isn't in a herdr pane, or when herdr isn't running.
    pub agent_state: Option<AgentState>,
    /// How a pending approval was answered, once it has been. `None` while it
    /// is still waiting — which, together with `pending_action_id`, is what
    /// tells the app whether to show Approve/Deny buttons.
    pub resolution: Option<Resolution>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    Allowed,
    Denied,
}

impl Row {
    fn from_event(event: &Event, now: DateTime<Utc>) -> Self {
        Self {
            session_id: event.session_id,
            host_id: event.host_id.clone(),
            source: event.source.clone(),
            project: event.project.clone(),
            category: event.category,
            title: event.title.clone(),
            message: event.message.clone(),
            updated_at: now,
            pending_action_id: event.pending_action_id,
            expires_at: event.expires_at,
            usage: event.usage.clone(),
            context_remaining: event.context_remaining,
            terminal: event.terminal.clone(),
            agent_state: None,
            resolution: None,
        }
    }

    /// A later event replaces the row's visible state, but keeps whatever the
    /// event left unsaid: usage snapshots and context arrive on some events
    /// only, and clearing them would make the rings flicker.
    fn update(&mut self, event: &Event, now: DateTime<Utc>) {
        self.host_id = event.host_id.clone();
        self.source = event.source.clone();
        if event.project.is_some() {
            self.project = event.project.clone();
        }
        self.category = event.category;
        self.title = event.title.clone();
        self.message = event.message.clone();
        self.updated_at = now;
        self.pending_action_id = event.pending_action_id;
        self.expires_at = event.expires_at;
        if !event.usage.is_empty() {
            self.usage = event.usage.clone();
        }
        if event.context_remaining.is_some() {
            self.context_remaining = event.context_remaining;
        }
        // A session's pane doesn't move, and not every hook fires from inside
        // it (a subagent's shell may not inherit the vars), so keep the last
        // one we learned rather than dropping back to "unknown".
        if event.terminal.is_some() {
            self.terminal = event.terminal.clone();
        }
        // A new event supersedes whatever the last approval resolved to.
        self.resolution = None;
    }
}

pub struct Inbox {
    rows: Mutex<HashMap<Uuid, Row>>,
    tx: tokio::sync::broadcast::Sender<Row>,
}

impl Inbox {
    pub fn new() -> Arc<Self> {
        let (tx, _) = tokio::sync::broadcast::channel(BROADCAST_CAPACITY);
        Arc::new(Self {
            rows: Mutex::new(HashMap::new()),
            tx,
        })
    }

    /// Subscribe to row updates — one message per ingested event, carrying
    /// the row as it now stands.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Row> {
        self.tx.subscribe()
    }

    /// Fold an event into its session's row and publish the result.
    pub fn apply(&self, event: Event) -> Row {
        self.apply_at(event, Utc::now())
    }

    fn apply_at(&self, event: Event, now: DateTime<Utc>) -> Row {
        let row = {
            let mut rows = self.rows.lock().unwrap();
            let row = rows
                .entry(event.session_id)
                .and_modify(|r| r.update(&event, now))
                .or_insert_with(|| Row::from_event(&event, now));
            row.clone()
        };
        // No subscribers is the normal case (no phone attached); ignore.
        let _ = self.tx.send(row.clone());
        row
    }

    /// Record how a pending approval was answered and republish the row, so
    /// a phone that didn't send the answer still sees the button go away.
    pub fn resolve(&self, pending_action_id: Uuid, allow: bool) {
        let updated = {
            let mut rows = self.rows.lock().unwrap();
            let row = rows
                .values_mut()
                .find(|r| r.pending_action_id == Some(pending_action_id));
            match row {
                Some(row) => {
                    row.resolution = Some(if allow {
                        Resolution::Allowed
                    } else {
                        Resolution::Denied
                    });
                    Some(row.clone())
                }
                None => None,
            }
        };
        if let Some(row) = updated {
            let _ = self.tx.send(row);
        }
    }

    /// Overlay Herdr's native per-pane state onto rows whose agent runs in
    /// one of `session`'s panes. Rows whose pane has no entry keep whatever
    /// state they had (a pane Herdr doesn't track isn't a reason to blank
    /// one). Publishes only the rows that actually changed.
    pub fn apply_agent_states(&self, session: &str, states: &crate::herdr::AgentStates) {
        let mut changed = Vec::new();
        {
            let mut rows = self.rows.lock().unwrap();
            for row in rows.values_mut() {
                let Some(terminal) = &row.terminal else {
                    continue;
                };
                if terminal.kind != crate::context::Kind::Herdr
                    || terminal.session.as_deref() != Some(session)
                {
                    continue;
                }
                let Some(pane) = terminal.pane.as_deref() else {
                    continue;
                };
                let Some(state) = states.get(pane) else {
                    continue;
                };
                if row.agent_state != Some(*state) {
                    row.agent_state = Some(*state);
                    changed.push(row.clone());
                }
            }
        }
        for row in changed {
            let _ = self.tx.send(row);
        }
    }

    /// Live rows, ordered the way the app renders them: approvals first,
    /// then most recently updated.
    pub fn rows(&self) -> Vec<Row> {
        self.rows_at(Utc::now())
    }

    fn rows_at(&self, now: DateTime<Utc>) -> Vec<Row> {
        let cutoff = now - Duration::hours(ARCHIVE_AFTER_HOURS);
        let mut out: Vec<Row> = self
            .rows
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.updated_at > cutoff)
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            let pinned = |r: &Row| r.category == Category::ApprovalRequired;
            pinned(b)
                .cmp(&pinned(a))
                .then(b.updated_at.cmp(&a.updated_at))
        });
        out
    }

    /// Drop archived rows. Called periodically by the daemon so a long-lived
    /// process doesn't hold every session it ever saw.
    pub fn prune(&self) -> usize {
        self.prune_at(Utc::now())
    }

    fn prune_at(&self, now: DateTime<Utc>) -> usize {
        let cutoff = now - Duration::hours(ARCHIVE_AFTER_HOURS);
        let mut rows = self.rows.lock().unwrap();
        let before = rows.len();
        rows.retain(|_, r| r.updated_at > cutoff);
        before - rows.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(category: Category, session: Uuid, title: &str) -> Event {
        let mut e = Event::new(category, "claude_code", title);
        e.session_id = session;
        e
    }

    #[test]
    fn later_events_update_the_row_in_place() {
        let inbox = Inbox::new();
        let session = Uuid::new_v4();
        inbox.apply(event(Category::SessionStarted, session, "Session started"));
        inbox.apply(event(Category::ToolRunning, session, "Running Bash"));

        let rows = inbox.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Running Bash");
        assert_eq!(rows[0].category, Category::ToolRunning);
    }

    #[test]
    fn approvals_pin_above_newer_rows() {
        let inbox = Inbox::new();
        let now = Utc::now();
        let approval = Uuid::new_v4();
        inbox.apply_at(
            event(Category::ApprovalRequired, approval, "Approve: Bash"),
            now - Duration::minutes(10),
        );
        inbox.apply_at(
            event(Category::ToolRunning, Uuid::new_v4(), "Running Read"),
            now,
        );

        let rows = inbox.rows();
        assert_eq!(rows[0].session_id, approval);
        assert_eq!(rows[1].title, "Running Read");
    }

    #[test]
    fn non_approvals_order_newest_first() {
        let inbox = Inbox::new();
        let now = Utc::now();
        inbox.apply_at(
            event(Category::TaskComplete, Uuid::new_v4(), "older"),
            now - Duration::minutes(5),
        );
        inbox.apply_at(event(Category::TaskComplete, Uuid::new_v4(), "newer"), now);

        let titles: Vec<_> = inbox.rows().into_iter().map(|r| r.title).collect();
        assert_eq!(titles, vec!["newer", "older"]);
    }

    #[test]
    fn rows_idle_past_the_window_are_archived_then_pruned() {
        let inbox = Inbox::new();
        let now = Utc::now();
        inbox.apply_at(
            event(Category::TaskComplete, Uuid::new_v4(), "stale"),
            now - Duration::hours(ARCHIVE_AFTER_HOURS + 1),
        );
        inbox.apply_at(event(Category::TaskComplete, Uuid::new_v4(), "fresh"), now);

        assert_eq!(inbox.rows_at(now).len(), 1);
        assert_eq!(inbox.prune_at(now), 1);
        assert_eq!(inbox.rows_at(now).len(), 1);
    }

    #[test]
    fn usage_and_context_survive_events_that_omit_them() {
        let inbox = Inbox::new();
        let session = Uuid::new_v4();
        let mut first = event(Category::ToolRunning, session, "Running Bash");
        first.usage = vec![UsageWindow {
            label: "5h".into(),
            used: 0.4,
            resets_at: None,
        }];
        first.context_remaining = Some(0.82);
        inbox.apply(first);
        inbox.apply(event(Category::TaskComplete, session, "Task complete"));

        let rows = inbox.rows();
        assert_eq!(rows[0].usage.len(), 1);
        assert_eq!(rows[0].context_remaining, Some(0.82));
    }

    #[test]
    fn the_pane_a_session_was_last_seen_in_sticks() {
        use crate::context::Kind;
        let inbox = Inbox::new();
        let session = Uuid::new_v4();
        let mut tagged = event(Category::SessionStarted, session, "Session started");
        tagged.terminal = Some(Context {
            kind: Kind::Tmux,
            session: Some("0".into()),
            pane: Some("%3".into()),
            ..Default::default()
        });
        inbox.apply(tagged);
        // A later hook that fired outside the pane must not erase it.
        inbox.apply(event(Category::TaskComplete, session, "Task complete"));

        let terminal = inbox.rows()[0].terminal.clone().unwrap();
        assert_eq!(terminal.kind, Kind::Tmux);
        assert_eq!(terminal.pane.as_deref(), Some("%3"));
    }

    #[test]
    fn resolving_an_approval_marks_the_row_answered() {
        let inbox = Inbox::new();
        let action = Uuid::new_v4();
        let mut approval = event(Category::ApprovalRequired, Uuid::new_v4(), "Approve: Bash");
        approval.pending_action_id = Some(action);
        inbox.apply(approval);

        assert_eq!(inbox.rows()[0].resolution, None);
        inbox.resolve(action, true);
        assert_eq!(inbox.rows()[0].resolution, Some(Resolution::Allowed));
    }

    #[test]
    fn resolving_an_unknown_action_changes_nothing() {
        let inbox = Inbox::new();
        inbox.apply(event(Category::ToolRunning, Uuid::new_v4(), "Running Read"));
        inbox.resolve(Uuid::new_v4(), true);
        assert_eq!(inbox.rows()[0].resolution, None);
    }

    #[test]
    fn a_new_event_clears_a_previous_resolution() {
        let inbox = Inbox::new();
        let session = Uuid::new_v4();
        let action = Uuid::new_v4();
        let mut approval = event(Category::ApprovalRequired, session, "Approve: Bash");
        approval.pending_action_id = Some(action);
        inbox.apply(approval);
        inbox.resolve(action, false);
        assert_eq!(inbox.rows()[0].resolution, Some(Resolution::Denied));

        inbox.apply(event(Category::ToolRunning, session, "Running Bash"));
        assert_eq!(inbox.rows()[0].resolution, None);
    }

    #[test]
    fn herdr_native_state_overlays_herdr_rows_only() {
        use crate::context::Kind;
        use crate::herdr::AgentState;

        let inbox = Inbox::new();
        let session = Uuid::new_v4();
        let mut herdr_row = event(Category::ToolRunning, session, "Running Bash");
        herdr_row.terminal = Some(Context {
            kind: Kind::Herdr,
            session: Some("gami".into()),
            pane: Some("w1:p1".into()),
            ..Default::default()
        });
        inbox.apply(herdr_row);
        // A tmux row in the same inbox must be untouched.
        let mut tmux_row = event(Category::ToolRunning, Uuid::new_v4(), "Running Read");
        tmux_row.terminal = Some(Context {
            kind: Kind::Tmux,
            session: Some("0".into()),
            pane: Some("%3".into()),
            ..Default::default()
        });
        inbox.apply(tmux_row);

        let mut states = crate::herdr::AgentStates::new();
        states.insert("w1:p1".into(), AgentState::Blocked);
        inbox.apply_agent_states("gami", &states);

        let rows = inbox.rows();
        let herdr = rows.iter().find(|r| r.session_id == session).unwrap();
        assert_eq!(herdr.agent_state, Some(AgentState::Blocked));
        let tmux = rows.iter().find(|r| r.session_id != session).unwrap();
        assert_eq!(tmux.agent_state, None);
    }

    #[tokio::test]
    async fn herdr_state_merge_only_publishes_changes() {
        use crate::context::Kind;
        use crate::herdr::AgentState;

        let inbox = Inbox::new();
        let mut rx = inbox.subscribe();
        let session = Uuid::new_v4();
        let mut row = event(Category::ToolRunning, session, "Running Bash");
        row.terminal = Some(Context {
            kind: Kind::Herdr,
            session: Some("gami".into()),
            pane: Some("w1:p1".into()),
            ..Default::default()
        });
        inbox.apply(row);
        let _ = rx.recv().await.unwrap(); // the ingest event

        let mut states = crate::herdr::AgentStates::new();
        states.insert("w1:p1".into(), AgentState::Working);
        inbox.apply_agent_states("gami", &states);
        let changed = rx.recv().await.unwrap();
        assert_eq!(changed.agent_state, Some(AgentState::Working));

        // Same state again: no publish.
        inbox.apply_agent_states("gami", &states);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn subscribers_receive_each_applied_row() {
        let inbox = Inbox::new();
        let mut rx = inbox.subscribe();
        inbox.apply(event(
            Category::ApprovalRequired,
            Uuid::new_v4(),
            "Approve: Bash",
        ));
        let row = rx.recv().await.unwrap();
        assert_eq!(row.title, "Approve: Bash");
    }
}
