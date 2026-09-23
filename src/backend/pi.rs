//! Pi's native TUI and extension API, with a private per-pane delivery socket.
use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

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
    fn readiness(&self, _command: &str) -> Readiness {
        Readiness::Channel
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
    fn conversation_id(&self, target: &Target<'_>) -> Option<String> {
        let socket = target.stamp?.strip_prefix("pi:")?;
        request(socket, serde_json::json!({"session": true})).ok()?["session"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }
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
    let mut command = format!(
        "{} --approve{}",
        &launch.base_command[..end],
        &launch.base_command[end..]
    );
    if !shlex::split(launch.base_command)
        .unwrap_or_default()
        .iter()
        .any(|word| {
            matches!(
                word.as_str(),
                "--session" | "--resume" | "-r" | "--continue" | "-c"
            ) || word.starts_with("--session=")
        })
    {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pi_launch_materializes_scoped_bridge_and_resumes_only_saved_chat() {
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
        assert_ne!(launch_socket(&command), launch_socket(&worker));
        context.agent_name = None;
        std::fs::remove_file(session).unwrap();
        assert!(
            !crate::manager::build_agent_command("pi", &prompt, &[], &context)
                .contains("--session")
        );
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
