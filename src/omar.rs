mod app;
mod backend;
mod backend_probe;
mod backend_runner;
mod chat_history;
mod computer;
mod config;
mod deploy;
mod diagram;
mod ea;
mod event;
mod manager;
mod mcp;
mod memory;
mod metrics;
mod panic_hook;
mod paths;
mod process;
mod projects;
// The generator runs under `cargo test`; nothing in a release build calls it.
#[cfg(test)]
mod protocol;
mod reaction;
mod scheduler;
mod serve;
mod sessions;
mod stub_agent;
mod supervision;
mod terminal;
mod tmux;
mod topology;
mod ui;
mod web_assets;
mod workspace;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::{
    event::{
        KeyCode, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::Mutex;

use app::App;
use config::Config;
use event::{AppEvent, EventHandler};
use tmux::{tmux_command, TmuxClient};

#[cfg(test)]
pub(crate) fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Tmux session name used when omar auto-launches into tmux
pub const DASHBOARD_SESSION: &str = "omar-dashboard";

const TMUX_SETUP_WARNING: &str = "⚠ tmux not configured for omar — run 'omar setup-tmux' to fix";

#[derive(Parser)]
#[command(
    name = "omar",
    about = "Independent agent runtimes and topology orchestration",
    version,
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Target an existing runtime session by id or name
    #[arg(short = 's', long, global = true)]
    session: Option<String>,

    /// Print structured command results
    #[arg(long, global = true)]
    json: bool,

    /// Path to config file
    #[arg(short, long)]
    config: Option<String>,

    /// Backend for the initial EA of a new session
    #[arg(short, long)]
    agent: Option<String>,

    /// Target an EA within the selected runtime (default: 0)
    #[arg(long, global = true)]
    ea: Option<String>,

    // Preserve the allocated EA across our own cold dashboard exec in tmux.
    /// Enable global spawn metrics logging sink
    #[arg(long, global = true)]
    spawn_metrics: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new independent background runtime
    Up(sessions::UpOptions),
    /// List independently addressable runtime sessions
    Ls,
    /// Inspect the selected runtime session
    Info,
    /// Attach to the selected runtime with the terminal dashboard or Mission Control
    #[command(group = clap::ArgGroup::new("mode").required(true).args(["tui", "web"]))]
    Attach {
        /// Terminal dashboard inside the session's tmux server (z detaches; Q stops the session)
        #[arg(long)]
        tui: bool,
        /// Mission Control in the browser
        #[arg(long)]
        web: bool,
        /// Print the Mission Control URL instead of opening it
        #[arg(long, requires = "web")]
        print_url: bool,
    },
    /// Read a runtime's log
    Logs {
        #[arg(long)]
        follow: bool,
        #[arg(long, default_value_t = 100)]
        tail: usize,
    },
    /// Shut down one runtime and its owned workloads
    Down {
        #[arg(long)]
        force: bool,
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
    /// Run a topology in the selected runtime, or in a new session when none is selected
    #[command(visible_alias = "start")]
    Run(sessions::StartOptions),
    /// List topology runs in the selected runtime
    Runs {
        #[arg(long)]
        all_eas: bool,
    },
    /// Manage EAs inside a runtime session
    Ea {
        #[command(subcommand)]
        action: sessions::EaAction,
    },
    #[command(hide = true)]
    SessionDaemon { directory: PathBuf },
    #[command(hide = true)]
    SessionExec {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Inspect and version team-instance files (not runtime checkpoints)
    Workspace {
        #[command(subcommand)]
        action: WorkspaceAction,
    },
    /// Spawn a new agent session
    Spawn {
        /// Name for the agent session
        #[arg(short, long)]
        name: String,

        /// Command to run in the session (defaults to configured default_command)
        #[arg(short, long)]
        command: Option<String>,

        /// Working directory
        #[arg(short, long)]
        workdir: Option<String>,
    },

    /// List agent sessions in the target EA
    List {
        /// List sessions across all EAs
        #[arg(long)]
        all_eas: bool,
    },

    /// Force kill a deployment, or kill an agent session
    Kill {
        /// Deployment (team) name, or an agent session name
        name: String,
    },

    /// Stop a deployment gracefully: the current tag closes, state and logs
    /// are persisted, sessions are cleaned up
    Stop {
        /// Deployment (team) name
        deployment: String,
    },

    /// Show a deployment's lifecycle state
    Status {
        /// Deployment (team) name
        deployment: String,
    },

    /// Hand queued OMAR events to a backend hook (invoked by the backend).
    ///
    /// Reads the spool named by $OMAR_EVENT_SPOOL, empties it, and prints the
    /// reply shape the backend expects on stdout.
    HookDrain {
        /// Which backend's hook contract to answer: cursor or agy.
        #[arg(long)]
        format: String,
    },

    /// Configure tmux for optimal omar experience
    SetupTmux,

    /// Start or interact with the manager agent
    #[command(arg_required_else_help = true)]
    Manager {
        /// Manager action (start, orchestrate)
        #[command(subcommand)]
        action: Option<ManagerAction>,
    },

    /// Manage scheduled events for the target EA
    Event {
        #[command(subcommand)]
        action: EventAction,
    },

    /// Restore authoritative coordination state to a backend lifecycle hook.
    AgentHook {
        #[arg(long)]
        context_file: PathBuf,
        #[arg(long, default_value = "claude")]
        format: String,
        #[arg(long)]
        event: Option<String>,
    },

    /// Run a protocol-backed agent with a durable side-channel inbox.
    BackendRunner {
        #[arg(long)]
        backend: String,
        #[arg(long)]
        config_file: PathBuf,
    },

    /// Start the OMAR MCP server over stdio
    McpServer {
        /// Path to a serialized MCP server context JSON file. When omitted,
        /// the server builds a default context from the current config and
        /// active EA — used by peer processes (e.g. the Slack bridge) that
        /// aren't spawned by a specific backend launch.
        #[arg(long)]
        context_file: Option<String>,
    },

    /// Answer topology invocations without a model (test backend `stub`)
    #[command(hide = true)]
    StubAgent {
        /// MCP context written for this agent by the runtime
        #[arg(long)]
        context_file: PathBuf,
    },

    /// Accept OMAR programs over HTTP and supervise their runs
    Serve {
        /// Name for a new independent foreground session
        #[arg(long)]
        name: Option<String>,
        /// Loopback address to bind the admission API
        #[arg(long)]
        address: Option<std::net::SocketAddr>,

        /// Serve the API without starting an executive assistant. The agent
        /// context is still written, so a test harness can stand in for one.
        #[arg(long)]
        no_ea: bool,

        /// Open Mission Control in a browser. It is served from this same
        /// address, so the page and the API share an origin.
        #[arg(long)]
        ui: bool,
    },
}

#[derive(Subcommand)]
enum WorkspaceAction {
    /// List workspaces owned by the selected EA
    List,
    /// Show workspace paths and immutable file snapshots
    Show { id: String },
    /// Snapshot all worktree files. Writers must be quiescent for a consistent version
    Snapshot {
        id: String,
        #[arg(long, default_value = "Manual snapshot")]
        label: String,
    },
    /// Restore files into a new workspace; does not rewind or resume a topology
    Restore { id: String, snapshot: String },
}

#[derive(Subcommand)]
enum ManagerAction {
    /// Start the manager session
    Start,
    /// Run in orchestration mode (interactive)
    Orchestrate,
}

#[derive(Subcommand)]
enum EventAction {
    /// Schedule an event for an agent or the EA
    Schedule {
        /// Receiver name ("ea" for the manager)
        #[arg(long)]
        receiver: String,

        /// Event payload text
        #[arg(long)]
        payload: String,

        /// Sender label shown in the delivered message
        #[arg(long)]
        sender: Option<String>,

        /// Absolute trigger timestamp in nanoseconds since epoch
        #[arg(long)]
        at_ns: Option<u64>,

        /// Relative delay in seconds from now
        #[arg(long)]
        in_seconds: Option<u64>,

        /// Relative delay in nanoseconds from now
        #[arg(long)]
        in_ns: Option<u64>,

        /// Recurrence interval in seconds
        #[arg(long)]
        every_seconds: Option<u64>,

        /// Recurrence interval in nanoseconds
        #[arg(long)]
        every_ns: Option<u64>,
    },

    /// List scheduled events for the target EA
    List,

    /// Cancel a scheduled event by id
    Cancel {
        /// Event id
        id: String,
    },
}

/// Asks the desktop to open a URL, and says it plainly if there is no desktop.
fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let opened = std::process::Command::new(opener)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !opened {
        println!("Open {url}");
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    sessions::prepare_process(&cli)?;
    // Install the persisted-panic hook FIRST, before tokio builds its
    // runtime (and spawns worker threads). If the tmux parent dies it
    // takes the stderr pane with it (see issue #118), so panics need
    // to be written to disk before unwind. The hook chains to whatever
    // hook was previously installed, preserving RUST_BACKTRACE /
    // default stderr behaviour for non-tmux runs.
    //
    // We deliberately don't use `#[tokio::main]`: that macro builds
    // the runtime BEFORE the `async fn` body runs, so any panic on a
    // worker spawned during runtime construction would miss the hook.
    panic_hook::install(omar_dir().join("logs").join("panics"));

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main(cli))
}

async fn async_main(mut cli: Cli) -> Result<()> {
    let internal = if let Some(Commands::SessionExec { args }) = &cli.command {
        let args = args.clone();
        cli = Cli::try_parse_from(std::iter::once("omar".to_string()).chain(args))?;
        sessions::validate_exec(&cli)?;
        true
    } else {
        false
    };
    if !internal {
        if let Some(result) = sessions::dispatch(&cli).await {
            return result;
        }
    }
    let mut config = Config::load(cli.config.as_deref())?;
    if internal {
        let target = ea::resolve_ea_selector(&omar_dir(), cli.ea.as_deref())?;
        if let Ok(command) = std::fs::read_to_string(
            ea::ea_state_dir(target.id, &omar_dir()).join("assistant-command"),
        ) {
            config.agent.default_command = command;
        }
    }
    if let Some(ref agent) = cli.agent {
        config.agent.default_command = crate::backend::resolve(agent)
            .map(|backend| backend.default_command().to_string())
            .map_err(|e| anyhow::anyhow!("{}", e))?;
    }
    if cli.spawn_metrics {
        config.metrics.spawn_metrics_enabled = true;
        config.save_to_path(&Config::resolve_path(cli.config.as_deref()));
    }
    metrics::configure(config.metrics.spawn_metrics_enabled);
    let omar_dir = omar_dir();
    match cli.command {
        Some(
            Commands::Up(_)
            | Commands::Ls
            | Commands::Info
            | Commands::Attach { .. }
            | Commands::Logs { .. }
            | Commands::Down { .. }
            | Commands::Run(_)
            | Commands::Runs { .. }
            | Commands::Ea { .. }
            | Commands::SessionDaemon { .. }
            | Commands::SessionExec { .. }
            | Commands::Serve { .. },
        ) => unreachable!(),
        Some(Commands::Spawn {
            name,
            command,
            workdir,
        }) => {
            let target = resolve_cli_ea(&omar_dir, cli.ea.as_deref())?;
            let client =
                TmuxClient::new(ea::ea_prefix(target.id, &config.dashboard.session_prefix));
            let cmd = command.unwrap_or_else(|| config.agent.default_command.clone());
            spawn_agent(&client, &name, &cmd, workdir.as_deref())
        }
        Some(Commands::List { all_eas }) => {
            if all_eas {
                list_agents_all(&omar_dir, &config.dashboard.session_prefix)
            } else {
                let target = resolve_cli_ea(&omar_dir, cli.ea.as_deref())?;
                list_agents_for_ea(&config.dashboard.session_prefix, &target)
            }
        }
        Some(Commands::Kill { name }) => {
            let target = resolve_cli_ea(&omar_dir, cli.ea.as_deref())?;
            let client =
                TmuxClient::new(ea::ea_prefix(target.id, &config.dashboard.session_prefix));
            // A deployment record wins the name: a team and an agent session
            // rarely collide, and the record is the more deliberate target.
            let deployment = deployment_dir(&omar_dir, target.id, &name)
                .map(|dir| deploy::record_path(&dir).exists())
                .unwrap_or(false);
            if deployment {
                kill_deployment(&omar_dir, target.id, &name)
            } else {
                kill_agent(
                    &client,
                    &name,
                    &ea::ea_manager_session(target.id, &config.dashboard.session_prefix),
                    &scheduler::Scheduler::with_store(scheduler::events_store_path(&omar_dir)),
                    target.id,
                )
            }
        }
        Some(Commands::Workspace { action }) => {
            let target = resolve_cli_ea(&omar_dir, cli.ea.as_deref())?;
            let value = match action {
                WorkspaceAction::List => {
                    serde_json::to_value(workspace::list(&omar_dir, target.id)?)?
                }
                action => {
                    let id = match &action {
                        WorkspaceAction::Show { id }
                        | WorkspaceAction::Snapshot { id, .. }
                        | WorkspaceAction::Restore { id, .. } => id,
                        WorkspaceAction::List => unreachable!(),
                    };
                    let ws = workspace::Workspace::load(&omar_dir, id)?;
                    anyhow::ensure!(ws.ea_id == target.id, "workspace belongs to another EA");
                    match action {
                        WorkspaceAction::Show { .. } => serde_json::json!({
                            "workspace": ws, "worktree": ws.worktree(&omar_dir),
                            "temp": ws.temp(&omar_dir), "snapshots": ws.snapshots(&omar_dir)?,
                        }),
                        WorkspaceAction::Snapshot { label, .. } => {
                            ws.ensure_inactive(&omar_dir)?;
                            serde_json::to_value(ws.snapshot(&omar_dir, &label)?)?
                        }
                        WorkspaceAction::Restore { snapshot, .. } => {
                            let restored = ws.restore(&omar_dir, &snapshot)?;
                            serde_json::json!({"workspace": restored, "worktree": restored.worktree(&omar_dir), "temp": restored.temp(&omar_dir)})
                        }
                        WorkspaceAction::List => unreachable!(),
                    }
                }
            };
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
        Some(Commands::Stop { deployment }) => {
            let target = resolve_cli_ea(&omar_dir, cli.ea.as_deref())?;
            stop_deployment(&omar_dir, target.id, &deployment)
        }
        Some(Commands::Status { deployment }) => {
            let target = resolve_cli_ea(&omar_dir, cli.ea.as_deref())?;
            status_deployment(&omar_dir, target.id, &deployment)
        }
        Some(Commands::SetupTmux) => setup_tmux(),
        Some(Commands::Manager { .. }) => unreachable!("manager commands go through the runtime"),
        Some(Commands::Event { action }) => {
            let target = resolve_cli_ea(&omar_dir, cli.ea.as_deref())?;
            let scheduler =
                scheduler::Scheduler::with_store(scheduler::events_store_path(&omar_dir));
            match action {
                EventAction::Schedule {
                    receiver,
                    payload,
                    sender,
                    at_ns,
                    in_seconds,
                    in_ns,
                    every_seconds,
                    every_ns,
                } => schedule_cli_event(
                    &scheduler,
                    target.id,
                    receiver,
                    payload,
                    sender,
                    at_ns,
                    in_seconds,
                    in_ns,
                    every_seconds,
                    every_ns,
                ),
                EventAction::List => list_cli_events(&scheduler, target.id),
                EventAction::Cancel { id } => cancel_cli_event(&scheduler, target.id, &id),
            }
        }
        Some(Commands::BackendRunner {
            backend: _,
            config_file,
        }) => backend_runner::run(&config_file).await,
        Some(Commands::AgentHook {
            context_file,
            format,
            event,
        }) => supervision::run_hook(&context_file, &format, event.as_deref()),
        Some(Commands::McpServer { context_file }) => match context_file {
            Some(path) => mcp::run_server_from_context_file(PathBuf::from(path)),
            None => mcp::run_server_with_default_context(),
        },
        Some(Commands::HookDrain { format }) => {
            // Always print valid JSON, even on misconfiguration: a hook that
            // writes nothing reads as a failure to the backend.
            // Check the format before touching the spool: draining first
            // would destroy every queued event on a typo.
            let reply = match crate::backend::by_name(&format) {
                Some(backend) if backend.hook_reply(&[]).is_some() => {
                    use std::io::Read;
                    let mut raw = String::new();
                    std::io::stdin().take(1_048_576).read_to_string(&mut raw)?;
                    let input =
                        serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({}));
                    let mut events = if backend.hook_consumes_spool(&input) {
                        std::env::var("OMAR_EVENT_SPOOL")
                            .ok()
                            .map(|spool| {
                                crate::backend::spool::drain_spool(std::path::Path::new(&spool))
                            })
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    let mut state = supervision::inherited_hook_response(&input, &format)?;
                    if input["hook_event_name"] == "stop" {
                        state.to_string()
                    } else if !backend.hook_takes_context(&input) {
                        "{}".to_string()
                    } else {
                        if let Some(value) = state["additional_context"].as_str() {
                            events.push(value.to_owned());
                        }
                        if let Some(steps) = state["injectSteps"].as_array_mut() {
                            for step in steps {
                                if let Some(value) = step["ephemeralMessage"].as_str() {
                                    events.push(value.to_owned());
                                }
                            }
                        }
                        backend
                            .hook_reply(&events)
                            .unwrap_or_else(|| "{}".to_string())
                    }
                }
                _ => "{}".to_string(),
            };
            println!("{}", reply);
            Ok(())
        }
        Some(Commands::StubAgent { context_file }) => stub_agent::run(&context_file),
        None => unreachable!("clap requires a command"),
    }
}

fn omar_dir() -> PathBuf {
    sessions::state_root()
}

fn resolve_cli_ea(omar_dir: &std::path::Path, selector: Option<&str>) -> Result<ea::EaInfo> {
    Ok(ea::resolve_or_create_ea_selector(omar_dir, selector)?.0)
}

fn list_agents_for_ea(base_prefix: &str, ea_info: &ea::EaInfo) -> Result<()> {
    let prefix = ea::ea_prefix(ea_info.id, base_prefix);
    let manager_session = ea::ea_manager_session(ea_info.id, base_prefix);
    let sessions = sessions_for_ea(base_prefix, ea_info.id)?;

    if sessions.is_empty() {
        println!(
            "No agent sessions found for EA {} ({})",
            ea_info.id, ea_info.name
        );
        return Ok(());
    }

    println!("EA {}: {}", ea_info.id, ea_info.name);
    println!("{:<20} {:<12} {:<10}", "NAME", "ATTACHED", "PID");
    println!("{}", "-".repeat(44));

    for session in sessions {
        let name = display_cli_session_name(&session.name, &prefix, &manager_session);
        let attached = if session.attached { "yes" } else { "no" };
        println!("{:<20} {:<12} {:<10}", name, attached, session.pane_pid);
    }

    Ok(())
}

fn list_agents_all(omar_dir: &std::path::Path, base_prefix: &str) -> Result<()> {
    let eas = ea::ensure_default_ea(omar_dir)?;
    let mut printed = false;

    println!(
        "{:<6} {:<20} {:<20} {:<12} {:<10}",
        "EA", "EA_NAME", "NAME", "ATTACHED", "PID"
    );
    println!("{}", "-".repeat(74));

    for ea_info in eas {
        let prefix = ea::ea_prefix(ea_info.id, base_prefix);
        let manager_session = ea::ea_manager_session(ea_info.id, base_prefix);
        for session in sessions_for_ea(base_prefix, ea_info.id)? {
            let name = display_cli_session_name(&session.name, &prefix, &manager_session);
            let attached = if session.attached { "yes" } else { "no" };
            println!(
                "{:<6} {:<20} {:<20} {:<12} {:<10}",
                ea_info.id, ea_info.name, name, attached, session.pane_pid
            );
            printed = true;
        }
    }

    if !printed {
        println!("No agent sessions found");
    }

    Ok(())
}

fn sessions_for_ea(base_prefix: &str, ea_id: ea::EaId) -> Result<Vec<tmux::Session>> {
    let prefix = ea::ea_prefix(ea_id, base_prefix);
    let manager_session = ea::ea_manager_session(ea_id, base_prefix);
    let client = TmuxClient::new("");
    let mut sessions = client.list_all_sessions()?;
    sessions.retain(|session| session.name == manager_session || session.name.starts_with(&prefix));
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(sessions)
}

/// What the CLI calls the EA's own pane.
///
/// Not the name of its session. A worker is `<prefix><ea>-<name>` and the
/// manager is `<prefix>ea-<id>`, so the manager cannot be reached by the rule
/// that reaches everything else. The scheduler already addresses it by this
/// name -- an event's receiver is `"ea"` -- so the listing is not inventing
/// one, and `omar kill` should answer to it too.
const CLI_MANAGER_NAME: &str = "ea";

fn display_cli_session_name(session_name: &str, prefix: &str, manager_session: &str) -> String {
    if session_name == manager_session {
        CLI_MANAGER_NAME.to_string()
    } else {
        session_name
            .strip_prefix(prefix)
            .unwrap_or(session_name)
            .to_string()
    }
}

fn spawn_agent(
    client: &TmuxClient,
    name: &str,
    command: &str,
    workdir: Option<&str>,
) -> Result<()> {
    let full_name = client.session_for(name);

    if client.has_session(&full_name)? {
        anyhow::bail!("Session '{}' already exists", name);
    }

    client.new_session(&full_name, command, workdir)?;
    println!("Spawned agent: {}", name);
    Ok(())
}

/// The session a name from `omar list` refers to.
///
/// The inverse of `display_cli_session_name`, and the reason this exists: the
/// listing prints the manager as `ea`, and without an inverse that is the one
/// name in the listing `omar kill` cannot resolve.
fn cli_session_name(client: &TmuxClient, name: &str, manager_session: &str) -> String {
    if name == CLI_MANAGER_NAME {
        manager_session.to_string()
    } else {
        client.session_for(name)
    }
}

fn kill_agent(
    client: &TmuxClient,
    name: &str,
    manager_session: &str,
    scheduler: &scheduler::Scheduler,
    ea_id: ea::EaId,
) -> Result<()> {
    let full_name = cli_session_name(client, name, manager_session);

    if !client.has_session(&full_name)? {
        anyhow::bail!("Session '{}' not found", name);
    }
    let _ = client.ensure_session_not_attached(&full_name)?;

    client.kill_session(&full_name)?;
    let _ = scheduler.cancel_by_receiver_and_ea(name, ea_id);
    println!("Killed agent: {}", name);
    Ok(())
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_nanos() as u64
}

/// Where a team's deployment record and artifacts live.
/// Checked, not trusted: the name comes off the command line, and
/// `PathBuf::join` treats an absolute path as a replacement rather than a
/// suffix -- so `/tmp/x` would name a directory outside the EA entirely, and
/// the record found there decides which pid gets killed.
///
/// The rule is the compiler's own. `verify` refuses any bytecode whose team is
/// not an identifier, so a name rejected here could never have been deployed
/// and has no record to act on.
fn deployment_dir(omar_dir: &std::path::Path, ea_id: ea::EaId, team: &str) -> Result<PathBuf> {
    if !topology::valid_identifier(team) {
        anyhow::bail!("invalid team name '{team}'");
    }
    Ok(deploy::dir_for(omar_dir, ea_id, team))
}

/// Ask the runner to stop, then wait, bounded by the run's own invocation
/// timeout: the longest a tag may take to close once the stop is seen.
fn stop_deployment(omar_dir: &std::path::Path, ea_id: ea::EaId, team: &str) -> Result<()> {
    let dir = deployment_dir(omar_dir, ea_id, team)?;
    let record = deploy::DeploymentRecord::load(&dir)?
        .ok_or_else(|| anyhow::anyhow!("no deployment '{}'", team))?;
    if record.state.is_terminal() {
        println!("Deployment '{}' is already {}", team, record.state);
        return Ok(());
    }
    if !record.runner_alive() {
        anyhow::bail!(
            "deployment '{}' is {} but its runner (pid {}) is gone; use 'omar kill {}' to clean up",
            team,
            record.state,
            record.pid,
            team
        );
    }
    deploy::request_stop(&dir)?;
    println!("Stop requested; waiting for the current tag to close");
    let deadline =
        std::time::Instant::now() + Duration::from_secs(record.timeout_seconds.saturating_add(120));
    while std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        let Some(current) = deploy::DeploymentRecord::load(&dir)? else {
            break;
        };
        if current.state.is_terminal() {
            println!("Deployment '{}' is {}", team, current.state);
            return Ok(());
        }
        if !current.runner_alive() {
            anyhow::bail!(
                "runner (pid {}) died while stopping; use 'omar kill {}' to clean up",
                current.pid,
                team
            );
        }
    }
    anyhow::bail!(
        "deployment '{}' did not stop within {}s; 'omar kill {}' forces it",
        team,
        record.timeout_seconds.saturating_add(120),
        team
    )
}

/// Report a deployment's state. A record claiming to run under a dead pid is
/// healed to FAILED here: nobody else is left to write the ending.
fn status_deployment(omar_dir: &std::path::Path, ea_id: ea::EaId, team: &str) -> Result<()> {
    let dir = deployment_dir(omar_dir, ea_id, team)?;
    let mut record = deploy::DeploymentRecord::load(&dir)?
        .ok_or_else(|| anyhow::anyhow!("no deployment '{}'", team))?;
    if record.is_active() && !record.runner_alive() {
        record.advance(deploy::DeploymentState::Failed, Some("runner process died"))?;
        record.error = Some("runner process died".to_string());
        record.save(&dir)?;
    }
    println!("Deployment '{}' ({})", team, record.deployment_id);
    println!("  state: {}", record.state);
    let pid_note = if record.is_active() { " (alive)" } else { "" };
    println!("  pid: {}{}", record.pid, pid_note);
    println!("  started_at: {}", record.started_at);
    if let Some(finished) = record.finished_at {
        println!("  finished_at: {}", finished);
    }
    if let Some(error) = &record.error {
        println!("  error: {}", error);
    }
    for event in &record.history {
        match &event.detail {
            Some(detail) => println!("  {} at {} ({})", event.state, event.at, detail),
            None => println!("  {} at {}", event.state, event.at),
        }
    }
    println!("  agents: {}", record.sessions.len());
    for (name, value) in &record.state_vars {
        println!("  state {name} = {value}");
    }
    println!("  files: {}", dir.display());
    Ok(())
}

/// Force kill: the runner dies first, then its sessions, then the record says
/// CANCELLED. Also sweeps sessions a crashed run left behind.
fn kill_deployment(omar_dir: &std::path::Path, ea_id: ea::EaId, team: &str) -> Result<()> {
    let dir = deployment_dir(omar_dir, ea_id, team)?;
    let mut record = deploy::DeploymentRecord::load(&dir)?
        .ok_or_else(|| anyhow::anyhow!("no deployment '{}'", team))?;
    let client = record.session_client()?;
    // A finished deployment's runner is done with it; the pid it recorded may
    // be a runtime that is still serving other work.
    if record.is_active() && record.pid != std::process::id() && record.runner_alive() {
        deploy::kill_process(record.pid);
        let waited = std::time::Instant::now();
        while crate::process::pid_alive(record.pid) && waited.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let failures = if record.sessions_cleaned {
        Vec::new()
    } else {
        deploy::teardown_sessions(&client, &record.sessions, &deploy::logs_dir(&dir))
    };
    for failure in &failures {
        eprintln!("warning: session not cleaned up: {failure}");
    }
    record.sessions_cleaned = failures.is_empty();
    deploy::clear_stop(&dir)?;
    if record.is_active() {
        record.advance(
            deploy::DeploymentState::Cancelled,
            Some("killed by operator"),
        )?;
    }
    record.save(&dir)?;
    anyhow::ensure!(
        failures.is_empty(),
        "deployment cleanup remains unconfirmed"
    );
    println!("Deployment '{}' is {}", team, record.state);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn schedule_cli_event(
    scheduler: &scheduler::Scheduler,
    ea_id: ea::EaId,
    receiver: String,
    payload: String,
    sender: Option<String>,
    at_ns: Option<u64>,
    in_seconds: Option<u64>,
    in_ns: Option<u64>,
    every_seconds: Option<u64>,
    every_ns: Option<u64>,
) -> Result<()> {
    let base = now_ns();
    let delay_ns = scheduler::combine_seconds_and_ns(in_seconds, in_ns).unwrap_or(0);
    let timestamp = at_ns.unwrap_or_else(|| base.saturating_add(delay_ns));
    let recurring_ns = scheduler::combine_seconds_and_ns(every_seconds, every_ns);

    let event = scheduler::ScheduledEvent {
        id: uuid::Uuid::new_v4().to_string(),
        sender: sender.unwrap_or_else(|| "ea".to_string()),
        receiver,
        timestamp,
        payload,
        created_at: base,
        recurring_ns,
        ea_id,
    };
    scheduler.insert(event.clone());
    println!(
        "Scheduled event: {} -> {} at {}",
        event.sender, event.receiver, event.timestamp
    );
    println!("Event id: {}", event.id);
    Ok(())
}

fn list_cli_events(scheduler: &scheduler::Scheduler, ea_id: ea::EaId) -> Result<()> {
    let mut events = scheduler.list_by_ea(ea_id);
    if events.is_empty() {
        println!("No scheduled events found for EA {}", ea_id);
        return Ok(());
    }
    events.sort_by_key(|event| (event.timestamp, event.created_at));
    println!(
        "{:<36} {:<14} {:<14} {:<18} {:<12} PAYLOAD",
        "ID", "SENDER", "RECEIVER", "TIMESTAMP_NS", "RECURRING"
    );
    println!("{}", "-".repeat(120));
    for event in events {
        let recurring = event
            .recurring_ns
            .map(|ns| ns.to_string())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<36} {:<14} {:<14} {:<18} {:<12} {}",
            event.id, event.sender, event.receiver, event.timestamp, recurring, event.payload
        );
    }
    Ok(())
}

fn cancel_cli_event(
    scheduler: &scheduler::Scheduler,
    ea_id: ea::EaId,
    event_id: &str,
) -> Result<()> {
    match scheduler.cancel_if_ea(event_id, ea_id) {
        Ok(event) => {
            println!("Cancelled event: {}", event.id);
            Ok(())
        }
        Err(true) => anyhow::bail!("Event '{}' belongs to a different EA", event_id),
        Err(false) => anyhow::bail!("Event '{}' not found", event_id),
    }
}

/// Recommended tmux settings for omar, keyed by option name.
const TMUX_RECOMMENDED: &[(&str, &str, &str)] = &[
    (
        "default-terminal",
        "set -g default-terminal tmux-256color",
        "256-color terminal support",
    ),
    ("mouse", "set -g mouse on", "mouse scrolling and selection"),
    (
        "history-limit",
        "set -g history-limit 9999",
        "larger scrollback history",
    ),
    (
        "extended-keys",
        "set -g extended-keys on",
        "Shift+Enter in agents (on, not always — always breaks Shift+Tab in omar)",
    ),
    (
        "set-clipboard",
        "set -g set-clipboard on",
        "clipboard integration",
    ),
];

#[cfg(target_os = "macos")]
const TMUX_PLATFORM_RECOMMENDED: &[(&str, &str, &str)] = &[(
    "copy-command",
    "set -g copy-command pbcopy",
    "copy tmux selections to the macOS clipboard",
)];

#[cfg(not(target_os = "macos"))]
const TMUX_PLATFORM_RECOMMENDED: &[(&str, &str, &str)] = &[];

/// Additional raw lines that need to appear in tmux.conf (checked by substring).
const TMUX_EXTRA_LINES: &[(&str, &str, &str)] = &[
    (
        "terminal-features',*:RGB'",
        "set -as terminal-features ',*:RGB'",
        "truecolor support",
    ),
    (
        "terminal-features',*:extkeys'",
        "set -as terminal-features ',*:extkeys'",
        "extended key passthrough",
    ),
    (
        "terminal-features',*:clipboard'",
        "set -as terminal-features ',*:clipboard'",
        "clipboard passthrough",
    ),
    (
        "bind-key-nC-\\\\",
        "bind-key -n C-\\\\ detach-client",
        "Ctrl+\\\\ to detach from popup",
    ),
];

/// Check if any recommended tmux settings are missing.
fn tmux_setup_needed() -> bool {
    for &(opt, cmd, _) in TMUX_RECOMMENDED
        .iter()
        .chain(TMUX_PLATFORM_RECOMMENDED.iter())
    {
        // Extract expected value from the command string (last word)
        let expected = cmd.split_whitespace().last().unwrap_or("on");
        if let Ok(out) = tmux_command().args(["show-options", "-gv", opt]).output() {
            let val = String::from_utf8_lossy(&out.stdout);
            if val.trim() != expected {
                return true;
            }
        }
    }
    false
}

fn sync_tmux_setup_warning(app: &mut app::App) {
    if tmux_setup_needed() {
        app.set_persistent_warning_if_clear_or_same(TMUX_SETUP_WARNING);
    } else {
        app.clear_persistent_warning_if(TMUX_SETUP_WARNING);
    }
}

/// Interactive tmux configuration setup.
fn setup_tmux() -> Result<()> {
    use std::io::Write;

    let conf_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tmux.conf");

    let existing = std::fs::read_to_string(&conf_path).unwrap_or_default();

    // Collect missing settings
    let mut to_add: Vec<(&str, &str)> = Vec::new();

    for &(opt, line, desc) in TMUX_RECOMMENDED
        .iter()
        .chain(TMUX_PLATFORM_RECOMMENDED.iter())
    {
        // Check runtime value — even if the line is in the config,
        // a later conflicting line may override it.
        let expected = line.split_whitespace().last().unwrap_or("on");
        let runtime_ok = tmux_command()
            .args(["show-options", "-gv", opt])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == expected)
            .unwrap_or(false);
        if !runtime_ok {
            to_add.push((line, desc));
        }
    }

    let normalized = existing.replace(' ', "");
    for &(needle, line, desc) in TMUX_EXTRA_LINES {
        if !normalized.contains(needle) {
            to_add.push((line, desc));
        }
    }

    if to_add.is_empty() {
        println!("✓ tmux is already configured for omar.");
        return Ok(());
    }

    println!(
        "The following settings will be added to {}:\n",
        conf_path.display()
    );
    for (line, desc) in &to_add {
        println!("  {}  # {}", line, desc);
    }

    print!("\nApply? [Y/n] ");
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();

    if !input.is_empty() && input != "y" && input != "yes" {
        println!("Aborted.");
        return Ok(());
    }

    // Append to tmux.conf
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&conf_path)?;

    writeln!(file, "\n# omar recommended settings")?;
    for (line, _) in &to_add {
        writeln!(file, "{}", line)?;
    }

    // Apply settings directly to the running tmux server.
    // source-file alone isn't reliable because earlier conflicting lines
    // in the config (e.g., oh-my-tmux sets mouse off) can override ours.
    let tmux_running = tmux_command()
        .args(["list-sessions"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if tmux_running {
        for (line, _) in &to_add {
            // Each line is a full tmux command (e.g. "set -g mouse on")
            let args: Vec<&str> = line.split_whitespace().collect();
            let _ = tmux_command().args(&args).status();
        }
        println!("✓ Applied to ~/.tmux.conf and running tmux server.");
    } else {
        println!("✓ Applied to ~/.tmux.conf (tmux not running, will take effect next session).");
    }

    Ok(())
}

/// Locate the `omar-slack` binary. Checks next to the current executable
/// first, then falls back to a PATH lookup.
fn find_slack_binary() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("omar-slack");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    // Fall back to PATH lookup
    Some(PathBuf::from("omar-slack"))
}

/// Spawn the Slack bridge binary if SLACK_BOT_TOKEN and SLACK_APP_TOKEN are set.
fn spawn_slack_bridge() -> Option<std::process::Child> {
    if std::env::var("SLACK_BOT_TOKEN").is_err() || std::env::var("SLACK_APP_TOKEN").is_err() {
        return None;
    }

    let binary = find_slack_binary()?;
    match std::process::Command::new(&binary)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => {
            eprintln!("[omar] Slack bridge started (pid {})", child.id());
            Some(child)
        }
        Err(e) => {
            eprintln!("[omar] Failed to start Slack bridge: {}", e);
            None
        }
    }
}

/// Locate the `omar-computer` binary. Checks next to the current executable
/// first, then falls back to a PATH lookup.
fn find_computer_binary() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("omar-computer");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    // Fall back to PATH lookup
    Some(PathBuf::from("omar-computer"))
}

/// Spawn the computer-use bridge binary on Linux when an X11 display is
/// available. The bridge wraps xdotool / ImageMagick `import`, which are
/// X11-only tools, so skip it entirely on non-Linux platforms — an
/// XQuartz `DISPLAY` on macOS would otherwise trigger a noisy spawn of a
/// bridge that cannot do anything useful there.
fn spawn_computer_bridge() -> Option<std::process::Child> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    // Treat both unset and empty DISPLAY as "no X11 session". A bare
    // `export DISPLAY=` in a shell profile would otherwise satisfy
    // `is_err() == false` and still trigger a useless bridge spawn.
    let display_ok = std::env::var("DISPLAY")
        .ok()
        .is_some_and(|v| !v.trim().is_empty());
    if !display_ok {
        return None;
    }

    let binary = find_computer_binary()?;
    match std::process::Command::new(&binary)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => {
            eprintln!("[omar] Computer bridge started (pid {})", child.id());
            Some(child)
        }
        Err(e) => {
            eprintln!("[omar] Failed to start computer bridge: {}", e);
            None
        }
    }
}

/// Kill a child process gracefully: SIGTERM first, then SIGKILL after timeout.
fn kill_child_gracefully(child: &mut std::process::Child, timeout: Duration) {
    // Send SIGTERM
    let _ = std::process::Command::new("kill")
        .arg(child.id().to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    // Wait for the process to exit
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            _ => {
                if start.elapsed() >= timeout {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    // Force kill if still running
    let _ = child.kill();
    let _ = child.wait();
}

/// The dashboard is a client of one runtime: the daemon owns the event loop,
/// assistant launches, and every workload, so nothing here starts a second
/// scheduler or stops anything on exit.
async fn run_dashboard(config: Config, session: sessions::Session) -> Result<()> {
    // Some shells/dev tools export NO_COLOR globally. That disables all ANSI
    // styling and makes the TUI monochrome. The dashboard is explicitly color-coded.
    if std::env::var_os("NO_COLOR").is_some() {
        std::env::remove_var("NO_COLOR");
    }

    // Create the ticker buffer and scheduler, then spawn the event loop
    let ticker = scheduler::TickerBuffer::new();
    let omar_dir = omar_dir();
    let scheduler = Arc::new(scheduler::Scheduler::with_store(
        scheduler::events_store_path(&omar_dir),
    ));
    // Create SINGLE shared App instance for the dashboard/runtime state.
    let mut app = App::new(&config, ticker.clone(), scheduler.clone());
    app.session = Some(session);
    let shared_app = Arc::new(Mutex::new(app));

    // Initialize terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    // Enable keyboard enhancement where supported (improves key reporting).
    let keyboard_enhanced = supports_keyboard_enhancement().unwrap_or(false);
    if keyboard_enhanced {
        let _ = execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    {
        let mut app = shared_app.lock().await;
        sync_tmux_setup_warning(&mut app);
    }

    // Initial refresh
    {
        let mut app = shared_app.lock().await;
        if let Err(e) = app.refresh() {
            app.set_status(format!("Error: {}", e));
        }
    }

    // Event loop — locks shared_app per-phase (render, then handle).
    // The lock is NOT held across events.next().await so API calls proceed.
    let tick_rate = Duration::from_secs(config.dashboard.refresh_interval);
    let mut events = EventHandler::new(tick_rate);
    let mut tick_count: u64 = 0;

    loop {
        // Phase 1: Render (brief lock — read-only access to App)
        {
            let app = shared_app.lock().await;
            terminal.draw(|f| ui::render(f, &app))?;
        }
        // Lock released — API calls can proceed during event wait

        // Phase 2: Wait for event (no lock held)
        let event = events.next().await;

        // Phase 3: Handle event (lock for mutation)
        if let Some(event) = event {
            match event {
                AppEvent::Key(key) => {
                    let mut app = shared_app.lock().await;

                    // Handle project input mode
                    if app.project_input_mode {
                        match key.code {
                            KeyCode::Esc => {
                                app.project_input_mode = false;
                                app.project_input.clear();
                            }
                            KeyCode::Enter => {
                                let name = app.project_input.clone();
                                if !name.trim().is_empty() {
                                    app.add_project(name.trim());
                                    app.set_status("Project added");
                                }
                                app.project_input_mode = false;
                                app.project_input.clear();
                            }
                            KeyCode::Backspace => {
                                app.project_input.pop();
                            }
                            KeyCode::Char(c) => {
                                app.project_input.push(c);
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Handle EA name input mode (for spawning a new EA)
                    if app.ea_input_mode {
                        match key.code {
                            KeyCode::Esc => {
                                app.ea_input_mode = false;
                                app.ea_input.clear();
                            }
                            KeyCode::Enter => {
                                let name = app.ea_input.clone();
                                if !name.trim().is_empty() {
                                    if let Err(e) = app.create_ea(name.trim().to_string(), None) {
                                        app.set_status(format!("Error: {}", e));
                                    }
                                }
                                app.ea_input_mode = false;
                                app.ea_input.clear();
                            }
                            KeyCode::Backspace => {
                                app.ea_input.pop();
                            }
                            KeyCode::Char(c) => {
                                app.ea_input.push(c);
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Handle confirmation dialog (kill, quit, or delete EA)
                    if let Some(action) = app.pending_confirm {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => match action {
                                app::ConfirmAction::Kill => {
                                    let short_name = app.selected_agent_short_name();
                                    if let Err(e) = app.kill_selected() {
                                        app.set_status(format!("Error: {}", e));
                                    } else if let Some(name) = short_name {
                                        scheduler.cancel_by_receiver_and_ea(&name, app.active_ea);
                                    }
                                }
                                app::ConfirmAction::ResetQuit => {
                                    // Quitting stops the session's runtime; the
                                    // daemon cleans up its own workloads. `z`
                                    // detaches instead.
                                    let session = app.session.clone().expect("dashboard session");
                                    match sessions::rpc(
                                        &session,
                                        serde_json::json!({"op": "down", "force": false}),
                                        Duration::from_secs(10),
                                    ) {
                                        Ok(_) => app.should_quit = true,
                                        Err(e) => {
                                            app.pending_confirm = None;
                                            app.set_status(format!("Error: {}", e));
                                        }
                                    }
                                }
                                app::ConfirmAction::DeleteEa => {
                                    let ea_id = app.active_ea;
                                    if let Err(e) = app.delete_ea(ea_id) {
                                        app.set_status(format!("Error: {}", e));
                                    }
                                }
                            },
                            _ => {
                                app.pending_confirm = None;
                            }
                        }
                        continue;
                    }

                    // Handle help popup
                    if app.show_help {
                        app.show_help = false;
                        continue;
                    }

                    // Handle events popup
                    if app.show_events {
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('e') | KeyCode::Enter => {
                                app.show_events = false;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Handle debug console popup
                    if app.show_debug_console {
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('G') => {
                                app.show_debug_console = false;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Handle settings popup
                    if app.show_settings {
                        // Text-edit mode: capture characters and structural keys.
                        // The buffer is only committed to config on Enter, so
                        // Esc safely discards in-flight edits.
                        if let Some(buf) = app.settings_edit_buffer.as_mut() {
                            match key.code {
                                KeyCode::Esc => {
                                    app.settings_edit_buffer = None;
                                }
                                KeyCode::Enter => {
                                    let value = buf.clone();
                                    let idx = app.settings_selected;
                                    app.settings_edit_buffer = None;
                                    app.config.set_text_setting(idx, &value);
                                }
                                KeyCode::Backspace => {
                                    buf.pop();
                                }
                                // Block control chars; allow normal text.
                                KeyCode::Char(c) if !c.is_control() => {
                                    buf.push(c);
                                }
                                _ => {}
                            }
                            continue;
                        }
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('S') => {
                                app.show_settings = false;
                            }
                            KeyCode::Up | KeyCode::Char('k') if app.settings_selected > 0 => {
                                app.settings_selected -= 1;
                            }
                            KeyCode::Down | KeyCode::Char('j')
                                if app.settings_selected + 1 < app.config.settings_count() =>
                            {
                                app.settings_selected += 1;
                            }
                            KeyCode::Enter => {
                                let idx = app.settings_selected;
                                let is_text = app
                                    .config
                                    .settings_item(idx)
                                    .map(|item| item.is_text())
                                    .unwrap_or(false);
                                if is_text {
                                    let current = match app.config.settings_item(idx) {
                                        Some(config::SettingItem::Text { value, .. }) => {
                                            value.to_string()
                                        }
                                        _ => String::new(),
                                    };
                                    app.settings_edit_buffer = Some(current);
                                } else {
                                    app.config.toggle_setting(idx);
                                    // If event queue was just hidden, move sidebar off Events panel
                                    if !app.config.dashboard.show_event_queue
                                        && app.sidebar_panel == app::SidebarPanel::Events
                                    {
                                        app.sidebar_panel = app::SidebarPanel::Projects;
                                    }
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Handle sidebar enlarged popup
                    if app.sidebar_popup.is_some() {
                        match key.code {
                            KeyCode::Esc | KeyCode::Enter => {
                                app.sidebar_popup = None;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Normal key handling
                    match key.code {
                        KeyCode::Char('Q') => {
                            app.pending_confirm = Some(app::ConfirmAction::ResetQuit);
                        }
                        KeyCode::Esc => {
                            app.drill_up();
                        }
                        KeyCode::Tab => {
                            if key.modifiers.contains(KeyModifiers::SHIFT) {
                                app.drill_up();
                            } else {
                                app.drill_down();
                            }
                        }
                        KeyCode::BackTab => {
                            // Shift+Tab sends BackTab in most terminals
                            app.drill_up();
                        }
                        KeyCode::Right => {
                            if app.config.dashboard.sidebar_right {
                                // Sidebar is on the right: try grid right first, then sidebar
                                if !app.grid_right() {
                                    app.sidebar_focused = true;
                                }
                            } else {
                                // Sidebar is on the left: try grid right (no fallback)
                                // If at right edge of grid, stay put (sidebar is the other direction)
                                if !app.grid_right() && app.sidebar_focused {
                                    app.sidebar_focused = false;
                                }
                            }
                        }
                        KeyCode::Left => {
                            if app.config.dashboard.sidebar_right {
                                // Sidebar is on the right: try grid left (no fallback)
                                if !app.grid_left() && app.sidebar_focused {
                                    app.sidebar_focused = false;
                                }
                            } else {
                                // Sidebar is on the left: try grid left first, then sidebar
                                if !app.grid_left() {
                                    app.sidebar_focused = true;
                                }
                            }
                        }
                        KeyCode::Char(']') => {
                            app.cycle_next_ea();
                        }
                        KeyCode::Char('[') => {
                            app.cycle_previous_ea();
                        }
                        KeyCode::Char('j') | KeyCode::Down => {
                            if app.sidebar_focused {
                                app.sidebar_next();
                            } else {
                                app.next();
                            }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            if app.sidebar_focused {
                                app.sidebar_previous();
                            } else {
                                app.previous();
                            }
                        }
                        KeyCode::Char('h') => {
                            // h = physical left
                            if app.config.dashboard.sidebar_right {
                                // Sidebar on right: left means try grid left, no sidebar fallback
                                if !app.grid_left() && app.sidebar_focused {
                                    app.sidebar_focused = false;
                                }
                            } else {
                                // Sidebar on left: left means try grid left, then sidebar
                                if !app.grid_left() {
                                    app.sidebar_focused = true;
                                }
                            }
                        }
                        KeyCode::Char('l') => {
                            // l = physical right
                            if app.config.dashboard.sidebar_right {
                                // Sidebar on right: right means try grid right, then sidebar
                                if !app.grid_right() {
                                    app.sidebar_focused = true;
                                }
                            } else {
                                // Sidebar on left: right means try grid right, no sidebar fallback
                                if !app.grid_right() && app.sidebar_focused {
                                    app.sidebar_focused = false;
                                }
                            }
                        }
                        KeyCode::Enter => {
                            if app.sidebar_focused {
                                if app.sidebar_panel == app::SidebarPanel::Events {
                                    app.scheduled_events = scheduler.list_by_ea(app.active_ea);
                                    app.scheduled_events.sort_by_key(|e| e.timestamp);
                                    app.show_events = true;
                                } else {
                                    app.sidebar_popup = Some(app.sidebar_panel);
                                }
                                continue;
                            }

                            if let Err(e) = app.refresh() {
                                app.set_status(format!("Error: {}", e));
                                continue;
                            }

                            let popup_info = app
                                .selected_agent()
                                .map(|a| (a.session.name.clone(), app.client().clone()));

                            // Release App lock before blocking popup call
                            drop(app);

                            if std::env::var("TMUX").is_ok() {
                                // Inside tmux: use display-popup overlay.
                                // Holding the lock across attach_popup blocks all API handlers
                                // that need app.lock() for the entire popup lifetime.
                                if let Some((session_name, client)) = popup_info {
                                    let popup_result =
                                        client.attach_popup(&session_name, "90%", "90%");
                                    let session_live = client
                                        .session_has_live_pane(&session_name)
                                        .unwrap_or(false);

                                    let mut app = shared_app.lock().await;
                                    match popup_result {
                                        Err(e) => app.set_status(format!("Error: {}", e)),
                                        Ok(()) if !session_live => {
                                            let _ = app.refresh();
                                            app.set_status(format!(
                                                "{} exited; press Enter to restart it",
                                                session_name
                                            ));
                                        }
                                        Ok(()) => {
                                            if let Err(e) = app.refresh() {
                                                app.set_status(format!("Error: {}", e));
                                            }
                                        }
                                    }
                                }
                                // Discard ticks that accumulated while popup was open
                                events.drain();
                                terminal.clear()?;
                            } else {
                                // Outside tmux: temporarily exit alternate screen
                                if keyboard_enhanced {
                                    let _ = execute!(
                                        terminal.backend_mut(),
                                        PopKeyboardEnhancementFlags
                                    );
                                }
                                disable_raw_mode()?;
                                execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

                                let app = shared_app.lock().await;
                                let result = app.attach_selected();
                                drop(app);

                                // Restore terminal
                                execute!(terminal.backend_mut(), EnterAlternateScreen)?;
                                enable_raw_mode()?;
                                if keyboard_enhanced {
                                    let _ = execute!(
                                        terminal.backend_mut(),
                                        PushKeyboardEnhancementFlags(
                                            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                                        )
                                    );
                                }
                                events.drain();
                                terminal.clear()?;

                                if let Err(e) = result {
                                    let mut app = shared_app.lock().await;
                                    app.set_status(format!("Error: {}", e));
                                }
                            }
                        }
                        KeyCode::Char('n') => {
                            if let Err(e) = app.spawn_agent() {
                                app.set_status(format!("Error: {}", e));
                            }
                        }
                        KeyCode::Char('d') if app.selected_agent().is_some() => {
                            app.pending_confirm = Some(app::ConfirmAction::Kill);
                        }
                        KeyCode::Char('N') => {
                            // Open EA name prompt to spawn a new EA
                            app.ea_input_mode = true;
                        }
                        KeyCode::Char('D') => {
                            // Delete the currently active EA (last EA is protected)
                            if app.session.is_some() {
                                app.set_status(
                                    "EA deletion is not available while attached; the runtime owns EAs",
                                );
                            } else if app.registered_eas.len() > 1 {
                                app.pending_confirm = Some(app::ConfirmAction::DeleteEa);
                            } else {
                                app.set_status("Cannot delete the only EA");
                            }
                        }
                        KeyCode::Char('p') => {
                            app.project_input_mode = true;
                        }
                        KeyCode::Char('r') => {
                            if let Err(e) = app.refresh() {
                                app.set_status(format!("Error: {}", e));
                            } else {
                                app.set_status("Refreshed");
                            }
                        }
                        KeyCode::Char('e') => {
                            // Fix V2: EA-scoped events instead of global list
                            app.scheduled_events = scheduler.list_by_ea(app.active_ea);
                            app.scheduled_events.sort_by_key(|e| e.timestamp);
                            app.show_events = true;
                        }
                        KeyCode::Char('G') => {
                            app.show_debug_console = true;
                        }
                        // Detach from tmux — dashboard + agents keep running
                        KeyCode::Char('z') if std::env::var("TMUX").is_ok() => {
                            let _ = tmux_command().args(["detach-client"]).status();
                        }
                        KeyCode::Char('S') => {
                            app.show_settings = true;
                        }
                        KeyCode::Char('?') => {
                            app.show_help = !app.show_help;
                        }
                        _ => {}
                    }
                }
                AppEvent::Tick => {
                    let mut app = shared_app.lock().await;
                    // Rotate quotes every ~30 ticks
                    tick_count += 1;
                    if tick_count.is_multiple_of(30) {
                        app.quote_index = app.quote_index.wrapping_add(1);
                        sync_tmux_setup_warning(&mut app);
                    }

                    // Fix V2: EA-scoped events instead of global list
                    app.scheduled_events = scheduler.list_by_ea(app.active_ea);
                    app.scheduled_events.sort_by_key(|e| e.timestamp);

                    // Skip refresh while a popup/input overlay is active
                    // to avoid interrupting user input.
                    if !app.has_popup() {
                        app.clear_status();
                        if let Err(e) = app.refresh() {
                            app.set_status(format!("Error: {}", e));
                        }
                    }

                    // Keep system_state.md reasonably fresh without capturing
                    // the manager pane and rewriting JSON on every dashboard
                    // tick. State-changing actions still write immediately.
                    if tick_count.is_multiple_of(3) && !app.has_popup() {
                        let state_dir = app.state_dir();
                        let manager_session = app.manager_session_name();
                        memory::write_memory_to(
                            &state_dir,
                            &app.agents,
                            app.manager.as_ref(),
                            &manager_session,
                            app.client(),
                            &app.scheduled_events,
                        );
                    }
                }
                AppEvent::TickerScroll => {
                    let mut app = shared_app.lock().await;
                    app.ticker_offset = app.ticker_offset.wrapping_add(1);
                }
                AppEvent::Resize(_, _) => {
                    // Terminal will handle resize automatically
                }
            }
        }

        // Check quit flag
        let should_quit = {
            let app = shared_app.lock().await;
            app.should_quit
        };
        if should_quit {
            break;
        }
    }

    // Restore terminal
    if keyboard_enhanced {
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: `extended-keys always` forces tmux to emit modify-other-keys
    /// sequences to every client, including omar's dashboard, which doesn't
    /// push the kitty flag. Crossterm can't parse those sequences and silently
    /// collapses Shift+Tab to plain Tab — so `drill_up` never runs. `on`
    /// emits extended sequences only for clients that opt in via DECSET 2017,
    /// which keeps Shift+Enter working in Claude panes while leaving the
    /// dashboard on legacy xterm encoding (where Shift+Tab → `\x1b[Z` →
    /// `KeyCode::BackTab`). Do not flip back to `always`.
    #[test]
    fn tmux_extended_keys_recommendation_is_on_not_always() {
        let entry = TMUX_RECOMMENDED
            .iter()
            .find(|(opt, _, _)| *opt == "extended-keys")
            .expect("TMUX_RECOMMENDED must include an extended-keys entry");
        let value = entry.1.split_whitespace().last().unwrap_or("");
        assert_eq!(
            value, "on",
            "extended-keys must be `on`, not `{}` — `always` breaks Shift+Tab \
             in the dashboard (see tests comment)",
            value
        );
    }

    /// A deployment name that is not a team name names nothing.
    ///
    /// `PathBuf::join` treats an absolute path as a replacement rather than a
    /// suffix, so without this `omar status /tmp/x` reads a record from outside
    /// the EA directory -- and `omar kill` would take the pid it found there at
    /// its word.
    #[test]
    fn a_deployment_path_cannot_leave_the_ea_directory() {
        let omar_dir = std::path::Path::new("/home/someone/.omar");
        let ea_id = ea::EaId::default();

        for escape in ["/tmp/x", "../../etc", "a/b", "", ".", ".."] {
            assert!(
                deployment_dir(omar_dir, ea_id, escape).is_err(),
                "'{escape}' should not name a deployment"
            );
        }

        // And a real team name still resolves, under the EA where it belongs.
        let dir = deployment_dir(omar_dir, ea_id, "Cadence").expect("a team name resolves");
        assert!(
            dir.starts_with(ea::ea_state_dir(ea_id, omar_dir)),
            "{dir:?}"
        );
        assert!(dir.ends_with("topologies/Cadence"), "{dir:?}");
    }

    /// Every name `omar list` prints is a name `omar kill` can resolve.
    ///
    /// The manager is the one that got this wrong: the listing calls it `ea`,
    /// and resolution ran it through the worker rule -- `<prefix><ea>-<name>`
    /// -- producing `omar-agent-0-ea`, a session that has never existed. So
    /// `omar kill ea` answered "Session 'ea' not found" about the pane it had
    /// just been shown.
    #[test]
    fn a_name_the_listing_prints_is_a_name_kill_resolves() {
        let base = "omar-agent-";
        let prefix = ea::ea_prefix(0, base);
        let manager = ea::ea_manager_session(0, base);
        let client = TmuxClient::new(prefix.clone());

        // The manager and a worker, as tmux actually names them.
        for session in [manager.as_str(), "omar-agent-0-worker"] {
            let shown = display_cli_session_name(session, &prefix, &manager);
            assert_eq!(
                cli_session_name(&client, &shown, &manager),
                session,
                "listing shows '{shown}' for {session}, which does not resolve back"
            );
        }

        // And the manager is spelled the way the scheduler already addresses
        // it, so killing it cancels its events too.
        assert_eq!(display_cli_session_name(&manager, &prefix, &manager), "ea");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn tmux_macos_copy_command_uses_pbcopy() {
        let entry = TMUX_PLATFORM_RECOMMENDED
            .iter()
            .find(|(opt, _, _)| *opt == "copy-command")
            .expect("macOS setup must configure tmux copy-command");
        let value = entry.1.split_whitespace().last().unwrap_or("");
        assert_eq!(
            value, "pbcopy",
            "tmux copy-command should pipe native tmux selections into the macOS clipboard"
        );
    }
}
