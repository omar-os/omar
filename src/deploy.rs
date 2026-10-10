use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::tmux::TmuxClient;
use crate::topology::write_json_atomic;

/// TERMINATED covers natural drain and graceful stop; CANCELLED is a force
/// kill; FAILED is any error. PAUSED means the run's runner has exited with
/// a durable checkpoint behind it: no process owns the run until a resume
/// starts a new one, so the record is terminal, and the run is not over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DeploymentState {
    Created,
    Deploying,
    Running,
    Stopping,
    Pausing,
    Paused,
    Terminated,
    Failed,
    Cancelled,
}

impl DeploymentState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Terminated | Self::Failed | Self::Cancelled | Self::Paused
        )
    }

    /// Whether `next` is a legal successor. Forward-only; any non-terminal
    /// state may fail or be cancelled; a terminal state is final.
    pub fn may_become(self, next: Self) -> bool {
        use DeploymentState::*;
        if self.is_terminal() {
            return false;
        }
        match next {
            Deploying => self == Created,
            Running => self == Deploying,
            // A stop may abort a pause whose capture is held at the boundary.
            Stopping => matches!(self, Running | Pausing),
            Pausing => self == Running,
            Paused => self == Pausing,
            Terminated => matches!(self, Running | Stopping | Pausing),
            Failed | Cancelled => true,
            Created => false,
        }
    }
}

impl fmt::Display for DeploymentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Created => "CREATED",
            Self::Deploying => "DEPLOYING",
            Self::Running => "RUNNING",
            Self::Stopping => "STOPPING",
            Self::Pausing => "PAUSING",
            Self::Paused => "PAUSED",
            Self::Terminated => "TERMINATED",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
        };
        f.write_str(name)
    }
}

/// One transition, kept so the record carries its own event history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionEvent {
    pub state: DeploymentState,
    pub at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentRecord {
    pub deployment_id: String,
    pub team: String,
    pub state: DeploymentState,
    /// Runner pid; its liveness separates a run in flight from a crash.
    pub pid: u32,
    pub started_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Agent name to tmux session, so teardown needs no re-verify.
    pub sessions: BTreeMap<String, String>,
    /// Missing is unknown (legacy); explicit null is the default server.
    #[serde(
        default,
        deserialize_with = "read_launch_server",
        skip_serializing_if = "Option::is_none"
    )]
    pub tmux_server: Option<Option<String>>,
    /// Inherited TMUX socket when no explicit named server was selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_socket: Option<String>,
    /// Confirmed teardown, independent of whether tmux still has a server/socket.
    #[serde(default)]
    pub sessions_cleaned: bool,
    /// Per-invocation timeout, which bounds a graceful stop.
    pub timeout_seconds: u64,
    pub history: Vec<TransitionEvent>,
    /// The last value of every state variable, kept so a stopped run can be
    /// read back and, once there is a resume, continued.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub state_vars: BTreeMap<String, serde_json::Value>,
    /// Instance name to stable workspace id; retained after the run ends.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workspaces: BTreeMap<String, String>,
    /// The last checkpoint this run published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<String>,
    /// Why the last capture failed, while the run holds at that boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_error: Option<String>,
    /// Unix time the next automatic capture is due, for status displays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_checkpoint_at: Option<u64>,
    /// The checkpoint a resumed run continued from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_from: Option<String>,
}

fn read_launch_server<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

impl DeploymentRecord {
    pub fn create(team: &str, sessions: BTreeMap<String, String>, timeout_seconds: u64) -> Self {
        let now = now_unix();
        let named = std::env::var("OMAR_TMUX_SERVER")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let inherited = if named.is_none() {
            std::env::var("TMUX").ok().filter(|s| !s.is_empty())
        } else {
            None
        };
        let socket = inherited
            .as_ref()
            .and_then(|value| value.rsplitn(3, ',').nth(2))
            .filter(|path| Path::new(path).is_absolute())
            .map(str::to_owned);
        let identity_known = inherited.is_none() || socket.is_some();
        Self {
            deployment_id: uuid::Uuid::new_v4().to_string(),
            team: team.to_string(),
            state: DeploymentState::Created,
            pid: std::process::id(),
            started_at: now,
            finished_at: None,
            error: None,
            sessions,
            tmux_server: identity_known.then_some(named),
            tmux_socket: socket,
            sessions_cleaned: false,
            timeout_seconds,
            history: vec![TransitionEvent {
                state: DeploymentState::Created,
                at: now,
                detail: None,
            }],
            state_vars: BTreeMap::new(),
            workspaces: BTreeMap::new(),
            checkpoint: None,
            checkpoint_error: None,
            next_checkpoint_at: None,
            resumed_from: None,
        }
    }

    /// Move to `next`, refusing an illegal transition.
    pub fn advance(&mut self, next: DeploymentState, detail: Option<&str>) -> Result<()> {
        if !self.state.may_become(next) {
            bail!("deployment cannot go {} -> {}", self.state, next);
        }
        let now = now_unix();
        self.state = next;
        if next.is_terminal() {
            self.finished_at = Some(now);
        }
        self.history.push(TransitionEvent {
            state: next,
            at: now,
            detail: detail.map(str::to_string),
        });
        Ok(())
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        // Preserve ownership before replacing the latest run, including legacy
        // records written before deployment history existed.
        if let Some(previous) = Self::load(dir)? {
            if previous.deployment_id != self.deployment_id {
                anyhow::ensure!(
                    !previous.deployment_id.is_empty()
                        && previous
                            .deployment_id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                    "invalid deployment id"
                );
                write_json_atomic(
                    &dir.join("deployments")
                        .join(format!("{}.json", previous.deployment_id)),
                    &previous,
                )?;
            }
        }
        write_json_atomic(&record_path(dir), self)
    }

    pub fn load(dir: &Path) -> Result<Option<Self>> {
        let path = record_path(dir);
        if !path.exists() {
            return Ok(None);
        }
        let bytes =
            std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
        let record = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid deployment record {}", path.display()))?;
        Ok(Some(record))
    }

    /// Current and archived runs, so redeployment cannot erase workspace ownership.
    pub fn load_all(dir: &Path) -> Result<Vec<Self>> {
        let mut records = Vec::new();
        let history = dir.join("deployments");
        if history.exists() {
            for entry in std::fs::read_dir(history)? {
                let path = entry?.path();
                if path.extension().is_some_and(|ext| ext == "json") {
                    records.push(serde_json::from_slice(&std::fs::read(&path)?).with_context(
                        || format!("invalid deployment record {}", path.display()),
                    )?);
                }
            }
        }
        if let Some(current) = Self::load(dir)? {
            records.retain(|record: &Self| record.deployment_id != current.deployment_id);
            records.push(current);
        }
        Ok(records)
    }

    pub fn session_client(&self) -> Result<TmuxClient> {
        let server = self.launch_server()?;
        if let Some(socket) = &self.tmux_socket {
            anyhow::ensure!(
                Path::new(socket).is_absolute(),
                "invalid recorded tmux socket"
            );
            return Ok(TmuxClient::on_socket(socket.clone()));
        }
        Ok(TmuxClient::on_server("", server.map(str::to_owned)))
    }

    pub fn launch_server(&self) -> Result<Option<&str>> {
        self.tmux_server.as_ref().map(|server| server.as_deref()).context(
            "deployment has no recorded tmux server; verify its original server and set tmux_server in its deployment record to that server name (or explicit null for the default server) before cleanup or snapshots"
        )
    }

    pub fn is_active(&self) -> bool {
        !self.state.is_terminal()
    }

    pub fn runner_alive(&self) -> bool {
        crate::process::pid_alive(self.pid)
    }
}

/// Where a team's deployment record and artifacts live.
///
/// One definition, because four places need this path and a copy that drifts
/// would have `omar stop` and the daemon looking in different directories for
/// the same run.
pub fn dir_for(omar_dir: &Path, ea_id: crate::ea::EaId, team: &str) -> PathBuf {
    crate::ea::ea_state_dir(ea_id, omar_dir)
        .join("topologies")
        .join(team)
}

pub fn record_path(dir: &Path) -> PathBuf {
    dir.join("deployment.json")
}

fn control_path(dir: &Path) -> PathBuf {
    dir.join("control.json")
}

pub fn logs_dir(dir: &Path) -> PathBuf {
    dir.join("logs")
}

pub fn outputs_path(dir: &Path) -> PathBuf {
    dir.join("outputs.json")
}

/// What an operator asked the runner to do at its next tag boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlOp {
    Stop,
    /// Checkpoint, then end the runner with the run resumable.
    Pause,
    /// Checkpoint now and keep going.
    Checkpoint,
    /// Try again after a capture failed and the run held at its boundary.
    RetryCheckpoint,
}

#[derive(Debug, Serialize, Deserialize)]
struct ControlRequest {
    /// Missing in records a pre-checkpoint runtime wrote, where the file's
    /// presence meant stop.
    #[serde(default)]
    op: Option<ControlOp>,
    requested_at: u64,
}

/// Admit one request at a time. The file is linked into place rather than
/// written over, so two operators racing to ask for a pause and a stop get
/// one request and one refusal, never a silently replaced one.
pub fn request(dir: &Path, op: ControlOp) -> Result<()> {
    let path = control_path(dir);
    let staged = dir.join(format!(".control-{}.json", uuid::Uuid::new_v4().simple()));
    write_json_atomic(
        &staged,
        &ControlRequest {
            op: Some(op),
            requested_at: now_unix(),
        },
    )?;
    let linked = std::fs::hard_link(&staged, &path);
    let _ = std::fs::remove_file(&staged);
    match linked {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let pending = pending_request(dir)
                .map(|op| format!("{op:?}").to_lowercase())
                .unwrap_or_else(|| "another".to_string());
            bail!("a {pending} request is already pending; wait for the runner to answer it")
        }
        Err(error) => Err(error).with_context(|| format!("failed to write {}", path.display())),
    }
}

/// Ask the runner to stop at the next tag boundary.
pub fn request_stop(dir: &Path) -> Result<()> {
    request(dir, ControlOp::Stop)
}

/// The request waiting for the runner, if any. Unreadable requests count as
/// a stop: an operator reached for the control file, and stopping is the
/// one answer that abandons nothing mid-tag.
pub fn pending_request(dir: &Path) -> Option<ControlOp> {
    let path = control_path(dir);
    if !path.exists() {
        return None;
    }
    let request: Option<ControlRequest> = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    Some(request.and_then(|r| r.op).unwrap_or(ControlOp::Stop))
}

#[cfg(test)]
pub fn stop_requested(dir: &Path) -> bool {
    pending_request(dir) == Some(ControlOp::Stop)
}

/// Remove any pending request, so a finished one cannot act on the next run.
pub fn clear_stop(dir: &Path) -> Result<()> {
    let path = control_path(dir);
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn policy_path(dir: &Path) -> PathBuf {
    dir.join("checkpoint-policy.json")
}

/// The run's checkpoint policy as last persisted, if an operator set one.
pub fn read_policy(dir: &Path) -> Result<Option<crate::checkpoint::Policy>> {
    let path = policy_path(dir);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&std::fs::read(&path)?)?))
}

pub fn write_policy(dir: &Path, policy: crate::checkpoint::Policy) -> Result<()> {
    write_json_atomic(&policy_path(dir), &policy)
}

pub fn kill_process(pid: u32) {
    let _ = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Terminal sessions beneath a deployment. tmux is the only implementation
/// today; this seam lets another process host slot in.
pub trait SessionHost {
    fn exists(&self, session: &str) -> Result<bool>;
    fn capture(&self, session: &str) -> Result<String>;
    fn kill(&self, session: &str) -> Result<()>;
}

/// Pane history to keep as an agent's log.
const CAPTURE_LINES: i32 = 10_000;

impl SessionHost for TmuxClient {
    fn exists(&self, session: &str) -> Result<bool> {
        self.has_session_for_cleanup(session)
    }

    fn capture(&self, session: &str) -> Result<String> {
        self.capture_pane_plain(session, CAPTURE_LINES)
    }

    fn kill(&self, session: &str) -> Result<()> {
        self.kill_session_tree(session)
    }
}

/// Capture each session's pane as a log, then kill it. Best effort per
/// session; failures come back for the caller to report.
pub fn teardown_sessions(
    host: &dyn SessionHost,
    sessions: &BTreeMap<String, String>,
    logs: &Path,
) -> Vec<String> {
    let mut failures = Vec::new();
    for (agent, session) in sessions {
        match host.exists(session) {
            Ok(false) => continue,
            Ok(true) => {}
            Err(error) => {
                failures.push(format!("{agent}: {error:#}"));
                continue;
            }
        }
        if let Ok(log) = host.capture(session) {
            let _ = std::fs::create_dir_all(logs);
            let _ = std::fs::write(
                logs.join(format!("{}.txt", crate::tmux::flatten_agent_name(agent))),
                log,
            );
        }
        if let Err(error) = host.kill(session) {
            failures.push(format!("{agent}: {error:#}"));
        }
    }
    failures
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_follow_the_lifecycle() {
        use DeploymentState::*;
        assert!(Created.may_become(Deploying));
        assert!(Deploying.may_become(Running));
        assert!(Running.may_become(Stopping));
        assert!(Running.may_become(Terminated));
        assert!(Stopping.may_become(Terminated));
        assert!(Running.may_become(Pausing));
        assert!(Pausing.may_become(Paused));
        assert!(Pausing.may_become(Stopping), "a stop aborts a held pause");
        assert!(Pausing.may_become(Terminated));
        assert!(!Stopping.may_become(Paused));
        for state in [Created, Deploying, Running, Stopping, Pausing] {
            assert!(state.may_become(Failed));
            assert!(state.may_become(Cancelled));
        }

        assert!(!Created.may_become(Running));
        assert!(!Deploying.may_become(Terminated));
        assert!(!Stopping.may_become(Running));
        for terminal in [Terminated, Failed, Cancelled, Paused] {
            for next in [
                Created, Deploying, Running, Stopping, Pausing, Paused, Terminated, Failed,
                Cancelled,
            ] {
                assert!(!terminal.may_become(next));
            }
        }
    }

    #[test]
    fn advance_refuses_illegal_and_records_history() {
        let mut record = DeploymentRecord::create("Demo", BTreeMap::new(), 300);
        assert!(record.advance(DeploymentState::Running, None).is_err());
        record.advance(DeploymentState::Deploying, None).unwrap();
        record.advance(DeploymentState::Running, None).unwrap();
        record
            .advance(DeploymentState::Terminated, Some("run completed"))
            .unwrap();
        assert!(record.finished_at.is_some());
        assert!(record.advance(DeploymentState::Failed, None).is_err());
        let states: Vec<_> = record.history.iter().map(|event| event.state).collect();
        use DeploymentState::*;
        assert_eq!(states, vec![Created, Deploying, Running, Terminated]);
    }

    #[test]
    fn legacy_missing_server_is_not_an_explicit_default() {
        let record = DeploymentRecord::create("Demo", BTreeMap::new(), 42);
        let mut json = serde_json::to_value(record).unwrap();
        json.as_object_mut().unwrap().remove("tmux_server");
        let legacy: DeploymentRecord = serde_json::from_value(json.clone()).unwrap();
        assert!(legacy.launch_server().is_err());
        assert!(serde_json::to_value(legacy)
            .unwrap()
            .get("tmux_server")
            .is_none());
        json["tmux_server"] = serde_json::Value::Null;
        let default: DeploymentRecord = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(default.launch_server().unwrap(), None);
        assert!(serde_json::to_value(default).unwrap()["tmux_server"].is_null());
        json["tmux_server"] = serde_json::json!("original-server");
        let named: DeploymentRecord = serde_json::from_value(json).unwrap();
        assert_eq!(named.launch_server().unwrap(), Some("original-server"));
    }

    #[test]
    fn record_round_trips_and_stop_control_toggles() {
        let dir = tempfile::tempdir().unwrap();
        let mut record = DeploymentRecord::create("Demo", BTreeMap::new(), 42);
        record.advance(DeploymentState::Deploying, None).unwrap();
        record.save(dir.path()).unwrap();

        let loaded = DeploymentRecord::load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.state, DeploymentState::Deploying);
        assert_eq!(loaded.team, "Demo");
        assert_eq!(loaded.timeout_seconds, 42);
        assert!(loaded.is_active());

        assert!(!stop_requested(dir.path()));
        request_stop(dir.path()).unwrap();
        assert!(stop_requested(dir.path()));
        clear_stop(dir.path()).unwrap();
        assert!(!stop_requested(dir.path()));
        // A pause is not a stop, and a legacy request without an op is.
        request(dir.path(), ControlOp::Pause).unwrap();
        assert!(!stop_requested(dir.path()));
        assert_eq!(pending_request(dir.path()), Some(ControlOp::Pause));
        // One request at a time: a stop cannot silently replace the pause.
        let refused = request_stop(dir.path()).unwrap_err().to_string();
        assert!(
            refused.contains("pause request is already pending"),
            "{refused}"
        );
        assert_eq!(pending_request(dir.path()), Some(ControlOp::Pause));
        clear_stop(dir.path()).unwrap();
        std::fs::write(dir.path().join("control.json"), b"{\"requested_at\":1}").unwrap();
        assert!(stop_requested(dir.path()));
        clear_stop(dir.path()).unwrap();
        assert_eq!(pending_request(dir.path()), None);
        assert!(read_policy(dir.path()).unwrap().is_none());
        write_policy(dir.path(), crate::checkpoint::Policy { period_secs: 90 }).unwrap();
        assert_eq!(read_policy(dir.path()).unwrap().unwrap().period_secs, 90);
    }

    #[test]
    fn dead_pid_is_not_alive() {
        let mut record = DeploymentRecord::create("Demo", BTreeMap::new(), 300);
        assert!(record.runner_alive());
        record.pid = u32::MAX - 1;
        assert!(!record.runner_alive());
    }
}
