//! Whether a backend's CLI is installed and signed in, as the CLI itself
//! reports it. Signed out is where every fresh install starts, so it is a
//! state to show, never an error.
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::Backend;
use crate::backend_probe;

/// A status command may reach its vendor's servers; past this it is unknown.
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Connection {
    NotInstalled,
    SignedOut,
    Connected {
        /// The account or credential it signed in with, when the CLI says.
        #[serde(skip_serializing_if = "Option::is_none")]
        via: Option<String>,
    },
    /// Installed, but its sign-in could not be read.
    Unknown {
        reason: String,
    },
}

impl Connection {
    pub fn connected(via: impl Into<String>) -> Self {
        Connection::Connected {
            via: Some(via.into()),
        }
    }
    pub fn unknown(reason: impl Into<String>) -> Self {
        Connection::Unknown {
            reason: reason.into(),
        }
    }
}

/// What a status command printed. Most CLIs split their answer across both
/// streams, so parsers see them together in `text`.
pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub text: String,
}

/// Run a status command with no terminal, giving up after [`STATUS_TIMEOUT`].
pub(crate) fn run(program: &str, args: &[&str]) -> Option<Output> {
    run_with_timeout(program, args, STATUS_TIMEOUT)
}

fn run_with_timeout(program: &str, args: &[&str], timeout: Duration) -> Option<Output> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let drain = |stream: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut stream) = stream {
                let _ = stream.read_to_string(&mut text);
            }
            text
        })
    };
    let stdout = drain(child.stdout.take().map(|s| Box::new(s) as _));
    let stderr = drain(child.stderr.take().map(|s| Box::new(s) as _));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let stdout = stdout.join().unwrap_or_default();
    let text = format!("{stdout}\n{}", stderr.join().unwrap_or_default());
    Some(Output {
        success: status.success(),
        stdout,
        text,
    })
}

/// Ask one backend, through the executable its default command runs.
pub fn connection(backend: &dyn Backend) -> Connection {
    let command = backend.default_command();
    let program = command.split_whitespace().next().unwrap_or(command);
    if !backend_probe::backend_version_probe_succeeds(program) {
        return Connection::NotInstalled;
    }
    backend.connection(program)
}

/// Every assistant backend, asked at once so the slowest bounds the wait.
pub fn assistants() -> Vec<(&'static dyn Backend, Connection)> {
    std::thread::scope(|scope| {
        let asks: Vec<_> = super::ASSISTANT
            .iter()
            .map(|kind| super::of(*kind))
            .map(|backend| (backend, scope.spawn(move || connection(backend))))
            .collect();
        asks.into_iter()
            .map(|(backend, ask)| {
                let connection = ask
                    .join()
                    .unwrap_or_else(|_| Connection::unknown("status check panicked"));
                (backend, connection)
            })
            .collect()
    })
}

/// One backend as the CLI, MCP and Mission Control report it.
pub fn describe(backend: &dyn Backend, connection: &Connection) -> serde_json::Value {
    let mut value = serde_json::to_value(connection).expect("a connection serializes");
    value["name"] = backend.kind().name().into();
    if let Some(login) = backend.login_command() {
        value["login"] = login.into();
    }
    value
}

/// Read the first credential-bearing variable that is set.
pub(crate) fn env_credential(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty()))
        .map(|name| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_serializes_with_its_status_tag() {
        assert_eq!(
            serde_json::to_value(Connection::connected("ChatGPT")).unwrap(),
            serde_json::json!({"status": "connected", "via": "ChatGPT"})
        );
        assert_eq!(
            serde_json::to_value(Connection::SignedOut).unwrap(),
            serde_json::json!({"status": "signed_out"})
        );
        assert_eq!(
            serde_json::to_value(Connection::Connected { via: None }).unwrap(),
            serde_json::json!({"status": "connected"})
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_status_command_is_read_from_both_streams() {
        let output = run_with_timeout(
            "sh",
            &["-c", "echo out; echo err >&2; exit 1"],
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(!output.success);
        assert_eq!(output.stdout, "out\n");
        assert!(output.text.contains("out") && output.text.contains("err"));
    }

    #[cfg(unix)]
    #[test]
    fn a_hanging_status_command_is_given_up_on() {
        let start = Instant::now();
        assert!(run_with_timeout("sleep", &["5"], Duration::from_millis(100)).is_none());
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_missing_cli_is_not_installed() {
        assert!(run("omar-no-such-backend-cli", &[]).is_none());
    }
}
