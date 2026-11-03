//! Whether a backend's CLI is installed and signed in, as the CLI itself
//! reports it. Signed out is where every fresh install starts, so it is a
//! state to show, never an error.
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
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
    run_until(program, args, status_deadline())
}

/// When a status check that starts now must be done, however many commands
/// it runs.
pub(crate) fn status_deadline() -> Instant {
    Instant::now() + STATUS_TIMEOUT
}

#[cfg(test)]
fn run_with_timeout(program: &str, args: &[&str], timeout: Duration) -> Option<Output> {
    run_until(program, args, Instant::now() + timeout)
}

/// Run a status command with no terminal, giving up at `deadline`.
pub(crate) fn run_until(program: &str, args: &[&str], deadline: Instant) -> Option<Output> {
    let mut command = Command::new(program);
    command.args(args);
    run_command(command, deadline)
}

fn run_command(mut command: Command, deadline: Instant) -> Option<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own group, so whatever it starts is killed with it.
        .process_group(0)
        .spawn()
        .ok()?;
    let (sender, outputs) = mpsc::channel();
    let drain = |stream: Option<Box<dyn Read + Send>>, out: bool| {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut stream) = stream {
                let _ = stream.read_to_string(&mut text);
            }
            let _ = sender.send((out, text));
        });
    };
    drain(child.stdout.take().map(|s| Box::new(s) as _), true);
    drain(child.stderr.take().map(|s| Box::new(s) as _), false);
    let give_up = |child: &mut std::process::Child| {
        // SAFETY: signals only the group this call created. A reader still
        // blocked means a member holds its pipe, so the group id is not reused.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        let _ = child.wait();
        None
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => return give_up(&mut child),
        }
    };
    // A descendant can keep a pipe open after the CLI exits; the deadline
    // covers reading too, and an unread stream is left to its thread.
    let (mut stdout, mut stderr) = (String::new(), String::new());
    for _ in 0..2 {
        match outputs.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok((true, text)) => stdout = text,
            Ok((false, text)) => stderr = text,
            Err(_) => return give_up(&mut child),
        }
    }
    let text = format!("{stdout}\n{stderr}");
    Some(Output {
        success: status.success(),
        stdout,
        text,
    })
}

/// What a sign-in check asks the CLI to answer.
pub const CHECK_PROMPT: &str = "Reply with OK";
/// One short model turn; well past the few seconds each CLI takes.
const CHECK_TIMEOUT: Duration = Duration::from_secs(90);

/// Whether `backend`'s sign-in actually works: its CLI answers one tiny
/// prompt headlessly, in an empty directory. A status command reads only
/// what is stored, so a key its vendor rejects still reads as signed in.
/// OMAR never sees the credential; the CLI uses its own. `Err` carries the
/// CLI's last words, which say why.
pub fn check_sign_in(backend: &dyn Backend) -> Result<(), String> {
    let command = backend.default_command();
    let program = command.split_whitespace().next().unwrap_or(command);
    let Some(args) = backend.sign_in_check(program) else {
        return Ok(());
    };
    let dir = std::env::temp_dir().join(format!(
        "omar-check-{}-{}",
        backend.kind().name(),
        std::process::id()
    ));
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot make {}: {error}", dir.display()))?;
    let mut run = Command::new(program);
    run.args(&args).current_dir(&dir);
    let output = run_command(run, Instant::now() + CHECK_TIMEOUT);
    let _ = std::fs::remove_dir_all(&dir);
    match output {
        Some(output) if output.success => Ok(()),
        Some(output) => Err(last_words(&output.text)),
        None => Err(format!("no answer within {}s", CHECK_TIMEOUT.as_secs())),
    }
}

/// The last few lines a failed CLI printed, where its error is, without
/// its colors.
fn last_words(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // CSI: ESC [ … final byte in @..~
            chars.find(|c| ('@'..='~').contains(c) && *c != '[');
        } else {
            plain.push(c);
        }
    }
    let lines: Vec<&str> = plain
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let said = lines[lines.len().saturating_sub(3)..].join(" ");
    match said.char_indices().nth(400) {
        Some((cut, _)) => format!("{}…", &said[..cut]),
        None if said.is_empty() => "it exited without saying why".into(),
        None => said,
    }
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

/// How the CLI shows a connection.
pub fn label(connection: &Connection) -> String {
    match connection {
        Connection::Connected { via: Some(via) } => format!("✓ {via}"),
        Connection::Connected { via: None } => "✓ signed in".to_string(),
        Connection::SignedOut => "not signed in".to_string(),
        Connection::NotInstalled => "not installed".to_string(),
        Connection::Unknown { reason } => format!("? {reason}"),
    }
}

/// One backend as the CLI, MCP and Mission Control report it.
pub fn describe(backend: &dyn Backend, connection: &Connection) -> serde_json::Value {
    let mut value = serde_json::to_value(connection).expect("a connection serializes");
    let name = backend.kind().name();
    value["name"] = name.into();
    value["login"] = format!("omar connect {name}").into();
    value
}

/// Read the first credential-bearing variable that is set.
/// Provider keys the multi-provider CLIs (pi, opencode) sign in with.
pub(crate) const PROVIDER_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_OAUTH_TOKEN",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "OPENROUTER_API_KEY",
    "XAI_API_KEY",
    "GROQ_API_KEY",
    "MISTRAL_API_KEY",
    "DEEPSEEK_API_KEY",
    "COPILOT_GITHUB_TOKEN",
];

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
    fn a_failed_check_says_what_the_cli_said_last() {
        assert_eq!(
            last_words("\nworking…\n\n  401: Invalid API Key  \n"),
            "working… 401: Invalid API Key"
        );
        assert_eq!(last_words("a\nb\nc\nd\n"), "b c d");
        assert_eq!(
            last_words("\x1b[33m⚠ Warning: The provided API key is invalid.\x1b[0m"),
            "⚠ Warning: The provided API key is invalid."
        );
        assert_eq!(last_words("\n \n"), "it exited without saying why");
        let long = "é".repeat(500);
        assert_eq!(last_words(&long).chars().count(), 401);
    }

    #[test]
    fn every_assistant_has_a_test_prompt_its_cli_runs_headless() {
        for kind in crate::backend::ASSISTANT {
            let backend = crate::backend::of(kind);
            let name = kind.name();
            let program = backend.executables()[0];
            let args = backend
                .sign_in_check(program)
                .unwrap_or_else(|| panic!("{name}"));
            assert_eq!(args.last(), Some(&CHECK_PROMPT), "{name}");
        }
        let cursor = crate::backend::by_name("cursor").unwrap();
        assert_eq!(
            cursor.sign_in_check("cursor").unwrap(),
            ["agent", "-p", "--trust", CHECK_PROMPT]
        );
        assert_eq!(
            cursor.sign_in_check("cursor-agent").unwrap(),
            ["-p", "--trust", CHECK_PROMPT]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_child_holding_the_output_open_is_given_up_on() {
        let start = Instant::now();
        // The wrapper exits at once; the backgrounded sleep keeps stdout open.
        let output = run_with_timeout(
            "sh",
            &["-c", "sleep 5 & exit 0"],
            Duration::from_millis(200),
        );
        assert!(output.is_none());
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_missing_cli_is_not_installed() {
        assert!(run("omar-no-such-backend-cli", &[]).is_none());
    }
}
