//! Independently owned runtime sessions. Clients discover a session, verify its
//! incarnation over a private socket, and ask its daemon to perform operations.
use crate::{config::Config, ea, serve::Serve, Cli, Commands};
use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::SocketAddr;
use std::os::unix::{
    fs::PermissionsExt,
    net::{UnixListener, UnixStream},
    process::CommandExt,
};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use uuid::Uuid;

const PROTOCOL: u32 = 1;
const MAX_FRAME: u64 = 8 * 1024 * 1024;

#[derive(Args, Debug, Clone)]
pub struct UpOptions {
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub workdir: Option<PathBuf>,
    #[arg(long, default_value = "127.0.0.1:0")]
    pub address: SocketAddr,
    #[arg(long)]
    pub no_ea: bool,
    #[arg(long, default_value_t = 60)]
    pub startup_timeout: u64,
    /// Open Mission Control once the runtime is ready
    #[arg(long)]
    pub web: bool,
    /// Attach the terminal dashboard once the runtime is ready
    #[arg(long)]
    pub tui: bool,
}
impl Default for UpOptions {
    fn default() -> Self {
        Self {
            name: None,
            workdir: None,
            address: "127.0.0.1:0".parse().unwrap(),
            no_ea: false,
            startup_timeout: 60,
            web: false,
            tui: false,
        }
    }
}
#[derive(Args, Debug)]
pub struct StartOptions {
    pub program: PathBuf,
    #[arg(long = "input")]
    pub inputs: Vec<String>,
    #[arg(long)]
    pub replace: bool,
    #[arg(long, default_value_t = 300)]
    pub timeout_seconds: u64,
    #[arg(long)]
    pub fast: bool,
    /// Wait for completion; disconnecting this client never stops the run.
    #[arg(long)]
    pub wait: bool,
}
#[derive(Subcommand, Debug)]
pub enum EaAction {
    List,
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        agent: Option<String>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub protocol: u32,
    pub incarnation: String,
    pub state: String,
    pub pid: u32,
    pub url: String,
    pub socket: PathBuf,
    pub directory: PathBuf,
    pub executable: PathBuf,
    pub source_executable: PathBuf,
    pub version: String,
    pub build_id: String,
    pub tmux_server: String,
}
#[derive(Serialize, Deserialize)]
struct Launch {
    session: Session,
    address: SocketAddr,
    no_ea: bool,
    /// Foreground startup prints the session record in the requested mode.
    #[serde(default)]
    json: bool,
}
#[derive(Serialize, Deserialize)]
struct Request {
    protocol: u32,
    id: String,
    incarnation: String,
    operation: Value,
}

pub fn home_root() -> PathBuf {
    std::env::var_os("OMAR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".omar")
        })
}
pub fn state_root() -> PathBuf {
    std::env::var_os("OMAR_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(home_root)
}
pub fn is_managed() -> bool {
    std::env::var_os("OMAR_SESSION_ID").is_some() && std::env::var_os("OMAR_STATE_DIR").is_some()
}
fn registry() -> PathBuf {
    home_root().join("registry")
}
fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    crate::topology::write_json_atomic(path, value)
}
fn publish(session: &Session) -> Result<()> {
    write_json(&registry().join(format!("{}.json", session.id)), session)
}
fn validate_name(name: &str) -> Result<()> {
    anyhow::ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "session names use 1–64 letters, digits, '-' or '_'"
    );
    Ok(())
}
struct RegistryLock(File);
impl RegistryLock {
    fn acquire() -> Result<Self> {
        private_dir(&registry())?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(registry().join(".lock"))?;
        anyhow::ensure!(
            unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&file), libc::LOCK_EX) } == 0,
            "cannot lock session registry"
        );
        Ok(Self(file))
    }
}
impl Drop for RegistryLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(std::os::fd::AsRawFd::as_raw_fd(&self.0), libc::LOCK_UN);
        }
    }
}
pub fn discover() -> Result<Vec<Session>> {
    if !registry().exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for entry in fs::read_dir(registry())? {
        let path = entry?.path();
        if path.extension().is_none_or(|s| s != "json") {
            continue;
        }
        let mut session: Session = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("invalid session record {}", path.display()))?;
        if matches!(session.state.as_str(), "ready" | "stopping" | "starting") {
            match rpc(&session, json!({"op":"hello"}), Duration::from_millis(500)) {
                Ok(value) => session = serde_json::from_value(value)?,
                Err(_) => {
                    session.state = if session.pid > 0 && crate::process::pid_alive(session.pid) {
                        "unreachable"
                    } else {
                        "stale"
                    }
                    .into()
                }
            }
        }
        result.push(session);
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}
pub fn resolve(selector: &str) -> Result<Session> {
    let sessions = discover()?;
    if let Some(s) = sessions.iter().find(|s| s.id == selector) {
        return Ok(s.clone());
    }
    let named: Vec<_> = sessions
        .into_iter()
        .filter(|s| s.name == selector)
        .collect();
    let live: Vec<_> = named
        .iter()
        .filter(|s| !matches!(s.state.as_str(), "stopped" | "failed" | "stale"))
        .cloned()
        .collect();
    let matches = if live.is_empty() { named } else { live };
    anyhow::ensure!(
        matches.len() == 1,
        "session '{selector}' is missing or ambiguous; use omar ls and select an id"
    );
    Ok(matches[0].clone())
}
fn target(cli: &Cli) -> Result<Session> {
    let selector = cli
        .session
        .clone()
        .or_else(|| std::env::var("OMAR_SESSION_ID").ok())
        .context("select a runtime with --session <id-or-name>; use omar ls or omar up")?;
    resolve(&selector)
}
fn ea_selector(cli: &Cli) -> String {
    cli.ea
        .clone()
        .or_else(|| {
            if cli.session.is_none() {
                std::env::var("OMAR_EA_ID").ok()
            } else {
                None
            }
        })
        .unwrap_or_else(|| "0".into())
}
fn read_frame(stream: impl Read) -> Result<Value> {
    let mut raw = String::new();
    BufReader::new(stream.take(MAX_FRAME + 1)).read_line(&mut raw)?;
    anyhow::ensure!(
        raw.len() as u64 <= MAX_FRAME && raw.ends_with('\n'),
        "invalid or oversized control frame"
    );
    Ok(serde_json::from_str(&raw)?)
}
pub fn rpc(session: &Session, operation: Value, timeout: Duration) -> Result<Value> {
    anyhow::ensure!(
        session.protocol == PROTOCOL,
        "unsupported session protocol {}; use that session's CLI build",
        session.protocol
    );
    let mut stream = UnixStream::connect(&session.socket)
        .with_context(|| format!("session '{}' is not reachable", session.name))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let request = Request {
        protocol: PROTOCOL,
        id: session.id.clone(),
        incarnation: session.incarnation.clone(),
        operation,
    };
    writeln!(stream, "{}", serde_json::to_string(&request)?)?;
    let response = read_frame(stream)?;
    anyhow::ensure!(
        response["id"] == session.id && response["incarnation"] == session.incarnation,
        "runtime identity mismatch"
    );
    if let Some(error) = response.get("error").and_then(Value::as_str) {
        bail!("{error}");
    }
    Ok(response["result"].clone())
}

/// Run before Tokio starts threads. A fresh launch never inherits parent routing.
pub fn prepare_process(cli: &Cli) -> Result<()> {
    if let Some(Commands::SessionDaemon { directory }) = &cli.command {
        let launch: Launch = serde_json::from_slice(&fs::read(directory.join("launch.json"))?)?;
        std::env::set_var("OMAR_STATE_DIR", directory);
        std::env::set_var("OMAR_SESSION_ID", &launch.session.id);
        std::env::set_var("OMAR_TMUX_SERVER", &launch.session.tmux_server);
        std::env::set_var("OMARC_BIN", directory.join("bin/omarc"));
        std::env::remove_var("TMUX");
    }
    if let Some(session) = dashboard_target(cli)? {
        prepare_dashboard(cli, session)?;
    }
    Ok(())
}

fn dashboard_target(cli: &Cli) -> Result<Option<Session>> {
    match &cli.command {
        Some(Commands::Attach { tui: true, .. }) => target(cli).map(Some),
        Some(Commands::Manager {
            action: Some(crate::ManagerAction::Orchestrate),
        }) => target(cli).map(Some),
        _ => Ok(None),
    }
}

/// The terminal dashboard is a client of one session: it reads that session's
/// state directory, drives its tmux server, and runs inside that server so
/// popups and agent attachment keep working. The daemon stays the owner of
/// the scheduler, assistant launches, and every workload.
fn prepare_dashboard(cli: &Cli, session: Session) -> Result<()> {
    anyhow::ensure!(
        session.state == "ready",
        "session '{}' is {}; attach needs a ready runtime",
        session.name,
        session.state
    );
    // Same precedence as every other session command: explicit `--ea`, an
    // inherited EA only with inherited routing, else EA 0. A dashboard that
    // is already running keeps its EA unless one is named.
    let explicit =
        cli.ea.is_some() || (cli.session.is_none() && std::env::var_os("OMAR_EA_ID").is_some());
    let ea = ea::resolve_ea_selector(&session.directory, Some(&ea_selector(cli)))?;
    std::env::set_var("OMAR_STATE_DIR", &session.directory);
    std::env::set_var("OMAR_SESSION_ID", &session.id);
    std::env::set_var("OMAR_TMUX_SERVER", &session.tmux_server);
    std::env::set_var("OMARC_BIN", session.directory.join("bin/omarc"));
    let running = dashboard_running(&session);
    if explicit || !running {
        ea::save_active_ea(&session.directory, ea.id)?;
    }
    if inside_tmux_server(&session.tmux_server) {
        if current_tmux_session(&session.tmux_server).as_deref() == Some(crate::DASHBOARD_SESSION) {
            return Ok(());
        }
        // An agent pane on this server switches to the dedicated dashboard
        // session rather than running a second dashboard inside the workload.
        return relaunch_dashboard(&session, ea.id, explicit, running, true);
    }
    relaunch_dashboard(&session, ea.id, explicit, running, false)
}

fn current_tmux_session(server: &str) -> Option<String> {
    let mut command = Command::new("tmux");
    command.args(["-L", server, "display-message", "-p"]);
    if let Ok(pane) = std::env::var("TMUX_PANE") {
        command.args(["-t", &pane]);
    }
    let output = command.arg("#{session_name}").output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn dashboard_running(session: &Session) -> bool {
    Command::new("tmux")
        .args(["-L", &session.tmux_server, "has-session", "-t"])
        .arg(format!("={}", crate::DASHBOARD_SESSION))
        .env_remove("TMUX")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn inside_tmux_server(server: &str) -> bool {
    std::env::var("TMUX")
        .ok()
        .and_then(|value| {
            let socket = value.split(',').next()?.to_string();
            Path::new(&socket).file_name().map(|name| name == server)
        })
        .unwrap_or(false)
}

/// Relaunch inside the session's own tmux server.
/// A dashboard that is already running there is joined (with a launch handoff
/// when an EA was named); otherwise a fresh one is started. From a pane on
/// that server (`nested`) this client switches to the dashboard session.
/// Never returns on success: the process becomes the tmux client.
fn relaunch_dashboard(
    session: &Session,
    ea: ea::EaId,
    explicit: bool,
    running: bool,
    nested: bool,
) -> Result<()> {
    let config = Config::load(session.directory.join("config.toml").to_str())?;
    let exe = std::env::current_exe()?;
    let dashboard = format!("={}", crate::DASHBOARD_SESSION);
    let tmux = || {
        let mut command = Command::new("tmux");
        command
            .args(["-L", &session.tmux_server, "-2"])
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        command
    };
    let launch = |detached: bool| {
        let mut command = tmux();
        command.arg("new-session");
        if detached {
            command.arg("-d");
        }
        command
            .args(["-s", crate::DASHBOARD_SESSION, "-c"])
            .arg(&config.agent.default_workdir)
            .arg(&exe)
            .args([
                "attach",
                "-s",
                &session.id,
                "--tui",
                "--ea",
                &ea.to_string(),
            ]);
        command
    };
    if running && explicit {
        ea::save_dashboard_launch_handoff(
            &session.directory,
            &ea::DashboardLaunchHandoff {
                active_ea: ea,
                default_command: config.agent.default_command.clone(),
                default_workdir: config.agent.default_workdir.clone(),
                restart_manager: false,
            },
        )?;
    }
    if nested {
        if !running {
            anyhow::ensure!(
                launch(true).status()?.success(),
                "failed to start the dashboard session"
            );
        }
        // Switch this client to the dashboard; a pane without a client of its
        // own (a script, say) attaches nested instead.
        let switched = Command::new("tmux")
            .args([
                "-L",
                &session.tmux_server,
                "switch-client",
                "-t",
                &dashboard,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if switched
            || tmux()
                .args(["attach-session", "-t", &dashboard])
                .status()?
                .success()
        {
            std::process::exit(0);
        }
        bail!("could not switch to the dashboard session");
    }
    if running {
        let status = tmux().args(["attach-session", "-t", &dashboard]).status()?;
        if status.success() {
            std::process::exit(0);
        }
        let _ = tmux().args(["kill-session", "-t", &dashboard]).status();
    }
    let error = launch(false).exec();
    bail!("failed to launch tmux: {error}")
}

/// Whether `up` was asked to launch an assistant. An attached dashboard never
/// starts one in a session created with `--no-ea`.
pub fn launched_with_assistant(session: &Session) -> bool {
    fs::read(session.directory.join("launch.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Launch>(&bytes).ok())
        .is_some_and(|launch| !launch.no_ea)
}

/// The terminal dashboard, attached to the session `prepare_process` selected.
async fn attach_dashboard() -> Result<()> {
    let session = resolve(&std::env::var("OMAR_SESSION_ID")?)?;
    let config = Config::load(session.directory.join("config.toml").to_str())?;
    crate::run_dashboard(config, session).await
}
fn executable_on_path(path: PathBuf) -> Result<PathBuf> {
    if path.components().count() > 1 {
        return Ok(fs::canonicalize(path)?);
    }
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let candidate = dir.join(&path);
        if candidate.is_file() {
            return Ok(fs::canonicalize(candidate)?);
        }
    }
    bail!("executable {} was not found", path.display())
}
fn pin_executable(source: &Path, dest: &Path) -> Result<String> {
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(dest)?;
    // The pinned copy, not its mutable original path, is the build we execute.
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    fs::set_permissions(dest, fs::Permissions::from_mode(0o500))?;
    let mut hash = Sha256::new();
    std::io::copy(&mut File::open(dest)?, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}
fn launch(cli: &Cli, options: UpOptions, foreground: bool) -> Result<Session> {
    anyhow::ensure!(
        cli.session.is_none(),
        "up creates a new session; --session selects an existing one"
    );
    anyhow::ensure!(
        options.address.ip().is_loopback(),
        "session APIs bind only to loopback"
    );
    anyhow::ensure!(
        cli.ea.is_none(),
        "a new session starts with EA 0; create more with omar --session <session> ea create"
    );
    let _lock = RegistryLock::acquire()?;
    let id = format!("s-{}", Uuid::new_v4().simple());
    let name = options.name.unwrap_or_else(|| id.clone());
    validate_name(&name)?;
    anyhow::ensure!(
        !discover()?
            .iter()
            .any(|s| s.name == name && !matches!(s.state.as_str(), "stopped" | "stale" | "failed")),
        "session name '{name}' is already in use"
    );
    let directory = home_root().join("sessions").join(&id);
    private_dir(&directory)?;
    private_dir(&directory.join("bin"))?;
    private_dir(&directory.join("logs"))?;
    let source_executable = fs::canonicalize(std::env::current_exe()?)?;
    let executable = directory.join("bin/omar");
    let build_id = pin_executable(&source_executable, &executable)?;
    // A newly built runtime launched by an agent must not inherit the
    // supervising runtime's pinned compiler. Explicit external overrides stay
    // supported; invoking the parent's pinned executable uses its sibling.
    let inherited_compiler = std::env::var_os("OMAR_STATE_DIR")
        .map(PathBuf::from)
        .is_some_and(|parent| {
            source_executable != parent.join("bin/omar")
                && std::env::var_os("OMARC_BIN").map(PathBuf::from)
                    == Some(parent.join("bin/omarc"))
        });
    let compiler = if inherited_compiler {
        crate::topology::resolve_build_omarc()
    } else {
        crate::topology::resolve_omarc()
    };
    if let Ok(compiler) = executable_on_path(compiler) {
        pin_executable(&compiler, &directory.join("bin/omarc"))?;
    }
    let config_path = cli
        .config
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| home_root().join("config.toml"));
    let mut config = if cli.config.is_some() || config_path.exists() {
        Config::load(config_path.to_str())?
    } else {
        Config::default()
    };
    if let Some(agent) = &cli.agent {
        config.agent.default_command = crate::backend::resolve(agent)
            .map_err(anyhow::Error::msg)?
            .default_command()
            .into();
    }
    let workdir = fs::canonicalize(options.workdir.unwrap_or(std::env::current_dir()?))?;
    config.agent.default_workdir = workdir.to_string_lossy().into_owned();
    config.metrics.spawn_metrics_enabled |= cli.spawn_metrics;
    fs::write(directory.join("config.toml"), toml::to_string(&config)?)?;
    // Short private socket paths also work on macOS's 104-byte Unix path limit.
    let socket = crate::paths::private_temp_dir()?.join(format!("{}.sock", &id[..18]));
    let mut session = Session {
        id: id.clone(),
        name,
        protocol: PROTOCOL,
        incarnation: Uuid::new_v4().to_string(),
        state: "starting".into(),
        pid: 0,
        url: String::new(),
        socket,
        directory: directory.clone(),
        executable,
        source_executable,
        version: env!("CARGO_PKG_VERSION").into(),
        build_id,
        tmux_server: format!("omar-{id}"),
    };
    write_json(
        &directory.join("launch.json"),
        &Launch {
            session: session.clone(),
            address: options.address,
            no_ea: options.no_ea,
            json: foreground && cli.json,
        },
    )?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("logs/runtime.log"))?;
    let mut command = Command::new(&session.executable);
    command
        .arg("session-daemon")
        .arg(&directory)
        .current_dir(&workdir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("OMAR_")
            || key == "TMUX"
            || key == "TMUX_PANE"
            || key == "OMARC_BIN"
        {
            command.env_remove(key);
        }
    }
    command.env("OMAR_HOME", home_root());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    if foreground {
        session.pid = std::process::id();
        publish(&session)?;
        drop(_lock);
        // Foreground mode keeps its terminal; only background starts detach.
        let mut foreground = Command::new(&session.executable);
        foreground
            .arg("session-daemon")
            .arg(&directory)
            .current_dir(&workdir);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("OMAR_")
                || key == "TMUX"
                || key == "TMUX_PANE"
                || key == "OMARC_BIN"
            {
                foreground.env_remove(key);
            }
        }
        foreground.env("OMAR_HOME", home_root());
        return Err(foreground.exec().into());
    }
    let mut child = command.spawn()?;
    session.pid = child.id();
    publish(&session)?;
    drop(_lock);
    let deadline = Instant::now() + Duration::from_secs(options.startup_timeout);
    loop {
        if let Some(status) = child.try_wait()? {
            session.state = "failed".into();
            publish(&session)?;
            bail!(
                "session startup failed ({status}); see {}",
                directory.join("logs/runtime.log").display()
            );
        }
        if let Ok(value) = rpc(&session, json!({"op":"hello"}), Duration::from_millis(300)) {
            let ready: Session = serde_json::from_value(value)?;
            if ready.state == "ready" {
                return Ok(ready);
            }
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            // The tmux server is unique to this attempt, never an inherited one.
            let _ = Command::new("tmux")
                .args(["-L", &session.tmux_server, "kill-server"])
                .status();
            session.state = "failed".into();
            publish(&session)?;
            bail!(
                "startup timed out; see {}",
                directory.join("logs/runtime.log").display()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn validate_exec(cli: &Cli) -> Result<()> {
    anyhow::ensure!(is_managed(), "internal command requires a session runtime");
    anyhow::ensure!(
        cli.config.is_none() && cli.agent.is_none() && !cli.spawn_metrics && cli.session.is_none(),
        "runtime configuration cannot be changed by a client invocation"
    );
    anyhow::ensure!(
        matches!(
            cli.command,
            Some(
                Commands::Spawn { .. }
                    | Commands::List { .. }
                    | Commands::Kill { .. }
                    | Commands::Workspace { .. }
                    | Commands::Event { .. }
            )
        ),
        "command cannot be executed as a runtime operation"
    );
    Ok(())
}
fn overview(server: &Serve, session: &Session) -> Result<Value> {
    let config = Config::load(session.directory.join("config.toml").to_str())?;
    let agents = crate::tmux::TmuxClient::new("")
        .list_all_sessions()
        .unwrap_or_default()
        .into_iter()
        .filter(|agent| agent.name.starts_with(&config.dashboard.session_prefix))
        .map(|agent| {
            let owner = ea::load_registry(&session.directory)
                .into_iter()
                .find(|ea| {
                    agent.name == ea::ea_manager_session(ea.id, &config.dashboard.session_prefix)
                        || agent
                            .name
                            .starts_with(&ea::ea_prefix(ea.id, &config.dashboard.session_prefix))
                })
                .map(|ea| ea.id);
            json!({"name":agent.name,"ea_id":owner})
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"session":session, "eas":ea::load_registry(&session.directory), "agents":agents, "runs":server.session_runs(None)}),
    )
}
/// Shutdown is admitted ahead of everything else: the shared stopping state
/// is published before any work that could wait behind an in-flight
/// admission, and a forced shutdown never does that work at all.
fn begin_shutdown(
    server: &Arc<Serve>,
    shared: &Mutex<Session>,
    record: &mut Session,
    force: &AtomicBool,
    forced: bool,
) -> Result<Value> {
    force.fetch_or(forced, Ordering::SeqCst);
    record.state = "stopping".into();
    publish(record)?;
    *shared.lock().unwrap() = record.clone();
    server.session_shutdown_now();
    if !forced {
        request_stops(server);
    }
    Ok(json!({"status":"stopping"}))
}

/// Graceful stop requests may wait behind an in-flight admission, so they run
/// off the request (acknowledged at once) and off the daemon loop.
fn request_stops(server: &Arc<Serve>) {
    let server = server.clone();
    std::thread::spawn(move || {
        if let Err(error) = server.session_stopping() {
            eprintln!("graceful stop requests failed: {error:#}");
        }
    });
}

/// A signal asks for a graceful shutdown.
fn request_graceful(server: &Arc<Serve>, session: &Mutex<Session>) -> Result<()> {
    let mut record = session.lock().unwrap();
    record.state = "stopping".into();
    publish(&record)?;
    server.session_shutdown_now();
    request_stops(server);
    Ok(())
}

fn handle_operation(server: &Serve, session: &mut Session, operation: Value) -> Result<Value> {
    let op = operation["op"].as_str().context("operation is required")?;
    if op == "hello" {
        return Ok(json!(session));
    }
    if op == "overview" {
        return overview(server, session);
    }
    anyhow::ensure!(
        session.state == "ready",
        "runtime is stopping; no new operations admitted"
    );
    let ea = operation["ea"].as_str().unwrap_or("0");
    let target = ea::resolve_ea_selector(&session.directory, Some(ea))?;
    match op {
        "eas" => Ok(json!(ea::load_registry(&session.directory))),
        "create_ea" => {
            let name = operation["name"].as_str().context("EA name is required")?;
            let command = operation["agent"]
                .as_str()
                .map(|name| {
                    crate::backend::resolve(name)
                        .map(|b| b.default_command().to_string())
                        .map_err(anyhow::Error::msg)
                })
                .transpose()?;
            let id = server.session_create_ea(name, command.as_deref())?;
            Ok(json!({"id":id,"name":name}))
        }
        "manager_start" => server.session_start_manager(target.id),
        "start" => server.session_start(target.id, operation["request"].clone()),
        "runs" => Ok(json!(server.session_runs(
            if operation["all_eas"] == true {
                None
            } else {
                Some(target.id)
            }
        ))),
        "status" => {
            let selector = operation["run"].as_str().context("run id required")?;
            let runs = server.session_runs(Some(target.id));
            let matches = runs
                .into_iter()
                .filter(|r| r["run_id"] == selector || r["team"] == selector)
                .collect::<Vec<_>>();
            anyhow::ensure!(
                matches.len() == 1,
                "run is missing or ambiguous; use omar runs and select a run id"
            );
            Ok(matches[0].clone())
        }
        "stop" => server.session_stop(
            target.id,
            operation["run"].as_str().context("run id required")?,
        ),
        "exec" => {
            let args: Vec<String> = serde_json::from_value(operation["args"].clone())?;
            let parsed =
                Cli::try_parse_from(std::iter::once("omar".to_string()).chain(args.clone()))?;
            validate_exec(&parsed)?;
            if let Some(Commands::Kill { name }) = &parsed.command {
                // A finished run's agents are ordinary leftovers; only a live one is daemon-owned.
                anyhow::ensure!(
                    !server.session_runs(Some(target.id)).iter().any(|r| {
                        (r["team"] == name.as_str() || r["run_id"] == name.as_str())
                            && matches!(
                                r["status"].as_str(),
                                Some("starting" | "running" | "stopping")
                            )
                    }),
                    "use stop for a daemon-owned topology, or down --force for the whole session"
                );
            }
            let output = Command::new(&session.executable)
                .arg("session-exec")
                .arg("--")
                .arg("--ea")
                .arg(target.id.to_string())
                .args(args)
                .current_dir(operation["cwd"].as_str().unwrap_or("."))
                .stdin(Stdio::null())
                .output()?;
            anyhow::ensure!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(
                json!({"stdout":String::from_utf8_lossy(&output.stdout), "stderr":String::from_utf8_lossy(&output.stderr)}),
            )
        }
        _ => bail!("unknown operation '{op}'"),
    }
}
struct RuntimeCleanup(Option<Session>);
impl RuntimeCleanup {
    fn cleanup(&mut self) {
        let Some(session) = self.0.take() else {
            return;
        };
        // Code reactions/compiler helpers are descendants of the daemon, while
        // agent panes may belong to a daemonized tmux server. Capture both.
        let children = crate::process::process_tree(std::process::id());
        let client = crate::tmux::TmuxClient::new("");
        for agent in client.list_all_sessions().unwrap_or_default() {
            let _ = client.kill_session_tree(&agent.name);
        }
        let _ = crate::tmux::tmux_command().arg("kill-server").output();
        crate::process::signal_tree(&children, "-TERM");
        std::thread::sleep(Duration::from_millis(500));
        crate::process::signal_tree(&children, "-KILL");
        // A handler that was mid-admission may have spawned after the snapshot.
        let late = crate::process::process_tree(std::process::id());
        crate::process::signal_tree(&late, "-KILL");
        let _ = fs::remove_file(&session.socket);
    }
}
impl Drop for RuntimeCleanup {
    fn drop(&mut self) {
        self.cleanup();
    }
}
async fn daemon(directory: &Path) -> Result<()> {
    let launch: Launch = serde_json::from_slice(&fs::read(directory.join("launch.json"))?)?;
    let mut session = launch.session;
    session.pid = std::process::id();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let saved = fs::read(registry().join(format!("{}.json", session.id)))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Session>(&bytes).ok());
        if saved.is_some_and(|s| s.pid == session.pid && s.incarnation == session.incarnation) {
            break;
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "launcher did not publish process ownership"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut cleanup = RuntimeCleanup(Some(session.clone()));
    let config = Config::load(directory.join("config.toml").to_str())?;
    crate::metrics::configure(config.metrics.spawn_metrics_enabled);
    ea::ensure_default_ea(directory)?;
    let server = Arc::new(Serve::start(launch.address, &config, directory, 0)?);
    // Slack and computer integrations belong to the runtime, never to a client.
    let mut bridges: Vec<std::process::Child> =
        [crate::spawn_slack_bridge(), crate::spawn_computer_bridge()]
            .into_iter()
            .flatten()
            .collect();
    match server.attach_ea(&config, directory, 0, false, !launch.no_ea)? {
        crate::serve::AttachEa::Attached(session) => eprintln!("Executive assistant running in {session}"),
        crate::serve::AttachEa::AlreadyRunningWithoutServe(session) => eprintln!(
            "Executive assistant {session} is running without this runtime's context; restart it with manager start"
        ),
        crate::serve::AttachEa::LaunchedWithoutServe { session, reason } => {
            eprintln!("Executive assistant {session} launched without serve context: {reason}")
        }
    }
    // A session owns exactly one scheduled-event delivery loop, even with no clients.
    let scheduler = Arc::new(crate::scheduler::Scheduler::with_store(
        crate::scheduler::events_store_path(directory),
    ));
    let scheduler_task = tokio::spawn(crate::scheduler::run_event_loop(
        scheduler,
        crate::scheduler::TickerBuffer::new(),
        config.dashboard.session_prefix.clone(),
    ));
    let listener = UnixListener::bind(&session.socket)?;
    listener.set_nonblocking(true)?;
    session.url = format!("http://{}", server.address());
    session.state = "ready".into();
    publish(&session)?;
    print_started(&session, launch.json)?;
    let session = Arc::new(Mutex::new(session));
    let force = Arc::new(AtomicBool::new(false));
    let operations = Arc::new(Mutex::new(()));
    let mut force_since: Option<Instant> = None;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let server = server.clone();
                let session = session.clone();
                let force = force.clone();
                let operations = operations.clone();
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
                    let request =
                        read_frame(&stream).and_then(|v| Ok(serde_json::from_value::<Request>(v)?));
                    // `down` must not queue behind a stuck operation it exists to end.
                    let direct = request.as_ref().is_ok_and(|r| {
                        matches!(r.operation["op"].as_str(), Some("hello" | "down"))
                    });
                    let _operation = (!direct).then(|| operations.lock().unwrap());
                    let mut record = session.lock().unwrap().clone();
                    let result = request.and_then(|request| {
                        anyhow::ensure!(
                            request.protocol == PROTOCOL
                                && request.id == record.id
                                && request.incarnation == record.incarnation,
                            "session identity/protocol mismatch"
                        );
                        if request.operation["op"] == "down" {
                            let forced = request.operation["force"].as_bool().unwrap_or(false);
                            return begin_shutdown(&server, &session, &mut record, &force, forced);
                        }
                        handle_operation(&server, &mut record, request.operation)
                    });
                    if record.state == "stopping" {
                        *session.lock().unwrap() = record.clone();
                    }
                    let response = match result {
                        Ok(value) => {
                            json!({"id":record.id,"incarnation":record.incarnation,"result":value})
                        }
                        Err(error) => {
                            json!({"id":record.id,"incarnation":record.incarnation,"error":format!("{error:#}")})
                        }
                    };
                    let _ = writeln!(stream, "{response}");
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => (),
            Err(error) => return Err(error.into()),
        }
        if session.lock().unwrap().state == "stopping" {
            // A handler past admission can still spawn a topology after the
            // last run check, so in-flight operations must drain first; a
            // forced shutdown waits for them only briefly.
            let idle = operations.try_lock().is_ok();
            if force.load(Ordering::SeqCst) {
                let since = *force_since.get_or_insert_with(Instant::now);
                if idle || since.elapsed() > Duration::from_secs(5) {
                    break;
                }
            } else if idle && !server.session_has_work() {
                break;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(40)) => (),
            _ = interrupt.recv() => request_graceful(&server, &session)?,
            _ = terminate.recv() => request_graceful(&server, &session)?,
        }
    }
    scheduler_task.abort();
    for bridge in &mut bridges {
        crate::kill_child_gracefully(bridge, Duration::from_secs(3));
    }
    cleanup.cleanup();
    let mut session = session.lock().unwrap();
    session.state = "stopped".into();
    publish(&session)?;
    Ok(())
}
/// The registry record once the daemon has published `stopped`.
fn wait_stopped(session: &Session, timeout: Duration) -> Result<Session> {
    let deadline = Instant::now() + timeout;
    loop {
        let saved: Session =
            serde_json::from_slice(&fs::read(registry().join(format!("{}.json", session.id)))?)?;
        if saved.state == "stopped" {
            return Ok(saved);
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "shutdown is still pending; inspect omar info -s {} or explicitly use down --force",
            session.id
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn print_value(value: Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
fn print_started(session: &Session, json_output: bool) -> Result<()> {
    if json_output {
        return print_value(json!(session));
    }
    println!(
        "Started session {} ({})\nURL:    {}\nTUI:    omar attach -s {} --tui\nWeb:    omar attach -s {} --web\nStop:   omar down -s {}",
        session.name, session.id, session.url, session.id, session.id, session.id
    );
    Ok(())
}
fn strip_target_args(args: impl Iterator<Item = String>) -> Vec<String> {
    let mut stripped = Vec::new();
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        if arg == "--" {
            stripped.push(arg);
            stripped.extend(args);
            break;
        }
        if matches!(arg.as_str(), "--session" | "-s" | "--ea") {
            args.next();
            continue;
        }
        if arg == "--json"
            || arg.starts_with("--session=")
            || arg.starts_with("--ea=")
            || (arg.starts_with("-s") && !arg.starts_with("--"))
        {
            continue;
        }
        stripped.push(arg);
    }
    stripped
}
pub async fn dispatch(cli: &Cli) -> Option<Result<()>> {
    if !matches!(
        cli.command,
        None | Some(Commands::Up(_) | Commands::Serve { .. } | Commands::Run(_))
    ) && (cli.config.is_some() || cli.agent.is_some() || cli.spawn_metrics)
    {
        return Some(Err(anyhow::anyhow!("configuration options apply when creating a session; target an existing session without changing its configuration")));
    }
    if matches!(
        cli.command,
        Some(
            Commands::Attach { tui: true, .. }
                | Commands::Manager {
                    action: Some(crate::ManagerAction::Orchestrate)
                }
        )
    ) {
        return Some(attach_dashboard().await);
    }
    let result = match &cli.command {
        Some(Commands::Serve { name, address, no_ea, ui }) => {
            // The runtime serves Mission Control whenever the build has it; `--ui` insists on it.
            if *ui && !crate::web_assets::is_bundled() { return Some(Err(anyhow::anyhow!("{}", crate::web_assets::MISSING))); }
            launch(cli, UpOptions { name: name.clone(), address: address.unwrap_or_else(|| UpOptions::default().address), no_ea: *no_ea, ..UpOptions::default() }, true).map(|_| ())
        },
        Some(Commands::SessionDaemon { directory }) => daemon(directory).await,
        None => Err(anyhow::anyhow!("a command is required; see omar --help")),
        Some(Commands::Up(options)) => launch(cli, options.clone(), false).and_then(|s| {
            print_started(&s, cli.json)?;
            if options.web { crate::open_browser(&s.url); }
            if options.tui {
                // A fresh process attaches: the dashboard selects its session before Tokio starts.
                let error = Command::new(std::env::current_exe()?).args(["attach", "-s", &s.id, "--tui"]).exec();
                bail!("failed to attach: {error}");
            }
            Ok(())
        }),
        Some(Commands::Ls) => discover().and_then(|sessions| {
            if cli.json { return print_value(json!(sessions)); }
            println!("{:<20} {:<12} {:<26} BUILD / ID", "NAME", "STATUS", "URL");
            for s in sessions { println!("{:<20} {:<12} {:<26} {} {} {}",s.name,s.state,s.url,s.version,&s.build_id[..12.min(s.build_id.len())],s.id); }
            Ok(())
        }),
        Some(Commands::Info) => target(cli).and_then(|s| {
            if matches!(s.state.as_str(), "stopped" | "failed" | "stale") { Ok(json!({"session":s})) }
            else { rpc(&s,json!({"op":"overview"}),Duration::from_secs(5)) }
        }).and_then(print_value),
        Some(Commands::Attach { web: true, print_url, .. }) => target(cli).and_then(|s| {
            rpc(&s,json!({"op":"hello"}),Duration::from_secs(5))?;
            if *print_url { println!("{}",s.url); } else { crate::open_browser(&s.url); }
            Ok(())
        }),
        Some(Commands::Attach { .. }) => Err(anyhow::anyhow!("attach needs --tui or --web")),
        Some(Commands::Logs { follow, tail }) => target(cli).and_then(|s| {
            let mut command = Command::new("tail"); command.arg("-n").arg(tail.to_string());
            if *follow { command.arg("-f"); }
            anyhow::ensure!(command.arg(s.directory.join("logs/runtime.log")).status()?.success(),"reading logs failed"); Ok(())
        }),
        Some(Commands::Down { force, timeout }) => target(cli).and_then(|s| {
            if s.state == "stopped" { return Ok(()); }
            rpc(&s,json!({"op":"down","force":force}),Duration::from_secs(10))?;
            let saved = wait_stopped(&s, Duration::from_secs(*timeout))?;
            if cli.json { print_value(json!(saved)) } else { println!("Stopped session {}",s.name); Ok(()) }
        }),
        Some(Commands::Manager { action: None | Some(crate::ManagerAction::Start) }) => target(cli).and_then(|s|rpc(&s,json!({"op":"manager_start","ea":ea_selector(cli)}),Duration::from_secs(120))).and_then(print_value),
        Some(Commands::Run(options)) => (|| {
            // No runtime selected: this run gets a session of its own, shut
            // down again after a --wait run ends.
            let (s, owned) = match target(cli) {
                Ok(s) => {
                    anyhow::ensure!(cli.config.is_none() && cli.agent.is_none() && !cli.spawn_metrics,
                        "configuration options apply only when run creates a session");
                    (s, false)
                }
                Err(_) if cli.session.is_none() && std::env::var_os("OMAR_SESSION_ID").is_none() => {
                    let s = launch(cli, UpOptions { no_ea: true, ..UpOptions::default() }, false)?;
                    if !cli.json { eprintln!("Started session {} ({}); omar down -s {} stops it", s.name, s.id, s.id); }
                    (s, true)
                }
                Err(error) => return Err(error),
            };
            let ea = ea_selector(cli);
            for input in &options.inputs { anyhow::ensure!(input.contains('='), "input must be NAME=VALUE"); }
            let body=json!({"program":fs::read_to_string(&options.program)?,"raw_inputs":options.inputs,"replace":options.replace,"timeout_seconds":options.timeout_seconds,"fast":options.fast});
            let mut run=rpc(&s,json!({"op":"start","ea":ea,"request":body}),Duration::from_secs(120))?;
            if owned { run["session_id"] = json!(s.id); }
            // One structured result: the admission record, or with --wait the terminal record.
            if !options.wait { return print_value(run); }
            let state = loop {
                let state=rpc(&s,json!({"op":"status","ea":ea,"run":run["run_id"]}),Duration::from_secs(5))?;
                match state["status"].as_str() {
                    Some("completed"|"stopped"|"failed") => break state,
                    _ => std::thread::sleep(Duration::from_millis(200)),
                }
            };
            if owned {
                let _ = rpc(&s,json!({"op":"down","force":false}),Duration::from_secs(10));
                let _ = wait_stopped(&s, Duration::from_secs(30));
            }
            if state["status"] == "failed" { bail!("topology failed: {}", state["error"]); }
            if cli.json { return print_value(state); }
            println!("Topology '{}' {}", state["team"].as_str().unwrap_or(""), state["status"].as_str().unwrap_or(""));
            for (port, value) in state["outputs"].as_object().into_iter().flatten() { println!("Output {port} = {value}"); }
            for (name, value) in state["state"].as_object().into_iter().flatten() { println!("State {name} = {value}"); }
            Ok(())
        })(),
        Some(Commands::Runs { all_eas }) => target(cli).and_then(|s|rpc(&s,json!({"op":"runs","ea":ea_selector(cli),"all_eas":all_eas}),Duration::from_secs(5))).and_then(print_value),
        Some(Commands::Status { deployment }) => target(cli).and_then(|s|rpc(&s,json!({"op":"status","ea":ea_selector(cli),"run":deployment}),Duration::from_secs(5))).and_then(print_value),
        Some(Commands::Stop { deployment }) => target(cli).and_then(|s|rpc(&s,json!({"op":"stop","ea":ea_selector(cli),"run":deployment}),Duration::from_secs(5))).and_then(print_value),
        Some(Commands::Ea { action }) => target(cli).and_then(|s| {
            let op=match action { EaAction::List=>json!({"op":"eas"}),EaAction::Create{name,agent}=>json!({"op":"create_ea","name":name,"agent":agent}) };
            rpc(&s,op,Duration::from_secs(10))
        }).and_then(print_value),
        Some(Commands::Spawn{..}|Commands::List{..}|Commands::Kill{..}|Commands::Workspace{..}|Commands::Event{..}) => target(cli).and_then(|s| {
            let args=strip_target_args(std::env::args().skip(1));
            let value=rpc(&s,json!({"op":"exec","ea":ea_selector(cli),"cwd":std::env::current_dir()?,"args":args}),Duration::from_secs(120))?;
            if cli.json { print_value(value) } else { print!("{}",value["stdout"].as_str().unwrap_or("")); eprint!("{}",value["stderr"].as_str().unwrap_or("")); Ok(()) }
        }),
        _ if cli.session.is_some() => Err(anyhow::anyhow!("this internal/setup command does not accept --session")),
        _ => return None,
    };
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_is_hierarchical_without_session_resolution() {
        for (group, children) in [
            ("event", vec!["schedule", "list", "cancel"]),
            ("workspace", vec!["list", "show", "snapshot", "restore"]),
            ("ea", vec!["create", "list"]),
            ("manager", vec!["start", "orchestrate"]),
        ] {
            let error = Cli::try_parse_from(["omar", group, "--help"])
                .err()
                .unwrap();
            assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
            for child in children {
                assert!(error.to_string().contains(child));
            }
            assert!(Cli::try_parse_from(["omar", group]).is_err());
        }
        let help = Cli::try_parse_from(["omar", "event", "schedule", "--help"])
            .err()
            .unwrap()
            .to_string();
        assert!(help.contains("--session"));
        assert!(help.contains("--ea"));
        assert!(help.contains("--receiver"));
    }

    #[test]
    fn forwarding_removes_target_flags_but_preserves_literal_payload() {
        let args = [
            "-souter",
            "--ea=2",
            "--json",
            "spawn",
            "worker",
            "--",
            "--session=payload",
        ];
        assert_eq!(
            strip_target_args(args.into_iter().map(String::from)),
            ["spawn", "worker", "--", "--session=payload"]
        );
    }

    #[test]
    fn control_frames_are_bounded_and_complete() {
        assert!(read_frame(&b"{}"[..]).is_err());
        assert_eq!(read_frame(&b"{}\n"[..]).unwrap(), json!({}));
        assert!(read_frame(vec![b'x'; MAX_FRAME as usize + 1].as_slice()).is_err());
    }
}
