//! Pi's native TUI and extension API, with a private per-pane delivery socket.
use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use super::{connection, Connection, Login};
use super::{Backend, Kind, Launch, NotReady, PaneSetup, Readiness, Target, WRITE_TIMEOUT};
use crate::manager::{
    materialize_mcp_context_file, mcp_ea_dir, omar_server_exe, shell_single_quote,
    short_protocol_dir, write_private_file,
};
use anyhow::{Context, Result};

pub struct Pi;

impl Backend for Pi {
    fn kind(&self) -> Kind {
        Kind::Pi
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["pi"]
    }
    fn executables(&self) -> &'static [&'static str] {
        &["pi"]
    }
    fn default_command(&self) -> &'static str {
        "pi"
    }
    /// Pi has no status command, so this lists the providers in its own
    /// store (names only), then the keys most people sign in with. The one
    /// place OMAR opens a credential file.
    fn connection(&self, _program: &str) -> Connection {
        let dir = std::env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".pi").join("agent")));
        let store = match dir {
            Some(dir) => std::fs::read_to_string(dir.join("auth.json")),
            None => Err(std::io::ErrorKind::NotFound.into()),
        };
        store_connection(store, connection::env_credential(connection::PROVIDER_KEYS))
    }
    fn install_command(&self) -> Option<&'static str> {
        Some("npm install -g @earendil-works/pi-coding-agent")
    }
    /// Pi signs in only from inside its TUI.
    fn login(&self, _headless: bool) -> Option<Login> {
        Some(Login {
            args: &[],
            note: Some("In pi, type /login and pick a provider, then /quit."),
        })
    }
    fn credential_vars(&self) -> &'static [&'static str] {
        connection::PROVIDER_KEYS
    }
    fn logout(&self) -> Option<Login> {
        Some(Login {
            args: &[],
            note: Some("In pi, type /logout and pick the provider, then /quit."),
        })
    }
    /// Its default model, through whichever provider that needs: a stored
    /// key can be rejected, or not reach that model.
    fn sign_in_check(&self, _program: &str) -> Option<Vec<&'static str>> {
        Some(vec![
            "-p",
            "--no-session",
            "--no-tools",
            connection::CHECK_PROMPT,
        ])
    }
    fn readiness(&self, _command: &str) -> Readiness {
        Readiness::Channel
    }
    /// `--model`, which also takes `provider/model`.
    fn model_flag(&self, model: &str) -> Option<String> {
        Some(format!(
            "--model {}",
            crate::manager::shell_single_quote(model)
        ))
    }
    fn launch_command(&self, launch: &Launch<'_>) -> String {
        launch_command(launch).unwrap_or_else(|error| {
            format!(
                "printf '%s\\n' {} >&2; exit 1",
                shell_single_quote(&format!("OMAR Pi launch failed: {error:#}"))
            )
        })
    }
    fn prepare_pane(&self, _session: &str, command: &str) -> Result<PaneSetup> {
        let mut setup = PaneSetup::interactive(command);
        setup.stamp = launch_socket(command).map(|path| format!("pi:{}", path.display()));
        Ok(setup)
    }
    fn discover_and_deliver(&self, target: &Target<'_>, text: &str) -> Result<()> {
        let socket = target
            .stamp
            .and_then(|stamp| stamp.strip_prefix("pi:"))
            .filter(|s| !s.is_empty())
            .ok_or(NotReady("Pi extension has no delivery socket"))?;
        if !std::path::Path::new(socket).exists() {
            return Err(NotReady("Pi OMAR tools are not ready").into());
        }
        let reply = request(socket, serde_json::json!({"text": text}))
            .with_context(|| super::delivery_failed(target, "Pi extension"))?;
        anyhow::ensure!(reply["accepted"] == true, "Pi rejected delivery: {reply}");
        Ok(())
    }
    /// The session file, which `--session` reopens; with none (a
    /// `--no-session` launch), the in-memory id it was opened with.
    fn conversation_id(&self, target: &Target<'_>) -> Option<String> {
        let socket = target.stamp?.strip_prefix("pi:")?;
        reported_conversation(&request(socket, serde_json::json!({"session": true})).ok()?)
    }
    /// An attempt's launch is `--no-session --session-id <native_session>`:
    /// an in-memory conversation with that id that reads nothing from disk.
    /// Pi before 0.80.3 ignores the id under `--no-session` and reports
    /// another, so the check fails closed.
    fn fresh_conversation(&self) -> bool {
        true
    }
}

fn reported_conversation(reply: &serde_json::Value) -> Option<String> {
    [&reply["session"], &reply["id"]]
        .into_iter()
        .find_map(|value| value.as_str().filter(|s| !s.is_empty()))
        .map(str::to_owned)
}

fn request(socket: &str, body: serde_json::Value) -> Result<serde_json::Value> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(WRITE_TIMEOUT))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    writeln!(stream, "{body}")?;
    let mut reply = String::new();
    std::io::BufReader::new(stream).read_line(&mut reply)?;
    Ok(serde_json::from_str(&reply)?)
}

fn launch_socket(command: &str) -> Option<PathBuf> {
    super::managed::unquote_single(command.split_once("OMAR_PI_SOCKET=")?.1).map(PathBuf::from)
}

fn launch_command(launch: &Launch<'_>) -> Result<String> {
    let context = launch.context;
    let dir = mcp_ea_dir(context)
        .context("Pi extension directory")?
        .join("pi-extension");
    for (name, body) in [
        ("index.js", include_str!("pi/index.js")),
        ("omar-mcp.js", include_str!("pi/omar-mcp.js")),
        ("delivery.js", include_str!("pi/delivery.js")),
        ("package.json", include_str!("pi/package.json")),
    ] {
        write_private_file(&dir.join(name), body.as_bytes())?;
    }
    let context_file = materialize_mcp_context_file(context).context("Pi MCP context")?;
    let socket = short_protocol_dir()?.join("pi.sock");
    let binary = omar_server_exe().context("OMAR executable")?;
    let (_, end) = super::executable(launch.base_command).context("Pi executable")?;
    let words = shlex::split(launch.base_command).unwrap_or_default();
    let opens_a_session = |word: &String| {
        matches!(
            word.as_str(),
            "--session" | "--resume" | "-r" | "--continue" | "-c"
        ) || word.starts_with("--session=")
    };
    let attempt = context.topology.as_ref().and_then(|t| t.attempt.as_ref());
    // An attempt's conversation is new and in memory, whatever the operator
    // keeps: nothing on its line may name another.
    if let Some(word) = attempt.and_then(|_| {
        words.iter().find(|word| {
            opens_a_session(word) || *word == "--fork" || word.starts_with("--session-id")
        })
    }) {
        anyhow::bail!("an agent that resets its conversation (per lifetime) has a command that must not pass {word}");
    }
    let mut command = format!(
        "{} --approve{}{}",
        &launch.base_command[..end],
        match attempt {
            Some(attempt) => format!(
                " --no-session --session-id {}",
                shell_single_quote(&attempt.native_session)
            ),
            None if super::ephemeral_worker(context) => " --no-session".to_string(),
            None => String::new(),
        },
        &launch.base_command[end..]
    );
    if attempt.is_none() && !words.iter().any(opens_a_session) {
        if let Some(path) =
            super::saved_conversation(context, "pi").filter(|p| std::path::Path::new(p).is_file())
        {
            command.push_str(&format!(" --session {}", shell_single_quote(&path)));
        }
    }
    Ok(format!(
        "OMAR_BINARY={} OMAR_DIR={} OMAR_EA_ID={} OMAR_MCP_CONTEXT_FILE={} OMAR_PI_SOCKET={} {} -e {} --system-prompt \"{}\"",
        shell_single_quote(&binary.display().to_string()), shell_single_quote(&context.omar_dir.display().to_string()), context.ea_id,
        shell_single_quote(&context_file.display().to_string()), shell_single_quote(&socket.display().to_string()), command,
        shell_single_quote(&dir.join("index.js").display().to_string()), launch.shell_expr
    ))
}

/// Pi's sign-in from its store and any provider key. Only a missing store or
/// one that parses can say signed out; anything else is unknown.
fn store_connection(store: std::io::Result<String>, env: Option<String>) -> Connection {
    let stored = match store {
        Ok(text) => stored_providers(&text).ok_or("pi's auth.json is not a store OMAR can read"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(_) => Err("cannot read pi's auth.json"),
    };
    match (stored, env) {
        (Ok(names), _) if !names.is_empty() => Connection::connected(names.join(", ")),
        (_, Some(name)) => Connection::connected(name),
        (Ok(_), None) => Connection::SignedOut,
        (Err(reason), None) => Connection::unknown(reason),
    }
}

/// `auth.json` maps each provider signed in through `/login` to its credential.
/// Only the names are kept; each credential is skipped as it is parsed.
fn stored_providers(text: &str) -> Option<Vec<String>> {
    serde_json::from_str::<std::collections::BTreeMap<String, serde::de::IgnoredAny>>(text)
        .ok()
        .map(|providers| providers.into_keys().collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn stored_providers_are_the_keys_of_auth_json() {
        use super::stored_providers;
        assert_eq!(
            stored_providers(r#"{"anthropic":{"type":"oauth"},"openai":{"type":"api_key"}}"#)
                .unwrap(),
            ["anthropic", "openai"]
        );
        assert_eq!(stored_providers("{}"), Some(vec![]));
        assert_eq!(stored_providers("not json"), None);
    }

    #[test]
    fn only_a_missing_or_empty_store_says_signed_out() {
        use super::{store_connection, Connection};
        use std::io::{Error, ErrorKind};
        let missing = || Err(Error::from(ErrorKind::NotFound));
        assert_eq!(store_connection(missing(), None), Connection::SignedOut);
        assert_eq!(
            store_connection(Ok("{}".into()), None),
            Connection::SignedOut
        );
        assert_eq!(
            store_connection(Ok(r#"{"openai":{}}"#.into()), None),
            Connection::connected("openai")
        );
        // Unreadable or corrupt: nobody knows, unless a key settles it.
        assert!(matches!(
            store_connection(Ok("{corrupt".into()), None),
            Connection::Unknown { .. }
        ));
        assert!(matches!(
            store_connection(Err(Error::from(ErrorKind::PermissionDenied)), None),
            Connection::Unknown { .. }
        ));
        assert_eq!(
            store_connection(Ok("{corrupt".into()), Some("OPENAI_API_KEY".into())),
            Connection::connected("OPENAI_API_KEY")
        );
    }

    use super::*;
    #[test]
    fn pi_launch_materializes_scoped_bridge_and_resumes_only_saved_chat() {
        // A worker's launch line reads OMAR_PERSIST_WORKER_SESSIONS, which
        // another test sets under this lock.
        let _lock = crate::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let prompt = dir.path().join("prompt.md");
        std::fs::write(&prompt, "hello").unwrap();
        let mut context = crate::manager::tests::test_mcp_context(dir.path());
        context.serve = Some(crate::manager::ServeMcpContext {
            endpoint: "localhost:1".into(),
            token: "test".into(),
        });
        let session = dir.path().join("saved chat.jsonl");
        std::fs::write(&session, "{}").unwrap();
        super::super::remember_conversation(&context, "pi", session.to_str().unwrap()).unwrap();
        let command = crate::manager::build_agent_command(
            "env FOO=bar '/opt/Pi Agent/pi' --model test",
            &prompt,
            &[],
            &context,
        );
        assert!(
            command.contains("'/opt/Pi Agent/pi' --approve --model test"),
            "{command}"
        );
        assert!(command.contains(&format!(
            "--session {}",
            shell_single_quote(session.to_str().unwrap())
        )));
        let setup = Pi.prepare_pane("test", &command).unwrap();
        assert!(setup.stamp.unwrap().starts_with("pi:/tmp/omar-protocol-"));
        assert!(dir
            .path()
            .join("mcp/ea-0/pi-extension/delivery.js")
            .is_file());
        assert_eq!(Pi.readiness(&command), Readiness::Channel);
        context.agent_name = Some("worker".into());
        let worker = crate::manager::build_agent_command("pi", &prompt, &[], &context);
        assert!(!worker.contains("--session"));
        assert!(worker.contains("pi --approve --no-session"), "{worker}");
        assert!(!command.contains("--no-session"));
        assert_ne!(launch_socket(&command), launch_socket(&worker));
        context.agent_name = None;
        std::fs::remove_file(session).unwrap();
        assert!(
            !crate::manager::build_agent_command("pi", &prompt, &[], &context)
                .contains("--session")
        );
    }
    /// An attempt's worker opens an in-memory conversation with the id its
    /// attempt names, even where workers keep their sessions, and its line
    /// may name no other.
    #[test]
    fn an_attempt_opens_the_conversation_it_names_and_no_other() {
        let _lock = crate::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let prompt = dir.path().join("prompt.md");
        std::fs::write(&prompt, "hello").unwrap();
        let mut context = crate::manager::tests::test_mcp_context(dir.path());
        context.topology = Some(crate::manager::TopologyMcpContext {
            team: "Scoped".into(),
            agent: "t.coder".into(),
            endpoint: "localhost:1".into(),
            token: "t".into(),
            attempt: Some(crate::manager::TopologyAttempt {
                id: "attempt".into(),
                key: "key".into(),
                native_session: "11111111-1111-4111-8111-111111111111".into(),
            }),
        });
        std::env::set_var("OMAR_PERSIST_WORKER_SESSIONS", "1");
        let line = crate::manager::build_agent_command("pi --model m", &prompt, &[], &context);
        std::env::remove_var("OMAR_PERSIST_WORKER_SESSIONS");
        assert!(
            line.contains(
                "pi --approve --no-session --session-id '11111111-1111-4111-8111-111111111111' --model m"
            ),
            "{line}"
        );
        assert!(!line.contains("--session "), "{line}");
        for flag in [
            "-c",
            "--continue",
            "--session x",
            "--session=x",
            "-r",
            "--fork x",
            "--session-id y",
        ] {
            let line =
                crate::manager::build_agent_command(&format!("pi {flag}"), &prompt, &[], &context);
            assert!(line.starts_with("printf "), "{flag}: {line}");
            assert!(line.contains("must not pass"), "{flag}: {line}");
        }
        assert!(Pi.fresh_conversation());
    }

    /// The file `--session` reopens wins; a conversation with none is known
    /// by its id.
    #[test]
    fn a_conversation_is_its_file_or_else_its_id() {
        use serde_json::json;
        assert_eq!(
            reported_conversation(&json!({"session": null, "id": "x"})),
            Some("x".into())
        );
        assert_eq!(
            reported_conversation(&json!({"session": "/f.jsonl", "id": "x"})),
            Some("/f.jsonl".into())
        );
        assert_eq!(
            reported_conversation(&json!({"session": "", "id": "x"})),
            Some("x".into())
        );
        assert_eq!(reported_conversation(&json!({})), None);
        assert_eq!(reported_conversation(&json!({"id": ""})), None);
    }

    #[test]
    fn missing_pi_extension_is_retryable_without_terminal_input() {
        let target = Target {
            name: "pi-test",
            pane_pid: 0,
            stamp: Some("pi:/nonexistent/omar.sock"),
        };
        assert!(Pi.deliver(&target, "hello").unwrap_err().is::<NotReady>());
    }
    #[test]
    fn pi_detection_only_classifies_executable_position() {
        for command in [
            "pi",
            "'/opt/Pi Agent/pi'",
            "env FOO=pi pi",
            "exec pi",
            "X=pi command pi",
        ] {
            assert_eq!(
                super::super::detect(command).map(Backend::kind),
                Some(Kind::Pi),
                "{command}"
            );
        }
        for command in [
            "echo pi",
            "cat '/opt/Pi Agent/pi'",
            "env FOO=pi bash -c pi",
            "echo hi; pi",
            "echo hi | pi",
            "env -u pi echo hi",
        ] {
            assert!(super::super::detect(command).is_none(), "{command}");
        }
    }
}
