//! Read-only permission observation. Execution and approval are independent:
//! one pane can need input while the rest of a topology keeps running.
//!
//! Observers never respond to approvals: the backend's existing TUI remains the
//! authority. Structured transports are preferred; backends without a passive
//! stream may conservatively recognize their currently visible native overlay.
//! A disconnected observer retains its pending requests.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use ts_rs::TS;
use uuid::Uuid;

mod backends;

#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalConnection {
    Connecting,
    Connected,
    Disconnected,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOutcome {
    Resolved,
    Denied,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalResolutionMode {
    Terminal,
}

#[derive(Clone, Debug, PartialEq, Serialize, TS)]
pub struct PendingApproval {
    pub request_id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub run_id: Option<String>,
    pub invocation_id: Option<String>,
    // Unix milliseconds, preserved when a request is replayed.
    pub requested_at: u64,
    pub summary: String,
    pub tool_name: String,
    pub scope: String,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub resolution_mode: ApprovalResolutionMode,
}

#[derive(Clone, Debug, PartialEq, Serialize, TS)]
pub struct ApprovalMonitor {
    pub agent_id: String,
    pub run_id: Option<String>,
    pub state: ApprovalConnection,
}

#[derive(Clone, Debug, PartialEq, Serialize, TS)]
pub struct ApprovalResolution {
    pub request: PendingApproval,
    pub outcome: ApprovalOutcome,
    pub resolved_at: u64,
}

#[derive(Clone, Debug, Default, Serialize, TS)]
pub struct ApprovalSnapshot {
    pub sequence: u64,
    pub requests: Vec<PendingApproval>,
    pub monitors: Vec<ApprovalMonitor>,
    pub recent: Vec<ApprovalResolution>,
}

#[derive(Default)]
struct State {
    snapshot: ApprovalSnapshot,
    watches: BTreeMap<String, Watch>,
    stopped: bool,
}

#[derive(Clone)]
struct Watch {
    monitor: ApprovalMonitor,
    name: String,
    invocation: Option<String>,
}

#[derive(Clone, Default)]
pub struct ApprovalHub(Arc<Mutex<State>>);

#[derive(Clone)]
pub struct ApprovalRun {
    pub hub: ApprovalHub,
    pub run_id: String,
}

impl ApprovalHub {
    pub fn snapshot(&self) -> ApprovalSnapshot {
        let state = self.0.lock().expect("approval state poisoned");
        let mut snapshot = state.snapshot.clone();
        snapshot.monitors = state.watches.values().map(|w| w.monitor.clone()).collect();
        snapshot
    }

    pub fn shutdown(&self) {
        self.0.lock().expect("approval state poisoned").stopped = true;
    }

    pub fn stopped(&self) -> bool {
        self.0.lock().expect("approval state poisoned").stopped
    }

    pub fn watch(&self, target: ApprovalTarget, agent: String, name: String, run: Option<String>) {
        self.watch_observer(agent, name, run, backends::observer(target));
    }

    fn watch_observer(
        &self,
        agent: String,
        name: String,
        run: Option<String>,
        observer: Option<Box<dyn ApprovalObserver>>,
    ) {
        let key = Uuid::new_v4().to_string();
        {
            let mut state = self.0.lock().expect("approval state poisoned");
            state.watches.insert(
                key.clone(),
                Watch {
                    monitor: ApprovalMonitor {
                        agent_id: agent,
                        run_id: run,
                        state: if observer.is_some() {
                            ApprovalConnection::Connecting
                        } else {
                            ApprovalConnection::Unsupported
                        },
                    },
                    name,
                    invocation: None,
                },
            );
            state.snapshot.sequence += 1;
        }
        if let Some(observer) = observer {
            let sink = ApprovalSink {
                hub: self.clone(),
                key,
            };
            thread::spawn(move || observer.observe(sink));
        }
    }

    fn active(&self, key: &str) -> bool {
        let state = self.0.lock().expect("approval state poisoned");
        !state.stopped && state.watches.contains_key(key)
    }

    fn connection(&self, key: &str, connection: ApprovalConnection) {
        let mut state = self.0.lock().expect("approval state poisoned");
        if let Some(watch) = state.watches.get_mut(key) {
            if watch.monitor.state != connection {
                watch.monitor.state = connection;
                state.snapshot.sequence += 1;
            }
        }
    }

    fn add(&self, key: &str, detail: ApprovalDetails) -> Option<PendingApproval> {
        let mut state = self.0.lock().expect("approval state poisoned");
        let watch = state.watches.get(key)?;
        let request = PendingApproval {
            request_id: Uuid::new_v4().to_string(),
            agent_id: watch.monitor.agent_id.clone(),
            agent_name: watch.name.clone(),
            run_id: watch.monitor.run_id.clone(),
            invocation_id: watch.invocation.clone(),
            requested_at: detail.started.unwrap_or_else(now),
            summary: detail.summary,
            tool_name: detail.tool,
            scope: detail.scope,
            command: detail.command,
            cwd: detail.cwd,
            resolution_mode: ApprovalResolutionMode::Terminal,
        };
        state.snapshot.requests.push(request.clone());
        state.snapshot.sequence += 1;
        Some(request)
    }

    fn resolve(&self, id: &str, outcome: ApprovalOutcome) {
        let mut state = self.0.lock().expect("approval state poisoned");
        if let Some(index) = state
            .snapshot
            .requests
            .iter()
            .position(|r| r.request_id == id)
        {
            let request = state.snapshot.requests.remove(index);
            state.snapshot.recent.push(ApprovalResolution {
                request,
                outcome,
                resolved_at: now(),
            });
            if state.snapshot.recent.len() > 64 {
                state.snapshot.recent.remove(0);
            }
            state.snapshot.sequence += 1;
        } else if outcome != ApprovalOutcome::Resolved {
            // request/resolved does not include a decision. A following item
            // completion can confirm denial; never call a bare ACK "approved".
            if let Some(recent) = state
                .snapshot
                .recent
                .iter_mut()
                .find(|r| r.request.request_id == id)
            {
                if recent.outcome != outcome {
                    recent.outcome = outcome;
                    state.snapshot.sequence += 1;
                }
            }
        }
    }

    fn supersede(&self, id: &str) {
        let mut state = self.0.lock().expect("approval state poisoned");
        state.snapshot.requests.retain(|r| r.request_id != id);
        state.snapshot.sequence += 1;
    }

    pub fn finish_run(&self, run: &str) {
        let mut state = self.0.lock().expect("approval state poisoned");
        state
            .watches
            .retain(|_, w| w.monitor.run_id.as_deref() != Some(run));
        let ids: Vec<_> = state
            .snapshot
            .requests
            .iter()
            .filter(|r| r.run_id.as_deref() == Some(run))
            .map(|r| r.request_id.clone())
            .collect();
        state.snapshot.sequence += 1;
        drop(state);
        for id in ids {
            self.resolve(&id, ApprovalOutcome::Cancelled);
        }
    }
}

impl ApprovalRun {
    pub fn watch(&self, session: &str, agent: &str, command: &str, backend: &str) {
        self.hub.watch(
            ApprovalTarget {
                session: session.into(),
                backend: backend.into(),
                command: Some(command.into()),
            },
            format!("agent::{agent}"),
            agent.into(),
            Some(self.run_id.clone()),
        );
    }

    pub fn invocation(&self, agent: &str, invocation: &str) -> InvocationGuard {
        let mut state = self.hub.0.lock().expect("approval state poisoned");
        for watch in state.watches.values_mut() {
            if watch.monitor.run_id.as_deref() == Some(&self.run_id)
                && watch.monitor.agent_id == format!("agent::{agent}")
            {
                watch.invocation = Some(invocation.into());
            }
        }
        InvocationGuard {
            run: self.clone(),
            agent: format!("agent::{agent}"),
            invocation: invocation.into(),
        }
    }
}

pub struct InvocationGuard {
    run: ApprovalRun,
    agent: String,
    invocation: String,
}
impl Drop for InvocationGuard {
    fn drop(&mut self) {
        let mut state = self.run.hub.0.lock().expect("approval state poisoned");
        for watch in state.watches.values_mut() {
            if watch.monitor.run_id.as_deref() == Some(&self.run.run_id)
                && watch.monitor.agent_id == self.agent
                && watch.invocation.as_deref() == Some(&self.invocation)
            {
                watch.invocation = None;
            }
        }
        // Finishing an OMAR invocation is not an approval acknowledgement.
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

struct ApprovalDetails {
    summary: String,
    tool: String,
    scope: String,
    command: Option<String>,
    cwd: Option<String>,
    started: Option<u64>,
}

/// Launch identity only. Transport discovery belongs to the backend adapter.
pub struct ApprovalTarget {
    pub session: String,
    pub backend: String,
    pub command: Option<String>,
}

/// Each adapter owns its transport, request parsing, replay and reconciliation.
/// Implementations observe only; they must never answer approvals or alter policy.
trait ApprovalObserver: Send {
    fn observe(self: Box<Self>, sink: ApprovalSink);
}

/// Backend-neutral access to a watch's lifecycle and display-only state.
/// The hub owns public identity, invocation correlation, timestamps and retention.
struct ApprovalSink {
    hub: ApprovalHub,
    key: String,
}
impl ApprovalSink {
    fn active(&self) -> bool {
        self.hub.active(&self.key)
    }
    fn connection(&self, state: ApprovalConnection) {
        self.hub.connection(&self.key, state);
    }
    fn request(&self, detail: ApprovalDetails) -> Option<PendingApproval> {
        self.hub.add(&self.key, detail)
    }
    fn resolve(&self, id: &str, outcome: ApprovalOutcome) {
        self.hub.resolve(id, outcome);
    }
    fn supersede(&self, id: &str) {
        self.hub.supersede(id);
    }
    fn pending(&self, id: &str) -> bool {
        self.hub
            .snapshot()
            .requests
            .iter()
            .any(|r| r.request_id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    struct FixtureObserver {
        connected: mpsc::Sender<()>,
        continue_observing: mpsc::Receiver<()>,
        completed: mpsc::Sender<()>,
    }
    impl ApprovalObserver for FixtureObserver {
        fn observe(self: Box<Self>, sink: ApprovalSink) {
            sink.connection(ApprovalConnection::Connected);
            self.connected.send(()).unwrap();
            self.continue_observing.recv().unwrap();
            let request = sink
                .request(ApprovalDetails {
                    summary: "Fixture backend requests permission".into(),
                    tool: "Fixture tool".into(),
                    scope: "Fixture workspace".into(),
                    command: None,
                    cwd: None,
                    started: Some(1234),
                })
                .unwrap();
            sink.connection(ApprovalConnection::Disconnected);
            assert!(
                sink.pending(&request.request_id),
                "disconnect must retain requests"
            );
            self.completed.send(()).unwrap();
            self.continue_observing.recv().unwrap();
            assert!(
                !sink.active(),
                "run teardown must stop every backend observer"
            );
            assert!(!sink.pending(&request.request_id));
            self.completed.send(()).unwrap();
        }
    }

    #[test]
    fn alternate_observer_uses_shared_identity_invocation_and_teardown() {
        let hub = ApprovalHub::default();
        let (connected, ready) = mpsc::channel();
        let (advance, continue_observing) = mpsc::channel();
        let (completed, done) = mpsc::channel();
        hub.watch_observer(
            "agent::reviewer".into(),
            "reviewer".into(),
            Some("run".into()),
            Some(Box::new(FixtureObserver {
                connected,
                continue_observing,
                completed,
            })),
        );
        ready.recv_timeout(Duration::from_secs(3)).unwrap();
        let run = ApprovalRun {
            hub: hub.clone(),
            run_id: "run".into(),
        };
        let invocation = run.invocation("reviewer", "invocation");
        advance.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(3)).unwrap();
        let snapshot = hub.snapshot();
        assert_eq!(snapshot.monitors[0].state, ApprovalConnection::Disconnected);
        let request = &snapshot.requests[0];
        assert_eq!(request.agent_id, "agent::reviewer");
        assert_eq!(request.invocation_id.as_deref(), Some("invocation"));
        assert_eq!(request.requested_at, 1234);
        drop(invocation);
        assert_eq!(
            hub.snapshot().requests.len(),
            1,
            "invocation completion is not an approval decision"
        );
        hub.finish_run("run");
        advance.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(3)).unwrap();
        let snapshot = hub.snapshot();
        assert!(snapshot.monitors.is_empty());
        assert_eq!(snapshot.recent[0].outcome, ApprovalOutcome::Cancelled);
    }

    #[test]
    fn unsupported_backend_is_reported_immediately_without_an_observer() {
        let hub = ApprovalHub::default();
        hub.watch(
            ApprovalTarget {
                session: "not-a-real-pane".into(),
                backend: "unknown".into(),
                command: None,
            },
            "assistant".into(),
            "Executive assistant".into(),
            None,
        );
        let snapshot = hub.snapshot();
        assert_eq!(snapshot.monitors[0].state, ApprovalConnection::Unsupported);
        assert!(snapshot.requests.is_empty());
        hub.shutdown();
        assert!(hub.stopped());
    }
}
