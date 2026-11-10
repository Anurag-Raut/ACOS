//! Session registry for stateful commands.
//!
//! Today `inspect` and `run-dex` are one-shot, so the registry is usually empty.
//! It exists so that `run`/`observe`/`action`/... can register sessions without
//! a server rewrite, and it sweeps idle sessions past a TTL.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use acos_protocol::{DisplaySpec, IsolationLevel};

/// One live session.
#[derive(Debug, Clone)]
pub struct SessionRecord {
    /// Session id (`s_*`).
    pub id: String,
    /// Creation time.
    pub created: Instant,
    /// Last time the session was touched.
    pub last_used: Instant,
    /// Effective isolation, once known.
    pub isolation: Option<IsolationLevel>,
    /// Effective display, once known.
    pub display: Option<DisplaySpec>,
    /// Human label, when provided.
    pub label: Option<String>,
}

/// A TTL-bounded map of sessions.
#[derive(Debug)]
pub struct SessionRegistry {
    ttl: Duration,
    sessions: HashMap<String, SessionRecord>,
}

impl SessionRegistry {
    /// Create an empty registry with an idle TTL.
    pub fn new(ttl: Duration) -> Self {
        SessionRegistry {
            ttl,
            sessions: HashMap::new(),
        }
    }

    /// The idle TTL.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Insert (or replace) a session.
    pub fn insert(&mut self, record: SessionRecord) {
        self.sessions.insert(record.id.clone(), record);
    }

    /// Create and insert a new session with sensible defaults.
    pub fn create(&mut self, id: impl Into<String>, label: Option<String>) -> &SessionRecord {
        let now = Instant::now();
        let id = id.into();
        self.sessions.insert(
            id.clone(),
            SessionRecord {
                id: id.clone(),
                created: now,
                last_used: now,
                isolation: None,
                display: None,
                label,
            },
        );
        self.sessions.get(&id).expect("just inserted")
    }

    /// Look up a session, touching its idle timer.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut SessionRecord> {
        let now = Instant::now();
        let record = self.sessions.get_mut(id)?;
        record.last_used = now;
        Some(record)
    }

    /// Remove a session, returning it.
    pub fn remove(&mut self, id: &str) -> Option<SessionRecord> {
        self.sessions.remove(id)
    }

    /// Remove sessions idle longer than the TTL and return how many were swept.
    pub fn sweep(&mut self, now: Instant) -> usize {
        let ttl = self.ttl;
        let before = self.sessions.len();
        self.sessions
            .retain(|_, record| now.saturating_duration_since(record.last_used) < ttl);
        before - self.sessions.len()
    }

    /// Number of live sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// True when no sessions exist.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Session ids, sorted.
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.sessions.keys().cloned().collect();
        ids.sort();
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_expire_after_ttl() {
        let mut registry = SessionRegistry::new(Duration::from_millis(20));
        registry.create("s_1", None);
        assert_eq!(registry.len(), 1);
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(registry.sweep(Instant::now()), 1);
        assert!(registry.is_empty());
    }

    #[test]
    fn get_touches_idle_timer() {
        let mut registry = SessionRegistry::new(Duration::from_millis(60));
        registry.create("s_1", None);
        std::thread::sleep(Duration::from_millis(30));
        assert!(registry.get_mut("s_1").is_some());
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(registry.sweep(Instant::now()), 0);
    }
}
