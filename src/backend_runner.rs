//! Protocol-owned Cursor, Antigravity, and Codex exec sessions. The inbox is durable and never
//! writes to a terminal composer. One backend turn runs at a time.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeSet, VecDeque};
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

fn protocol_line(mut line: &str) -> &str {
    // Native Cursor ACP can prefix its initialize reply with ESC M ESC [ K
    // while clearing a startup spinner. Remove only leading terminal controls;
    // never search for a JSON object inside arbitrary diagnostic text.
    loop {
        line = line.trim_start();
        let bytes = line.as_bytes();
        if bytes.first() != Some(&0x1b) {
            return line;
        }
        match bytes.get(1) {
            Some(b'[') => {
                let end = bytes
                    .iter()
                    .skip(2)
                    .position(|b| !(0x20..=0x3f).contains(b));
                match end.map(|n| n + 2) {
                    Some(n) if (0x40..=0x7e).contains(&bytes[n]) => line = &line[n + 1..],
                    _ => return line,
                }
            }
            Some(b'M' | b'D' | b'E' | b'7' | b'8') => line = &line[2..],
            _ => return line,
        }
    }
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
            match serde_json::from_str(protocol_line(&line)) {
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
                crate::backend::remember_conversation(context, "codex", session)?;
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

/// What an invocation-scoped attempt's runner may start from: nothing an
/// earlier worker had. Its config, inbox and socket directory are new to its
/// launch; this refuses any sign they are not, the exec runner (which resumes
/// one conversation turn after turn), and an agy line that would resume one.
fn fresh_start(config: &Config, inbox_existed: bool) -> Result<()> {
    anyhow::ensure!(
        matches!(config.backend.as_str(), "cursor" | "agy"),
        "the {} runner resumes one conversation; an invocation-scoped attempt cannot use it",
        config.backend
    );
    anyhow::ensure!(
        !inbox_existed && config.initial_session.is_none(),
        "an invocation-scoped attempt was handed a conversation to resume"
    );
    if config.backend == "agy" {
        if let Some(word) = resume_flag(&config.command) {
            bail!("an invocation-scoped agent's command must not pass {word}");
        }
    }
    Ok(())
}

/// A word on an agy line that reopens a conversation. agy reads flags as Go
/// does: one dash or two, a value after `=` or not.
fn resume_flag(command: &str) -> Option<String> {
    shlex::split(command)
        .unwrap_or_default()
        .into_iter()
        .find(|word| {
            let name = word
                .strip_prefix("--")
                .or_else(|| word.strip_prefix('-'))
                .unwrap_or_default();
            matches!(
                name.split('=').next(),
                Some("c" | "continue" | "conversation")
            )
        })
}

/// Where a backend's CLI keeps one entry per conversation it opens:
/// cursor-agent 2026.09 adds `acp-sessions/<id>/meta.json` before
/// `session/new` answers, agy 1.5 `conversations/<id>.db` before its `init`.
fn conversation_store(backend: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(match backend {
        "cursor" => home.join(".cursor").join("acp-sessions"),
        _ => home
            .join(".gemini")
            .join("antigravity-cli")
            .join("conversations"),
    })
}

fn store_entry(backend: &str, store: &Path, id: &str) -> PathBuf {
    match backend {
        "cursor" => store.join(id).join("meta.json"),
        _ => store.join(format!("{id}.db")),
    }
}

/// What a directory holds by name; none when it does not exist.
fn names(dir: &Path) -> std::io::Result<BTreeSet<String>> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(error) => Err(error),
    }
}

/// A conversation store as it stood before an attempt's worker started, or
/// why it could not be read.
type Snapshot = std::result::Result<(PathBuf, BTreeSet<String>), String>;

fn snapshot(backend: &str) -> Snapshot {
    let store = conversation_store(backend).ok_or("there is no home directory")?;
    let before = names(&store).map_err(|error| format!("{}: {error}", store.display()))?;
    Ok((store, before))
}

/// Why the conversation `id` an attempt's backend opened cannot be shown new,
/// if it cannot: it is no plain id, its CLI's store held it before the worker
/// started or holds nothing for it now, or (agy) the CLI holds Knowledge
/// Items, whose summaries it hands every new conversation.
fn unproven(backend: &str, snapshot: &Snapshot, id: &str) -> Option<String> {
    let (store, before) = match snapshot {
        Ok(snapshot) => snapshot,
        Err(error) => return Some(format!("its store could not be read: {error}")),
    };
    if id.is_empty() || id.starts_with('.') || id.contains(['/', '\\']) {
        return Some(format!("{id:?} is not a conversation id"));
    }
    let held = |name: &String| {
        name == id
            || name
                .strip_prefix(id)
                .is_some_and(|rest| rest.starts_with('.'))
    };
    if before.iter().any(held) {
        return Some(format!(
            "{} already held it before the worker started",
            store.display()
        ));
    }
    let entry = store_entry(backend, store, id);
    if !entry.exists() {
        return Some(format!("{} does not exist", entry.display()));
    }
    if backend == "agy" {
        // agy keeps its own `knowledge.lock` there, items or none.
        let knowledge = store.with_file_name("knowledge");
        match names(&knowledge) {
            Ok(items) if items.iter().all(|name| name.ends_with(".lock")) => {}
            Ok(_) => {
                return Some(format!(
                "agy holds Knowledge Items in {}, whose summaries it hands every new conversation",
                knowledge.display()
            ))
            }
            Err(error) => return Some(format!("{}: {error}", knowledge.display())),
        }
    }
    None
}

/// Report the conversation an attempt's backend opened, and why it cannot be
/// shown new if it cannot, where `crate::backend::managed` reads it: written
/// once, before the socket exists, into the launch's own directory.
fn report(socket: &Path, id: &str, refused: Option<String>) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(crate::backend::managed::report_path(socket))?;
    let report = json!({"conversation_id": id, "pid": std::process::id(), "refused": refused});
    file.write_all(report.to_string().as_bytes())?;
    file.sync_all()?;
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
    // An invocation-scoped attempt opens a new conversation, checked by the
    // runtime from this runner's report before it hands the worker anything.
    let attempt = context
        .topology
        .as_ref()
        .is_some_and(|topology| topology.attempt.is_some());
    if attempt {
        fresh_start(&config, inbox_path.exists())?;
    }
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
    // Taken before the backend starts, as agy opens its conversation then.
    let store = attempt.then(|| snapshot(&config.backend));
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
            // An attempt never loads a session, so it never falls back from one.
            let mut method = if let Some(session) = previous.filter(|_| store.is_none()) { params["sessionId"] = session.into(); "session/load" } else { "session/new" };
            let result = match engine.rpc(method, params.clone()).await {
                Ok(result) => result,
                Err(error) if method == "session/load" => {
                    eprintln!("Could not resume Cursor session: {error:#}; restoring saved chat context.");
                    method = "session/new";
                    params.as_object_mut().unwrap().remove("sessionId");
                    engine.rpc(method, params.clone()).await?
                }
                Err(error) => return Err(error),
            };
            let session = if method == "session/load" { params["sessionId"].as_str() } else { result["sessionId"].as_str() }.context("ACP omitted sessionId")?;
            if let Some(store) = &store {
                report(&config.socket, session, unproven("cursor", store, session))?;
            }
            let mut state = queue.state.lock().await;
            crate::backend::remember_conversation(&context, "cursor", session)?;
            state.session = Some(session.to_owned());
            save(&queue.path, &state)?;
            Ok::<_, anyhow::Error>(())
        }).await.context("ACP startup timed out")??;
    }
    // agy reports the conversation it opened before it reads any input; an
    // attempt's is reported before anything can be sent, and is the only one
    // its turns may name.
    let mut opened = None;
    if let (Some(store), "agy") = (&store, config.backend.as_str()) {
        let engine = engine.as_mut().unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(45), engine.next())
            .await
            .context("agy did not report its conversation in time")??;
        anyhow::ensure!(
            event["event"] == "init",
            "agy said {event} before reporting its conversation"
        );
        let session = event["conversation_id"]
            .as_str()
            .context("agy's init named no conversation")?;
        report(&config.socket, session, unproven("agy", store, session))?;
        let mut state = queue.state.lock().await;
        state.session = Some(session.to_owned());
        save(&queue.path, &state)?;
        opened = Some(session.to_owned());
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
                    if let Some(opened) = &opened {
                        for id in [&event["conversation_id"], &event["result"]["conversation_id"]] {
                            if let Some(id) = id.as_str() {
                                anyhow::ensure!(id == opened, "agy moved to conversation {id} from {opened}");
                            }
                        }
                    }
                    if event["event"] == "init" {
                        if let Some(session) = event["conversation_id"].as_str() {
                            let mut state = queue.state.lock().await;
                            crate::backend::remember_conversation(&context, "agy", session)?;
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

#[cfg(test)]
mod protocol_tests {
    use super::*;

    #[test]
    fn cursor_spinner_prefix_does_not_discard_initialize_reply() {
        let wire =
            "\u{1b}M\u{1b}[K{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1}}";
        let reply: Value = serde_json::from_str(protocol_line(wire)).unwrap();
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["result"]["protocolVersion"], 1);
    }

    fn attempt_config(backend: &str, command: &str) -> Config {
        Config {
            backend: backend.into(),
            command: command.into(),
            context_file: "context.json".into(),
            prompt_file: "prompt.md".into(),
            socket: "agent.sock".into(),
            initial_session: None,
        }
    }

    /// An attempt starts from nothing: no inbox, no conversation to resume,
    /// not the exec runner, and no agy flag that reopens a conversation,
    /// spelled however Go's flag parser takes it.
    #[test]
    fn an_attempt_starts_from_no_conversation() {
        fresh_start(&attempt_config("cursor", "cursor-agent --yolo"), false).unwrap();
        fresh_start(
            &attempt_config("agy", "agy --dangerously-skip-permissions"),
            false,
        )
        .unwrap();
        let error =
            |config: &Config, existed| fresh_start(config, existed).unwrap_err().to_string();
        assert!(error(&attempt_config("codex", "codex"), false).contains("resumes one"));
        assert!(error(&attempt_config("cursor", "cursor-agent"), true).contains("to resume"));
        let mut handed = attempt_config("agy", "agy");
        handed.initial_session = Some("earlier".into());
        assert!(error(&handed, false).contains("to resume"));
        for flag in [
            "-c",
            "--continue",
            "-continue",
            "--conversation x",
            "--conversation=x",
            "-conversation=x",
            "--continue=true",
        ] {
            let config =
                attempt_config("agy", &format!("agy --dangerously-skip-permissions {flag}"));
            assert!(error(&config, false).contains("must not pass"), "{flag}");
        }
        for line in ["agy --model gemini-3 --mode auto", "agy --cwd x"] {
            assert_eq!(resume_flag(line), None, "{line}");
        }
    }

    /// A conversation is taken only when its CLI's store had no entry for it
    /// before the worker started and has one now; for agy, only while it
    /// holds no Knowledge Items.
    #[test]
    fn a_conversation_new_to_its_store_and_no_other() {
        let home = tempfile::tempdir().unwrap();
        let cursor = home.path().join("acp-sessions");
        let agy = home.path().join("antigravity-cli").join("conversations");
        std::fs::create_dir_all(cursor.join("old")).unwrap();
        std::fs::create_dir_all(&agy).unwrap();
        std::fs::write(agy.join("old.db"), "").unwrap();
        let before = |store: &Path| -> Snapshot { Ok((store.to_owned(), names(store).unwrap())) };
        let (cursor_before, agy_before) = (before(&cursor), before(&agy));
        for (backend, store) in [("cursor", &cursor), ("agy", &agy)] {
            let entry = store_entry(backend, store, "new");
            std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
            std::fs::write(entry, "").unwrap();
        }
        assert_eq!(unproven("cursor", &cursor_before, "new"), None);
        assert_eq!(unproven("agy", &agy_before, "new"), None);
        for (backend, snapshot) in [("cursor", &cursor_before), ("agy", &agy_before)] {
            let why =
                |id| unproven(backend, snapshot, id).unwrap_or_else(|| panic!("{backend} {id}"));
            assert!(why("old").contains("already held"), "{backend}");
            assert!(why("missing").contains("does not exist"), "{backend}");
            for id in ["", "../new", ".", "a/b"] {
                assert!(
                    why(id).contains("not a conversation id"),
                    "{backend} {id:?}"
                );
            }
        }
        // A store that does not exist holds nothing, so nothing is new to it.
        let none = before(&home.path().join("nowhere"));
        assert!(unproven("cursor", &none, "new")
            .unwrap()
            .contains("does not exist"));
        assert!(unproven("agy", &Err("unreadable".into()), "new")
            .unwrap()
            .contains("unreadable"));
        let knowledge = agy.with_file_name("knowledge");
        std::fs::create_dir_all(&knowledge).unwrap();
        std::fs::write(knowledge.join("knowledge.lock"), "").unwrap();
        assert_eq!(unproven("agy", &agy_before, "new"), None);
        std::fs::create_dir_all(knowledge.join("item")).unwrap();
        assert!(unproven("agy", &agy_before, "new")
            .unwrap()
            .contains("Knowledge Items"));
    }

    /// The report is written once, beside the socket, for this runner.
    #[test]
    fn the_report_is_written_once_for_this_runner() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("agent.sock");
        report(&socket, "new", None).unwrap();
        let path = crate::backend::managed::report_path(&socket);
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            written,
            json!({"conversation_id": "new", "pid": std::process::id(), "refused": null})
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(report(&socket, "other", None).is_err());
    }

    #[test]
    fn protocol_prefix_handling_preserves_payload_and_rejects_diagnostics() {
        let payload = r#"{"text":"literal \\u001b[K and { braces }"}"#;
        assert_eq!(protocol_line(payload), payload);
        for line in ["warning: {\"id\":1}", "\u{1b}[", "\u{1b}[31é"] {
            assert!(serde_json::from_str::<Value>(protocol_line(line)).is_err());
        }
    }
}
