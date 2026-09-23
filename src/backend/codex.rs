//! Codex: a per-agent app-server the TUI attaches to, or a native exec
//! runner when the TUI would refuse the flags.

use anyhow::{Context, Result};
use uuid::Uuid;

use super::{detect, detect_token, Backend, Kind, Launch};
use super::{managed, NotReady, PaneSetup, WRITE_TIMEOUT};
use crate::manager::{
    managed_agent_command, materialize_mcp_context_file, materialize_prompt_file, omar_server_exe,
    shell_single_quote, short_protocol_dir, write_private_file, McpLaunchContext,
};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

pub struct Codex;

impl Backend for Codex {
    fn kind(&self) -> Kind {
        Kind::Codex
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["codex"]
    }
    fn executables(&self) -> &'static [&'static str] {
        &["codex"]
    }
    fn default_command(&self) -> &'static str {
        "codex --dangerously-bypass-approvals-and-sandbox"
    }
    fn conversation_id(&self, target: &super::Target<'_>) -> Option<String> {
        let socket = from_stamp(target.stamp?)?;
        CodexSession::open(&socket).ok()?.only_thread().ok()
    }
    fn readiness_markers(&self) -> &'static [&'static str] {
        &["OpenAI Codex"]
    }
    fn normalize_command(&self, command: &str) -> String {
        ensure_codex_runtime_flags(command)
    }
    /// Conversations stay in the operator's normal home. A dedicated
    /// app-server owns the per-agent overrides and a socket the TUI connects
    /// to explicitly; custom flags that the TUI refuses to attach with use a
    /// native exec runner instead, preserving Codex config layers.
    fn launch_command(&self, launch: &Launch<'_>) -> String {
        let resumed = super::saved_conversation(launch.context, "codex")
            .map(|id| resume_command(launch.base_command, &id));
        let base_command = resumed.as_deref().unwrap_or(launch.base_command);
        let mcp_context = launch.context;
        let instructions = std::fs::read_to_string(launch.prompt_file).map(|body| {
            launch
                .substitutions
                .iter()
                .fold(body, |body, (pattern, replacement)| {
                    body.replace(pattern, replacement)
                })
        });
        let (tui_command, reasoning_effort) = codex_launch_reasoning_effort(base_command);
        if !codex_refuses_to_attach(&tui_command) {
            if let Some(command) = instructions.ok().and_then(|instructions| {
                codex_server_command(
                    mcp_context,
                    &instructions,
                    reasoning_effort.as_deref(),
                    &tui_command,
                )
            }) {
                return command;
            }
        }
        let rendered = materialize_prompt_file(launch.prompt_file, launch.substitutions);
        let protocol = (|| -> Result<String> {
            let (mut command, initial_session) = codex_exec_command(launch.base_command)?;
            let overrides =
                codex_mcp_overrides(mcp_context).context("cannot configure Codex MCP")?;
            command.push(' ');
            command.push_str(&overrides);
            managed_agent_command("codex", &command, &rendered, mcp_context, initial_session)
        })();
        protocol.unwrap_or_else(|error| {
            format!(
                "printf '%s\n' {} >&2; exit 1",
                shell_single_quote(&format!("OMAR Codex protocol launch failed: {error:#}"))
            )
        })
    }

    /// Detached panes have no client to answer OSC 10/11 palette queries.
    /// Codex caches that failed probe and suppresses RGB composer effects.
    /// Give its pane the same default palette as our web terminal, while
    /// preserving an operator's explicitly configured window style. And a
    /// long-lived tmux server may retain another launcher's environment:
    /// select the caller's normal Codex home explicitly.
    fn prepare_pane(&self, _session: &str, command: &str) -> Result<PaneSetup> {
        let mut setup = PaneSetup::interactive(command);
        let home = std::env::var_os("CODEX_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
            .map(|home| {
                if home.is_absolute() {
                    Ok(home)
                } else {
                    std::env::current_dir().map(|cwd| cwd.join(home))
                }
            })
            .transpose()?;
        if let Some(home) = home {
            setup
                .env
                .push(("CODEX_HOME".to_string(), home.display().to_string()));
        }
        setup.window_style = Some("fg=#d8d5e0,bg=#0b0b0e".to_string());
        Ok(setup)
    }

    fn provision(&self, _session: &str, command: &str) -> Result<Option<String>> {
        if let Some(stamp) = managed::stamp(command) {
            return Ok(Some(stamp));
        }
        Ok(codex_launch_socket(command).map(|socket| format!("codex:{}", socket.display())))
    }
}

fn resume_command(command: &str, id: &str) -> String {
    let words = shlex::split(command).unwrap_or_default();
    let mut resumed = Vec::new();
    for word in words {
        if word == "--dangerously-bypass-approvals-and-sandbox" {
            continue;
        }
        resumed.push(
            shlex::try_quote(&word)
                .expect("command contains no NUL")
                .into_owned(),
        );
        if detect_token(&word).is_some_and(|b| b.kind() == Kind::Codex) {
            resumed.push("resume".into());
            resumed.push(shell_single_quote(id));
        }
    }
    resumed.join(" ")
}

pub(crate) fn ensure_codex_runtime_flags(base_command: &str) -> String {
    if !detect(base_command).is_some_and(|b| b.kind() == Kind::Codex) {
        return base_command.to_string();
    }

    // Remote resume must use the thread's persisted permissions. Codex rejects
    // permission overrides on this path, including an automatically added YOLO.
    let mut words = base_command.split_whitespace();
    if words.any(|word| detect_token(word).is_some_and(|b| b.kind() == Kind::Codex))
        && matches!(words.next(), Some("resume" | "fork"))
    {
        return base_command.to_string();
    }
    let mut command = base_command.to_string();

    if !base_command
        .split_whitespace()
        .any(|token| token == "--dangerously-bypass-approvals-and-sandbox")
    {
        command.push_str(" --dangerously-bypass-approvals-and-sandbox");
    }

    command
}

pub(crate) fn codex_mcp_overrides(context: &McpLaunchContext) -> Option<String> {
    let server_exe = omar_server_exe()?;
    let context_file = materialize_mcp_context_file(context)?;
    let command = serde_json::to_string(&server_exe.display().to_string()).ok()?;
    let args = serde_json::to_string(&vec![
        "mcp-server".to_string(),
        "--context-file".to_string(),
        context_file.display().to_string(),
    ])
    .ok()?;
    let command_arg = format!("mcp_servers.omar.command={}", command);
    let args_arg = format!("mcp_servers.omar.args={}", args);
    Some(format!(
        "-c features.scheduled_tasks=false -c {} -c {}",
        shell_single_quote(&command_arg),
        shell_single_quote(&args_arg)
    ))
}

/// The longest socket path codex will bind.
///
/// Not the operating system's limit — macOS itself accepts 103 bytes — but
/// codex's own, measured against the shipped 0.147.0 binary: 95 binds and 96
/// fails with "path must be shorter than SUN_LEN". A path in between passes
/// the OS and is refused by codex, which loses the channel silently and leaves
/// a home behind for nothing, so the stricter bound is the useful one. It may
/// move with the codex version.
pub(crate) const SUN_PATH_MAX: usize = 96;

/// Flags that make codex refuse to attach to a running app-server.
///
/// The refusal is silent, so a pane carrying one of these would start a server
/// nobody joins, sit out the socket wait, and then have provisioning poll it
/// for ninety seconds — all to arrive where it started. Better to see the flag
/// and not build the home at all.
pub(crate) const CODEX_NO_ATTACH_FLAGS: &[&str] = &[
    "-c",
    "--config",
    "--profile",
    "-p",
    "--strict-config",
    "--search",
    "--approve-for-me",
    "--enable",
    "--disable",
    "--dangerously-bypass-hook-trust",
];

/// Flags unsupported by remote TUI attachment run through native `exec` instead.
/// Keep profile/config semantics in Codex itself; never flatten its config layers.
pub(crate) fn codex_exec_command(command: &str) -> Result<(String, Option<String>)> {
    let words = shlex::split(command).context("invalid quoted Codex command")?;
    let executable = words
        .iter()
        .position(|word| detect_token(word).is_some_and(|b| b.kind() == Kind::Codex))
        .context("missing Codex executable")?;
    anyhow::ensure!(
        !words
            .iter()
            .any(|word| matches!(word.as_str(), ";" | "&&" | "||" | "|")),
        "use a wrapper executable for compound Codex commands"
    );
    let resume = words
        .iter()
        .enumerate()
        .skip(executable + 1)
        .find(|(_, word)| word.as_str() == "resume")
        .map(|(i, _)| i);
    let initial_session = resume
        .map(|i| {
            words
                .get(i + 1)
                .filter(|v| !v.starts_with('-'))
                .cloned()
                .context("managed Codex resume requires an explicit session ID")
        })
        .transpose()?;
    let mut out = Vec::new();
    for (i, word) in words.iter().enumerate() {
        if resume.is_some_and(|r| i == r || i == r + 1) {
            continue;
        }
        if i < executable {
            if let Some((name, value)) = word.split_once('=') {
                if name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                    out.push(format!("{name}={}", shell_single_quote(value)));
                    continue;
                }
            }
        }
        match word.as_str() {
            "--search" if i > executable => out.push("-c 'web_search=\"live\"'".into()),
            "--no-alt-screen" if i > executable => {}
            _ => out.push(shell_single_quote(word)),
        }
        if i == executable {
            out.push("exec --skip-git-repo-check".into());
        }
    }
    Ok((out.join(" "), initial_session))
}

pub(crate) fn codex_refuses_to_attach(base_command: &str) -> bool {
    base_command.split_whitespace().any(|token| {
        let flag = token.split_once('=').map_or(token, |(flag, _)| flag);
        CODEX_NO_ATTACH_FLAGS.contains(&flag)
            || (token.starts_with("-c") && !token.starts_with("--"))
            || (token.starts_with("-p") && !token.starts_with("--"))
    })
}

/// Move the override emitted by `spawn_agent` onto the dedicated app-server.
/// Only consume that known literal spelling, leaving arbitrary shell/config
/// expressions on the fallback path. Keep the original command for fallback
/// so an IO failure never loses the requested effort.
pub(crate) fn codex_launch_reasoning_effort(base_command: &str) -> (String, Option<String>) {
    // spawn_agent appends the override; runtime flag normalization may append
    // the bypass flag after it. Do not search inside arbitrary shell arguments.
    let runtime_flag = " --dangerously-bypass-approvals-and-sandbox";
    let (command, suffix) = base_command
        .strip_suffix(runtime_flag)
        .map_or((base_command, ""), |command| (command, runtime_flag));
    for value in ["low", "medium", "high", "xhigh"] {
        let flag = format!(" -c model_reasoning_effort='\"{value}\"'");
        if let Some(command) = command.strip_suffix(&flag) {
            // Multiple overrides or other config flags still trigger the
            // fallback; do not reorder or reinterpret their precedence.
            return (format!("{command}{suffix}"), Some(value.to_string()));
        }
    }
    (base_command.to_string(), None)
}

/// Isolate the live connection, not the user's saved conversations. Both the
/// server and TUI inherit the operator's CODEX_HOME (or Codex's normal default).
/// Only transient socket and prompt files live under OMAR's runtime directory.
pub(crate) fn codex_server_command(
    context: &McpLaunchContext,
    instructions: &str,
    reasoning_effort: Option<&str>,
    tui_command: &str,
) -> Option<String> {
    // The server must come from the same executable and environment wrapper
    // as the requested TUI, including an explicitly selected Codex install.
    let executable = tui_command
        .split_whitespace()
        .find(|word| detect_token(word).is_some_and(|b| b.kind() == Kind::Codex))?;
    let executable_end = tui_command.find(executable)? + executable.len();
    let server_command = &tui_command[..executable_end];
    let overrides = codex_mcp_overrides(context)?;
    let id = Uuid::new_v4().simple().to_string();
    let runtime = context.omar_dir.join("codex-runtime").join(&id[..12]);
    let socket = if runtime.join("app.sock").as_os_str().len() >= SUN_PATH_MAX {
        short_protocol_dir().ok()?.join("app.sock")
    } else {
        runtime.join("app.sock")
    };
    std::fs::create_dir_all(runtime.parent()?).ok()?;
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&runtime)
        .ok()?;
    let prompt_file = runtime.join("instructions.json");
    let encoded = serde_json::to_vec(instructions).ok()?;
    if write_private_file(&prompt_file, &encoded).is_err() {
        let _ = std::fs::remove_dir_all(&runtime);
        return None;
    }
    let effort = reasoning_effort
        .map(|effort| {
            format!(
                " -c {}",
                shell_single_quote(&format!("model_reasoning_effort=\"{effort}\""))
            )
        })
        .unwrap_or_default();
    let endpoint = shell_single_quote(&format!("unix://{}", socket.display()));
    let mut tui = format!("{tui_command} --remote {endpoint}");
    if let Some(words) =
        super::saved_conversation(context, "codex").and_then(|_| shlex::split(tui_command))
    {
        if let Some(index) = words.iter().position(|word| word == "resume") {
            let fresh = words
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != index && *i != index + 1)
                .map(|(_, word)| {
                    shlex::try_quote(word)
                        .expect("command contains no NUL")
                        .into_owned()
                })
                .collect::<Vec<_>>()
                .join(" ");
            tui.push_str(&format!(
                " || {} --remote {endpoint}",
                ensure_codex_runtime_flags(&fresh)
            ));
        }
    }
    Some(format!(
        "export OMAR_CODEX_SOCKET={socket}; \
         {server_command} app-server --listen {endpoint} {overrides}{effort} \
         -c \"developer_instructions=$(cat {prompt_file})\" >{log} 2>&1 & \
         omar_srv=$!; omar_waited=0; \
         while [ ! -S \"$OMAR_CODEX_SOCKET\" ] && [ \"$omar_waited\" -lt 100 ] \
         && kill -0 \"$omar_srv\" 2>/dev/null; \
         do sleep 0.2; omar_waited=$((omar_waited+1)); done; \
         if [ ! -S \"$OMAR_CODEX_SOCKET\" ]; then \
         cat {log} >&2; kill \"$omar_srv\" 2>/dev/null; exit 1; fi; \
         {tui}",
        socket = shell_single_quote(&socket.display().to_string()),
        prompt_file = shell_single_quote(&prompt_file.display().to_string()),
        log = shell_single_quote(&runtime.join("server.log").display().to_string()),
    ))
}

/// Parse a stamp written at launch, e.g. `codex:/path/to/app.sock`.
pub(crate) fn from_stamp(stamp: &str) -> Option<PathBuf> {
    let rest = stamp.strip_prefix("codex:")?;
    (!rest.is_empty()).then(|| PathBuf::from(rest))
}

/// Hand an event to the one thread the pane has loaded.
pub(crate) fn deliver_via_app_server(socket: &Path, text: &str) -> Result<()> {
    let mut session = CodexSession::open(socket)?;
    let thread = session.only_thread()?;
    session.deliver_event(&thread, text)
}

pub(crate) fn codex_launch_socket(command: &str) -> Option<PathBuf> {
    if let Some((_, assignment)) = command.split_once("export OMAR_CODEX_SOCKET=") {
        let path = managed::unquote_single(assignment)?;
        return (!path.is_empty()).then(|| PathBuf::from(path));
    }
    let assignment = command.split_once("export CODEX_HOME=")?.1;
    let dir = managed::unquote_single(assignment)?;
    (!dir.is_empty()).then(|| codex_socket_path(Path::new(&dir)))
}

/// Where a codex home's app-server listens.
pub fn codex_socket_path(home: &Path) -> PathBuf {
    home.join("app-server-control")
        .join("app-server-control.sock")
}

/// A JSON-RPC conversation with a codex app-server.
///
/// The transport is a WebSocket over a Unix socket. `tungstenite::client`
/// performs the upgrade: it wants a URL only to build the request line and
/// `Host` header, so a placeholder is fine — the bytes go wherever the stream
/// already points, which here is a socket path.
pub(crate) struct CodexSession {
    socket: tungstenite::WebSocket<UnixStream>,
    next_id: u64,
}

impl CodexSession {
    /// Connect, upgrade, and complete the app-server's opening handshake.
    pub(crate) fn open(path: &Path) -> Result<CodexSession> {
        let stream =
            UnixStream::connect(path).with_context(|| format!("connect to {}", path.display()))?;
        stream.set_read_timeout(Some(WRITE_TIMEOUT))?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;

        let (socket, _) = tungstenite::client("ws://localhost/", stream).map_err(|error| {
            match error {
                // The read timeout set above surfaces as WouldBlock, which
                // tungstenite renders as a bare "Interrupted handshake" —
                // say what actually ran out, since provisioning retries this
                // for 90s and every line would otherwise read the same.
                tungstenite::HandshakeError::Interrupted(_) => anyhow::anyhow!(
                    "app-server did not answer the websocket upgrade within {}s",
                    WRITE_TIMEOUT.as_secs()
                ),
                tungstenite::HandshakeError::Failure(error) => {
                    anyhow::anyhow!("websocket upgrade failed: {error}")
                }
            }
        })?;

        let mut session = CodexSession { socket, next_id: 1 };
        session
            .call(
                "initialize",
                serde_json::json!({
                    "clientInfo": {
                        "name": "omar",
                        "title": "omar",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }),
            )
            .context("initialize the app-server session")?;
        session.notify("initialized")?;
        Ok(session)
    }

    fn send(&mut self, message: serde_json::Value) -> Result<()> {
        self.socket
            .send(tungstenite::Message::Text(message.to_string()))
            .context("write to the app-server")
    }

    fn notify(&mut self, method: &str) -> Result<()> {
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": {},
        }))
    }

    /// Send a request and read until its answer arrives.
    ///
    /// The server pushes notifications of its own down the same socket, so a
    /// reply is found by id rather than by being the next message.
    fn call(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;

        loop {
            let message = self.socket.read().context("read from the app-server")?;
            let tungstenite::Message::Text(body) = message else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
                continue;
            };
            if value.get("id").and_then(serde_json::Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                anyhow::bail!("{} failed: {}", method, error);
            }
            return Ok(value.get("result").cloned().unwrap_or_default());
        }
    }

    /// The thread the pane is showing, when that is unambiguous.
    pub(crate) fn only_thread(&mut self) -> Result<String> {
        let listed = self.call("thread/loaded/list", serde_json::json!({}))?;
        if listed
            .get("data")
            .and_then(serde_json::Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            return Err(NotReady("the app-server has not loaded a thread yet").into());
        }
        only_thread(&listed).context("the pane has no single loaded thread to inject into")
    }

    /// Start an idle thread, or queue tool output on its active turn.
    ///
    /// App-server owns the idle/active decision atomically. A separate read
    /// followed by inject_items/start can strand an event when a turn ends,
    /// or deliver it twice. Tool output also keeps scheduler messages from
    /// masquerading as user instructions. No thread settings are overridden.
    /// Protocol: https://learn.chatgpt.com/docs/app-server#start-a-turn
    pub(crate) fn deliver_event(&mut self, thread: &str, text: &str) -> Result<()> {
        self.call(
            "turn/start",
            serde_json::json!({
                "threadId": thread,
                "input": [],
                "toolOutput": {
                    "name": "omar_event",
                    "namespace": "omar",
                    "output": text,
                },
            }),
        )?;
        Ok(())
    }
}

/// The thread an event belongs in, out of a `thread/loaded/list` reply — and
/// only when there is no doubt which that is.
///
/// The app-server will say which threads a pane has loaded but not which one
/// it is showing, and neither creation order nor activity separates them:
/// `/new` leaves the old thread loaded under a newer id, and `/resume` puts
/// the pane back on an *older* id while the abandoned new one stays loaded. A
/// guess that lands on the thread the user has moved on from is delivered,
/// acknowledged, and never read.
///
/// So OMAR only claims a channel it is sure of. More than one loaded thread
/// means delivery fails rather than guessing or using the input box.
pub(crate) fn only_thread(listed: &serde_json::Value) -> Option<String> {
    let threads = listed.get("data")?.as_array()?;
    match threads.as_slice() {
        [only] => only.as_str().map(str::to_string),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::build_agent_command;
    use crate::manager::tests::*;
    use std::os::unix::net::UnixListener;
    use std::time::Duration;

    #[test]
    fn codex_launch_does_not_install_untrusted_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let overrides = codex_mcp_overrides(&test_mcp_context(dir.path())).unwrap();
        assert!(overrides.contains("mcp_servers.omar.command"));
        assert!(!overrides.contains("hooks."));
        assert!(!overrides.contains("developer_instructions"));
    }

    #[test]
    fn codex_uses_its_alternate_screen_unless_explicitly_disabled() {
        let default = ensure_codex_runtime_flags("codex");
        assert_eq!(default, "codex --dangerously-bypass-approvals-and-sandbox");
        assert_eq!(ensure_codex_runtime_flags(&default), default);
        let explicit = "codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox";
        assert_eq!(ensure_codex_runtime_flags(explicit), explicit);
        assert_eq!(ensure_codex_runtime_flags("bash"), "bash");
    }

    #[test]
    fn codex_exec_preserves_config_precedence_and_native_resume() {
        let (command, session) = codex_exec_command(
            "env TEAM='a b' codex --profile work --search -c model_reasoning_effort='\"low\"' -c model_reasoning_effort='\"high\"' resume saved-session"
        ).unwrap();
        assert_eq!(session.as_deref(), Some("saved-session"));
        assert_eq!(
            shlex::split(&command).unwrap(),
            [
                "env",
                "TEAM=a b",
                "codex",
                "exec",
                "--skip-git-repo-check",
                "--profile",
                "work",
                "-c",
                "web_search=\"live\"",
                "-c",
                "model_reasoning_effort=\"low\"",
                "-c",
                "model_reasoning_effort=\"high\""
            ]
        );
        assert!(codex_exec_command("codex --search resume --last").is_err());
    }

    #[test]
    fn the_socket_bound_is_the_one_codex_enforces_not_the_kernel() {
        // Measured against codex 0.147.0: a 95-byte socket path binds, 96
        // fails with "path must be shorter than SUN_LEN". macOS itself would
        // take 103, and a path in that gap is the bad case — it passes the
        // kernel, codex refuses it, and the channel is lost silently while a
        // home is left behind for nothing.
        assert_eq!(
            SUN_PATH_MAX, 96,
            "the guard must reject the paths codex rejects, not the ones the OS does"
        );
    }

    #[test]
    fn deep_state_paths_keep_a_short_native_codex_socket() {
        let dir = short_tempdir();
        let deep = dir.path().join("d".repeat(90));
        std::fs::create_dir_all(&deep).unwrap();
        let prompt = deep.join("ea.md");
        std::fs::write(&prompt, "be helpful").unwrap();
        let cmd = build_agent_command("codex", &prompt, &[], &test_mcp_context(&deep));
        let socket = codex_launch_socket(&cmd).unwrap();
        assert!(socket.as_os_str().len() < SUN_PATH_MAX);
        assert!(socket.parent().unwrap().is_dir());
        assert!(!cmd.contains("CODEX_HOME="));
        assert!(cmd.contains("mcp_servers.omar.command"));
    }

    /// The dedup joined two strings; a missing separator is a launch that
    /// never reaches codex, and no `contains` assertion would notice.
    #[test]
    fn a_provisioning_launch_is_valid_shell() {
        let out = std::process::Command::new("sh")
            .args(["-n", "-c", ""])
            .output()
            .expect("sh -n");
        assert!(out.status.success(), "sh cannot syntax-check");

        let dir = short_tempdir();
        let cmd = codex_server_command(&test_mcp_context(dir.path()), "be helpful", None, "codex")
            .unwrap();
        let checked = std::process::Command::new("sh")
            .arg("-n")
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.take().unwrap().write_all(cmd.as_bytes())?;
                child.wait_with_output()
            })
            .expect("syntax-check the launch command");
        assert!(
            checked.status.success(),
            "generated launch is not valid shell: {}\n{}",
            String::from_utf8_lossy(&checked.stderr),
            cmd
        );
        // Syntax alone does not catch a lost separator: the append would just
        // redirect into `config.tomlcodex` and `sh -n` would still be happy.
        assert!(
            cmd.contains("; codex app-server"),
            "the trust fragment must end in a separator: {cmd}"
        );
    }

    #[test]
    fn launching_codex_preserves_legacy_conversation_history() {
        let dir = short_tempdir();
        let history = dir.path().join("codex/old/sessions/conversation.jsonl");
        std::fs::create_dir_all(history.parent().unwrap()).unwrap();
        std::fs::write(&history, "persistent conversation").unwrap();
        codex_server_command(&test_mcp_context(dir.path()), "be helpful", None, "codex").unwrap();
        assert_eq!(
            std::fs::read_to_string(history).unwrap(),
            "persistent conversation"
        );
    }

    /// Play the app-server's half of one delivery and report what it was
    /// asked, so the framing is checked against a real socket rather than a
    /// string.
    fn fake_app_server(listener: UnixListener, threads: Vec<&'static str>) -> Vec<String> {
        fake_app_server_reply(
            listener,
            threads,
            serde_json::json!({
                "result": { "turn": { "id": "new-turn", "status": "inProgress", "items": [], "error": null } }
            }),
        )
    }

    fn fake_app_server_reply(
        listener: UnixListener,
        threads: Vec<&'static str>,
        turn_reply: serde_json::Value,
    ) -> Vec<String> {
        let (stream, _) = listener.accept().unwrap();
        // `tungstenite::accept` answers the upgrade, so the test exercises the
        // same handshake the app-server does rather than a hand-made reply.
        let mut socket = tungstenite::accept(stream).unwrap();
        let answer = |socket: &mut tungstenite::WebSocket<UnixStream>,
                      request: &serde_json::Value,
                      result: serde_json::Value| {
            let reply = serde_json::json!({ "id": request["id"], "result": result });
            socket
                .send(tungstenite::Message::Text(reply.to_string()))
                .unwrap();
        };

        let mut asked = Vec::new();
        loop {
            let message = match socket.read() {
                Ok(message) => message,
                Err(_) => return asked,
            };
            let tungstenite::Message::Text(body) = message else {
                continue;
            };
            asked.push(body.clone());
            let request: serde_json::Value = serde_json::from_str(&body).unwrap();
            match request["method"].as_str().unwrap_or_default() {
                "initialize" => {
                    // The server talks unprompted; a reply is found by id, not
                    // by being the next thing to arrive.
                    socket
                        .send(tungstenite::Message::Text(
                            serde_json::json!({
                                "method": "remoteControl/status/changed",
                                "params": { "status": "disabled" },
                            })
                            .to_string(),
                        ))
                        .unwrap();
                    answer(
                        &mut socket,
                        &request,
                        serde_json::json!({ "userAgent": "codex-tui/0.147.0" }),
                    );
                }
                "thread/loaded/list" => answer(
                    &mut socket,
                    &request,
                    serde_json::json!({ "data": threads, "nextCursor": null }),
                ),
                "turn/start" => {
                    // Notifications can interleave with the acknowledgement,
                    // including completion of a previously active turn.
                    socket
                        .send(tungstenite::Message::Text(
                            serde_json::json!({
                                "method": "turn/completed", "params": { "threadId": threads[0] }
                            })
                            .to_string(),
                        ))
                        .unwrap();
                    let mut reply = turn_reply.clone();
                    reply["id"] = request["id"].clone();
                    socket
                        .send(tungstenite::Message::Text(reply.to_string()))
                        .unwrap();
                    return asked;
                }
                _ => {}
            }
        }
    }

    #[test]
    fn an_event_wakes_codex_with_tool_output_over_the_app_server_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("app-server-control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            fake_app_server(listener, vec!["01a02051-f65b-7260-9afe-13ffe5229bf6"])
        });

        deliver_via_app_server(&socket, "CI went red on main")
            .expect("deliver over the app-server");

        let asked: Vec<serde_json::Value> = server
            .join()
            .unwrap()
            .iter()
            .map(|body| serde_json::from_str(body).unwrap())
            .collect();
        let methods: Vec<&str> = asked
            .iter()
            .map(|request| request["method"].as_str().unwrap())
            .collect();
        assert_eq!(
            methods,
            vec![
                "initialize",
                "initialized",
                "thread/loaded/list",
                "turn/start"
            ]
        );

        let inject = asked.last().unwrap();
        assert_eq!(
            inject["params"]["threadId"], "01a02051-f65b-7260-9afe-13ffe5229bf6",
            "the event belongs in the thread the pane is showing"
        );
        assert_eq!(inject["params"]["input"], serde_json::json!([]));
        assert_eq!(
            inject["params"]["toolOutput"],
            serde_json::json!({
                "name": "omar_event", "namespace": "omar", "output": "CI went red on main"
            })
        );
        assert_eq!(
            inject["params"].as_object().unwrap().len(),
            3,
            "delivery must not override the thread's model, permissions or effort"
        );
        // Every request is JSON-RPC; a notification carries no id.
        assert_eq!(asked[1].get("id"), None);
    }

    #[test]
    fn an_active_turn_accepts_one_event_without_injection_or_interrupt() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("active.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            fake_app_server_reply(
                listener,
                vec!["thread"],
                serde_json::json!({"result": {"turn": {"id": "already-active", "status": "inProgress", "items": [], "error": null}}}),
            )
        });
        deliver_via_app_server(&socket, "event during a tool call").unwrap();
        let requests: Vec<serde_json::Value> = server
            .join()
            .unwrap()
            .iter()
            .map(|body| serde_json::from_str(body).unwrap())
            .collect();
        let writes: Vec<_> = requests
            .iter()
            .filter(|request| request["params"].get("threadId").is_some())
            .collect();
        assert_eq!(
            writes.len(),
            1,
            "start and inject together would duplicate the event"
        );
        assert_eq!(writes[0]["method"], "turn/start");
        assert_eq!(writes[0]["params"]["input"], serde_json::json!([]));
        assert_eq!(
            writes[0]["params"]["toolOutput"]["output"],
            "event during a tool call"
        );
    }

    #[test]
    fn a_rejected_turn_start_is_not_reported_as_delivered() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("rejected.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            fake_app_server_reply(
                listener,
                vec!["thread"],
                serde_json::json!({"error": {"code": -32602, "message": "tool output rejected"}}),
            )
        });
        let error = deliver_via_app_server(&socket, "event").unwrap_err();
        assert!(error.to_string().contains("turn/start failed"));
        assert_eq!(
            server.join().unwrap().len(),
            4,
            "do not inject after a rejected wake"
        );
    }

    #[test]
    fn a_dead_app_server_is_a_failed_delivery_rather_than_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        assert!(deliver_via_app_server(&dir.path().join("gone.sock"), "standup in 5").is_err());
    }

    /// Provisioning retries `open` for 90s, so a bare "Interrupted handshake"
    /// would be 90s of identical lines that name neither the socket nor the
    /// timeout that produced them.
    #[test]
    fn a_silent_app_server_names_the_timeout_rather_than_the_symptom() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("mute.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        // Accept, then say nothing: the upgrade waits out the read timeout.
        let server = std::thread::spawn(move || {
            let held = listener.accept();
            std::thread::sleep(WRITE_TIMEOUT + Duration::from_secs(1));
            drop(held);
        });

        let error = deliver_via_app_server(&socket, "standup in 5")
            .expect_err("a server that never answers cannot take a delivery");
        let error = format!("{error:#}");
        assert!(
            error.contains("did not answer the websocket upgrade"),
            "want the timeout named, got: {error}"
        );
        server.join().unwrap();
    }

    #[test]
    fn a_codex_stamp_names_the_socket_to_inject_through() {
        assert_eq!(
            from_stamp("codex:/Users/ke/.omar/codex/ab12/app.sock"),
            Some(PathBuf::from("/Users/ke/.omar/codex/ab12/app.sock"))
        );
        assert_eq!(from_stamp("codex:"), None);
    }

    #[test]
    fn a_codex_home_is_read_back_out_of_a_launch_command() {
        assert_eq!(
            codex_launch_socket(
                "export CODEX_HOME='/Users/ke/.omar/codex/ab12'; codex --no-alt-screen"
            ),
            Some(codex_socket_path(Path::new("/Users/ke/.omar/codex/ab12")))
        );
        assert_eq!(codex_launch_socket("codex --no-alt-screen"), None);
        assert_eq!(codex_launch_socket("export CODEX_HOME=''; codex"), None);
    }

    #[test]
    fn explicit_codex_socket_wins_over_an_inherited_home() {
        for path in ["/tmp/my sockets/app.sock", "/tmp/it's mine/app.sock"] {
            let command = format!(
                "export CODEX_HOME='/user/home'; export OMAR_CODEX_SOCKET={}; codex",
                crate::manager::shell_single_quote(path)
            );
            assert_eq!(codex_launch_socket(&command), Some(PathBuf::from(path)));
        }
    }

    #[test]
    fn a_home_under_a_directory_with_a_space_comes_back_whole() {
        // Read back through the quoting rather than split on whitespace. Half
        // a path parses as a plausible home, and provisioning would then claim
        // and poll somewhere the pane never was — while the pane's real home,
        // never claimed, ages into the prune.
        for home in [
            "/Users/ke/My Home/.omar/codex/ab12",
            "/Users/ke/it's mine/.omar/codex/ab12",
            "/Users/ke/two  spaces/ab12",
        ] {
            let command = format!(
                "export CODEX_HOME={}; codex app-server --listen unix://",
                crate::manager::shell_single_quote(home)
            );
            assert_eq!(
                codex_launch_socket(&command),
                Some(codex_socket_path(Path::new(home))),
                "in {command}"
            );
        }
        // A word that never closes is not a path worth guessing at.
        assert_eq!(
            codex_launch_socket("export CODEX_HOME='/Users/ke/unterminated"),
            None
        );
    }

    #[test]
    fn an_event_goes_only_to_a_thread_there_is_no_doubt_about() {
        let listed = serde_json::json!({ "data": ["01a0204b-f2e8-73e3-b95a-09abf7616b22"], "nextCursor": null });
        assert_eq!(
            only_thread(&listed).as_deref(),
            Some("01a0204b-f2e8-73e3-b95a-09abf7616b22")
        );

        // Two loaded threads and the app-server will not say which the pane is
        // showing. Creation order does not settle it: `/new` moves the pane to
        // the newer id, `/resume` moves it back to the older one while the
        // abandoned new thread stays loaded. Guessing wrong is delivered,
        // acknowledged, and never read — so OMAR declines and the event goes
        // pending instead.
        let ambiguous = serde_json::json!({
            "data": ["01a0204b-f2e8-73e3-b95a-09abf7616b22", "01a02051-f65b-7260-9afe-13ffe5229bf6"],
        });
        assert_eq!(only_thread(&ambiguous), None);

        // A pane whose TUI has not opened a thread yet is not a channel.
        assert_eq!(only_thread(&serde_json::json!({ "data": [] })), None);
        assert_eq!(only_thread(&serde_json::json!({})), None);
    }

    #[test]
    fn an_ambiguous_pane_reports_a_failure_rather_than_injecting_somewhere() {
        // The caller's cue to retain pending work is an error. Returning `Ok` after
        // injecting into a thread nobody is reading would lose the event with
        // no sign that anything went wrong.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("app-server-control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            fake_app_server(
                listener,
                vec![
                    "01a0204b-f2e8-73e3-b95a-09abf7616b22",
                    "01a02051-f65b-7260-9afe-13ffe5229bf6",
                ],
            )
        });

        let failure = deliver_via_app_server(&socket, "CI went red")
            .expect_err("two loaded threads must not be guessed between");
        assert!(
            failure.to_string().contains("no single loaded thread"),
            "unexpected error: {failure}"
        );
        assert_eq!(server.join().unwrap().len(), 3);
    }

    #[test]
    fn a_pane_that_is_gone_takes_its_app_server_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("app.sock");
        let stamp = format!("codex:{}", socket.display());
        let target = super::super::Target {
            name: "pane",
            pane_pid: 0,
            stamp: Some(&stamp),
        };
        let gone = Codex.deliver(&target, "event").unwrap_err();
        assert!(gone.is::<NotReady>(), "{gone}");

        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || fake_app_server(listener, vec!["thread"]));
        Codex.deliver(&target, "event").unwrap();
        server.join().unwrap();
    }
}
