//! Protocol-owned Cursor, Antigravity, and Codex exec sessions. The inbox is durable and never
//! writes to a terminal composer. One backend turn runs at a time.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, Notify};

#[derive(Serialize, Deserialize)]
pub struct Config {
    pub backend: String,
    pub command: String,
    pub context_file: PathBuf,
    pub prompt_file: PathBuf,
    pub socket: PathBuf,
    #[serde(default)]
    pub initial_session: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Message {
    id: String,
    text: String,
    operator: bool,
}
#[derive(Default, Serialize, Deserialize)]
struct Inbox {
    session: Option<String>,
    pending: VecDeque<Message>,
}
struct Queue {
    path: PathBuf,
    state: Mutex<Inbox>,
    changed: Notify,
}
fn save(path: &Path, state: &Inbox) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("pending");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(&serde_json::to_vec(state)?)?;
    file.sync_all()?;
    std::fs::rename(tmp, path)?;
    Ok(())
}
impl Queue {
    async fn push(&self, text: String, operator: bool) -> Result<String> {
        let mut state = self.state.lock().await;
        // A scheduled event can be retried while a long turn is still using it.
        if !operator {
            if let Some(existing) = state.pending.iter().find(|m| !m.operator && m.text == text) {
                return Ok(existing.id.clone());
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        state.pending.push_back(Message {
            id: id.clone(),
            text,
            operator,
        });
        if let Err(error) = save(&self.path, &state) {
            state.pending.pop_back();
            return Err(error);
        }
        self.changed.notify_one();
        Ok(id)
    }
}

struct Engine {
    child: tokio::process::Child,
    input: tokio::process::ChildStdin,
    output: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    next_id: u64,
    allow_permissions: bool,
}
impl Engine {
    async fn write(&mut self, value: Value) -> Result<()> {
        self.input
            .write_all(format!("{value}\n").as_bytes())
            .await?;
        self.input.flush().await?;
        Ok(())
    }
    async fn next(&mut self) -> Result<Value> {
        loop {
            let line = self
                .output
                .next_line()
                .await?
                .context("backend protocol exited")?;
            match serde_json::from_str(&line) {
                Ok(value) => return Ok(value),
                Err(_) => eprintln!("[backend] {line}"),
            }
        }
    }
    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.write(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        loop {
            let msg = self.next().await?;
            if msg["id"] == id && msg.get("method").is_none() {
                if let Some(error) = msg.get("error") {
                    bail!("{method}: {error}");
                }
                return Ok(msg["result"].clone());
            }
            if msg["method"] == "session/request_permission" {
                let chosen = msg["params"]["options"]
                    .as_array()
                    .and_then(|options| options.iter().find(|o| o["kind"] == "allow_once"))
                    .and_then(|o| o["optionId"].as_str());
                let outcome = match chosen.filter(|_| self.allow_permissions) {
                    Some(option) => json!({"outcome":"selected","optionId":option}),
                    None => {
                        eprintln!("Permission requested; use an explicitly authorized --yolo/--force launch to allow unattended tools.");
                        json!({"outcome":"cancelled"})
                    }
                };
                self.write(json!({"jsonrpc":"2.0","id":msg["id"],"result":{"outcome":outcome}}))
                    .await?;
            } else if msg.get("id").is_some() && msg.get("method").is_some() {
                self.write(json!({"jsonrpc":"2.0","id":msg["id"],"error":{"code":-32601,"message":"Unsupported client method"}})).await?;
            } else if let Some(text) = msg["params"]["update"]["content"]["text"].as_str() {
                print!("{text}");
                std::io::Write::flush(&mut std::io::stdout())?;
            }
        }
    }
}

async fn accept_messages(listener: tokio::net::UnixListener, queue: Arc<Queue>) -> Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let queue = queue.clone();
        tokio::spawn(async move {
            let mut stream = BufReader::new(stream);
            let reply: Result<Value> = async {
                let mut line = String::new();
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    (&mut stream).take(1_048_577).read_line(&mut line),
                )
                .await??;
                anyhow::ensure!(
                    line.len() <= 1_048_576 && line.ends_with('\n'),
                    "invalid or oversized message"
                );
                let request: Value = serde_json::from_str(&line)?;
                let text = request["text"].as_str().context("text is required")?;
                let id = queue.push(text.to_owned(), false).await?;
                Ok(json!({"accepted":id}))
            }
            .await;
            let response = match reply {
                Ok(value) => value,
                Err(error) => json!({"error":error.to_string()}),
            };
            let _ = stream
                .get_mut()
                .write_all(format!("{response}\n").as_bytes())
                .await;
        });
    }
}

async fn codex_turn(
    config: &Config,
    context: &crate::manager::McpLaunchContext,
    queue: &Queue,
    prompt: &str,
) -> Result<()> {
    let previous = queue.state.lock().await.session.clone();
    let resume = previous
        .map(|session| format!(" resume {}", crate::manager::shell_single_quote(&session)))
        .unwrap_or_default();
    let command = format!("exec env {} --json{resume} -", config.command);
    let mut child = tokio::process::Command::new("sh")
        .args(["-c", &command])
        .env("OMAR_MCP_CONTEXT_FILE", &config.context_file)
        .env("OMAR_EA_ID", context.ea_id.to_string())
        .env(
            "OMAR_AGENT_NAME",
            context.agent_name.as_deref().unwrap_or("ea"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child.stdin.take().unwrap();
    input.write_all(prompt.as_bytes()).await?;
    input.shutdown().await?;
    drop(input);
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut completed = false;
    while let Some(line) = output.next_line().await? {
        let event: Value = serde_json::from_str(&line).context("invalid Codex exec JSON")?;
        match event["type"].as_str() {
            Some("thread.started") => {
                let session = event["thread_id"]
                    .as_str()
                    .context("Codex omitted thread_id")?;
                let mut state = queue.state.lock().await;
                state.session = Some(session.into());
                save(&queue.path, &state)?;
            }
            Some("item.completed") if event["item"]["type"] == "agent_message" => {
                if let Some(text) = event["item"]["text"].as_str() {
                    println!("{text}");
                }
            }
            Some("turn.completed") => completed = true,
            Some("turn.failed" | "error") => bail!("Codex exec turn failed: {event}"),
            _ => {}
        }
    }
    anyhow::ensure!(
        child.wait().await?.success() && completed,
        "Codex exec exited without completing its turn"
    );
    Ok(())
}

pub async fn run(path: &Path) -> Result<()> {
    let config: Config = serde_json::from_slice(&std::fs::read(path)?)?;
    anyhow::ensure!(
        matches!(config.backend.as_str(), "cursor" | "agy" | "codex"),
        "unsupported protocol backend"
    );
    let context: crate::manager::McpLaunchContext =
        serde_json::from_slice(&std::fs::read(&config.context_file)?)?;
    let instructions = std::fs::read_to_string(&config.prompt_file)?;
    let inbox_path = path.with_extension("inbox.json");
    // An OS lock prevents two runners from replaying the same inbox/session.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("lock"))?;
    lock.try_lock()
        .context("protocol session already running")?;
    let inbox: Inbox = match std::fs::read(&inbox_path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("invalid protocol inbox; preserved")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Inbox {
            session: config.initial_session.clone(),
            ..Default::default()
        },
        Err(e) => return Err(e.into()),
    };
    let queue = Arc::new(Queue {
        path: inbox_path,
        state: Mutex::new(inbox),
        changed: Notify::new(),
    });
    let mut command = config.command.clone();
    match config.backend.as_str() {
        "cursor" => command.push_str(" acp"),
        "agy" => {
            if let Some(session) = &queue.state.lock().await.session {
                command.push_str(&format!(
                    " --conversation {}",
                    crate::manager::shell_single_quote(session)
                ));
            }
            command.push_str(" --input-format stream-json --output-format stream-json");
        }
        "codex" => {}
        _ => unreachable!(),
    }
    let mut engine = if config.backend == "codex" {
        None
    } else {
        Some({
            let mut child = tokio::process::Command::new("sh")
                .args(["-c", &format!("exec env {command}")])
                .env("OMAR_MCP_CONTEXT_FILE", &config.context_file)
                .env("OMAR_EA_ID", context.ea_id.to_string())
                .env(
                    "OMAR_AGENT_NAME",
                    context.agent_name.as_deref().unwrap_or("ea"),
                )
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .kill_on_drop(true)
                .spawn()?;
            Engine {
                input: child.stdin.take().unwrap(),
                output: BufReader::new(child.stdout.take().unwrap()).lines(),
                child,
                next_id: 0,
                allow_permissions: config
                    .command
                    .split_whitespace()
                    .any(|v| matches!(v, "--yolo" | "--force" | "-f")),
            }
        })
    };
    if config.backend == "cursor" {
        let engine = engine.as_mut().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(45), async {
            engine.rpc("initialize", json!({"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false},"clientInfo":{"name":"omar","version":env!("CARGO_PKG_VERSION")}})).await?;
            engine.rpc("authenticate", json!({"methodId":"cursor_login"})).await?;
            let previous = queue.state.lock().await.session.clone();
            let mut params = json!({"cwd":std::env::current_dir()?,"mcpServers":[{"name":"omar","command":std::env::current_exe()?,"args":["mcp-server","--context-file",config.context_file],"env":[]}]});
            let method = if let Some(session) = previous { params["sessionId"] = session.into(); "session/load" } else { "session/new" };
            let result = engine.rpc(method, params.clone()).await?;
            let session = if method == "session/load" { params["sessionId"].as_str() } else { result["sessionId"].as_str() }.context("ACP omitted sessionId")?;
            let mut state = queue.state.lock().await;
            state.session = Some(session.to_owned());
            save(&queue.path, &state)?;
            Ok::<_, anyhow::Error>(())
        }).await.context("ACP startup timed out")??;
    }
    if config.socket.exists() {
        std::fs::remove_file(&config.socket)?;
    }
    let listener = tokio::net::UnixListener::bind(&config.socket)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&config.socket, std::fs::Permissions::from_mode(0o600))?;
    let accepting = tokio::spawn(accept_messages(listener, queue.clone()));
    let operator_queue = queue.clone();
    let runtime = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if let Err(error) = runtime.block_on(operator_queue.push(line, true)) {
                eprintln!("Cannot save operator message: {error:#}");
            }
        }
    });
    println!(
        "OMAR {} protocol session ready. Type an operator message, or send one through OMAR.",
        config.backend
    );
    let result = async {
        loop {
            let notified = queue.changed.notified();
            let message = queue.state.lock().await.pending.front().cloned();
            let Some(message) = message else {
                let persistent = engine.is_some();
                tokio::select! {
                    _ = notified => {},
                    status = async { engine.as_mut().unwrap().child.wait().await }, if persistent => { bail!("backend exited while idle: {:?}", status?); }
                }
                continue;
            };
            let live = if context.topology.is_none() {
                crate::supervision::context_message(&crate::supervision::context(&context.omar_dir, context.ea_id, context.agent_name.as_deref().unwrap_or("ea"))?)
            } else { String::new() };
            let origin = if message.operator { "Operator message" } else { "OMAR coordination event (not operator input)" };
            // Queued events may contain an older ownership snapshot. Finish with
            // the state read immediately before this native turn.
            let prompt = format!("{instructions}\n\n[{origin}; message {}]\n{}\n\n{live}", message.id, message.text);
            if config.backend == "codex" {
                codex_turn(&config, &context, &queue, &prompt).await?;
            } else if config.backend == "cursor" {
                let engine = engine.as_mut().unwrap();
                let session = queue.state.lock().await.session.clone().context("missing ACP session")?;
                let result = engine.rpc("session/prompt", json!({"sessionId":session,"prompt":[{"type":"text","text":prompt}]})).await?;
                anyhow::ensure!(result["stopReason"] != "cancelled", "Cursor cancelled the turn; message remains pending");
            } else {
                let engine = engine.as_mut().unwrap();
                engine.write(json!({"event":"user","message":{"content":prompt}})).await?;
                loop {
                    let event = engine.next().await?;
                    if event["event"] == "init" {
                        if let Some(session) = event["conversation_id"].as_str() {
                            let mut state = queue.state.lock().await;
                            state.session = Some(session.to_owned()); save(&queue.path, &state)?;
                        }
                    }
                    if let Some(text) = event["step_update"]["text_delta"].as_str() { print!("{text}"); std::io::Write::flush(&mut std::io::stdout())?; }
                    if event["event"] == "result" {
                        anyhow::ensure!(event["result"]["status"] == "SUCCESS", "Antigravity turn failed: {}", event["result"]);
                        break;
                    }
                }
            }
            println!("\n[OMAR turn complete: {}]", message.id);
            let mut state = queue.state.lock().await;
            state.pending.pop_front();
            save(&queue.path, &state)?;
        }
    }.await;
    accepting.abort();
    let _ = std::fs::remove_file(&config.socket);
    result
}
