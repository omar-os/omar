//! Claude Code: a system prompt on the line, hooks and the peer socket
//! through one `--settings`, and OMAR's tools over MCP.
use std::path::{Path, PathBuf};

use super::{delivery_failed, NotReady, PaneSetup, Target, WRITE_TIMEOUT};
use super::{detect, detect_token, Backend, Kind, Launch};
use crate::manager::{
    actor_file, backend_native_disallowed_tools_csv, materialize_mcp_context_file, mcp_ea_dir,
    omar_server_exe, shell_single_quote, write_private_file, McpLaunchContext,
};
use anyhow::{Context, Result};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::process::Command;

pub struct Claude;

impl Backend for Claude {
    fn kind(&self) -> Kind {
        Kind::Claude
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["claude", "claude-code", "claude_code", "claudecode"]
    }
    fn executables(&self) -> &'static [&'static str] {
        // Homebrew ships Claude Code as a single-file executable named
        // `claude.exe`, which is what the pane reports it is running.
        &["claude", "claude.exe"]
    }
    fn default_command(&self) -> &'static str {
        "claude --dangerously-skip-permissions"
    }
    fn conversation_id(&self, target: &Target<'_>) -> Option<String> {
        let directory = claude_sessions_dir()?;
        std::iter::once(target.pane_pid)
            .chain(child_pids(target.pane_pid))
            .find_map(|pid| {
                let value: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(directory.join(format!("{pid}.json"))).ok()?,
                )
                .ok()?;
                value["sessionId"].as_str().map(str::to_owned)
            })
    }
    fn readiness_markers(&self) -> &'static [&'static str] {
        &["Claude Code", "❯"]
    }
    fn launch_command(&self, launch: &Launch<'_>) -> String {
        let base_command = with_coordination_hooks(launch.base_command, launch.context);
        match materialize_claude_mcp_config(launch.context) {
            Some(mcp_config) => format!(
                "{} --append-system-prompt \"{}\" --mcp-config {} --disallowedTools {}",
                base_command,
                launch.shell_expr,
                shell_single_quote(&mcp_config.display().to_string()),
                shell_single_quote(&backend_native_disallowed_tools_csv()),
            ),
            None => format!(
                "{} --append-system-prompt \"{}\"",
                base_command, launch.shell_expr
            ),
        }
    }
    /// The prompt goes in by file, so it never touches argv and is unbounded;
    /// the placeholders are resolved on disk since nothing pipes it through
    /// sed at launch.
    fn ea_launch_command(
        &self,
        base_command: &str,
        prompt_file: &Path,
        prompt: &str,
        context: &McpLaunchContext,
    ) -> Option<String> {
        std::fs::write(prompt_file, prompt).ok();
        let base_command = with_coordination_hooks(base_command, context);
        // Use a known ID from the first launch, so even an interrupted first
        // turn can be resumed without guessing from the operator's global history.
        let saved = super::saved_conversation(context, "claude");
        let native = saved
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let managed = context.serve.is_some();
        if managed {
            let _ = super::remember_conversation(context, "claude", &native);
        }
        let launch_base = if managed {
            format!(
                "{base_command} {} {}",
                if saved.is_some() {
                    "--resume"
                } else {
                    "--session-id"
                },
                shell_single_quote(&native)
            )
        } else {
            base_command.to_owned()
        };
        let base_command = launch_base.as_str();
        let launch = match materialize_claude_mcp_config(context) {
            Some(mcp_config) => format!(
                "{} --append-system-prompt-file {} --mcp-config {} --disallowedTools {}",
                base_command,
                shell_single_quote(&prompt_file.display().to_string()),
                shell_single_quote(&mcp_config.display().to_string()),
                shell_single_quote(&backend_native_disallowed_tools_csv()),
            ),
            None => format!(
                "{} --append-system-prompt-file {}",
                base_command,
                shell_single_quote(&prompt_file.display().to_string()),
            ),
        };
        Some(if saved.is_some() {
            let fresh = launch.replace(
                &format!("--resume {}", shell_single_quote(&native)),
                &format!("--session-id {}", shell_single_quote(&native)),
            );
            format!("{launch} || {fresh}")
        } else {
            launch
        })
    }

    /// Every launch passes through here, so this is where a claude pane is
    /// told to take OMAR's events over its peer socket.
    fn prepare_pane(&self, _session: &str, command: &str) -> Result<PaneSetup> {
        Ok(PaneSetup::interactive(&ensure_claude_inbound_settings(
            command,
        )))
    }
    /// The peer socket is found from the process, not from a stamp: a pane
    /// whose backend has restarted has a new registry entry.
    fn discover_and_deliver(&self, target: &Target<'_>, text: &str) -> Result<()> {
        let Some(peer) = resolve_peer(target.pane_pid) else {
            return Err(NotReady("no claude peer socket accepts messages").into());
        };
        peer.deliver(text)
            .with_context(|| delivery_failed(target, "claude peer socket"))
    }
}

/// `--settings '{"crossSessionInbound":"accept"}'`, as it goes on a launch line.
///
/// It lets OMAR deliver events over Claude Code's cross-session peer socket.
/// Without it the session holds an inbound message behind an approval dialog —
/// OMAR does not attest a permission mode, and an agent launched with
/// `--dangerously-skip-permissions` distrusts a sender that has not. The dialog
/// covers the composer, so the held message is worse than no channel at all.
/// `channel` looks for the same setting before it uses the socket.
pub(crate) fn claude_inbound_settings() -> String {
    format!("--settings {}", shell_single_quote(CLAUDE_INBOUND_ACCEPT))
}

/// Put the inbound-peer setting on a claude launch line that has none.
///
/// It goes right after the `claude` token: order means nothing to claude, and
/// the end of the line may belong to a pipe or to a second command. A line
/// that already carries `--settings` is the operator's and is left alone —
/// Claude Code keeps only the last one it is given, so adding another would
/// replace theirs. Other backends' lines come back unchanged.
pub fn ensure_claude_inbound_settings(command: &str) -> String {
    if !detect(command).is_some_and(|b| b.kind() == Kind::Claude)
        || command
            .split_whitespace()
            .any(|token| token == "--settings" || token.starts_with("--settings="))
    {
        return command.to_string();
    }
    let mut end = 0;
    for token in command.split_whitespace() {
        let start = end + command[end..].find(token).expect("token was cut from here");
        end = start + token.len();
        if detect_token(token).is_some_and(|b| b.kind() == Kind::Claude) {
            break;
        }
    }
    format!(
        "{} {}{}",
        &command[..end],
        claude_inbound_settings(),
        &command[end..]
    )
}

/// Keep caller-supplied settings intact. The durable scheduler and MCP
/// projections still supervise sessions whose custom settings own their hooks.
pub(crate) fn with_coordination_hooks(command: &str, context: &McpLaunchContext) -> String {
    if context.topology.is_some()
        || command
            .split_whitespace()
            .any(|t| t == "--settings" || t.starts_with("--settings="))
    {
        return command.to_owned();
    }
    let Some(context_file) = materialize_mcp_context_file(context) else {
        return command.to_owned();
    };
    let Some(exe) = omar_server_exe() else {
        return command.to_owned();
    };
    let hook = format!(
        "{} agent-hook --context-file {}",
        shell_single_quote(&exe.display().to_string()),
        shell_single_quote(&context_file.display().to_string())
    );
    let handler = serde_json::json!([{"hooks":[{"type":"command","command":hook,"timeout":5}]}]);
    let settings = serde_json::json!({"crossSessionInbound":"accept","hooks":{
        "SessionStart":handler,"UserPromptSubmit":handler,"Stop":handler
    }});
    format!(
        "{command} --settings {}",
        shell_single_quote(&settings.to_string())
    )
}

pub(crate) fn materialize_claude_mcp_config(context: &McpLaunchContext) -> Option<PathBuf> {
    let server_exe = omar_server_exe()?;
    let context_file = materialize_mcp_context_file(context)?;
    let json = serde_json::json!({
        "mcpServers": {
            "omar": {
                "type": "stdio",
                "command": server_exe,
                "args": ["mcp-server", "--context-file", context_file],
            }
        }
    });

    let dir = mcp_ea_dir(context)?;
    let path = match &context.topology {
        Some(topology) => dir.join(format!(
            "claude-mcp-topology-{}-{}.json",
            topology.team, topology.agent
        )),
        None => dir.join(actor_file(context.agent_name.as_deref(), "claude-mcp")),
    };
    write_private_file(&path, &serde_json::to_vec(&json).ok()?).ok()?;
    Some(path)
}

/// The setting a Claude Code session must be launched with before a peer
/// message reaches the model instead of an approval dialog. This is how it
/// appears in the process's argv, once the shell has stripped the quotes the
/// launch command put around it.
pub(crate) const CLAUDE_INBOUND_ACCEPT: &str = r#"{"crossSessionInbound":"accept"}"#;

/// Claude Code's cross-session peer socket. The message is appended to the
/// session's command queue, which is a separate structure from the input
/// buffer, so a draft in the composer is untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Peer {
    pub(crate) socket: PathBuf,
    pub(crate) token: String,
}

impl Peer {
    pub(crate) fn deliver(&self, text: &str) -> Result<()> {
        let mut stream = UnixStream::connect(&self.socket)
            .with_context(|| format!("connect to {}", self.socket.display()))?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        stream
            .write_all(claude_peer_frames(&self.token, text).as_bytes())
            .context("write peer message")?;
        stream.flush().context("flush peer message")?;
        Ok(())
    }
}

/// The peer of a pane, if its backend offers one.
///
/// `pane_pid` is the pane's own process. The backend may be a child of it
/// when the pane runs a shell, so its children are checked too.
pub(crate) fn resolve_peer(pane_pid: u32) -> Option<Peer> {
    let sessions = claude_sessions_dir()?;
    std::iter::once(pane_pid)
        .chain(child_pids(pane_pid))
        .find_map(|pid| claude_peer(&sessions, pid).filter(|_| accepts_peer_messages(pid)))
}

/// The two newline-delimited JSON frames a peer sends: authenticate, then the
/// message itself.
///
/// `priority: "next"` queues the event behind any turn already running instead
/// of preempting it — an event is news, not an interrupt.
pub(crate) fn claude_peer_frames(token: &str, text: &str) -> String {
    let auth = serde_json::json!({ "type": "auth", "token": token });
    let message = serde_json::json!({
        "type": "user",
        "priority": "next",
        "message": { "role": "user", "content": text },
    });
    format!("{}\n{}\n", auth, message)
}

pub(crate) fn claude_sessions_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".claude").join("sessions"))
}

/// Read one session's registry entry and its peer token.
///
/// Claude Code writes `<pid>.json` describing the session and a sibling
/// `<pid>.<hash>.key` holding the token a peer must present.
pub(crate) fn claude_peer(sessions: &Path, pid: u32) -> Option<Peer> {
    let registry = std::fs::read_to_string(sessions.join(format!("{}.json", pid))).ok()?;
    let registry: serde_json::Value = serde_json::from_str(&registry).ok()?;
    let socket = PathBuf::from(registry.get("messagingSocketPath")?.as_str()?);
    if !socket.exists() {
        return None;
    }

    let prefix = format!("{}.", pid);
    let token = std::fs::read_dir(sessions)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| {
            path.extension().is_some_and(|ext| ext == "key")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&prefix))
        })
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        .and_then(|key| key.get("peerToken")?.as_str().map(str::to_string))?;

    Some(Peer { socket, token })
}

/// Will this Claude Code session take a peer message, rather than hold it?
///
/// A session that bypasses permission prompts holds a peer message behind an
/// approval dialog unless it was told to accept them — on its launch line, or
/// in the operator's own settings. The socket takes the bytes either way, so
/// the delivery looks successful while the event sits unread until a human
/// approves it. A session told neither way — started by an older OMAR, or by
/// hand — has no available channel until configured to accept peer messages.
pub(crate) fn accepts_peer_messages(pid: u32) -> bool {
    launched_to_accept(pid) || settings_accept(&claude_user_settings())
}

/// Was `--settings` with [`CLAUDE_INBOUND_ACCEPT`] on the process's launch line?
fn launched_to_accept(pid: u32) -> bool {
    process_argv(pid).is_some_and(|argv| argv_settings_accept(&argv))
}

fn argv_settings_accept(argv: &str) -> bool {
    // OMAR may combine inbound acceptance with lifecycle hooks in one native
    // settings object. Compare the setting, not one exact JSON serialization.
    let Some((_, tail)) = argv.rsplit_once("--settings") else {
        return false;
    };
    let Some(settings) = tail.strip_prefix('=').or_else(|| tail.strip_prefix(' ')) else {
        return false;
    };
    serde_json::Deserializer::from_str(settings.trim_start())
        .into_iter::<serde_json::Value>()
        .next()
        .and_then(Result::ok)
        .is_some_and(|value| value["crossSessionInbound"] == "accept")
}

/// A process's arguments, space-joined. `/proc` is exact and needs no other
/// tool; `ps` covers macOS.
fn process_argv(pid: u32) -> Option<String> {
    if let Ok(raw) = std::fs::read(format!("/proc/{}/cmdline", pid)) {
        return Some(String::from_utf8_lossy(&raw).replace('\0', " "));
    }
    Command::new("ps")
        .args(["-ww", "-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Does a Claude Code settings file accept inbound peer messages?
///
/// A managed or repository setting can still tighten this to `hold`. That is
/// the operator's stated wish, and the message is then shown to them.
fn settings_accept(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        .is_some_and(|settings| {
            settings
                .get("crossSessionInbound")
                .and_then(|value| value.as_str())
                == Some("accept")
        })
}

fn claude_user_settings() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("settings.json")
}

pub(crate) fn child_pids(parent: u32) -> Vec<u32> {
    Command::new("pgrep")
        .args(["-P", &parent.to_string()])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| line.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::tests::*;
    use crate::manager::TopologyMcpContext;
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    #[test]
    fn a_claude_launch_line_is_told_to_accept_peer_messages_once() {
        // Without this the session holds OMAR's messages behind an approval
        // dialog that covers the composer, and no event is ever delivered.
        let flag = r#"--settings '{"crossSessionInbound":"accept"}'"#;
        let once = ensure_claude_inbound_settings("claude --dangerously-skip-permissions");
        assert_eq!(
            once,
            format!("claude {flag} --dangerously-skip-permissions")
        );
        assert_eq!(ensure_claude_inbound_settings(&once), once);
        // By the executable: the tail of a line may belong to another program.
        assert_eq!(
            ensure_claude_inbound_settings("TERM=xterm claude -p hi 2>&1 | tee run.log"),
            format!("TERM=xterm claude {flag} -p hi 2>&1 | tee run.log")
        );
        // The delivery side looks for exactly this pair in the pane's argv.
        assert!(once.contains(&format!("--settings '{}'", CLAUDE_INBOUND_ACCEPT)));
    }

    #[test]
    fn an_operators_own_settings_are_not_replaced() {
        // Claude Code keeps only the last `--settings`; adding ours after the
        // operator's would silently drop theirs.
        for line in [
            "claude --settings ~/team.json",
            "claude --settings=~/team.json --dangerously-skip-permissions",
        ] {
            assert_eq!(ensure_claude_inbound_settings(line), line);
        }
    }

    #[test]
    fn only_claude_is_told_about_inbound_peer_messages() {
        // The flag is Claude Code's; handing it to another backend would be an
        // unrecognised argument at launch.
        for base in [
            "codex",
            "opencode",
            "cursor agent --yolo",
            "agy --dangerously-skip-permissions",
            "bash",
        ] {
            let cmd = ensure_claude_inbound_settings(base);
            assert!(
                !cmd.contains("crossSessionInbound"),
                "{base} must not receive a Claude-only flag: {cmd}"
            );
        }
    }

    #[test]
    fn coordination_hooks_preserve_explicit_settings_and_skip_topology() {
        let dir = tempfile::tempdir().unwrap();
        let context = test_mcp_context(dir.path());
        let custom = "claude --settings '/tmp/my settings.json'";
        assert_eq!(with_coordination_hooks(custom, &context), custom);
        let hooked = with_coordination_hooks("claude", &context);
        for event in ["SessionStart", "UserPromptSubmit", "Stop"] {
            assert!(hooked.contains(event));
        }
        let mut topology = context;
        topology.topology = Some(TopologyMcpContext {
            team: "t".into(),
            agent: "a".into(),
            endpoint: "localhost:1".into(),
            token: "t".into(),
        });
        assert_eq!(with_coordination_hooks("claude", &topology), "claude");
    }

    #[test]
    fn combined_claude_hook_settings_still_allow_peer_delivery() {
        for separator in [" ", "="] {
            let argv = format!(
                "claude --settings{separator}{} --append-system-prompt instructions",
                serde_json::json!({
                    "hooks":{"Stop":[{"hooks":[{"type":"command","command":"omar agent-hook --context-file /tmp/a b.json"}]}]},
                    "crossSessionInbound":"accept"
                })
            );
            assert!(argv_settings_accept(&argv));
        }
        assert!(!argv_settings_accept("claude --settings {}"));
        assert!(!argv_settings_accept(
            "claude --settings {\"crossSessionInbound\":\"hold\"}"
        ));
    }

    #[test]
    fn a_peer_message_is_two_json_frames_and_never_touches_the_composer() {
        let frames = claude_peer_frames("deadbeef", "standup in 5 minutes");
        let mut lines = frames.lines();

        let auth: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(auth["type"], "auth");
        assert_eq!(auth["token"], "deadbeef");

        let message: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(message["type"], "user");
        assert_eq!(message["message"]["role"], "user");
        assert_eq!(message["message"]["content"], "standup in 5 minutes");
        // Queued behind a running turn rather than preempting it.
        assert_eq!(message["priority"], "next");

        assert!(lines.next().is_none());
        assert!(frames.ends_with('\n'), "frames are newline-delimited");
    }

    #[test]
    fn delivery_writes_both_frames_to_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("peer.sock");
        let listener = UnixListener::bind(&socket).unwrap();

        let reader = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = String::new();
            let _ = stream.read_to_string(&mut received);
            received
        });

        Peer {
            socket: socket.clone(),
            token: "t0ken".to_string(),
        }
        .deliver("ship it")
        .expect("deliver over the peer socket");

        let received = reader.join().unwrap();
        assert_eq!(received, claude_peer_frames("t0ken", "ship it"));
    }

    #[test]
    fn a_session_whose_socket_is_gone_is_not_offered_as_a_channel() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("4242.json"),
            r#"{"pid":4242,"messagingSocketPath":"/tmp/does-not-exist-omar.sock"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("4242.abc.key"), r#"{"peerToken":"unused"}"#).unwrap();

        assert_eq!(claude_peer(dir.path(), 4242), None);
    }

    /// Guards against the real registry drifting from the shape we parse.
    /// Skips when no Claude Code session is running, so CI stays green.
    #[test]
    fn a_live_claude_session_resolves_to_a_channel() {
        let Some(sessions) = claude_sessions_dir().filter(|dir| dir.is_dir()) else {
            eprintln!("Skipping test: no Claude Code sessions directory");
            return;
        };

        let live: Vec<u32> = std::fs::read_dir(&sessions)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                name.strip_suffix(".json")?.parse::<u32>().ok()
            })
            .filter(|pid| claude_peer(&sessions, *pid).is_some())
            .collect();

        if live.is_empty() {
            eprintln!("Skipping test: no running Claude Code session to resolve");
            return;
        }

        for pid in live {
            // A session can exit between listing and reading it; that is the
            // registry behaving correctly, not a parse failure.
            let Some(Peer { socket, token }) = claude_peer(&sessions, pid) else {
                continue;
            };
            assert!(!token.is_empty(), "peer token for {pid} must be non-empty");
            assert!(
                socket.to_string_lossy().ends_with(".sock"),
                "peer socket path looks wrong: {}",
                socket.display()
            );
        }
    }

    #[test]
    fn a_launch_line_that_accepts_peer_messages_is_read_back_off_the_process() {
        // Stand-ins for a claude process: what matters is the argv, and a
        // shell's arguments are read the way claude's are. `; :` keeps sh
        // from exec'ing sleep in its place, which would replace the argv.
        let mut accepting = Command::new("sh")
            .args([
                "-c",
                "sleep 30; :",
                "sh",
                "--settings",
                CLAUDE_INBOUND_ACCEPT,
            ])
            .spawn()
            .unwrap();
        let mut holding = Command::new("sh")
            .args(["-c", "sleep 30; :", "sh", "--dangerously-skip-permissions"])
            .spawn()
            .unwrap();

        assert!(launched_to_accept(accepting.id()));
        assert!(!launched_to_accept(holding.id()));

        let _ = accepting.kill();
        let _ = holding.kill();
        let _ = accepting.wait();
        let _ = holding.wait();
    }

    #[test]
    fn an_operator_who_accepts_peer_messages_in_their_settings_is_believed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"theme":"dark","crossSessionInbound":"accept"}"#).unwrap();
        assert!(settings_accept(&path));
        std::fs::write(&path, r#"{"crossSessionInbound":"hold"}"#).unwrap();
        assert!(!settings_accept(&path));
        assert!(!settings_accept(&dir.path().join("missing.json")));
    }

    #[test]
    fn a_registry_entry_and_its_key_resolve_to_a_channel() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("live.sock");
        let _listener = UnixListener::bind(&socket).unwrap();

        std::fs::write(
            dir.path().join("77.json"),
            serde_json::json!({ "pid": 77, "messagingSocketPath": socket }).to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("77.9f3.key"),
            r#"{"peerToken":"s3cret","procStart":"now"}"#,
        )
        .unwrap();

        assert_eq!(
            claude_peer(dir.path(), 77),
            Some(Peer {
                socket,
                token: "s3cret".to_string()
            })
        );
    }
}
