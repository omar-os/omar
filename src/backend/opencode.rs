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
use std::time::Duration;

pub struct Opencode;

impl Backend for Opencode {
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
    /// not be ours, from answering.
    fn connection(&self, program: &str) -> Connection {
        let args = ["auth", "list", "--format", "json", "--standalone"];
        let Some(output) = connection::run(program, &args) else {
            return Connection::unknown("`opencode auth list` did not answer");
        };
        if let Some(connection) = auth_list(&output.stdout) {
            return connection;
        }
        // opencode 1 has no JSON, only its table.
        match connection::run(program, &["auth", "list"]) {
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
    fn logout(&self) -> Option<Login> {
        Some(Login {
            args: &["auth", "logout"],
            note: None,
        })
    }
    fn conversation_id(&self, target: &super::Target<'_>) -> Option<String> {
        from_stamp(target.stamp?).map(|(_, session)| session)
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
        match opencode_config_env(launch.context) {
            Some(config) => format!(
                "OPENCODE_CONFIG_CONTENT={} {}",
                shell_single_quote(&config),
                base_command
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
    if context.topology.is_some() {
        return None;
    }
    let exe = omar_server_exe()?;
    let context_file = materialize_mcp_context_file(context)?;
    let path = mcp_ea_dir(context)?
        .join(actor_file(context.agent_name.as_deref(), "coordination").replace(".json", ".mjs"));
    let body = format!(
        "const exe = {};\nconst contextFile = {};\n{}",
        serde_json::to_string(&exe).ok()?,
        serde_json::to_string(&context_file).ok()?,
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
        "permission": {
            "doom_loop": "deny"
        }
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

/// Loopback HTTP is either immediate or wedged; nothing in between.
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
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))
        .with_context(|| format!("connect to 127.0.0.1:{}", port))?;
    stream.set_read_timeout(Some(HTTP_TIMEOUT))?;
    stream.set_write_timeout(Some(HTTP_TIMEOUT))?;

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
    stream.write_all(request.as_bytes())?;
    stream.flush()?;

    let mut response = Vec::new();
    std::io::Read::read_to_end(&mut stream, &mut response)?;
    let response = String::from_utf8_lossy(&response).into_owned();
    let (status, body) = split_response(&response).context("parse opencode response")?;
    Ok((status, body))
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
