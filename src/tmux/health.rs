#![allow(dead_code)]

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::TmuxClient;

/// Health state of an agent
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    /// Agent is actively producing output
    Running,
    /// Agent has not produced new output recently
    Idle,
}

impl HealthState {
    pub fn as_str(&self) -> &'static str {
        match self {
            HealthState::Running => "running",
            HealthState::Idle => "idle",
        }
    }
}

/// Reports terminal activity, never semantic task completion. An unchanged
/// frame becomes idle only after the configured interval has elapsed.
pub struct HealthChecker {
    client: TmuxClient,
    /// Last captured pane content per session name
    last_frames: HashMap<String, (String, Instant)>,
    idle_threshold: Duration,
}

impl HealthChecker {
    pub fn new(client: TmuxClient, idle_threshold: i64) -> Self {
        Self {
            client,
            last_frames: HashMap::new(),
            idle_threshold: Duration::from_secs(idle_threshold.max(0) as u64),
        }
    }

    /// Check the health of a session by comparing against the previous frame.
    /// Initial observations use tmux activity instead of assuming Running.
    pub fn check(&mut self, session_name: &str) -> HealthState {
        let current = self
            .client
            .capture_pane(session_name, 50)
            .unwrap_or_default();

        let now = Instant::now();
        let initial_change = if self.last_frames.contains_key(session_name) {
            now
        } else {
            let activity = self
                .client
                .get_pane_activity(session_name)
                .unwrap_or(0)
                .max(0) as u64;
            let seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            now.checked_sub(Duration::from_secs(seconds.saturating_sub(activity)))
                .unwrap_or(now)
        };
        self.observe(session_name, current, now, initial_change)
    }

    fn observe(
        &mut self,
        session: &str,
        current: String,
        now: Instant,
        initial_change: Instant,
    ) -> HealthState {
        let entry = self
            .last_frames
            .entry(session.to_owned())
            .or_insert_with(|| (current.clone(), initial_change));
        if entry.0 != current {
            *entry = (current, now);
        }
        if now.saturating_duration_since(entry.1) >= self.idle_threshold {
            HealthState::Idle
        } else {
            HealthState::Running
        }
    }

    /// Remove stale entries for sessions that no longer exist
    pub fn retain_sessions(&mut self, active_sessions: &[String]) {
        self.last_frames
            .retain(|name, _| active_sessions.contains(name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_state_display() {
        assert_eq!(HealthState::Running.as_str(), "running");
        assert_eq!(HealthState::Idle.as_str(), "idle");
    }
    #[test]
    fn idle_threshold_and_initial_activity_survive_frequent_checks() {
        let mut checker = HealthChecker::new(TmuxClient::new(""), 15);
        let now = Instant::now();
        assert_eq!(
            checker.observe("old", "frame".into(), now, now - Duration::from_secs(20)),
            HealthState::Idle
        );
        assert_eq!(
            checker.observe("new", "frame".into(), now, now),
            HealthState::Running
        );
        assert_eq!(
            checker.observe("new", "frame".into(), now + Duration::from_secs(1), now),
            HealthState::Running
        );
        assert_eq!(
            checker.observe("new", "frame".into(), now + Duration::from_secs(16), now),
            HealthState::Idle
        );
        assert_eq!(
            checker.observe("new", "changed".into(), now + Duration::from_secs(17), now),
            HealthState::Running
        );
    }
}
