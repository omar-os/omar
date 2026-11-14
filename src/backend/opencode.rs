//! opencode: launched bare on a port, configured through the environment,
//! and given its prompt as the first message over HTTP.

use super::managed;
use super::{connection, Connection, Login};
use super::{Backend, Kind, Launch};
use crate::manager::{
    actor_file, materialize_mcp_context_file, mcp_ea_dir, omar_server_exe, shell_single_quote,
    write_private_file, McpLaunchContext, BACKEND_NATIVE_AGENT_TOOLS, BACKEND_NATIVE_WAKE_TOOLS,
};
use anyhow::{Context, Result};
use std::io::Write;
use std::time::{Duration, Instant};

pub struct Opencode;

impl Backend for Opencode {
    fn blocked_input(
        &self,
        target: &super::Target<'_>,
        _hook_file: &std::path::Path,
    ) -> Option<String> {
        pending_input(target).map(|(id, _)| id)
    }
    fn answer_input(
        &self,
        target: &super::Target<'_>,
        _hook_file: &std::path::Path,
        id: &str,
        text: &str,
    ) -> Result<bool> {
        let Some((current, request)) = pending_input(target).filter(|(current, _)| current == id)
        else {
            return Ok(false);
        };
        let (port, _) = from_stamp(target.stamp.context("OpenCode has no stamp")?)
            .context("invalid OpenCode stamp")?;
        let (kind, id) = current.split_once(':').context("invalid input id")?;
        let body = if kind == "question" {
            serde_json::json!({"answers":request["questions"].as_array().context("question omitted questions")?.iter().map(|_| vec![text]).collect::<Vec<_>>()})
        } else {
            serde_json::json!({"reply":"reject","message":text})
        };
        let (status, _) = http_json(
            port,
            "POST",
            &format!("/{kind}/{id}/reply"),
            Some(&body.to_string()),
        )?;
        if status == 404 {
            return Ok(false);
        }
        anyhow::ensure!(
            (200..300).contains(&status),
            "OpenCode rejected question response (HTTP {status})"
        );
        Ok(true)
    }
    fn ended_turn(
        &self,
        target: &super::Target<'_>,
        hook_file: &std::path::Path,
    ) -> Option<String> {
        let (_, session) = from_stamp(target.stamp?)?;
        let path = hook_file.with_extension(format!("{session}.json"));
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        (value["idle"] == true)
            .then(|| value["turn"].as_str().map(str::to_owned))
            .flatten()
    }
    fn kind(&self) -> Kind {
        Kind::Opencode
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["opencode"]
    }
    fn executables(&self) -> &'static [&'static str] {
        &["opencode"]
    }
    fn default_command(&self) -> &'static str {
        // opencode has no permission-skip flag.
        "opencode"
    }
    /// `--standalone` keeps a running opencode daemon, whose environment may
    /// not be ours, from answering. Both attempts share one deadline.
    fn connection(&self, program: &str) -> Connection {
        let deadline = connection::status_deadline();
        let args = ["auth", "list", "--format", "json", "--standalone"];
        let Some(output) = connection::run_until(program, &args, deadline) else {
            return Connection::unknown("`opencode auth list` did not answer");
        };
        if let Some(connection) = auth_list(&output.stdout) {
            return connection;
        }
        // opencode 1 has no JSON, only its table.
        match connection::run_until(program, &["auth", "list"], deadline) {
            Some(output) => auth_table(&output.stdout),
            None => Connection::unknown("`opencode auth list` did not answer"),
        }
    }
    fn install_command(&self) -> Option<&'static str> {
        Some("curl -fsSL https://opencode.ai/install | bash")
    }
    fn login(&self, _headless: bool) -> Option<Login> {
        Some(Login {
            args: &["auth", "login"],
            note: None,
        })
    }
    fn credential_vars(&self) -> &'static [&'static str] {
        connection::PROVIDER_KEYS
    }
    fn logout(&self) -> Option<Login> {
        Some(Login {
            args: &["auth", "logout"],
            note: None,
        })
    }
    fn sign_in_check(&self, _program: &str) -> Option<Vec<&'static str>> {
        Some(vec!["run", connection::CHECK_PROMPT])
    }
    fn conversation_id(&self, target: &super::Target<'_>) -> Option<String> {
        from_stamp(target.stamp?).map(|(_, session)| session)
    }
    /// The attempt's id is created over `/api/session`, which takes the id
    /// it is given, only once the server says it has no such session, and
    /// is used only once it holds no message.
    fn fresh_conversation(&self) -> bool {
        true
    }
    /// The form `/api/session` takes.
    fn new_conversation_id(&self) -> String {
        format!("ses_{}", uuid::Uuid::new_v4().simple())
    }
    /// Asked of the live server just before delivery, not only the stamp:
    /// it is this worker's own, and the conversation is there, still empty.
    fn prove_fresh(
        &self,
        target: &super::Target<'_>,
        id: &str,
        _since: std::time::SystemTime,
        deadline: Instant,
    ) -> Result<()> {
        let (port, _) = target
            .stamp
            .and_then(from_stamp)
            .context("no opencode delivery channel")?;
        served_by_worker(port, target.pane_pid)?;
        empty_session(port, id, deadline)
    }
    fn forget(&self, target: &super::Target<'_>) {
        if let Some((port, session)) = target.stamp.and_then(from_stamp) {
            let _ = http_json(port, "DELETE", &format!("/session/{session}"), None);
        }
    }
    /// Single words: a long workspace path wraps the footer and splits
    /// "tab agents" and "ctrl+p commands" across lines.
    fn readiness_markers(&self) -> &'static [&'static str] {
        &["tab", "agents", "ctrl+p", "commands"]
    }
    /// opencode has no `--append-system-prompt`, and `--prompt` is treated
    /// as the first user message, which makes the model read the agent
    /// prompt descriptively and ask back "What is your agent name?". So the
    /// prompt rides in the first message instead.
    fn takes_prompt_in_first_message(&self) -> bool {
        true
    }
    /// Spawned bare, with a port so OMAR can reach it without the input box;
    /// the prompt arrives through that channel as synthetic context.
    fn launch_command(&self, launch: &Launch<'_>) -> String {
        let resumed = super::saved_conversation(launch.context, "opencode").map(|id| {
            format!(
                "{} --session {}",
                launch.base_command,
                shell_single_quote(&id)
            )
        });
        let base_command = with_opencode_port(resumed.as_deref().unwrap_or(launch.base_command));
        let base_command = match opencode_config_env(launch.context) {
            Some(config) => format!(
                "OPENCODE_CONFIG_CONTENT={} {}",
                shell_single_quote(&config),
                base_command
            ),
            None => base_command,
        };
        // An attempt's conversation is the one `provision` creates with this
        // id; opencode has no flag that opens a new one by id.
        match launch
            .context
            .topology
            .as_ref()
            .and_then(|t| t.attempt.as_ref())
        {
            Some(attempt) => format!(
                "{ATTEMPT_SESSION}={} {base_command}",
                shell_single_quote(&attempt.native_session)
            ),
            None => base_command,
        }
    }

    /// The HTTP server needs a session created and selected in the TUI before
    /// an event can be addressed to it. Waited for here, so the launcher
    /// cannot return with a pane nothing can reach.
    fn provision(&self, _session: &str, command: &str) -> Result<Option<String>> {
        if let Some(stamp) = managed::stamp(command) {
            return Ok(Some(stamp));
        }
        let words = shlex::split(command).unwrap_or_default();
        if let Some(id) = words
            .iter()
            .find_map(|word| word.strip_prefix(&format!("{ATTEMPT_SESSION}=")))
        {
            let port = opencode_port(command)
                .context("an opencode agent that resets its conversation (per lifetime) needs a port to reach")?;
            return provision_fresh_session(port, id, PROVISION_TIMEOUT).map(Some);
        }
        match opencode_port(command) {
            Some(port) => Ok(Some(
                provision_opencode_session(
                    port,
                    shlex::split(command).and_then(|words| {
                        words
                            .windows(2)
                            .find(|pair| pair[0] == "--session")
                            .map(|pair| pair[1].clone())
                    }),
                )
                .context("OpenCode delivery channel did not become ready before launch timeout")?,
            )),
            None => Ok(None),
        }
    }
}

fn pending_input(target: &super::Target<'_>) -> Option<(String, serde_json::Value)> {
    let (port, session) = from_stamp(target.stamp?)?;
    for kind in ["question", "permission"] {
        let Ok((200, body)) = http_json(port, "GET", &format!("/{kind}"), None) else {
            continue;
        };
        let Ok(requests) = serde_json::from_str::<serde_json::Value>(&body) else {
            continue;
        };
        if let Some(request) = requests
            .as_array()?
            .iter()
            .find(|request| request["sessionID"] == session)
        {
            return Some((
                format!("{kind}:{}", request["id"].as_str()?),
                request.clone(),
            ));
        }
    }
    None
}

/// Give opencode a port so OMAR can reach it without the input box.
///
/// opencode only listens when it is told a port: with none it talks to an
/// in-process worker over a fake hostname, and there is nothing to connect to.
/// Nothing is broken if the port cannot be claimed — the pane simply launches
/// without a side channel and events go through the composer.
pub(crate) fn with_opencode_port(base_command: &str) -> String {
    if base_command
        .split_whitespace()
        .any(|token| token == "--port" || token.starts_with("--port=") || token == "--hostname")
    {
        return base_command.to_string();
    }
    match free_port() {
        Some(port) => format!("{} --port {}", base_command, port),
        None => base_command.to_string(),
    }
}

pub(crate) fn materialize_opencode_coordination_plugin(
    context: &McpLaunchContext,
) -> Option<String> {
    let exe = omar_server_exe()?;
    let context_file = materialize_mcp_context_file(context)?;
    let path = if context.topology.is_some() {
        context_file.with_extension("coordination.mjs")
    } else {
        mcp_ea_dir(context)?.join(
            actor_file(context.agent_name.as_deref(), "coordination").replace(".json", ".mjs"),
        )
    };
    let body = format!(
        "const exe = {};\nconst contextFile = {};\nconst topology = {};\n{}",
        serde_json::to_string(&exe).ok()?,
        serde_json::to_string(&context_file).ok()?,
        context.topology.is_some(),
        include_str!("opencode/plugin.mjs")
    );
    write_private_file(&path, body.as_bytes()).ok()?;
    let encoded: String = path
        .to_str()?
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    Some(format!("file://{encoded}"))
}

/// Nobody answers a permission prompt in a worker, so no rule may be left at
/// opencode's `ask` default: `external_directory` (paths outside the
/// workspace) and `read` of `*.env` files. `doom_loop` is denied, not asked.
fn permission_rules() -> serde_json::Value {
    serde_json::json!({
        "doom_loop": "deny",
        "external_directory": "allow",
        "read": "allow"
    })
}

pub(crate) fn opencode_config_env(context: &McpLaunchContext) -> Option<String> {
    let server_exe = omar_server_exe()?;
    let context_file = materialize_mcp_context_file(context)?;
    // Disable every backend-native tool that overlaps an OMAR MCP tool so
    // delegation/scheduling can only flow through OMAR and stays visible in
    // the dashboard. Names that opencode does not expose are no-ops.
    let mut tools = serde_json::Map::new();
    for name in BACKEND_NATIVE_WAKE_TOOLS
        .iter()
        .chain(BACKEND_NATIVE_AGENT_TOOLS.iter())
    {
        tools.insert((*name).to_string(), serde_json::Value::Bool(false));
    }
    let mut config = serde_json::json!({
        "mcp": {
            "omar": {
                "type": "local",
                "enabled": true,
                // opencode lists MCP tools as `<server>_<tool>`. Bare names
                // make that `omar_set_port`, the name OMAR's prompts use,
                // rather than `omar_omar_set_port`, which literal-minded
                // models never find.
                "command": [
                    server_exe.display().to_string(),
                    "mcp-server",
                    "--context-file",
                    context_file.display().to_string(),
                    "--bare-tool-names"
                ]
            }
        },
        "tools": tools,
        "permission": permission_rules()
    });
    if let Some(plugin) = materialize_opencode_coordination_plugin(context) {
        config["plugin"] = serde_json::json!([plugin]);
    }
    Some(config.to_string())
}

/// Parse a stamp written at launch, e.g. `opencode:47455:ses_abc`.
pub(crate) fn from_stamp(stamp: &str) -> Option<(u16, String)> {
    let rest = stamp.strip_prefix("opencode:")?;
    let (port, session) = rest.split_once(':')?;
    (!session.is_empty()).then_some((port.parse().ok()?, session.to_string()))
}

pub(crate) fn deliver_over_http(port: u16, session: &str, text: &str) -> Result<()> {
    let body = serde_json::json!({
        "parts": [{ "type": "text", "text": text, "synthetic": true }],
    })
    .to_string();
    let (status, _) = http_json(
        port,
        "POST",
        &format!("/session/{}/prompt_async", session),
        Some(&body),
    )
    .context("post message to opencode")?;
    if status != 204 {
        anyhow::bail!("opencode answered {}", status);
    }
    Ok(())
}

/// Bound each blocking operation; startup can wait through several timeouts.
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to keep waiting for a freshly launched backend to open its port.
/// opencode takes several seconds to boot; past this it is not coming up.
const PROVISION_TIMEOUT: Duration = Duration::from_secs(90);

/// Claim a free loopback port for a backend that must be told one at launch.
///
/// The socket is closed immediately, so this reserves nothing — it only picks
/// a number the OS was willing to hand out. If something else takes it first
/// opencode cannot bind; observed behaviour is that it keeps running without a
/// listener, so provisioning times out and launch reports the channel failure.
pub(crate) fn free_port() -> Option<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .ok()?
        .local_addr()
        .ok()
        .map(|addr| addr.port())
}

/// `--port N` as it appears in a launch command.
pub(crate) fn opencode_port(command: &str) -> Option<u16> {
    let mut tokens = command.split_whitespace();
    while let Some(token) = tokens.next() {
        if token == "--port" {
            return tokens.next()?.parse().ok();
        }
        if let Some(value) = token.strip_prefix("--port=") {
            return value.parse().ok();
        }
    }
    None
}

/// Create a session on a running opencode server and point its TUI at it.
///
/// opencode's API cannot say which session a given pane is showing, and a pane
/// only creates one once the user speaks. So OMAR makes the session itself:
/// the id it gets back is then unambiguously this pane's, even when several
/// agents share a directory.
fn provision_opencode_session(port: u16, previous: Option<String>) -> Option<String> {
    let deadline = std::time::Instant::now() + PROVISION_TIMEOUT;
    let mut session = previous;
    let mut verified = session.is_none();
    while std::time::Instant::now() < deadline {
        if !verified {
            let id = session.as_ref()?;
            match http_json(port, "GET", &format!("/session/{id}"), None) {
                Ok((200, _)) => verified = true,
                Ok((404, _)) => {
                    session = None;
                    verified = true;
                }
                _ => {
                    // A server still booting is not a missing conversation.
                    std::thread::sleep(Duration::from_millis(250));
                    continue;
                }
            }
        }
        if session.is_none() {
            if let Ok((200, body)) = http_json(port, "POST", "/session", Some("{}")) {
                session = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|value| value.get("id")?.as_str().map(str::to_string));
            }
        }
        if let Some(id) = &session {
            let select = serde_json::json!({ "sessionID": id }).to_string();
            if let Ok((200, _)) = http_json(port, "POST", "/tui/select-session", Some(&select)) {
                return Some(format!("opencode:{}:{}", port, id));
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    None
}

/// The launch word naming the session an attempt's `provision` creates.
const ATTEMPT_SESSION: &str = "OMAR_OPENCODE_SESSION";

/// Create the attempt's session `id` on a newly started server and point its
/// TUI at it. `/api/session` hands back an existing session of that id rather
/// than refuse, so the id must be unknown before and the session empty after.
/// Nothing falls back to `/session`, where opencode picks the id: an opencode
/// without `/api/session` cannot run an attempt.
fn provision_fresh_session(port: u16, id: &str, timeout: Duration) -> Result<String> {
    let deadline = std::time::Instant::now() + timeout;
    let wait = || {
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "opencode on port {port} did not answer within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(250));
        Ok(())
    };
    loop {
        // A boot-time GET can remain unanswered even after newer connections
        // work. Reconnect read-only probes; never replay the creation POST.
        let probe_deadline = deadline.min(Instant::now() + HTTP_TIMEOUT);
        match http_json_until(port, "GET", &format!("/session/{id}"), None, probe_deadline) {
            Ok((404, _)) => break,
            Ok((200, _)) => anyhow::bail!("opencode already has a session {id}"),
            // A server still booting is not yet an answer.
            _ => wait().context("waiting for opencode session API")?,
        }
    }
    let body = serde_json::json!({ "id": id }).to_string();
    let (status, created) = http_json_until(port, "POST", "/api/session", Some(&body), deadline)?;
    let created: Option<serde_json::Value> = serde_json::from_str(&created).ok();
    let created = created.as_ref().and_then(|v| v["data"]["id"].as_str());
    anyhow::ensure!(
        status == 200 && created == Some(id),
        "opencode did not create session {id} through /api/session (HTTP {status}, got {created:?}); \
         a `per` lifetime needs an opencode that has it"
    );
    empty_session(port, id, deadline)?;
    let select = serde_json::json!({ "sessionID": id }).to_string();
    while !matches!(
        http_json_until(port, "POST", "/tui/select-session", Some(&select), deadline),
        Ok((200, _))
    ) {
        wait().context("waiting for opencode TUI session selection")?;
    }
    Ok(format!("opencode:{port}:{id}"))
}

/// Only processes of the worker in pane `pane_pid` listen on `port`: the
/// port was free when it was picked, not held, so another server may have
/// taken it first, and that one's session is neither new nor stopped with
/// the worker.
fn served_by_worker(port: u16, pane_pid: u32) -> Result<()> {
    let listeners = crate::process::tcp_listeners(port)?;
    let worker: std::collections::BTreeSet<u32> = crate::process::process_tree(pane_pid)
        .into_iter()
        .map(|(pid, _)| pid)
        .collect();
    anyhow::ensure!(
        !listeners.is_empty() && listeners.is_subset(&worker),
        "port {port} is served by {listeners:?}, not by this worker's opencode"
    );
    Ok(())
}

/// The server has session `id`, and it holds no message.
fn empty_session(port: u16, id: &str, deadline: Instant) -> Result<()> {
    let (status, session) =
        http_json_until(port, "GET", &format!("/session/{id}"), None, deadline)?;
    let session: Option<serde_json::Value> = serde_json::from_str(&session).ok();
    anyhow::ensure!(
        status == 200 && session.as_ref().and_then(|v| v["id"].as_str()) == Some(id),
        "opencode does not have session {id} (HTTP {status})"
    );
    let (status, messages) = http_json_until(
        port,
        "GET",
        &format!("/session/{id}/message"),
        None,
        deadline,
    )?;
    let messages: Option<Vec<serde_json::Value>> = serde_json::from_str(&messages).ok();
    match messages {
        Some(messages) if status == 200 && messages.is_empty() => Ok(()),
        Some(messages) if status == 200 => anyhow::bail!(
            "opencode session {id} already holds {} messages",
            messages.len()
        ),
        _ => anyhow::bail!("opencode did not list session {id}'s messages (HTTP {status})"),
    }
}

/// POST JSON to opencode's loopback server and return the status code.
///
/// Hand-rolled rather than pulling in an HTTP stack: the server is on
/// 127.0.0.1, the request shape is fixed, and the response is discarded.
pub(crate) fn http_json(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<(u16, String)> {
    http_json_until(port, method, path, body, Instant::now() + HTTP_TIMEOUT)
}

fn http_json_until(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
    deadline: Instant,
) -> Result<(u16, String)> {
    let address = ([127, 0, 0, 1], port).into();
    let mut stream = retry_io_until(deadline, |timeout| {
        std::net::TcpStream::connect_timeout(&address, timeout)
    })
    .with_context(|| format!("connect to 127.0.0.1:{port}"))?;

    let body = body.unwrap_or("");
    let request = format!(
        "{} {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        method,
        path,
        port,
        body.len(),
        body
    );
    let mut pending = request.as_bytes();
    while !pending.is_empty() {
        let written = retry_io_until(deadline, |timeout| {
            stream.set_write_timeout(Some(timeout))?;
            stream.write(pending)
        })
        .with_context(|| format!("{method} {path}: write opencode request"))?;
        anyhow::ensure!(written > 0, "opencode closed while writing {method} {path}");
        pending = &pending[written..];
    }

    let mut response = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let read = retry_io_until(deadline, |timeout| {
            stream.set_read_timeout(Some(timeout))?;
            std::io::Read::read(&mut stream, &mut buffer)
        })
        .with_context(|| format!("{method} {path}: read opencode response"))?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..read]);
        // Some opencode routes keep the socket open despite Connection: close.
        // A Content-Length body is complete without waiting for EOF.
        if response_complete(&response) {
            break;
        }
    }
    let response = String::from_utf8_lossy(&response).into_owned();
    let (status, body) = split_response(&response).context("parse opencode response")?;
    Ok((status, body))
}

fn response_complete(response: &[u8]) -> bool {
    let Some(end) = response.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
        return false;
    };
    let Ok(headers) = std::str::from_utf8(&response[..end]) else {
        return false;
    };
    headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .is_some_and(|length| response.len() - end - 4 >= length)
}

// A blocking socket timeout is WouldBlock on Linux and TimedOut elsewhere.
// Keep the connection and partial bytes; never replay a session-creation POST.
fn retry_io_until<T>(
    deadline: Instant,
    mut operation: impl FnMut(Duration) -> std::io::Result<T>,
) -> std::io::Result<T> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "opencode request deadline expired",
            ));
        }
        match operation(remaining.min(HTTP_TIMEOUT)) {
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue
            }
            result => return result,
        }
    }
}

/// Split an HTTP/1.x response into its status code and body.
pub(crate) fn split_response(response: &str) -> Option<(u16, String)> {
    let status = response.split_whitespace().nth(1)?.parse().ok()?;
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    Some((status, body))
}

/// opencode 2 lists every provider it can use, with how it signed in.
fn auth_list(stdout: &str) -> Option<Connection> {
    let Ok(serde_json::Value::Array(integrations)) = serde_json::from_str(stdout) else {
        return None;
    };
    let names: Vec<&str> = integrations
        .iter()
        .filter(|integration| {
            integration["connections"]
                .as_array()
                .is_some_and(|connections| !connections.is_empty())
        })
        .filter_map(|integration| integration["name"].as_str())
        .collect();
    Some(signed_in(&names))
}

/// opencode 1 marks each stored or environment credential with `●`, its
/// provider name ending where the dimmed source begins.
fn auth_table(stdout: &str) -> Connection {
    if !stdout.contains("Credentials") {
        return Connection::unknown("`opencode auth list` gave no answer OMAR knows");
    }
    let names: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.split_once('●'))
        .filter_map(|(_, rest)| rest.split('\x1b').next())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    signed_in(&names)
}

fn signed_in(providers: &[&str]) -> Connection {
    if providers.is_empty() {
        Connection::SignedOut
    } else {
        Connection::connected(providers.join(", "))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn auth_list_names_the_providers_with_a_connection() {
        use super::{auth_list, auth_table, Connection};
        let listed = r#"[
            {"id":"anthropic","name":"Anthropic","connections":[{"type":"env","name":"ANTHROPIC_API_KEY"}]},
            {"id":"openai","name":"OpenAI","connections":[{"type":"credential","id":"c1","label":"me","method":"oauth"}]},
            {"id":"xai","name":"xAI","connections":[]}
        ]"#;
        assert_eq!(
            auth_list(listed),
            Some(Connection::connected("Anthropic, OpenAI"))
        );
        assert_eq!(auth_list("[]"), Some(Connection::SignedOut));
        assert_eq!(auth_list("Usage: opencode auth list"), None);

        // opencode 1's table, as printed with no credentials, then with one
        // stored and one from the environment.
        let empty = "\x1b[0m\n┌  Credentials \x1b[90m~/.local/share/opencode/auth.json\n│\n└  0 credentials\n";
        assert_eq!(auth_table(empty), Connection::SignedOut);
        let two = "┌  Credentials \x1b[90m~/.local/share/opencode/auth.json\n│\n\
                   ●  OpenAI \x1b[90mapi\n│\n└  1 credentials\n\n┌  Environment\n│\n\
                   ●  Google Vertex \x1b[90mGOOGLE_CLOUD_PROJECT\n│\n└  1 environment variable\n";
        assert_eq!(
            auth_table(two),
            Connection::connected("OpenAI, Google Vertex")
        );
        assert!(matches!(
            auth_table("Error: boom"),
            Connection::Unknown { .. }
        ));
    }

    use super::*;
    use std::io::Read;

    #[test]
    fn a_complete_response_does_not_wait_for_the_server_to_close() {
        use std::io::{BufRead, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut stream = BufReader::new(stream);
            loop {
                let mut line = String::new();
                stream.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            stream
                .get_mut()
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                .unwrap();
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).unwrap(), 0);
        });
        let reply = http_json(port, "GET", "/session/id", None);
        server.join().unwrap();
        assert_eq!(reply.unwrap(), (200, "{}".into()));
    }

    #[test]
    fn config_never_leaves_a_blocking_permission_at_ask() {
        let rules = permission_rules();
        assert_eq!(rules["external_directory"], "allow");
        assert_eq!(rules["read"], "allow");
        assert!(rules
            .as_object()
            .unwrap()
            .values()
            .all(|action| action != "ask"));
    }

    #[test]
    fn a_footer_wrapped_by_a_long_path_still_reads_as_ready() {
        let screen = "  ┃  Build · GPT-5.6 Terra Fast OpenAI\n\
            \x20  /tmp/omar/sessions/cosmic- tab      ctrl+p\n\
            \x20  badger/workspaces/0c4c37e5agents   commands\n";
        assert!(Opencode
            .readiness_markers()
            .iter()
            .all(|marker| screen.contains(marker)));
    }

    #[test]
    fn native_resume_waits_for_startup_and_only_creates_when_history_is_missing() {
        use std::io::{BufRead, BufReader};
        for missing in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = std::thread::spawn(move || {
                let script = if missing {
                    vec![
                        ("GET /session/saved ", 404, "{}"),
                        ("POST /session ", 200, r#"{"id":"fresh"}"#),
                        ("POST /tui/select-session ", 200, "{}"),
                    ]
                } else {
                    vec![
                        ("GET /session/saved ", 503, "{}"),
                        ("GET /session/saved ", 200, r#"{"id":"saved"}"#),
                        ("POST /tui/select-session ", 200, "{}"),
                    ]
                };
                for (expected, status, body) in script {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut first = String::new();
                    reader.read_line(&mut first).unwrap();
                    assert!(first.starts_with(expected), "{first}");
                    let mut length = 0;
                    loop {
                        let mut header = String::new();
                        reader.read_line(&mut header).unwrap();
                        if header == "\r\n" {
                            break;
                        }
                        if let Some(value) = header.strip_prefix("Content-Length:") {
                            length = value.trim().parse().unwrap();
                        }
                    }
                    let mut request_body = vec![0; length];
                    reader.read_exact(&mut request_body).unwrap();
                    if expected.starts_with("POST /tui") {
                        let value: serde_json::Value =
                            serde_json::from_slice(&request_body).unwrap();
                        assert_eq!(value["sessionID"], if missing { "fresh" } else { "saved" });
                    }
                    write!(stream, "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
            });
            assert_eq!(
                provision_opencode_session(port, Some("saved".into())),
                Some(format!(
                    "opencode:{port}:{}",
                    if missing { "fresh" } else { "saved" }
                ))
            );
            server.join().unwrap();
        }
    }

    /// A loopback server that answers each request in turn from `script`
    /// and hands back every request line with its body.
    fn scripted(
        script: Vec<(u16, &'static str)>,
    ) -> (u16, std::thread::JoinHandle<Vec<(String, String)>>) {
        scripted_startup(script, Duration::ZERO, false)
    }

    fn scripted_startup(
        script: Vec<(u16, &'static str)>,
        delay: Duration,
        stalled_probe: bool,
    ) -> (u16, std::thread::JoinHandle<Vec<(String, String)>>) {
        use std::io::{BufRead, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            // Accept a probe but never answer it; later connections work.
            let _stalled = stalled_probe.then(|| listener.accept().unwrap().0);
            let mut seen = Vec::new();
            for (status, body) in script {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" {
                        break;
                    }
                    if let Some(value) = header.strip_prefix("Content-Length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut request_body = vec![0; length];
                reader.read_exact(&mut request_body).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                let (first_half, second_half) = body.as_bytes().split_at(body.len() / 2);
                stream.write_all(first_half).unwrap();
                stream.flush().unwrap();
                if first.starts_with("POST /api/session ") && !delay.is_zero() {
                    std::thread::sleep(delay);
                }
                stream.write_all(second_half).unwrap();
                seen.push((
                    first.trim_end().to_string(),
                    String::from_utf8(request_body).unwrap(),
                ));
            }
            seen
        });
        (port, server)
    }

    #[test]
    fn user_input_is_scoped_to_its_session_and_permissions_are_declined() {
        for kind in ["question", "permission"] {
            let request = r#"[{"id":"other","sessionID":"ses_other","questions":[{}]}, {"id":"own","sessionID":"ses_own","questions":[{},{}]}]"#;
            let mut script = Vec::new();
            for _ in 0..2 {
                if kind == "permission" {
                    script.push((200, r#"[{"id":"other","sessionID":"ses_other"}]"#));
                }
                script.push((200, request));
            }
            script.push((200, "true"));
            let (port, server) = scripted(script);
            let stamp = format!("opencode:{port}:ses_own");
            let target = super::super::Target {
                name: "worker",
                pane_pid: 0,
                stamp: Some(&stamp),
            };
            let path = std::path::Path::new("unused");
            let id = Opencode.blocked_input(&target, path).unwrap();
            assert_eq!(id, format!("{kind}:own"));
            assert!(Opencode
                .answer_input(&target, path, &id, "OMAR REMINDER")
                .unwrap());
            let seen = server.join().unwrap();
            let (route, body) = seen.last().unwrap();
            assert_eq!(route, &format!("POST /{kind}/own/reply HTTP/1.1"));
            let body: serde_json::Value = serde_json::from_str(body).unwrap();
            if kind == "question" {
                assert_eq!(
                    body["answers"],
                    serde_json::json!([["OMAR REMINDER"], ["OMAR REMINDER"]])
                );
            } else {
                assert_eq!(body["reply"], "reject");
                assert_eq!(body["message"], "OMAR REMINDER");
            }
        }
    }

    #[test]
    fn startup_accepts_complete_responses_without_waiting_for_connection_close() {
        use std::io::{BufRead, BufReader, Read};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (release, wait) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut connections = Vec::new();
            let mut requests = Vec::new();
            for (status, body) in [
                (404, "{}"),
                (200, r#"{"data":{"id":"ses_new"}}"#),
                (200, r#"{"id":"ses_new"}"#),
                (200, "[]"),
                (200, "true"),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" {
                        break;
                    }
                    if let Some(value) = header.strip_prefix("Content-Length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                reader.read_exact(&mut vec![0; length]).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
                requests.push(first);
                connections.push(stream);
            }
            // The client must finish while every response socket remains open.
            let _ = wait.recv();
            requests
        });
        let result = provision_fresh_session(port, "ses_new", Duration::from_secs(2));
        release.send(()).unwrap();
        assert_eq!(result.unwrap(), format!("opencode:{port}:ses_new"));
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 5);
        assert_eq!(
            requests
                .iter()
                .filter(|line| line.starts_with("POST /api/session "))
                .count(),
            1
        );
    }

    #[test]
    fn response_framing_requires_complete_headers_and_the_declared_byte_length() {
        for response in [
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n".as_slice(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{",
            b"HTTP/1.1 200 OK\r\n\r\n{}",
        ] {
            assert!(!response_complete(response));
        }
        assert!(response_complete(
            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}"
        ));
        assert!(response_complete(
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"
        ));
    }

    #[test]
    fn startup_reconnects_an_unanswered_probe_without_replaying_creation() {
        let (port, server) = scripted_startup(
            vec![
                (404, "{}"),
                (200, r#"{"data":{"id":"ses_new"}}"#),
                (200, r#"{"id":"ses_new"}"#),
                (200, "[]"),
                (200, "true"),
            ],
            Duration::ZERO,
            true,
        );
        let result = provision_fresh_session(port, "ses_new", Duration::from_secs(15));
        assert_eq!(result.unwrap(), format!("opencode:{port}:ses_new"));
        let requests = server.join().unwrap();
        assert!(requests[0].0.starts_with("GET /session/ses_new "));
        assert_eq!(
            requests
                .iter()
                .filter(|(line, _)| line.starts_with("POST /api/session "))
                .count(),
            1
        );
    }

    #[test]
    fn startup_keeps_partial_responses_and_never_replays_session_creation_after_a_read_timeout() {
        let (port, server) = scripted_startup(
            vec![
                (404, "{}"),
                (200, r#"{"data":{"id":"ses_slow"}}"#),
                (200, r#"{"id":"ses_slow"}"#),
                (200, "[]"),
                (200, "true"),
            ],
            HTTP_TIMEOUT + Duration::from_secs(1),
            false,
        );
        let result = provision_fresh_session(port, "ses_slow", Duration::from_secs(15)).unwrap();
        assert_eq!(result, format!("opencode:{port}:ses_slow"));
        let requests = server.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(line, _)| line.starts_with("POST /api/session "))
                .count(),
            1
        );
    }

    #[test]
    fn startup_retries_transient_io_but_keeps_its_deadline_and_permanent_errors() {
        use std::io::{Error, ErrorKind};
        let mut errors = [
            ErrorKind::WouldBlock,
            ErrorKind::TimedOut,
            ErrorKind::Interrupted,
        ]
        .into_iter();
        assert_eq!(
            retry_io_until(Instant::now() + Duration::from_secs(1), |_| {
                match errors.next() {
                    Some(kind) => Err(Error::from(kind)),
                    None => Ok(42),
                }
            })
            .unwrap(),
            42
        );
        let error =
            retry_io_until::<()>(Instant::now(), |_| panic!("deadline expired")).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        let error = retry_io_until::<()>(Instant::now() + Duration::from_secs(1), |_| {
            Err(Error::from(ErrorKind::PermissionDenied))
        })
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn an_unresponsive_startup_socket_cannot_extend_the_start_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (release, wait) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            let _ = wait.recv();
        });
        let started = Instant::now();
        let error = http_json_until(
            port,
            "GET",
            "/session/slow",
            None,
            started + Duration::from_millis(50),
        )
        .unwrap_err();
        release.send(()).unwrap();
        server.join().unwrap();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::TimedOut
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// An attempt's session is created with the id it was given, once the
    /// server is up and has none of that id, and taken only empty.
    #[test]
    fn an_attempt_creates_its_session_by_id_and_takes_it_only_new_and_empty() {
        let created = r#"{"data":{"id":"ses_new"}}"#;
        let (port, server) = scripted(vec![
            (503, "{}"),
            (404, r#"{"name":"NotFoundError"}"#),
            (200, created),
            (200, r#"{"id":"ses_new"}"#),
            (200, "[]"),
            (200, "true"),
        ]);
        assert_eq!(
            provision_fresh_session(port, "ses_new", Duration::from_secs(10)).unwrap(),
            format!("opencode:{port}:ses_new")
        );
        let seen = server.join().unwrap();
        let lines: Vec<&str> = seen.iter().map(|(line, _)| line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "GET /session/ses_new HTTP/1.1",
                "GET /session/ses_new HTTP/1.1",
                "POST /api/session HTTP/1.1",
                "GET /session/ses_new HTTP/1.1",
                "GET /session/ses_new/message HTTP/1.1",
                "POST /tui/select-session HTTP/1.1",
            ]
        );
        assert_eq!(seen[2].1, r#"{"id":"ses_new"}"#);

        // Every way it could be some other conversation fails, and nothing
        // falls back to a session opencode names.
        for (script, expected) in [
            (vec![(200, r#"{"id":"ses_new"}"#)], "already has a session"),
            (
                vec![(404, "{}"), (200, r#"{"data":{"id":"ses_old"}}"#)],
                "did not create session",
            ),
            (
                vec![(404, "{}"), (404, "Not Found")],
                "did not create session",
            ),
            (
                vec![
                    (404, "{}"),
                    (200, created),
                    (200, r#"{"id":"ses_new"}"#),
                    (200, r#"[{"info":{}}]"#),
                ],
                "already holds 1 messages",
            ),
        ] {
            let (port, server) = scripted(script);
            let error = provision_fresh_session(port, "ses_new", Duration::from_secs(10))
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error}");
            let seen = server.join().unwrap();
            assert!(
                seen.iter()
                    .all(|(line, _)| !line.starts_with("POST /session ")),
                "{seen:?}"
            );
        }
    }

    /// The session is proved new only on the worker's own server: one that
    /// some other process listens on is refused before it is asked anything.
    #[test]
    fn a_session_is_proved_only_on_the_workers_own_server() {
        let (port, server) = scripted(vec![(200, r#"{"id":"ses_new"}"#), (200, "[]")]);
        let stamp = format!("opencode:{port}:ses_new");
        let prove = |pane_pid| {
            Opencode.prove_fresh(
                &super::super::Target {
                    name: "t",
                    pane_pid,
                    stamp: Some(&stamp),
                },
                "ses_new",
                std::time::SystemTime::now(),
                Instant::now() + Duration::from_secs(60),
            )
        };
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let error = prove(other.id()).unwrap_err().to_string();
        other.kill().unwrap();
        other.wait().unwrap();
        assert!(error.contains("not by this worker's opencode"), "{error}");
        // This process listens, so as its own worker it passes.
        prove(std::process::id()).unwrap();
        server.join().unwrap();
    }

    /// An attempt's launch names its session only to `provision`, which
    /// creates it; it never resumes one.
    #[test]
    fn an_attempt_launch_names_the_session_to_create() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = crate::manager::tests::test_mcp_context(dir.path());
        context.topology = Some(crate::manager::TopologyMcpContext {
            resume: None,
            team: "Scoped".into(),
            agent: "t.coder".into(),
            endpoint: "localhost:1".into(),
            token: "t".into(),
            attempt: Some(crate::manager::TopologyAttempt {
                id: "attempt".into(),
                key: "key".into(),
                native_session: "ses_abc".into(),
            }),
        });
        let line = Opencode.launch_command(&Launch {
            base_command: "opencode",
            prompt_file: std::path::Path::new("/tmp/system.md"),
            substitutions: &[],
            shell_expr: "$(cat '/tmp/system.md')",
            context: &context,
        });
        assert!(
            line.starts_with("OMAR_OPENCODE_SESSION='ses_abc' "),
            "{line}"
        );
        assert!(!line.contains("--session"), "{line}");
        assert!(opencode_port(&line).is_some(), "{line}");
        assert!(Opencode.new_conversation_id().starts_with("ses_"));
        context.topology.as_mut().unwrap().attempt = None;
        let first = materialize_opencode_coordination_plugin(&context).unwrap();
        context.topology.as_mut().unwrap().agent = "t.other".into();
        let second = materialize_opencode_coordination_plugin(&context).unwrap();
        assert_ne!(
            first, second,
            "workers must not overwrite one another's event context"
        );
        context.topology.as_mut().unwrap().agent = "t.coder".into();
        let line = Opencode.launch_command(&Launch {
            base_command: "opencode",
            prompt_file: std::path::Path::new("/tmp/system.md"),
            substitutions: &[],
            shell_expr: "$(cat '/tmp/system.md')",
            context: &context,
        });
        assert!(!line.contains("OMAR_OPENCODE_SESSION"), "{line}");
    }

    #[test]
    fn opencode_wakes_with_synthetic_context_without_a_user_prompt() {
        use std::io::BufRead;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "POST /session/session_test/prompt_async HTTP/1.1\r\n");
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert!(body.get("noReply").is_none());
            assert_eq!(body["parts"][0]["synthetic"], true);
            assert_eq!(body["parts"][0]["text"], "agent event");
            stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        deliver_over_http(port, "session_test", "agent event").unwrap();
        server.join().unwrap();
    }

    #[test]
    fn forgetting_a_worker_deletes_its_session_over_http() {
        use std::io::BufRead;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stamp = format!("opencode:{}:ses_abc", listener.local_addr().unwrap().port());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            std::io::BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntrue")
                .unwrap();
            line
        });
        Opencode.forget(&super::super::Target {
            name: "pane",
            pane_pid: 0,
            stamp: Some(&stamp),
        });
        assert_eq!(
            server.join().unwrap(),
            "DELETE /session/ses_abc HTTP/1.1\r\n"
        );
    }

    #[test]
    fn a_launch_stamp_names_the_port_and_session_to_address() {
        assert_eq!(
            from_stamp("opencode:47455:ses_abc"),
            Some((47455, "ses_abc".to_string()))
        );
        // A stamp wins over backend sniffing, so a malformed one must not be
        // silently treated as a working channel.
        for bad in [
            "opencode:47455:",
            "opencode:notaport:ses_abc",
            "opencode:47455",
            "something-else:1:2",
            "",
        ] {
            assert_eq!(from_stamp(bad), None, "stamp {bad:?} must not parse");
        }
    }

    #[test]
    fn a_port_is_read_back_out_of_a_launch_command() {
        assert_eq!(opencode_port("opencode --port 47455"), Some(47455));
        assert_eq!(opencode_port("FOO=1 opencode --port=47455"), Some(47455));
        assert_eq!(opencode_port("opencode"), None);
        assert_eq!(opencode_port("opencode --port bogus"), None);
    }

    #[test]
    fn an_http_response_yields_its_status_and_body() {
        assert_eq!(
            split_response("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}"),
            Some((200, "{}".to_string()))
        );
        assert_eq!(
            split_response("HTTP/1.1 404 Not Found\r\n\r\n").map(|(status, _)| status),
            Some(404)
        );
    }
}
