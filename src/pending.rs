//! Approvals waiting on the phone.
//!
//! A decidable approval (`adapters::is_decidable`) parks the agent: the hook
//! process holds its socket connection open while the daemon keeps a
//! one-shot channel keyed by `pending_action_id`. `sailor-hook approve`
//! resolves it, the answer travels back down the held connection, and the
//! hook prints the agent's decision JSON.
//!
//! The safety property that matters: **every failure path must end in "no
//! decision", never "allow".** A daemon that dies, a phone that never
//! answers, a timeout — all of them fall through to the agent asking in the
//! terminal, which is exactly what would have happened without sailor.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

#[derive(Default)]
pub struct Pending {
    waiting: Mutex<HashMap<Uuid, tokio::sync::oneshot::Sender<bool>>>,
}

impl Pending {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Register an approval and get the receiver the hook's connection waits
    /// on. Dropping the receiver (hook exited, timed out) leaves a stale
    /// entry, which `resolve` cleans up when it finds a closed channel.
    pub fn register(&self, id: Uuid) -> tokio::sync::oneshot::Receiver<bool> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.waiting.lock().unwrap().insert(id, tx);
        rx
    }

    /// Answer an approval. `false` means there was nothing waiting on that
    /// id — already answered, timed out, or never existed — which the CLI
    /// reports rather than silently succeeding.
    pub fn resolve(&self, id: Uuid, allow: bool) -> bool {
        let Some(tx) = self.waiting.lock().unwrap().remove(&id) else {
            return false;
        };
        // `send` fails when the hook already gave up; that is still "nothing
        // is waiting", so report it honestly.
        tx.send(allow).is_ok()
    }

    /// Forget an approval whose hook has gone away.
    pub fn cancel(&self, id: Uuid) {
        self.waiting.lock().unwrap().remove(&id);
    }

    pub fn len(&self) -> usize {
        self.waiting.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolve_delivers_the_answer_to_the_waiting_hook() {
        let pending = Pending::new();
        let id = Uuid::new_v4();
        let rx = pending.register(id);
        assert!(pending.resolve(id, true));
        assert!(rx.await.unwrap());
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn deny_travels_the_same_path() {
        let pending = Pending::new();
        let id = Uuid::new_v4();
        let rx = pending.register(id);
        pending.resolve(id, false);
        assert!(!rx.await.unwrap());
    }

    #[test]
    fn resolving_an_unknown_id_reports_failure() {
        let pending = Pending::new();
        assert!(!pending.resolve(Uuid::new_v4(), true));
    }

    #[test]
    fn resolving_twice_only_works_once() {
        let pending = Pending::new();
        let id = Uuid::new_v4();
        let _rx = pending.register(id);
        assert!(pending.resolve(id, true));
        assert!(!pending.resolve(id, true));
    }

    #[test]
    fn an_abandoned_hook_cannot_be_answered() {
        let pending = Pending::new();
        let id = Uuid::new_v4();
        drop(pending.register(id)); // hook timed out and exited
        assert!(!pending.resolve(id, true));
        assert!(pending.is_empty());
    }

    #[test]
    fn cancel_forgets_the_entry() {
        let pending = Pending::new();
        let id = Uuid::new_v4();
        let _rx = pending.register(id);
        pending.cancel(id);
        assert!(pending.is_empty());
    }
}
