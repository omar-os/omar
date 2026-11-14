use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::backend::Freshness;
use crate::checkpoint::{self, ExecutionState, QueuedTag, Trigger};
use crate::deploy::{self, ControlOp, DeploymentState};
use crate::diagram::{DiagramServer, NoopTopologyObserver, TopologyObserver};
use crate::manager::{self, McpLaunchContext, TopologyMcpContext};
use crate::tmux::flatten_agent_name;
use crate::tmux::DeliveryOptions;
use crate::tmux::TmuxClient;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bytecode {
    pub version: u32,
    pub team: String,
    pub instructions: Vec<Instruction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Instruction {
    BeginPlan {
        team: String,
    },
    /// A team instantiated by `main`. The container a diagram draws.
    DeclareInstance {
        name: String,
        team: String,
        /// The instance that declared it, empty for one `main` declared.
        #[serde(default)]
        parent: String,
    },
    SpawnAgent {
        name: String,
        backend: String,
        #[serde(default)]
        instance: String,
        /// Absent for the whole run, so a program without `per` has the
        /// bytecode it always had.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lifetime: Option<Lifetime>,
    },
    DefinePort {
        name: String,
        kind: PortKind,
        #[serde(rename = "type")]
        ty: String,
        /// The type as the program spelled it, when that names an imported
        /// type: `list<Decision>` where `ty` is the expanded refinement.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        declared: Option<String>,
        #[serde(default)]
        delay: Option<u64>,
        #[serde(default)]
        instance: String,
    },
    /// `type Decision from "./schemas/decision.json"`: a name for a type the
    /// program imported. Ports carry the expanded type, so this changes
    /// nothing the VM checks; it is how an agent is told `Decision` rather
    /// than the refinement spelled out, and what the schema said about it.
    DefineType {
        name: String,
        #[serde(rename = "type")]
        ty: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// `state round : int = 0`: a value a reaction keeps between
    /// invocations.
    DeclareState {
        name: String,
        #[serde(rename = "type")]
        ty: String,
        initial: Value,
        #[serde(default)]
        instance: String,
        /// For a `context`: the agents whose tasks are handed it before they
        /// run and asked to update it after. Empty for plain state.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        agents: Vec<String>,
        /// How the program says a context should be kept, if it does.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        directive: Option<String>,
    },
    /// A team parameter and the argument its instantiation bound to it.
    DeclareParam {
        name: String,
        #[serde(rename = "type")]
        ty: String,
        value: Value,
        #[serde(default)]
        instance: String,
    },
    /// A trigger the runtime fires itself, from the logical clock.
    DeclareTimer {
        name: String,
        offset: u64,
        period: u64,
        #[serde(default)]
        instance: String,
        /// `shutdown`: fired once at the run's last tag, not by the clock.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        shutdown: bool,
    },
    ConnectPorts {
        source: String,
        target: String,
        /// Absent for a plain connection, which is instantaneous. `Some(0)` is
        /// `after 0` and costs a microstep; larger values are nanoseconds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delay: Option<u64>,
    },
    InstallReaction {
        id: String,
        agent: String,
        triggers: Vec<String>,
        effects: Vec<String>,
        contract: String,
        prompt: String,
        /// A Rust body, when the reaction is code rather than a prompt.
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        instance: String,
        /// Nanoseconds one invocation may take before it is given up on.
        /// Absent leaves it to the run-wide timeout.
        #[serde(default)]
        within: Option<u64>,
    },
    CommitPlan,
    /// Opens a mutation plan: what turns the running program into a new one.
    /// Everything below up to `commit_plan` is the diff, removals first; the
    /// rest of the program is left as it was.
    BeginMutation {
        team: String,
    },
    RemoveType {
        name: String,
    },
    RemoveInstance {
        name: String,
    },
    KillAgent {
        name: String,
    },
    RemovePort {
        name: String,
    },
    RemoveTimer {
        name: String,
    },
    RemoveState {
        name: String,
    },
    RemoveParam {
        name: String,
    },
    DisconnectPorts {
        source: String,
        target: String,
    },
    RemoveReaction {
        id: String,
    },
    /// A reaction kept under its id with anything else about it changed.
    UpdateReaction {
        id: String,
        agent: String,
        triggers: Vec<String>,
        effects: Vec<String>,
        contract: String,
        prompt: String,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        instance: String,
        #[serde(default)]
        within: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum PortKind {
    Input,
    Output,
    Action,
}

/// What a lifetime counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum LifetimeUnit {
    /// The agent's own invocations.
    Task,
    /// The tags in which the agent is invoked.
    Tag,
}

/// How long an agent's native conversation lives: `coder : Claude per 5
/// tasks`. An agent without one keeps a single conversation for the whole
/// run. `per task` and `per tag` are counts of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct Lifetime {
    pub unit: LifetimeUnit,
    pub count: u32,
}

impl std::fmt::Display for Lifetime {
    /// As the program spells it, with a count of one left out.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let unit = match self.unit {
            LifetimeUnit::Task => "task",
            LifetimeUnit::Tag => "tag",
        };
        match self.count {
            1 => write!(f, "per {unit}"),
            count => write!(f, "per {count} {unit}s"),
        }
    }
}

/// The bytecode version a program with an agent lifetime carries. Version 1
/// runtimes ignore fields they do not know, so they would read `lifetime` as
/// absent and keep the conversation; the version is what makes them refuse it
/// instead.
pub const LIFETIME_BYTECODE: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentState {
    pub backend: String,
    /// Which instance declared it. Empty only for bytecode that predates
    /// instances being carried through.
    #[serde(default)]
    pub instance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifetime: Option<Lifetime>,
}

/// A team instantiated by `main`, and the team it came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceState {
    pub team: String,
    /// Which instance declared it. Empty for a top-level one, and for bytecode
    /// that predates teams being able to instantiate teams.
    #[serde(default)]
    pub parent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortState {
    pub kind: PortKind,
    #[serde(rename = "type")]
    pub ty: String,
    /// How the program spelled the type, when that differs from `ty`: the
    /// name of an imported type, possibly inside `list` or `option`. The VM
    /// checks `ty`; the agent is told this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay: Option<u64>,
    #[serde(default)]
    pub instance: String,
}

/// A named type the program imported, kept so an agent can be told the name
/// and what the schema said about it. The VM itself only ever sees `ty`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeState {
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `timer t(offset, period)`.
///
/// It occupies the same trigger namespace as ports but is not one: nothing can
/// write to it, and it carries no value of its own beyond the time it fired.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerState {
    pub offset: u64,
    /// `0` fires once. Anything else re-arms, forever.
    pub period: u64,
    #[serde(default)]
    pub instance: String,
    /// `shutdown`: fired once, at the run's last tag, and never by the clock.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shutdown: bool,
}

/// A state variable: typed as OMAR's types so its value can be recorded and
/// shown, and starting from a value the program wrote down.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateVarState {
    #[serde(rename = "type")]
    pub ty: String,
    pub initial: Value,
    #[serde(default)]
    pub instance: String,
    /// A context's sharing agents and directive; see `Instruction::DeclareState`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directive: Option<String>,
}

/// A team parameter with the argument bound to it, which is constant for the
/// life of the run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamState {
    #[serde(rename = "type")]
    pub ty: String,
    pub value: Value,
    #[serde(default)]
    pub instance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionState {
    pub source: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactionState {
    pub order: usize,
    pub agent: String,
    #[serde(default)]
    pub instance: String,
    pub triggers: Vec<String>,
    pub effects: Vec<String>,
    pub contract: String,
    pub prompt: String,
    /// A Rust body, when the reaction is code rather than a prompt.
    #[serde(default)]
    pub body: Option<String>,
    /// Nanoseconds one invocation may take. `None` uses the run-wide timeout.
    #[serde(default)]
    pub within: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmState {
    pub version: u32,
    pub team: String,
    #[serde(default)]
    pub instances: BTreeMap<String, InstanceState>,
    pub agents: BTreeMap<String, AgentState>,
    #[serde(default)]
    pub timers: BTreeMap<String, TimerState>,
    pub ports: BTreeMap<String, PortState>,
    pub connections: Vec<ConnectionState>,
    pub reactions: BTreeMap<String, ReactionState>,
    #[serde(default)]
    pub state_vars: BTreeMap<String, StateVarState>,
    #[serde(default)]
    pub params: BTreeMap<String, ParamState>,
    /// Named types, by the name the program gave them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub types: BTreeMap<String, TypeState>,
}

pub fn load_bytecode(path: &std::path::Path) -> Result<Bytecode> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read bytecode {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid bytecode JSON in {}", path.display()))
}

/// Compile and load a program a runtime was sent.
///
/// Installed builds find `omarc` beside the `omar` executable or on `PATH`;
/// development builds also recognize the compiler built under `lang/.lake`.
///
/// Every program a runtime compiles arrived as text: a client sent it, with
/// the files it imports, and the runtime wrote them down together. What the
/// program imports is confined to the directory it was written in, so a
/// program sent over the wire cannot read the host's files by naming them.
pub fn load_staged_program(path: &Path) -> Result<Bytecode> {
    load_program_with(path, None)
}

fn load_program_with(path: &Path, compiler: Option<&Path>) -> Result<Bytecode> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("omar") {
        bail!(
            "OMAR programs must use the .omar extension: {}",
            path.display()
        );
    }

    compile_source(path, compiler)
}

/// The schema types a program imports: each `type Name from "path"`, with
/// the path as the program spells it.
///
/// A client reads these from beside the program to send them with it. The
/// compiler is the authority on the syntax; a declaration this scan misses
/// is reported by the runtime as an import it was not sent.
pub fn imports_of(source: &str) -> Vec<(String, String)> {
    let mut imports = Vec::new();
    for line in source.lines() {
        let line = line.split("//").next().unwrap_or_default().trim();
        let Some(rest) = line.strip_prefix("type") else {
            continue;
        };
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let rest = rest.trim_start();
        let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (name, rest) = rest.split_at(name_end);
        let Some(rest) = rest.trim_start().strip_prefix("from") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('"') else {
            continue;
        };
        let Some(end) = rest.find('"') else {
            continue;
        };
        imports.push((name.to_string(), rest[..end].to_string()));
    }
    imports
}

/// Whether an import names a file inside the program's own directory: a
/// relative path with no `..` component, as `omarc --local-imports` accepts.
pub fn is_local_import(path: &str) -> bool {
    use std::path::Component;
    if path.is_empty() || path.contains('\\') {
        return false;
    }
    let mut named = false;
    for component in Path::new(path).components() {
        match component {
            Component::Normal(_) => named = true,
            Component::CurDir => {}
            _ => return false,
        }
    }
    named
}

/// The files a program imports, read from beside it and keyed by the path it
/// imports them under, ready to travel with its text to a runtime.
///
/// An import that leaves the program's directory is not read: the runtime
/// refuses it, and says why.
pub fn imported_files(program: &Path, source: &str) -> Result<BTreeMap<String, String>> {
    let directory = program.parent().unwrap_or_else(|| Path::new("."));
    let mut files = BTreeMap::new();
    for (name, import) in imports_of(source) {
        if !is_local_import(&import) {
            continue;
        }
        let text = fs::read_to_string(directory.join(&import)).with_context(|| {
            format!(
                "schema type '{name}' imports '{import}', which is not beside {}",
                program.display()
            )
        })?;
        files.insert(import, text);
    }
    Ok(files)
}

/// Where a program's generated artifacts go.
///
/// A compiler should leave something behind to look at, so the bytecode and
/// the reaction crate land beside the source rather than under `~/.omar`.
/// Lingua Franca's layout: a program in `src/` belongs to the project that
/// contains it, so `src-gen` is that project's, not `src`'s. A program
/// anywhere else is its own project.
pub fn generated_dir(source: &Path) -> PathBuf {
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "program".to_string());
    let here = source.parent().unwrap_or_else(|| Path::new("."));
    let root = if here.file_name() == Some(std::ffi::OsStr::new("src")) {
        here.parent().unwrap_or(here)
    } else {
        here
    };
    // One directory per program: a `src/` holds many, and they would otherwise
    // fight over a single Cargo.toml.
    root.join("src-gen").join(stem)
}

fn compile_source(source: &Path, compiler: Option<&Path>) -> Result<Bytecode> {
    let generated = generated_dir(source);
    fs::create_dir_all(&generated)
        .with_context(|| format!("failed to create {}", generated.display()))?;
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "program".to_string());
    // `<project>/foo.omar` and `<project>/src/foo.omar` are different programs
    // that name the same directory. Neither may quietly overwrite the other's
    // bytecode and crate, so the first one to arrive claims it.
    let identity = fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let identity = identity.to_string_lossy().into_owned();
    let claim = generated.join(".source");
    // Created rather than written, so two runs arriving together cannot both
    // decide they were first. Whoever loses the create reads what is there.
    match fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&claim)
    {
        Ok(mut file) => file
            .write_all(identity.as_bytes())
            .with_context(|| format!("failed to write {}", claim.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let held = read_claim(&claim);
            // Still empty after waiting, so no run is filling it in: one made
            // it and died before it could.
            if held.is_empty() {
                fs::write(&claim, &identity)
                    .with_context(|| format!("failed to write {}", claim.display()))?;
            } else if held != identity {
                bail!(
                    "'{held}' and '{identity}' both generate into {}; rename one of them",
                    generated.display()
                );
            }
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to create {}", claim.display()))
        }
    }

    let output_path = generated.join(format!("{stem}.json"));
    // The generated directory belongs to the program, not to one run of it, so
    // two runs compiling at once would otherwise interleave in the same file
    // and one could read what the other half-wrote. Each writes its own, then
    // renames: a reader sees either the old document or a whole new one.
    let draft = generated.join(format!(
        ".{stem}.{}.{}.json",
        std::process::id(),
        DRAFTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let compiler = compiler
        .map(Path::to_path_buf)
        .unwrap_or_else(resolve_omarc);

    let output = Command::new(&compiler)
        .arg("--local-imports")
        .arg(source)
        .arg(&draft)
        .output()
        .with_context(|| {
            format!(
                "failed to invoke OMAR compiler '{}'; install omarc beside omar, \
                 add it to PATH, or set OMARC_BIN",
                compiler.display()
            )
        })?;
    if !output.status.success() {
        let _ = fs::remove_file(&draft);
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let details = if stderr.is_empty() { stdout } else { stderr };
        if details.is_empty() {
            bail!("omarc failed with status {}", output.status);
        }
        bail!("omarc failed: {details}");
    }
    fs::rename(&draft, &output_path).with_context(|| {
        format!(
            "failed to move the compiled program into {}",
            output_path.display()
        )
    })?;

    load_bytecode(&output_path)
        .with_context(|| format!("omarc failed to compile {}", source.display()))
}

/// The plan that turns the program at `old` into the one at `new`. The
/// compiler computes it (`omarc --diff`), with the plan function its proofs
/// are about; the runtime checks it before applying anything.
pub fn compile_mutation(old: &Path, new: &Path) -> Result<Bytecode> {
    let plan_path = new
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".plan.json");
    let compiler = resolve_omarc();
    let output = Command::new(&compiler)
        .arg("--local-imports")
        .arg("--diff")
        .arg(old)
        .arg(new)
        .arg(&plan_path)
        .output()
        .with_context(|| {
            format!(
                "failed to invoke OMAR compiler '{}'; install omarc beside omar, \
                 add it to PATH, or set OMARC_BIN",
                compiler.display()
            )
        })?;
    if !output.status.success() {
        let _ = fs::remove_file(&plan_path);
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!("omarc failed: {stderr}");
    }
    let plan = load_bytecode(&plan_path);
    let _ = fs::remove_file(&plan_path);
    plan
}

/// The identity in a claim, waiting for it to be written.
///
/// A claim is created and then filled in, so a reader can arrive between the
/// two and find it empty. That is not an abandoned claim — it is a live one,
/// mid-write, and taking it over would let both programs compile into the same
/// directory. Only one that stays empty was left by a run that died.
fn read_claim(path: &Path) -> String {
    let waited = std::time::Instant::now();
    loop {
        let held = fs::read_to_string(path)
            .unwrap_or_default()
            .trim()
            .to_string();
        if !held.is_empty() || waited.elapsed() > Duration::from_secs(2) {
            return held;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Tells one compile's draft from another's in the same process.
static DRAFTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn resolve_omarc() -> PathBuf {
    if let Some(path) = std::env::var_os("OMARC_BIN") {
        return PathBuf::from(path);
    }

    resolve_build_omarc()
}

pub(crate) fn resolve_build_omarc() -> PathBuf {
    let executable_name = format!("omarc{}", std::env::consts::EXE_SUFFIX);
    if let Ok(current_executable) = std::env::current_exe() {
        if let Some(directory) = current_executable.parent() {
            let sibling = directory.join(&executable_name);
            if sibling.is_file() {
                return sibling;
            }
        }
    }

    let development_compiler = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("lang/.lake/build/bin")
        .join(&executable_name);
    if development_compiler.is_file() {
        return development_compiler;
    }

    PathBuf::from(executable_name)
}

pub fn verify(bytecode: &Bytecode) -> Result<VmState> {
    if bytecode.version != 1 && bytecode.version != LIFETIME_BYTECODE {
        bail!("unsupported bytecode version {}", bytecode.version);
    }
    if !valid_identifier(&bytecode.team) {
        bail!("invalid team name '{}'", bytecode.team);
    }
    if bytecode.instructions.len() < 2 {
        bail!("bytecode plan is incomplete");
    }
    match bytecode.instructions.first() {
        Some(Instruction::BeginPlan { team }) if team == &bytecode.team => {}
        Some(Instruction::BeginPlan { team }) => {
            bail!("plan team '{team}' does not match '{}'", bytecode.team)
        }
        _ => bail!("bytecode must begin with begin_plan"),
    }
    if !matches!(bytecode.instructions.last(), Some(Instruction::CommitPlan)) {
        bail!("bytecode must end with commit_plan");
    }

    let mut state = VmState {
        version: bytecode.version,
        team: bytecode.team.clone(),
        instances: BTreeMap::new(),
        agents: BTreeMap::new(),
        timers: BTreeMap::new(),
        ports: BTreeMap::new(),
        connections: Vec::new(),
        reactions: BTreeMap::new(),
        state_vars: BTreeMap::new(),
        params: BTreeMap::new(),
        types: BTreeMap::new(),
    };
    let mut committed = false;

    for (index, instruction) in bytecode.instructions.iter().enumerate() {
        if committed {
            bail!("instruction {index} appears after commit_plan");
        }
        match instruction {
            Instruction::BeginPlan { .. } if index == 0 => {}
            Instruction::BeginPlan { .. } => bail!("duplicate begin_plan at instruction {index}"),
            Instruction::DeclareInstance { name, team, parent } => {
                require_identifier("instance", name)?;
                if team.trim().is_empty() {
                    bail!("instance '{name}' names no team");
                }
                // A parent is declared before its children, so this also
                // rejects a cycle: neither end could be declared first.
                if !parent.is_empty() && !state.instances.contains_key(parent) {
                    bail!("instance '{name}' names undeclared parent '{parent}'");
                }
                if state
                    .instances
                    .insert(
                        name.clone(),
                        InstanceState {
                            team: team.clone(),
                            parent: parent.clone(),
                        },
                    )
                    .is_some()
                {
                    bail!("duplicate instance '{name}'");
                }
            }
            Instruction::SpawnAgent {
                name,
                backend,
                instance,
                lifetime,
            } => {
                check_instance(&state, "agent", name, instance)?;
                require_identifier("agent", name)?;
                if backend.trim().is_empty() {
                    bail!("agent '{name}' has an empty backend");
                }
                if let Some(lifetime) = lifetime {
                    if lifetime.count == 0 {
                        bail!("agent '{name}' lifetime must count at least 1");
                    }
                    // Version 1 promises a runtime that predates lifetimes can
                    // run it, and that runtime would keep the conversation.
                    if bytecode.version < LIFETIME_BYTECODE {
                        bail!(
                            "agent '{name}' resets its conversation ({lifetime}), which needs bytecode version {LIFETIME_BYTECODE}"
                        );
                    }
                    // Checked before anything starts: a backend that cannot
                    // promise a fresh conversation must not get the chance to
                    // reuse one.
                    if !crate::backend::by_name(backend).is_some_and(|b| b.fresh_conversation()) {
                        bail!(
                            "agent '{name}' resets its conversation ({lifetime}), but backend '{backend}' cannot start a fresh conversation; supported: {}",
                            crate::backend::fresh_conversation_backends().join(", ")
                        );
                    }
                }
                // Agents live in tmux sessions, and qualification is flattened
                // to get there, so two names that differ only by a '.' would
                // land in one session and answer each other's invocations.
                if let Some(clash) = state
                    .agents
                    .keys()
                    .find(|existing| flatten_agent_name(existing) == flatten_agent_name(name))
                {
                    bail!("agents '{clash}' and '{name}' need the same tmux session");
                }
                if state
                    .agents
                    .insert(
                        name.clone(),
                        AgentState {
                            backend: backend.clone(),
                            instance: instance.clone(),
                            lifetime: *lifetime,
                        },
                    )
                    .is_some()
                {
                    bail!("duplicate agent '{name}'");
                }
            }
            Instruction::DefinePort {
                name,
                kind,
                ty,
                declared,
                delay,
                instance,
            } => {
                require_identifier("port", name)?;
                check_instance(&state, "port", name, instance)?;
                if ty.trim().is_empty() {
                    bail!("port '{name}' has an empty type");
                }
                check_type("port", name, ty)?;
                // The spelling is shown to the agent in place of the type, so
                // it has to be the type: every name in it a type this plan
                // defined, expanding to exactly `ty`.
                if let Some(declared) = declared {
                    let definitions: BTreeMap<String, String> = state
                        .types
                        .iter()
                        .map(|(name, defined)| (name.clone(), defined.ty.clone()))
                        .collect();
                    if rename_type(declared, &definitions) != *ty {
                        bail!("port '{name}' is declared as '{declared}', which is not {ty}");
                    }
                }
                if delay.is_some() && *kind != PortKind::Action {
                    bail!("only action ports may declare a fixed delay");
                }
                if state
                    .ports
                    .insert(
                        name.clone(),
                        PortState {
                            kind: *kind,
                            ty: ty.clone(),
                            declared: declared.clone(),
                            delay: *delay,
                            instance: instance.clone(),
                        },
                    )
                    .is_some()
                {
                    bail!("duplicate port '{name}'");
                }
            }
            Instruction::DefineType {
                name,
                ty,
                title,
                description,
            } => {
                require_identifier("type", name)?;
                if ty.trim().is_empty() {
                    bail!("type '{name}' has an empty definition");
                }
                check_type("type", name, ty)?;
                if state
                    .types
                    .insert(
                        name.clone(),
                        TypeState {
                            ty: ty.clone(),
                            title: title.clone(),
                            description: description.clone(),
                        },
                    )
                    .is_some()
                {
                    bail!("duplicate type '{name}'");
                }
            }
            Instruction::DeclareState {
                name,
                ty,
                initial,
                instance,
                agents,
                directive,
            } => {
                require_identifier("state", name)?;
                check_instance(&state, "state", name, instance)?;
                if state.ports.contains_key(name) || state.timers.contains_key(name) {
                    bail!("state '{name}' is also a port or timer");
                }
                let starts_well = match ty.as_str() {
                    "int" => initial.is_i64(),
                    "bool" => initial.is_boolean(),
                    "string" => initial.is_string(),
                    other => bail!("state '{name}' has unsupported type '{other}'"),
                };
                if !starts_well {
                    bail!("state '{name}' is {ty} but starts as {initial}");
                }
                // A context is a note its agents read and rewrite: text, and
                // shared only by agents of its own instance.
                if !agents.is_empty() || directive.is_some() {
                    if ty != "string" {
                        bail!("context '{name}' is {ty}; a context is a string");
                    }
                    if agents.is_empty() {
                        bail!("context '{name}' lists no agents");
                    }
                }
                for agent in agents {
                    match state.agents.get(agent) {
                        Some(spec) if spec.instance == *instance => {}
                        _ => bail!("context '{name}' lists '{agent}', which is not an agent of its instance"),
                    }
                }
                let var = StateVarState {
                    ty: ty.clone(),
                    initial: initial.clone(),
                    instance: instance.clone(),
                    agents: agents.clone(),
                    directive: directive.clone(),
                };
                if state.state_vars.insert(name.clone(), var).is_some() {
                    bail!("duplicate state '{name}'");
                }
            }
            Instruction::DeclareParam {
                name,
                ty,
                value,
                instance,
            } => {
                require_identifier("parameter", name)?;
                check_instance(&state, "parameter", name, instance)?;
                // A body names a parameter the way it names a port, so the
                // name has to mean one thing.
                if state.ports.contains_key(name)
                    || state.timers.contains_key(name)
                    || state.state_vars.contains_key(name)
                {
                    bail!("parameter '{name}' is also a port, timer or state");
                }
                let holds = match ty.as_str() {
                    "int" => value.is_i64(),
                    "float" => value.is_f64() || value.is_i64(),
                    "bool" => value.is_boolean(),
                    "string" => value.is_string(),
                    other => bail!("parameter '{name}' has unsupported type '{other}'"),
                };
                if !holds {
                    bail!("parameter '{name}' is {ty} but was given {value}");
                }
                let param = ParamState {
                    ty: ty.clone(),
                    value: value.clone(),
                    instance: instance.clone(),
                };
                if state.params.insert(name.clone(), param).is_some() {
                    bail!("duplicate parameter '{name}'");
                }
            }
            Instruction::DeclareTimer {
                name,
                offset,
                period,
                instance,
                shutdown,
            } => {
                require_identifier("timer", name)?;
                check_instance(&state, "timer", name, instance)?;
                // Triggers are looked up by name in one namespace, so a timer
                // that shadowed a port would silently take its reactions.
                if state.ports.contains_key(name) {
                    bail!("timer '{name}' is also a port; a trigger has one name");
                }
                if *offset == 0 && *period == 0 && !shutdown {
                    bail!("timer '{name}' never fires; give it an offset, a period, or both");
                }
                if state
                    .timers
                    .insert(
                        name.clone(),
                        TimerState {
                            offset: *offset,
                            period: *period,
                            instance: instance.clone(),
                            shutdown: *shutdown,
                        },
                    )
                    .is_some()
                {
                    bail!("duplicate timer '{name}'");
                }
            }
            Instruction::ConnectPorts {
                source,
                target,
                delay,
            } => {
                let source_port = state
                    .ports
                    .get(source)
                    .with_context(|| format!("connection references unknown source '{source}'"))?;
                let target_port = state
                    .ports
                    .get(target)
                    .with_context(|| format!("connection references unknown target '{target}'"))?;
                // Targeting an input is how one instance feeds the next, so it
                // is allowed; the port kinds only have to agree on type.
                if source_port.ty != target_port.ty {
                    bail!("connection type mismatch from '{source}' to '{target}'");
                }
                if state
                    .connections
                    .iter()
                    .any(|connection| connection.source == *source && connection.target == *target)
                {
                    bail!("duplicate connection from '{source}' to '{target}'");
                }
                // A port holds one value per tag; a second incoming connection
                // would drop one of the two. omarc rejects it; so does the
                // runtime, for bytecode that did not come from omarc.
                if let Some(other) = state
                    .connections
                    .iter()
                    .find(|connection| connection.target == *target)
                {
                    bail!(
                        "port '{target}' already has an incoming connection from '{}'; a port takes one",
                        other.source
                    );
                }
                state.connections.push(ConnectionState {
                    source: source.clone(),
                    target: target.clone(),
                    delay: *delay,
                });
            }
            Instruction::InstallReaction {
                id,
                agent,
                triggers,
                effects,
                contract,
                prompt,
                body,
                instance,
                within,
            } => {
                let order = state.reactions.len();
                check_instance(&state, "reaction", id, instance)?;
                // A body and an agent are exclusive: whichever the source
                // gave, the other is empty.
                if body.is_some() {
                    if !agent.is_empty() || !prompt.is_empty() {
                        bail!("reaction '{id}' has a body, so it names no agent or prompt");
                    }
                } else if !state.agents.contains_key(agent) {
                    bail!("reaction '{id}' references unknown agent '{agent}'");
                }
                for trigger in triggers {
                    if state.timers.contains_key(trigger) {
                        continue;
                    }
                    let port = state.ports.get(trigger).with_context(|| {
                        format!("reaction '{id}' has unknown trigger '{trigger}'")
                    })?;
                    // A reaction may read the output of a team its own team
                    // instantiated — that is how a parent observes what it
                    // contains. Its own output is what it is there to write.
                    if port.kind == PortKind::Output && port.instance == *instance {
                        bail!("reaction '{id}' cannot be triggered by output '{trigger}'");
                    }
                }
                for effect in effects {
                    if state.timers.contains_key(effect) {
                        bail!("reaction '{id}' cannot write to timer '{effect}'");
                    }
                    let port = state.ports.get(effect).with_context(|| {
                        format!("reaction '{id}' has unknown effect '{effect}'")
                    })?;
                    if port.kind == PortKind::Input {
                        bail!("reaction '{id}' cannot affect input '{effect}'");
                    }
                }
                if state
                    .reactions
                    .insert(
                        id.clone(),
                        ReactionState {
                            order,
                            instance: instance.clone(),
                            agent: agent.clone(),
                            triggers: triggers.clone(),
                            effects: effects.clone(),
                            contract: contract.clone(),
                            prompt: prompt.clone(),
                            body: body.clone(),
                            within: *within,
                        },
                    )
                    .is_some()
                {
                    bail!("duplicate reaction '{id}'");
                }
            }
            Instruction::CommitPlan => committed = true,
            other => bail!(
                "instruction {index} ({}) belongs to a mutation plan, not a program",
                crate::mutation::op_name(other)
            ),
        }
    }
    reject_causality_loops(&state)?;
    reject_shared_names(&state)?;
    reject_bodies_that_cannot_be_generated(&state)?;
    Ok(state)
}

/// One name, one thing — checked across every namespace at once.
///
/// Each declaration also checks the namespaces filled in before it, which is
/// enough for bytecode in the order the compiler emits. It is not enough in
/// general: `declare_param` before `declare_state` is a collision neither
/// branch is looking for, and the VM's namespace is flat, so the two would
/// both be there under one name. Order is not something a verifier may assume.
fn reject_shared_names(state: &VmState) -> Result<()> {
    let declared = state
        .ports
        .keys()
        .map(|name| ("port", name))
        .chain(state.timers.keys().map(|name| ("timer", name)))
        .chain(state.state_vars.keys().map(|name| ("state", name)))
        .chain(state.params.keys().map(|name| ("parameter", name)));
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for (kind, name) in declared {
        if let Some(first) = seen.insert(name.as_str(), kind) {
            bail!("'{name}' is declared as both a {first} and a {kind}; a name means one thing");
        }
    }
    Ok(())
}

/// What a code body binds, checked here rather than left to rustc.
///
/// The generated crate declares a local per trigger, effect, state variable and
/// parameter, so a type it cannot carry or a name Rust has taken is an error
/// about the program. Caught at verification, a bad program is refused when it
/// is read; caught at build time it would be admitted and then fail a run.
fn reject_bodies_that_cannot_be_generated(state: &VmState) -> Result<()> {
    let timer_int = "int".to_string();
    for (id, reaction) in &state.reactions {
        if reaction.body.is_none() {
            continue;
        }
        let mine = |instance: &str| instance == reaction.instance;
        let bound = reaction
            .triggers
            .iter()
            .chain(reaction.effects.iter())
            // A timer carries the timestamp it fired at, so it has no port and
            // is an int — but it is still a local, and still needs a name.
            .map(|name| {
                let ty = state
                    .ports
                    .get(name)
                    .map(|port| &port.ty)
                    .unwrap_or(&timer_int);
                (name, ty)
            })
            .chain(
                state
                    .state_vars
                    .iter()
                    .filter(|(_, var)| mine(&var.instance))
                    .map(|(name, var)| (name, &var.ty)),
            )
            .chain(
                state
                    .params
                    .iter()
                    .filter(|(_, param)| mine(&param.instance))
                    .map(|(name, param)| (name, &param.ty)),
            );

        // A body may read a refined string: what arrives was already checked
        // against the refinement when it was written. It may not write one,
        // because the Rust `String` it would write through admits anything.
        for effect in &reaction.effects {
            if let Some(ty) = state.ports.get(effect).map(|port| &port.ty) {
                if is_refined(ty) {
                    bail!(
                        "reaction '{id}' has a body and writes '{effect}', which is \
                         {ty}; a body's String does not enforce the values it admits, \
                         so an enum port is written by a prompt"
                    );
                }
            }
        }

        for (name, ty) in bound {
            if !crate::reaction::supports_type(ty) {
                bail!(
                    "reaction '{id}' has a body and reaches '{name}', which is \
                     {ty}; a body carries int, float, bool, string, path or bytes"
                );
            }
            if let Some(reason) = crate::reaction::reserved_name(name) {
                bail!(
                    "reaction '{id}' has a body and names '{name}', which is \
                     {reason}; a body could not bind it"
                );
            }
            let local = name.rsplit('.').next().unwrap_or(name);
            if local == "_" {
                bail!(
                    "reaction '{id}' has a body and names '{name}'; '_' discards \
                     a value rather than naming one, so a body cannot read or write it"
                );
            }
        }
    }
    Ok(())
}

/// Ports a value written to `port` reaches without any logical delay.
///
/// A write incurs the target's own fixed delay, and a connection adds its own,
/// so a hop is instantaneous only when both are zero. Following those hops is
/// what says which reactions must be ordered against each other at one tag.
fn zero_delay_reach(state: &VmState, port: &str) -> BTreeSet<String> {
    let mut reached = BTreeSet::new();
    if !port_delay(state, port).is_instant() {
        return reached;
    }
    let mut frontier = vec![port.to_string()];
    while let Some(name) = frontier.pop() {
        if !reached.insert(name.clone()) {
            continue;
        }
        for connection in &state.connections {
            if connection.source != name {
                continue;
            }
            let hop = connection_delay(connection)
                .then(port_delay(state, &connection.target))
                .unwrap_or(Delay::MICROSTEP);
            if hop.is_instant() {
                frontier.push(connection.target.clone());
            }
        }
    }
    reached
}

/// Reaction ids that must run after `id` at a tag it fires in.
///
/// Instantaneous data dependencies order producers before consumers. Reactions
/// in the same team instance also follow definition order, even on different
/// agents: they share a workspace. Shared agents and output ports retain their
/// ordering constraints across instances.
pub fn must_follow(state: &VmState, id: &str) -> BTreeSet<String> {
    let Some(reaction) = state.reactions.get(id) else {
        return BTreeSet::new();
    };
    // A chain establishes the total order without materializing every
    // transitive pair (quadratic storage for large team instances).
    let next_in_instance = state
        .reactions
        .iter()
        .filter(|(_, other)| other.instance == reaction.instance && other.order > reaction.order)
        .min_by_key(|(_, other)| other.order)
        .map(|(id, _)| id.as_str());
    // An action carries a microstep even at zero delay, so writing one settles
    // nothing at this tag and orders nobody.
    let instant_writes: BTreeSet<String> = reaction
        .effects
        .iter()
        .flat_map(|effect| zero_delay_reach(state, effect))
        .collect();
    state
        .reactions
        .iter()
        .filter(|(other, _)| other.as_str() != id)
        .filter(|(other, state_of)| {
            let reads = state_of
                .triggers
                .iter()
                .any(|trigger| instant_writes.contains(trigger));
            let shares_port = state_of
                .effects
                .iter()
                .any(|effect| reaction.effects.contains(effect));
            // A reaction has no agent, so two of them share nothing here.
            let shares_agent = !reaction.agent.is_empty() && state_of.agent == reaction.agent;
            let shares_instance = state_of.instance == reaction.instance;
            // Definition order serializes a team's invocations; data
            // dependencies must agree with it or verification rejects a cycle.
            reads
                || next_in_instance == Some(other.as_str())
                || (!shares_instance
                    && (shares_port || shares_agent)
                    && state_of.order > reaction.order)
        })
        .map(|(other, _)| other.clone())
        .collect()
}

/// A cycle in that ordering is a tag that cannot be scheduled.
///
/// Every hop in it is instantaneous, so each reaction would have to run before
/// the other at one tag. Lingua Franca rejects the same shape for the same
/// reason. A loop through an action is not one: an action carries a microstep,
/// so the second firing is at a later tag and the order is decided by time
/// rather than by precedence.
fn reject_causality_loops(state: &VmState) -> Result<()> {
    let mut colour: BTreeMap<&str, u8> = state
        .reactions
        .keys()
        .map(|id| (id.as_str(), 0u8))
        .collect();
    let edges: BTreeMap<String, BTreeSet<String>> = state
        .reactions
        .keys()
        .map(|id| (id.clone(), must_follow(state, id)))
        .collect();

    // Iterative depth-first search: a topology can nest deeply enough that
    // recursion is a stack overflow rather than a compile error.
    for root in state.reactions.keys() {
        if colour[root.as_str()] != 0 {
            continue;
        }
        let mut path: Vec<String> = Vec::new();
        let mut stack: Vec<(String, bool)> = vec![(root.clone(), false)];
        while let Some((id, leaving)) = stack.pop() {
            if leaving {
                colour.insert(state.reactions.get_key_value(&id).unwrap().0.as_str(), 2);
                path.pop();
                continue;
            }
            match colour[id.as_str()] {
                1 => {
                    let from = path.iter().position(|step| *step == id).unwrap_or(0);
                    let mut cycle: Vec<&str> = path[from..].iter().map(String::as_str).collect();
                    cycle.push(&id);
                    bail!(
                        "causality loop: {} — every hop is instantaneous, so none of these can run first",
                        cycle.join(" -> ")
                    );
                }
                2 => continue,
                _ => {}
            }
            colour.insert(state.reactions.get_key_value(&id).unwrap().0.as_str(), 1);
            path.push(id.clone());
            stack.push((id.clone(), true));
            for next in edges.get(&id).into_iter().flatten() {
                if colour[next.as_str()] != 2 {
                    stack.push((next.clone(), false));
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
struct InvocationRecord {
    id: u64,
    team: String,
    agent: String,
    reaction: String,
    contract: String,
    allowed_effects: BTreeMap<String, String>,
    /// What the reaction was triggered by, and the instruction as the agent
    /// would read it. An agent in a pane is handed both in its prompt; a human
    /// has no pane, so the record is where a panel reads them from.
    trigger_values: BTreeMap<String, Value>,
    /// Its instance's state as the invocation found it, and each variable's
    /// type. Every task of an instance may read and write that instance's
    /// state: its reactions run one at a time in declaration order, so no
    /// two of them hold it at once.
    state_values: BTreeMap<String, Value>,
    state_types: BTreeMap<String, String>,
    /// The contexts its agent shares, with their directives, and every
    /// context of the instance; see `InvocationSpec`.
    contexts: BTreeMap<String, Option<String>>,
    context_names: BTreeSet<String>,
    prompt: String,
    writes: BTreeMap<String, Value>,
    completed: bool,
    /// The key of the worker attempt an agent that resets its conversation was started
    /// for. Only a call carrying it may answer; `None` for every other agent.
    attempt: Option<String>,
}

/// One invocation waiting to be answered, as a panel needs to draw it.
///
/// Only what the invocation already scopes: the values fed in, and the ports
/// this reaction may write with their types. Nothing else in the run is
/// reachable from here, because nothing else was wired to it.
#[derive(Debug, Serialize, Deserialize)]
struct PendingInvocation {
    task_id: u64,
    reaction: String,
    contract: String,
    prompt: String,
    trigger_values: BTreeMap<String, Value>,
    allowed_effects: BTreeMap<String, String>,
    /// Its instance's state, which it may also write, and each one's type.
    #[serde(default)]
    state: BTreeMap<String, Value>,
    #[serde(default)]
    state_types: BTreeMap<String, String>,
    /// The contexts its agent shares: each one's directive and value.
    #[serde(default)]
    contexts: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SetPortArgs {
    task_id: u64,
    port: String,
    value: Value,
}

/// A write to the team's state, which is not a port and has its own tool so
/// an agent never mistakes one for the other.
#[derive(Debug, Serialize, Deserialize)]
struct SetStateArgs {
    task_id: u64,
    name: String,
    value: Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct CompleteArgs {
    task_id: u64,
    /// Contexts the agent shares and leaves as they were. Every one it shares
    /// must be set with `omar_set_state` in this invocation or named here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    keep: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum InvocationCommand {
    SetPort(SetPortArgs),
    SetState(SetStateArgs),
    Complete(CompleteArgs),
    /// What this agent owes right now. An agent in a pane is told; a human has
    /// to be able to ask, because a panel can be opened at any time and a
    /// reload must not lose the invocation.
    Pending,
}

#[derive(Debug, Serialize, Deserialize)]
struct InvocationRequest {
    token: String,
    team: String,
    agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attempt: Option<String>,
    command: InvocationCommand,
}

#[derive(Debug, Serialize, Deserialize)]
struct InvocationResponse {
    result: Option<Value>,
    error: Option<String>,
}

struct InvocationEntry {
    record: InvocationRecord,
    completion: Option<mpsc::SyncSender<BTreeMap<String, Value>>>,
}

#[derive(Clone, Default)]
struct InvocationRegistry {
    entries: Arc<Mutex<BTreeMap<u64, InvocationEntry>>>,
}

/// The effect a `set_port` call names, and its type.
///
/// Effects are instance-qualified (`faq.objection`), but a prompt names its
/// ports bare (`$(objection)`), and small models copy that form. A bare name
/// that matches the last segment of exactly one effect means that effect;
/// anything else is an error naming the ports that would have worked, so the
/// agent can correct itself instead of retrying the same name.
fn resolve_effect_port<'a>(
    allowed: &'a BTreeMap<String, String>,
    port: &str,
) -> Result<(&'a String, &'a String)> {
    if let Some(entry) = resolve_name(allowed, port) {
        return Ok(entry);
    }
    let names: Vec<&str> = allowed.keys().map(String::as_str).collect();
    bail!(
        "port '{}' is not an effect of this invocation; valid ports: {}",
        port,
        names.join(", ")
    )
}

/// `name` exactly, or the one entry whose last segment it is.
fn resolve_name<'a>(
    names: &'a BTreeMap<String, String>,
    name: &str,
) -> Option<(&'a String, &'a String)> {
    if let Some(entry) = names.get_key_value(name) {
        return Some(entry);
    }
    let mut matches = names
        .iter()
        .filter(|(full, _)| full.rsplit_once('.').is_some_and(|(_, last)| last == name));
    match (matches.next(), matches.next()) {
        (Some(entry), None) => Some(entry),
        _ => None,
    }
}

impl InvocationRegistry {
    fn register(
        &self,
        record: InvocationRecord,
    ) -> Result<mpsc::Receiver<BTreeMap<String, Value>>> {
        let id = record.id;
        let (completion, receiver) = mpsc::sync_channel(1);
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("invocation registry lock poisoned"))?;
        if entries.contains_key(&id) {
            bail!("duplicate invocation '{id}'");
        }
        entries.insert(
            id,
            InvocationEntry {
                record,
                completion: Some(completion),
            },
        );
        Ok(receiver)
    }

    fn remove(&self, task_id: &u64) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(task_id);
        }
    }

    /// Whether this invocation has already been answered.
    ///
    /// Delivery needs this because an agent that reads its pane a line at a
    /// time answers before the submit, leaving the terminal with no change to
    /// show for it. An entry that has gone counts as answered: there is
    /// nothing left to deliver to either way.
    fn answered(&self, task_id: &u64) -> bool {
        match self.entries.lock() {
            Ok(entries) => entries
                .get(task_id)
                .is_none_or(|entry| entry.record.completed),
            // A poisoned lock is not evidence of an answer, and claiming one
            // would turn a lost delivery into a silent hang.
            Err(_) => false,
        }
    }

    fn execute(
        &self,
        team: &str,
        agent: &str,
        attempt: Option<&str>,
        command: InvocationCommand,
    ) -> Result<Value> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("invocation registry lock poisoned"))?;
        match command {
            InvocationCommand::SetPort(args) => {
                let invocation = entries
                    .get_mut(&args.task_id)
                    .with_context(|| format!("unknown task_id {}", args.task_id))?;
                validate_invocation_owner(team, agent, attempt, &invocation.record)?;
                if invocation.record.completed {
                    bail!("invocation '{}' is already complete", invocation.record.id);
                }
                if resolve_name(&invocation.record.state_types, &args.port).is_some() {
                    bail!(
                        "'{}' is the team's state, not a port; set it with omar_set_state",
                        args.port
                    );
                }
                let (port, ty) =
                    resolve_effect_port(&invocation.record.allowed_effects, &args.port)?;
                let (port, ty) = (port.clone(), ty.clone());
                validate_value(&ty, &args.value)
                    .with_context(|| format!("invalid value for port '{}'", port))?;
                invocation.record.writes.insert(port.clone(), args.value);
                Ok(json!({"status":"buffered","port":port}))
            }
            // Any state of the invocation's own instance: its reactions run
            // one at a time, so no contract needs to say who may write it.
            InvocationCommand::SetState(args) => {
                let invocation = entries
                    .get_mut(&args.task_id)
                    .with_context(|| format!("unknown task_id {}", args.task_id))?;
                validate_invocation_owner(team, agent, attempt, &invocation.record)?;
                if invocation.record.completed {
                    bail!("invocation '{}' is already complete", invocation.record.id);
                }
                let state = &invocation.record.state_types;
                let Some((name, ty)) = resolve_name(state, &args.name) else {
                    if state.is_empty() {
                        bail!("this task's team declares no state");
                    }
                    let names: Vec<&str> = state.keys().map(String::as_str).collect();
                    bail!(
                        "'{}' is not a state of this task's team; its state: {}",
                        args.name,
                        names.join(", ")
                    );
                };
                let (name, ty) = (name.clone(), ty.clone());
                validate_value(&ty, &args.value)
                    .with_context(|| format!("invalid value for state '{name}'"))?;
                if invocation.record.context_names.contains(&name) {
                    check_context_size(&name, &args.value)?;
                }
                invocation.record.writes.insert(name.clone(), args.value);
                Ok(json!({"status":"buffered","state":name}))
            }
            InvocationCommand::Pending => {
                let pending: Vec<PendingInvocation> = entries
                    .values()
                    .filter(|entry| {
                        !entry.record.completed
                            && entry.record.team == team
                            && entry.record.agent == agent
                            && (entry.record.attempt.is_none()
                                || entry.record.attempt.as_deref() == attempt)
                    })
                    .map(|entry| PendingInvocation {
                        task_id: entry.record.id,
                        reaction: entry.record.reaction.clone(),
                        contract: entry.record.contract.clone(),
                        prompt: entry.record.prompt.clone(),
                        trigger_values: entry.record.trigger_values.clone(),
                        allowed_effects: entry.record.allowed_effects.clone(),
                        state: entry.record.state_values.clone(),
                        state_types: entry.record.state_types.clone(),
                        contexts: context_view(&entry.record.contexts, &entry.record.state_values),
                    })
                    .collect();
                Ok(json!({ "pending": pending }))
            }
            InvocationCommand::Complete(args) => {
                let invocation = entries
                    .get_mut(&args.task_id)
                    .with_context(|| format!("unknown task_id {}", args.task_id))?;
                validate_invocation_owner(team, agent, attempt, &invocation.record)?;
                if invocation.record.completed {
                    return Ok(json!({"status":"already_complete"}));
                }
                // Refused rather than failed: the invocation stays open, so
                // the agent can set or keep what it missed in the same task.
                check_contexts_kept(&invocation.record, &args.keep)?;
                // State is the instance's, not the contract's: any task may
                // leave it as it was or change it.
                let effects: BTreeMap<_, _> = invocation
                    .record
                    .writes
                    .iter()
                    .filter(|(name, _)| !invocation.record.state_types.contains_key(*name))
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect();
                validate_contract(&invocation.record.contract, &effects).with_context(|| {
                    format!(
                        "reaction '{}' invocation '{}' failed",
                        invocation.record.reaction, invocation.record.id
                    )
                })?;
                invocation.record.completed = true;
                let writes = invocation.record.writes.clone();
                let completion = invocation
                    .completion
                    .take()
                    .context("invocation completion channel is unavailable")?;
                completion
                    .send(writes)
                    .map_err(|_| anyhow::anyhow!("invocation is no longer active"))?;
                Ok(json!({"status":"complete"}))
            }
        }
    }
}

struct InvocationServer {
    endpoint: String,
    token: String,
    registry: InvocationRegistry,
    shutdown: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl InvocationServer {
    fn start() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .context("failed to bind topology invocation service")?;
        let endpoint = listener.local_addr()?.to_string();
        let token = Uuid::new_v4().to_string();
        let registry = InvocationRegistry::default();
        let thread_registry = registry.clone();
        let thread_token = token.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = shutdown.clone();
        let thread = thread::spawn(move || {
            for connection in listener.incoming() {
                if thread_shutdown.load(Ordering::Acquire) {
                    break;
                }
                match connection {
                    Ok(stream) => {
                        let _ =
                            serve_invocation_connection(stream, &thread_token, &thread_registry);
                    }
                    Err(_) if thread_shutdown.load(Ordering::Acquire) => break,
                    Err(_) => continue,
                }
            }
        });
        Ok(Self {
            endpoint,
            token,
            registry,
            shutdown,
            thread: Some(thread),
        })
    }
}

impl Drop for InvocationServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = TcpStream::connect(&self.endpoint);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve_invocation_connection(
    mut stream: TcpStream,
    token: &str,
    registry: &InvocationRegistry,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut request_line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut request_line)?;
    let response = match serde_json::from_str::<InvocationRequest>(&request_line) {
        Ok(request) if request.token == token => {
            match registry.execute(
                &request.team,
                &request.agent,
                request.attempt.as_deref(),
                request.command,
            ) {
                Ok(result) => InvocationResponse {
                    result: Some(result),
                    error: None,
                },
                Err(error) => InvocationResponse {
                    result: None,
                    error: Some(format!("{error:#}")),
                },
            }
        }
        Ok(_) => InvocationResponse {
            result: None,
            error: Some("invalid topology invocation token".into()),
        },
        Err(error) => InvocationResponse {
            result: None,
            error: Some(format!("invalid topology invocation request: {error}")),
        },
    };
    serde_json::to_writer(&mut stream, &response)?;
    writeln!(stream)?;
    Ok(())
}

fn send_invocation_command(
    context: &TopologyMcpContext,
    command: InvocationCommand,
) -> Result<Value> {
    let mut stream = TcpStream::connect(&context.endpoint)
        .with_context(|| format!("topology runtime '{}' is unavailable", context.endpoint))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let request = InvocationRequest {
        token: context.token.clone(),
        team: context.team.clone(),
        agent: context.agent.clone(),
        attempt: context.attempt.as_ref().map(|attempt| attempt.key.clone()),
        command,
    };
    serde_json::to_writer(&mut stream, &request)?;
    writeln!(stream)?;
    let mut response_line = String::new();
    BufReader::new(stream).read_line(&mut response_line)?;
    let response: InvocationResponse =
        serde_json::from_str(&response_line).context("invalid response from topology runtime")?;
    match (response.result, response.error) {
        (Some(result), None) => Ok(result),
        (_, Some(error)) => Err(anyhow::anyhow!(error)),
        _ => bail!("topology runtime returned an empty response"),
    }
}

pub(crate) fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub(crate) fn mcp_set_port(context: &TopologyMcpContext, arguments: Value) -> Result<Value> {
    send_invocation_command(
        context,
        InvocationCommand::SetPort(serde_json::from_value(arguments)?),
    )
}

pub(crate) fn mcp_set_state(context: &TopologyMcpContext, arguments: Value) -> Result<Value> {
    send_invocation_command(
        context,
        InvocationCommand::SetState(serde_json::from_value(arguments)?),
    )
}

pub(crate) fn mcp_complete(context: &TopologyMcpContext, arguments: Value) -> Result<Value> {
    send_invocation_command(
        context,
        InvocationCommand::Complete(serde_json::from_value(arguments)?),
    )
}

/// What this agent owes right now, for a panel to draw.
///
/// Scoped to the asking agent, so what comes back is what that agent's
/// reactions were wired to see and allowed to set, and nothing else in the run.
pub(crate) fn pending_invocations(context: &TopologyMcpContext) -> Result<Value> {
    send_invocation_command(context, InvocationCommand::Pending)
}

fn panel_context(access: &PanelAccess, team: &str, agent: &str) -> TopologyMcpContext {
    TopologyMcpContext {
        resume: None,
        team: team.to_string(),
        agent: agent.to_string(),
        endpoint: access.endpoint.clone(),
        token: access.token.clone(),
        attempt: None,
    }
}

/// Everything the run's web agents are waiting on, each tagged with whose it is.
///
/// Asked per agent because an invocation is addressed to one, which is also
/// what keeps a panel from being handed work that belongs to a pane.
pub fn panel_pending(access: &PanelAccess, team: &str) -> Result<Vec<Value>> {
    let mut waiting = Vec::new();
    for agent in &access.agents {
        let answer = pending_invocations(&panel_context(access, team, agent))?;
        let Some(items) = answer.get("pending").and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            let mut item = item.clone();
            if let Some(object) = item.as_object_mut() {
                object.insert("agent".to_string(), json!(agent));
            }
            waiting.push(item);
        }
    }
    Ok(waiting)
}

/// What a panel answers with: effect values, state to set, and the contexts
/// it leaves as they were.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PanelValues {
    #[serde(default)]
    pub values: BTreeMap<String, Value>,
    #[serde(default)]
    pub state: BTreeMap<String, Value>,
    #[serde(default)]
    pub keep: Vec<String>,
}

/// Answer one invocation: every value, then complete.
///
/// The whole batch lands as one completion, so a reaction reading several of
/// these ports sees them together — the same reason the runtime drains a tag's
/// writes at once rather than per reaction. A rejected value fails here, before
/// anything is completed, so a bad field cannot leave a half-answered
/// invocation behind.
pub fn panel_submit(
    access: &PanelAccess,
    team: &str,
    agent: &str,
    task_id: &u64,
    answer: &PanelValues,
) -> Result<()> {
    let context = panel_context(access, team, agent);
    for (port, value) in &answer.values {
        send_invocation_command(
            &context,
            InvocationCommand::SetPort(SetPortArgs {
                task_id: *task_id,
                port: port.clone(),
                value: value.clone(),
            }),
        )?;
    }
    for (name, value) in &answer.state {
        send_invocation_command(
            &context,
            InvocationCommand::SetState(SetStateArgs {
                task_id: *task_id,
                name: name.clone(),
                value: value.clone(),
            }),
        )?;
    }
    // Through the same completion as an agent's, so a context the web agent
    // shares must be set above or kept here.
    send_invocation_command(
        &context,
        InvocationCommand::Complete(CompleteArgs {
            task_id: *task_id,
            keep: answer.keep.clone(),
        }),
    )?;
    Ok(())
}

fn validate_invocation_owner(
    team: &str,
    agent: &str,
    attempt: Option<&str>,
    invocation: &InvocationRecord,
) -> Result<()> {
    if invocation.team != team || invocation.agent != agent {
        bail!("invocation does not belong to this topology agent");
    }
    // An earlier worker of an agent with a `per` lifetime has the same name; only the
    // attempt tells it from the worker this invocation was started for.
    // The key is not echoed: the caller that lacks it must not learn it.
    if let Some(expected) = &invocation.attempt {
        if attempt != Some(expected.as_str()) {
            bail!(
                "invocation '{}' belongs to another worker attempt",
                invocation.id
            );
        }
    }
    Ok(())
}

/// The values a refined string type admits, or `None` if `ty` is not one.
///
/// A refinement travels inside the flat type string -- `string in ["a","b"]` --
/// rather than in a field of its own. Nothing downstream has to learn a new
/// shape: the bytecode's `type` stays a string, and connection checking stays
/// equality over the canonical spelling `omarc` emits.
pub(crate) fn string_enum(ty: &str) -> Option<Vec<String>> {
    serde_json::from_str(ty.strip_prefix("string in ")?).ok()
}

/// Whether `ty` is, or wraps, a refined string.
fn is_refined(ty: &str) -> bool {
    leaf_type(ty).starts_with("string in ")
}

/// `ty` with its `list` and `option` wrappers taken off.
fn leaf_type(ty: &str) -> &str {
    for outer in ["list", "option"] {
        if let Some(inner) = generic_inner(ty, outer) {
            return leaf_type(inner);
        }
    }
    ty
}

/// `ty` with each leaf that `names` maps replaced, `list` and `option`
/// wrappers kept: `list<string in ["a"]>` is `list<Decision>` under
/// `{"string in [\"a\"]": "Decision"}`, and the reverse map turns it back.
pub(crate) fn rename_type(ty: &str, names: &BTreeMap<String, String>) -> String {
    for outer in ["list", "option"] {
        if let Some(inner) = generic_inner(ty, outer) {
            return format!("{outer}<{}>", rename_type(inner, names));
        }
    }
    names.get(ty).cloned().unwrap_or_else(|| ty.to_string())
}

fn validate_value(ty: &str, value: &Value) -> Result<()> {
    if let Some(allowed) = string_enum(ty) {
        // The agent acts on this message, so it names every legal answer
        // rather than only reporting that this one was wrong.
        let text = value
            .as_str()
            .with_context(|| format!("expected {ty}, got {value}"))?;
        // `omarc` rejects an empty refinement, so this is reachable only from
        // hand-written bytecode. Say what is wrong with the port rather than
        // offering the agent a choice of nothing.
        if allowed.is_empty() {
            bail!("port type {ty} admits no value");
        }
        if !allowed.iter().any(|option| option == text) {
            // Quoted by serde rather than by hand: an admitted value may itself
            // contain a quote or a newline, and the agent reads this message.
            let options: Vec<String> = allowed.iter().map(|o| json!(o).to_string()).collect();
            bail!("expected one of {}, got {value}", options.join(", "));
        }
        return Ok(());
    }
    if let Some(inner) = generic_inner(ty, "list") {
        let values = value
            .as_array()
            .with_context(|| format!("expected {ty}, got {value}"))?;
        for (index, item) in values.iter().enumerate() {
            validate_value(inner, item)
                .with_context(|| format!("invalid element {index} of {ty}"))?;
        }
        return Ok(());
    }
    if let Some(inner) = generic_inner(ty, "option") {
        return if value.is_null() {
            Ok(())
        } else {
            validate_value(inner, value).with_context(|| format!("invalid value for {ty}"))
        };
    }

    let valid = match ty {
        "signal" => value.is_null(),
        "bool" => value.is_boolean(),
        "int" => value.as_i64().is_some(),
        "float" => value.as_f64().is_some(),
        "string" | "path" | "bytes" => value.is_string(),
        _ => false,
    };
    if !valid {
        bail!("expected {ty}, got {value}");
    }
    Ok(())
}

/// Reject a type the VM cannot check a value against, wherever the fault sits.
///
/// `validate_value` looks inside `list` and `option`, so this has to as well,
/// or a nested fault would reach the agent and be reported against the value
/// it wrote rather than against the port. A refinement that does not parse is
/// a broken port, not a plain `string`; saying so once, here, keeps
/// `string_enum` free to answer "not refined" everywhere downstream. And what
/// the wrappers come off to has to be a type the VM knows: `list<string>>` is
/// a list of nothing it can check.
fn check_type(what: &str, name: &str, ty: &str) -> Result<()> {
    for outer in ["list", "option"] {
        if ty.starts_with(&format!("{outer}<")) {
            let Some(inner) = generic_inner(ty, outer) else {
                bail!("{what} '{name}' has an invalid type '{ty}'");
            };
            return check_type(what, name, inner);
        }
    }
    if let Some(list) = ty.strip_prefix("string in ") {
        return match serde_json::from_str::<Vec<String>>(list) {
            Ok(values) if values.is_empty() => bail!("{what} '{name}' admits no value"),
            Ok(values) => {
                // `omarc` refuses a duplicate; so does the plan, or the bytecode
                // is the one place the rule does not hold.
                let mut seen = BTreeSet::new();
                for value in &values {
                    if !seen.insert(value) {
                        bail!(
                            "{what} '{name}' has a duplicate enum value {}",
                            json!(value)
                        );
                    }
                }
                Ok(())
            }
            Err(error) => bail!("{what} '{name}' has an invalid string refinement: {error}"),
        };
    }
    match ty {
        "signal" | "bool" | "int" | "float" | "string" | "path" | "bytes" => Ok(()),
        _ => bail!("{what} '{name}' has an invalid type '{ty}'"),
    }
}

fn generic_inner<'a>(ty: &'a str, outer: &str) -> Option<&'a str> {
    ty.strip_prefix(outer)?
        .strip_prefix('<')?
        .strip_suffix('>')
        .filter(|inner| !inner.is_empty())
}

#[derive(Debug)]
struct ContractGroup {
    optional: bool,
    alternatives: Vec<(String, Option<Value>)>,
}

fn parse_contract(contract: &str) -> Result<Vec<ContractGroup>> {
    let tokens: Vec<&str> = contract.split_whitespace().collect();
    let mut groups = Vec::new();
    let mut current = Vec::new();
    let mut depth = 0usize;
    for token in tokens {
        match token {
            "(" => {
                depth += 1;
                current.push(token);
            }
            ")" => {
                depth = depth.saturating_sub(1);
                current.push(token);
            }
            "," if depth == 0 => {
                groups.push(parse_contract_group(&current)?);
                current.clear();
            }
            _ => current.push(token),
        }
    }
    if !current.is_empty() {
        groups.push(parse_contract_group(&current)?);
    }
    Ok(groups)
}

fn parse_contract_group(tokens: &[&str]) -> Result<ContractGroup> {
    let mut tokens = tokens.to_vec();
    let optional = tokens.last() == Some(&"?");
    if optional {
        tokens.pop();
    }
    if tokens.first() == Some(&"(") && tokens.last() == Some(&")") {
        tokens.remove(0);
        tokens.pop();
    }
    let mut alternatives = Vec::new();
    for atom in tokens.split(|token| *token == "|") {
        let name = atom
            .first()
            .context("empty effect in contract")?
            .to_string();
        let constant = if atom.get(1) == Some(&"=") {
            Some(parse_literal(
                atom.get(2).context("missing constant value")?,
            )?)
        } else {
            None
        };
        alternatives.push((name, constant));
    }
    Ok(ContractGroup {
        optional,
        alternatives,
    })
}

fn parse_literal(value: &str) -> Result<Value> {
    match value {
        "true" => Ok(Value::Bool(true)),
        "false" => Ok(Value::Bool(false)),
        _ if value.parse::<i64>().is_ok() => Ok(json!(value.parse::<i64>()?)),
        _ => Ok(Value::String(value.trim_matches('"').to_string())),
    }
}

fn validate_contract(contract: &str, writes: &BTreeMap<String, Value>) -> Result<()> {
    let groups = parse_contract(contract)?;
    let contract_ports: BTreeSet<_> = groups
        .iter()
        .flat_map(|group| group.alternatives.iter())
        .map(|(port, _)| port.as_str())
        .collect();
    if let Some(port) = writes
        .keys()
        .find(|port| !contract_ports.contains(port.as_str()))
    {
        bail!("effect '{port}' is not permitted by contract '{contract}'");
    }

    for group in groups {
        let present: Vec<_> = group
            .alternatives
            .iter()
            .filter(|(port, _)| writes.contains_key(port))
            .collect();
        if (!group.optional && present.len() != 1) || (group.optional && present.len() > 1) {
            bail!("effect contract '{}' is not satisfied", contract);
        }
        for (port, constant) in present {
            if let Some(expected) = constant {
                if writes.get(port) != Some(expected) {
                    bail!("effect '{}' must equal {}", port, expected);
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct InvocationSpec {
    id: u64,
    reaction_id: String,
    agent: String,
    trigger_values: BTreeMap<String, Value>,
    allowed_effects: BTreeMap<String, String>,
    /// Each effect's type as the program spelled it -- `Decision` where
    /// `allowed_effects` has the refinement it expands to -- which is what the
    /// agent is shown.
    effect_names: BTreeMap<String, String>,
    /// The named types those spellings use, so the agent is told what the
    /// schema said about each.
    types: BTreeMap<String, TypeState>,
    /// Its instance's state variables as they stand, which a code body reads
    /// as `self` and a prompt as `$(name)`.
    state_values: BTreeMap<String, Value>,
    /// Their types, which is what a task's write of one is checked against.
    state_types: BTreeMap<String, String>,
    /// The contexts this invocation's agent shares, each with the directive
    /// the program gave it: handed to the agent ahead of its prompt.
    contexts: BTreeMap<String, Option<String>>,
    /// Every context of the instance, shared with this agent or not: a write
    /// to one is held to `CONTEXT_LIMIT`.
    context_names: BTreeSet<String>,
    contract: String,
    prompt: String,
    /// What the program allows this invocation, overriding the run-wide
    /// timeout. `None` means the program said nothing and the run's applies.
    within: Option<Duration>,
    /// The tag it runs in, which an agent's `per tag` lifetime counts.
    tag: Tag,
}

trait ReactionExecutor: Sync {
    fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>>;
    fn record_attempt(&self, _invocation: &InvocationSpec, _record: Value) {}
}

/// What an expired deadline means, which the effect contract decides.
///
/// A contract that writing nothing already satisfies is one the program said
/// may stay silent, so the tag completes with no writes and whatever those
/// ports feed simply does not fire. A contract that requires an effect has not
/// been honoured and there is no value to invent, so the run stops rather than
/// carrying on from a decision nobody made.
fn expired(invocation: &InvocationSpec, deadline: Duration) -> Result<BTreeMap<String, Value>> {
    match validate_contract(&invocation.contract, &BTreeMap::new()) {
        Ok(()) => Ok(BTreeMap::new()),
        Err(_) => bail!(
            "reaction '{}' invocation '{}': '{}' did not answer within {:?}, and contract '{}' requires an effect",
            invocation.reaction_id,
            invocation.id,
            // A body has no agent to name.
            if invocation.agent.is_empty() { "its body" } else { &invocation.agent },
            deadline,
            invocation.contract
        ),
    }
}

/// What a prompt's `$(name)` may read: the triggers present, and the
/// instance's state as the invocation found it. Both are qualified names, and
/// the compiler keeps them apart.
fn prompt_values(invocation: &InvocationSpec) -> BTreeMap<String, Value> {
    invocation
        .trigger_values
        .iter()
        .chain(&invocation.state_values)
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// The most a context may hold, in bytes. It is handed whole to every task
/// that shares it, so it is a note to keep short, not a transcript.
const CONTEXT_LIMIT: usize = 16 * 1024;

fn check_context_size(name: &str, value: &Value) -> Result<()> {
    let size = value.as_str().map_or(0, str::len);
    if size > CONTEXT_LIMIT {
        bail!(
            "context '{}' would be {size} bytes, over its {CONTEXT_LIMIT}-byte limit; condense it and set it again",
            unqualified(name)
        );
    }
    Ok(())
}

/// A name as the team wrote it, without the instance path before it.
fn unqualified(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// The contexts an agent shares, as its invocation carries them: each one's
/// directive (`null` when the program gave none) and what it holds now.
fn context_view(
    contexts: &BTreeMap<String, Option<String>>,
    state: &BTreeMap<String, Value>,
) -> BTreeMap<String, Value> {
    contexts
        .iter()
        .map(|(name, directive)| {
            let value = state.get(name).cloned().unwrap_or_else(|| json!(""));
            (
                name.clone(),
                json!({"directive": directive, "value": value}),
            )
        })
        .collect()
}

/// Whether an invocation may complete as far as its contexts go: every
/// context its agent shares was set in it with `omar_set_state` or is named
/// in `keep`, and `keep` names nothing else. That is the whole of the
/// write-back a context asks for, enforced rather than requested.
fn check_contexts_kept(record: &InvocationRecord, keep: &[String]) -> Result<()> {
    let shared: BTreeMap<String, String> = record
        .contexts
        .keys()
        .map(|name| (name.clone(), "string".to_string()))
        .collect();
    let mut kept = BTreeSet::new();
    for name in keep {
        let Some((full, _)) = resolve_name(&shared, name) else {
            let names: Vec<&str> = record.contexts.keys().map(|n| unqualified(n)).collect();
            if names.is_empty() {
                bail!("keep names '{name}', but this task shares no context");
            }
            bail!(
                "keep names '{name}', which is not a context this task shares; it shares: {}",
                names.join(", ")
            );
        };
        kept.insert(full.clone());
    }
    let missing: Vec<&str> = record
        .contexts
        .keys()
        .filter(|name| !record.writes.contains_key(*name) && !kept.contains(*name))
        .map(|name| unqualified(name))
        .collect();
    if !missing.is_empty() {
        let quoted: Vec<String> = missing.iter().map(|n| format!("\"{n}\"")).collect();
        let named: Vec<String> = missing.iter().map(|n| format!("`{n}`")).collect();
        bail!(
            "not complete: set {} with omar_set_state, or keep {}: omar_complete(task_id, keep: [{}])",
            named.join(" and "),
            if missing.len() == 1 { "it" } else { "them" },
            quoted.join(", ")
        );
    }
    Ok(())
}

/// The registry's record of an agent invocation, its prompt rendered.
fn invocation_record(team: &str, invocation: &InvocationSpec) -> Result<InvocationRecord> {
    Ok(InvocationRecord {
        id: invocation.id,
        team: team.to_string(),
        agent: invocation.agent.clone(),
        reaction: invocation.reaction_id.clone(),
        contract: invocation.contract.clone(),
        allowed_effects: invocation.allowed_effects.clone(),
        trigger_values: invocation.trigger_values.clone(),
        state_values: invocation.state_values.clone(),
        state_types: invocation.state_types.clone(),
        contexts: invocation.contexts.clone(),
        context_names: invocation.context_names.clone(),
        prompt: render_prompt(&invocation.prompt, &prompt_values(invocation))?,
        writes: BTreeMap::new(),
        completed: false,
        attempt: None,
    })
}

struct AgentReactionExecutor {
    client: TmuxClient,
    team: String,
    registry: InvocationRegistry,
    timeout: Duration,
    run_started: Instant,
    /// Agents on the `web` backend. Nothing was spawned for them, so there is
    /// no pane to deliver a prompt to — the registration is the work item, and
    /// a web client picks it up from there.
    web: BTreeSet<String>,
    /// Process workers and the run's attempt log. Web agents need neither.
    scoped: Option<ScopedWorkers>,
}

pub(crate) const MAX_TASK_RETRIES: u32 = 2;
const WORKER_POLL: Duration = Duration::from_millis(250);
const TURN_GRACE: Duration = Duration::from_secs(2);

#[derive(Default)]
struct TaskTurnWatch {
    seen: Option<String>,
    idle: Option<(String, Instant)>,
    reminded: bool,
    waiting: Option<UsageWait>,
    limit_seen: Option<String>,
    backoff: Option<Duration>,
}

struct UsageWait {
    until: Instant,
    backoff: Duration,
    automatic: bool,
}

struct TaskClock {
    started: Instant,
    paused: Duration,
    waiting: Option<Instant>,
}
impl TaskClock {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            paused: Duration::ZERO,
            waiting: None,
        }
    }
    fn elapsed(&self) -> Duration {
        self.waiting
            .unwrap_or_else(Instant::now)
            .duration_since(self.started)
            .saturating_sub(self.paused)
    }
    fn pause(&mut self) {
        self.waiting.get_or_insert_with(Instant::now);
    }
    fn resume(&mut self) {
        if let Some(since) = self.waiting.take() {
            self.paused += since.elapsed();
        }
    }
}

impl TaskTurnWatch {
    fn stopped(&mut self, ended: Option<String>, now: Instant) -> bool {
        let Some(ended) = ended.filter(|id| Some(id) != self.seen.as_ref()) else {
            self.idle = None;
            return false;
        };
        let (id, since) = self.idle.get_or_insert_with(|| (ended.clone(), now));
        if *id != ended {
            *id = ended;
            *since = now;
        }
        now.duration_since(*since) >= TURN_GRACE
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{reason}")]
struct WorkerDied {
    reason: String,
    conversation: String,
}

fn retry_task(invocation: &InvocationSpec, retries: u32, death: &WorkerDied) -> Result<()> {
    anyhow::ensure!(
        retries < MAX_TASK_RETRIES,
        "agent '{}' task {} ({}): worker died after {MAX_TASK_RETRIES} retries: {}",
        invocation.agent,
        invocation.id,
        invocation.reaction_id,
        death.reason
    );
    Ok(())
}

impl ReactionExecutor for AgentReactionExecutor {
    fn record_attempt(&self, invocation: &InvocationSpec, mut record: Value) {
        if let Some(workers) = &self.scoped {
            record["run"] = json!(workers.run);
            record["logical_agent"] = json!(invocation.agent);
            record["invocations"] = json!([served_invocation(invocation)]);
            workers.record(&record);
            workers.publish(&invocation.agent, &record);
        }
    }
    fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
        let workers = self.scoped.as_ref().filter(|workers| {
            workers
                .states
                .get(&invocation.agent)
                .is_some_and(|agent| needs_agent_process(&agent.backend))
        });
        // One invocation of such an agent at a time: they share a
        // conversation, and the window an invocation falls in depends on the
        // ones before it.
        let mut slot = workers.map(|workers| workers.slot(&invocation.agent));
        // Decided here, before the invocation can be answered, so only the
        // worker of its window can answer.
        let mut admitted = match (workers, slot.as_mut()) {
            (Some(workers), Some(slot)) => Some(workers.admit(&invocation, slot)?),
            _ => None,
        };
        let mut clock = TaskClock::new();
        let deadline = invocation.within.unwrap_or(self.timeout);
        let mut delivered = invocation.clone();
        for retries in 0..=MAX_TASK_RETRIES {
            let mut record = invocation_record(&self.team, &delivered)?;
            record.attempt = admitted
                .as_ref()
                .filter(|_| workers.is_some_and(|w| w.states[&invocation.agent].lifetime.is_some()))
                .map(|(attempt, _)| attempt.key.clone());
            let rendered = record.prompt.clone();
            let completion = self.registry.register(record)?;
            let result = match (workers, slot.as_mut(), &admitted) {
                (Some(workers), Some(slot), Some((attempt, fresh))) => {
                    workers.run(&invocation, attempt, *fresh, retries, slot, |window| {
                        self.answer(
                            &delivered,
                            &rendered,
                            completion,
                            Some(window),
                            (deadline, &mut clock),
                            retries,
                        )
                    })
                }
                _ => self
                    .answer(
                        &delivered,
                        &rendered,
                        completion,
                        None,
                        (deadline, &mut clock),
                        retries,
                    )
                    .map(|(writes, _)| writes),
            };
            // Removing the entry discards both port and state writes. A new
            // wire id also fences delayed calls from a non-per worker.
            self.registry.remove(&delivered.id);
            let death = match &result {
                Err(error) => error.downcast_ref::<WorkerDied>(),
                _ => None,
            };
            let Some(death) = death else {
                return result;
            };
            retry_task(&invocation, retries, death)?;
            if clock.elapsed() >= deadline {
                return expired(&invocation, deadline);
            }
            let workers = workers.context("dead worker has no launcher")?;
            let agent = &workers.states[&invocation.agent];
            let backend = crate::backend::by_name(&agent.backend).context("unknown backend")?;
            admitted = Some((
                manager::TopologyAttempt {
                    id: Uuid::new_v4().to_string(),
                    key: Uuid::new_v4().to_string(),
                    native_session: if agent.lifetime.is_some() {
                        backend.new_conversation_id()
                    } else {
                        death.conversation.clone()
                    },
                },
                true,
            ));
            delivered.id = next_task_id()?;
        }
        unreachable!("retry limit is checked after every death")
    }
}

impl AgentReactionExecutor {
    /// Hand the invocation to its agent and wait for the answer, watching the
    /// agent's worker meanwhile. The flag is whether the deadline expired, in
    /// which case the writes are empty and the agent may still be working on
    /// it. A web agent has no pane to deliver to: the registration is the work
    /// item a panel asks for, and a client answering is a slow agent.
    fn answer(
        &self,
        invocation: &InvocationSpec,
        rendered: &str,
        completion: mpsc::Receiver<BTreeMap<String, Value>>,
        mut window: Option<&mut Window>,
        budget: (Duration, &mut TaskClock),
        retries: u32,
    ) -> Result<(BTreeMap<String, Value>, bool)> {
        let (deadline, clock) = budget;
        let mut turns = TaskTurnWatch::default();
        if !self.web.contains(&invocation.agent) {
            if let Some(window) = window.as_deref_mut() {
                self.client
                    .finish_backend_startup(&window.session, WORKER_START_TIMEOUT)?;
                self.watch_worker(window, None)?;
                let ended = window.ended_turn(&self.client);
                turns.seen = window
                    .blocked_input(&self.client)
                    .map(|id| format!("input:{id}"))
                    .or(ended);
            }
            let mut message = invocation_message(invocation, rendered)?;
            if retries > 0 {
                let conversation = window.as_ref().map_or("fresh conversation", |w| {
                    w.record["conversation_mode"]
                        .as_str()
                        .unwrap_or("fresh conversation")
                });
                message.push_str(&format!("\n\nThis is retry {retries}/{MAX_TASK_RETRIES} after your worker died. Your workspace may contain files from the earlier attempt. Earlier buffered port/state writes were discarded; answer this task_id again. Conversation: {conversation}."));
            }
            let session = self.client.session_for(&invocation.agent);
            let delivery = self.client.deliver_prompt_until(
                &session,
                &message,
                &DeliveryOptions::default(),
                &|| self.registry.answered(&invocation.id),
            );
            if !self.registry.answered(&invocation.id) {
                if let Some(window) = window.as_deref_mut() {
                    if delivery.is_err() {
                        self.watch_worker(window, None)?;
                    }
                }
                delivery
                    .with_context(|| format!("failed to deliver {}", invocation.reaction_id))?;
            }
        }
        loop {
            anyhow::ensure!(
                self.run_started.elapsed() < self.timeout,
                "run-wide timeout exceeded while waiting for agent '{}': {}s",
                invocation.agent,
                self.timeout.as_secs()
            );
            let remaining = deadline.saturating_sub(clock.elapsed());
            match completion.recv_timeout(remaining.min(WORKER_POLL)) {
                Ok(writes) => {
                    if let Some(window) = window.as_deref_mut() {
                        self.end_usage_wait(window, clock);
                    }
                    return Ok((writes, false));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("topology invocation service stopped unexpectedly")
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if let Some(window) = window.as_deref_mut() {
                let status = self.watch_worker(window, Some((invocation, &mut turns, clock)));
                // Completion can race the process observation. A worker that
                // completed before exiting has already answered this task.
                if let Ok(writes) = completion.try_recv() {
                    self.end_usage_wait(window, clock);
                    return Ok((writes, false));
                }
                status?;
            }
            if clock.elapsed() >= deadline {
                return expired(invocation, invocation.within.unwrap_or(self.timeout))
                    .map(|writes| (writes, true));
            }
        }
    }

    fn end_usage_wait(&self, window: &mut Window, clock: &mut TaskClock) {
        clock.resume();
        if let Some(wait) = window.record["usage_waits"]
            .as_array_mut()
            .and_then(|v| v.last_mut())
            .filter(|v| v["ended_at"].is_null())
        {
            wait["ended_at"] = json!(chrono::Utc::now().to_rfc3339());
            self.publish_wait(window);
        }
    }

    fn publish_wait(&self, window: &Window) {
        if let Some(workers) = &self.scoped {
            workers.record(&window.record);
            workers.publish(&window.agent, &window.record);
        }
    }

    /// The single periodic check for an open task's worker.
    fn watch_worker(
        &self,
        window: &mut Window,
        mut task: Option<(&InvocationSpec, &mut TaskTurnWatch, &mut TaskClock)>,
    ) -> Result<()> {
        // This decision precedes both process-death and stopped-turn recovery.
        // A limited worker must never be interrupted, restarted, or reminded.
        // Keep the native conversation before protocol probes can race a death.
        window.observe_conversation(&self.client);
        let ended = window.ended_turn(&self.client);
        if let Some(credit) = window.out_of_credit(&self.client) {
            return Err(credit.into());
        }
        if let Some((invocation, turns, clock)) = task.as_mut() {
            if !self.registry.answered(&invocation.id) {
                let limit = window.usage_limit(&self.client);
                let native_retry = window.record["backend"]
                    .as_str()
                    .and_then(crate::backend::by_name)
                    .is_some_and(|backend| {
                        matches!(
                            backend.kind(),
                            crate::backend::Kind::Pi | crate::backend::Kind::Opencode
                        )
                    });
                let native_resumed = native_retry && ended.is_none() && limit.is_none();
                if let Some((identity, limit)) = limit {
                    if turns.limit_seen.as_ref() != Some(&identity) {
                        turns.limit_seen = Some(identity);
                        if turns.waiting.is_none() {
                            clock.pause();
                            let backoff = turns.backoff.unwrap_or(Duration::from_secs(5));
                            if limit.resets_at.is_none() {
                                turns.backoff = Some((backoff * 2).min(Duration::from_secs(300)));
                            }
                            turns.waiting = Some(UsageWait {
                                until: Instant::now() + limit.delay(backoff),
                                backoff,
                                automatic: window.record["backend"]
                                    .as_str()
                                    .and_then(crate::backend::by_name)
                                    .is_some_and(|backend| {
                                        backend.kind() == crate::backend::Kind::Claude
                                    }),
                            });
                            let waits = window
                                .record
                                .as_object_mut()
                                .unwrap()
                                .entry("usage_waits")
                                .or_insert_with(|| json!([]));
                            waits.as_array_mut().unwrap().push(json!({
                                "task_id": invocation.id, "reaction": invocation.reaction_id,
                                "started_at": chrono::Utc::now().to_rfc3339(), "resets_at": limit.resets_at,
                            }));
                            self.publish_wait(window);
                        }
                    }
                } else if turns.waiting.is_none() {
                    turns.limit_seen = None;
                }
                if let Some(wait) = turns.waiting.as_mut() {
                    turns.idle = None;
                    if native_resumed {
                        // Native work clears the limit before the turn ends.
                        turns.waiting = None;
                        self.end_usage_wait(window, clock);
                        return self.watch_worker(window, None);
                    }
                    if Instant::now() < wait.until {
                        return Ok(());
                    }
                    if native_retry && ended.is_none() {
                        let status = self.watch_worker(window, None);
                        if status.is_err() {
                            self.end_usage_wait(window, clock);
                        }
                        status?;
                        return Ok(());
                    }
                    let unknown = window.record["usage_waits"]
                        .as_array()
                        .and_then(|v| v.last())
                        .is_some_and(|v| v["resets_at"].is_null());
                    if wait.automatic && unknown {
                        // Claude owns automatic continuation; never send keys or prompts.
                        let screen = self
                            .client
                            .capture_pane_plain(&window.session, 0)
                            .unwrap_or_default();
                        if crate::backend::UsageLimit::from_error(&json!(screen)).is_some() {
                            wait.backoff = (wait.backoff * 2).min(Duration::from_secs(300));
                            wait.until = Instant::now() + wait.backoff;
                            return Ok(());
                        }
                    } else if !wait.automatic {
                        let message = format!("OMAR CONTINUE\ntask_id: {}\nThe usage limit may have reset. Continue the same task, set its required outputs, and call omar_complete with this task_id.", invocation.id);
                        let delivery = self.client.deliver_prompt_until(
                            &window.session,
                            &message,
                            &DeliveryOptions::default(),
                            &|| self.registry.answered(&invocation.id),
                        );
                        if delivery.is_err() {
                            self.end_usage_wait(window, clock);
                            self.watch_worker(window, None)?;
                        }
                        delivery?;
                    }
                    turns.seen = ended.clone();
                    turns.waiting = None;
                    clock.resume();
                    if let Some(wait) = window.record["usage_waits"]
                        .as_array_mut()
                        .and_then(|v| v.last_mut())
                    {
                        wait["ended_at"] = json!(chrono::Utc::now().to_rfc3339());
                    }
                    self.publish_wait(window);
                    return Ok(());
                }
            }
        }
        let tree = if self.client.has_session(&window.session)? {
            self.client
                .session_process_tree(&window.session)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if tree.is_empty() {
            anyhow::ensure!(
                !self
                    .scoped
                    .as_ref()
                    .is_some_and(|workers| workers.launcher.shutdown.load(Ordering::Acquire)),
                "runtime is shutting down"
            );
            return Err(WorkerDied {
                reason: "worker process exited".into(),
                conversation: window.conversation.clone(),
            }
            .into());
        }
        window.sessions.extend(crate::process::sessions(&tree));
        window.tree.extend(tree);
        window.tree.sort_unstable();
        window.tree.dedup();
        if let Some((invocation, turns, _)) = task {
            let input = window.blocked_input(&self.client);
            let stopped = input.as_ref().map(|id| format!("input:{id}")).or(ended);
            if !self.registry.answered(&invocation.id) && turns.stopped(stopped, Instant::now()) {
                if turns.reminded {
                    return Err(WorkerDied {
                        reason: "stopped without finishing after a reminder".into(),
                        conversation: window.conversation.clone(),
                    }
                    .into());
                }
                let message = format!(
                    "OMAR REMINDER\ntask_id: {}\nYour task is still open. Required output contract: {}. Outputs: {}. Finish the task, set its required outputs with omar_set_port, and call omar_complete with this task_id. No questions will be answered. Continue now.",
                    invocation.id, invocation.contract, serde_json::to_string(&invocation.effect_names)?,
                );
                if let Some(id) = &input {
                    if !window.answer_input(&self.client, id, &message)? {
                        return Ok(());
                    }
                } else {
                    self.client.deliver_prompt_until(
                        &window.session,
                        &message,
                        &DeliveryOptions::default(),
                        &|| self.registry.answered(&invocation.id),
                    )?;
                }
                turns.seen = turns.idle.take().map(|(id, _)| id);
                turns.reminded = true;
                window.record["outcome"] = json!("open");
                let logical_id = window.record["invocations"]
                    .as_array()
                    .and_then(|items| items.last())
                    .map(|item| item["invocation_id"].clone());
                let reminders = window
                    .record
                    .as_object_mut()
                    .unwrap()
                    .entry("reminders")
                    .or_insert_with(|| json!([]));
                reminders.as_array_mut().unwrap().push(json!({
                    "task_id": invocation.id, "reaction": invocation.reaction_id,
                    "invocation_id": logical_id,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                    "reason": if input.is_some() { "asked the user a question" } else { "stopped without finishing" },
                }));
                if let Some(workers) = &self.scoped {
                    workers.record(&window.record);
                    workers.publish(&window.agent, &window.record);
                }
            }
        }
        Ok(())
    }
}

fn worker_conversation(client: &TmuxClient, session: &str) -> Option<String> {
    let backend = client.session_backend(session)?;
    let backend = crate::backend::by_name(&backend)?;
    let stamp = client.session_delivery(session);
    let target = crate::backend::Target {
        name: session,
        pane_pid: client.get_pane_pid(session).ok()?,
        stamp: stamp.as_deref(),
    };
    backend
        .conversation_id(&target)
        .or_else(|| crate::backend::managed::conversation_id(&target))
}

/// Every process agent's window: a worker, and a native conversation.
///
/// For an agent with a `per` lifetime, a window serves the invocations the
/// lifetime lets share it: `per task` serves one, `per tag` all the agent's
/// invocations in a tag, `per 5 tasks` five. The first is started, proved to
/// have opened a new conversation -- the one the attempt named, or, for a
/// backend that mints its own id, one no earlier attempt reported -- and
/// handed the invocation. Later ones are handed to the same worker, and the
/// conversation it reports must still be that one before an invocation's
/// writes are released. The window ends lazily, when an invocation falls
/// outside it, or at once when its count of tasks is spent, or when the run
/// ends or an invocation in it fails: its process tree is stopped and seen
/// gone. If any of that cannot be made sure of, the invocation fails: there is
/// no falling back to a worker that already has a conversation.
///
/// An agent without `per` has one window for the run, adopting the worker
/// started with it. A worker that dies with a task open is stopped and
/// replaced, and the task delivered again (#411): a `per` agent's in a fresh,
/// verified window, an unscoped agent's resuming its native conversation,
/// whose history is kept for that. A worker that dies while idle fails the
/// run.
struct ScopedWorkers {
    states: BTreeMap<String, AgentState>,
    /// Each agent's open window, if any. Locked for a whole invocation.
    windows: BTreeMap<String, Mutex<Option<Window>>>,
    /// Confirmed stops, separate from unscoped agents' retained session history.
    stopped: Mutex<BTreeSet<String>>,
    launcher: AgentLauncher,
    run: String,
    /// The run's record, whose `sessions` holds a `per` window's session from
    /// before its worker is launched until it is seen stopped, so a run that
    /// dies in between leaves it for `--replace` and snapshots to find, and
    /// one that does not leaves nothing to clean. Unscoped session names
    /// remain in the record as history after a confirmed stop.
    deployment: Arc<Mutex<deploy::DeploymentRecord>>,
    /// One JSON line per window as it starts, once its conversation is
    /// checked, after each invocation it serves, and as it ends. The last line
    /// of an `attempt_id` is its record.
    log: PathBuf,
    /// Every conversation an attempt has reported, seeded from `log`, which
    /// outlives the process: a backend that mints its own id must never hand
    /// one back. An error is a log that could not be read; it refuses every
    /// minted id rather than trust a partial set.
    seen: Mutex<Result<BTreeSet<String>, String>>,
    publisher: Option<crate::diagram::DiagramPublisher>,
}

/// One agent's open window: the worker, and what is needed to stop it and to
/// tell whether the next invocation still belongs to it.
struct Window {
    agent: String,
    attempt: manager::TopologyAttempt,
    session: String,
    /// Every process the worker had, captured while it runs. Identities, not
    /// bare pids: confirming the stop must not mistake a recycled pid. With
    /// the sessions they ran in, which a process its parent leaves behind
    /// keeps.
    tree: Vec<(u32, String)>,
    sessions: BTreeSet<u32>,
    since: std::time::SystemTime,
    /// The conversation the worker reported when it started.
    conversation: String,
    used: Used,
    failed: bool,
    /// What `attempts.jsonl` holds for it.
    record: Value,
    turn_file: PathBuf,
    turn_monitor: Option<Box<dyn crate::backend::TurnMonitor>>,
}

impl Window {
    fn out_of_credit(&mut self, client: &TmuxClient) -> Option<crate::backend::OutOfCredit> {
        if let Some(credit) = self
            .turn_monitor
            .as_mut()
            .and_then(|monitor| monitor.out_of_credit())
        {
            return Some(credit);
        }
        let backend = crate::backend::by_name(self.record["backend"].as_str()?)?;
        if backend.kind() == crate::backend::Kind::Claude {
            let screen = client.capture_pane_plain(&self.session, 0).ok()?;
            return Self::claude_credit_error(&screen);
        }
        let stamp = client.session_delivery(&self.session);
        backend.out_of_credit(
            &crate::backend::Target {
                name: &self.session,
                pane_pid: client.get_pane_pid(&self.session).ok()?,
                stamp: stamp.as_deref(),
            },
            &self.turn_file,
        )
    }

    fn claude_credit_error(screen: &str) -> Option<crate::backend::OutOfCredit> {
        // Match Claude's error notice, not a task discussing billing.
        let mut lines = screen.lines().peekable();
        while let Some(line) = lines.next() {
            let text = line.trim_start();
            let text = text.strip_prefix('⎿').unwrap_or(text).trim_start();
            let lower = text.to_lowercase();
            if !lower.starts_with("api error:")
                && !lower.starts_with("error:")
                && !lower.starts_with("credit balance is too low")
            {
                continue;
            }
            let column = line.chars().count() - text.chars().count();
            let mut notice = text.to_owned();
            while let Some(line) = lines.peek().filter(|line| {
                !line.trim().is_empty()
                    && line.chars().take_while(|c| c.is_whitespace()).count() >= column
                    && !line.trim_start().starts_with(['❯', '⏺', '⎿', '─'])
            }) {
                notice.push(' ');
                notice.push_str(line.trim());
                lines.next();
            }
            if let Some(credit) = crate::backend::OutOfCredit::from_error(&json!(notice)) {
                return Some(credit);
            }
        }
        None
    }

    fn blocked_input(&self, client: &TmuxClient) -> Option<String> {
        if let Some(monitor) = &self.turn_monitor {
            return monitor.blocked_input();
        }
        let backend = crate::backend::by_name(self.record["backend"].as_str()?)?;
        let stamp = client.session_delivery(&self.session);
        backend.blocked_input(
            &crate::backend::Target {
                name: &self.session,
                pane_pid: client.get_pane_pid(&self.session).ok()?,
                stamp: stamp.as_deref(),
            },
            &self.turn_file,
        )
    }

    fn answer_input(&mut self, client: &TmuxClient, id: &str, text: &str) -> Result<bool> {
        if let Some(monitor) = &mut self.turn_monitor {
            return monitor.answer_input(id, text);
        }
        let backend = crate::backend::by_name(
            self.record["backend"]
                .as_str()
                .context("worker has no backend")?,
        )
        .context("unknown worker backend")?;
        let stamp = client.session_delivery(&self.session);
        backend.answer_input(
            &crate::backend::Target {
                name: &self.session,
                pane_pid: client.get_pane_pid(&self.session)?,
                stamp: stamp.as_deref(),
            },
            &self.turn_file,
            id,
            text,
        )
    }

    fn usage_limit(&mut self, client: &TmuxClient) -> Option<(String, crate::backend::UsageLimit)> {
        if let Some(limit) = self
            .turn_monitor
            .as_mut()
            .and_then(|monitor| monitor.usage_limit())
        {
            return Some((Uuid::new_v4().to_string(), limit));
        }
        let backend = crate::backend::by_name(self.record["backend"].as_str()?)?;
        if backend.kind() == crate::backend::Kind::Claude {
            let screen = client
                .capture_pane_plain(&self.session, 0)
                .ok()?
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            // Read only the live screen, never old scrollback.
            if !screen.contains("Continuing automatically") {
                return None;
            }
            let limit = crate::backend::UsageLimit::from_error(&json!(screen))?;
            // Progress below a stale notice must not start the same wait again.
            return Some((format!("claude:{:?}", limit.resets_at), limit));
        }
        let stamp = client.session_delivery(&self.session);
        backend.usage_limit(
            &crate::backend::Target {
                name: &self.session,
                pane_pid: client.get_pane_pid(&self.session).ok()?,
                stamp: stamp.as_deref(),
            },
            &self.turn_file,
        )
    }

    fn ended_turn(&mut self, client: &TmuxClient) -> Option<String> {
        let backend = crate::backend::by_name(self.record["backend"].as_str()?)?;
        let stamp = client.session_delivery(&self.session);
        let target = crate::backend::Target {
            name: &self.session,
            pane_pid: client.get_pane_pid(&self.session).ok()?,
            stamp: stamp.as_deref(),
        };
        if self.turn_monitor.is_none() {
            self.turn_monitor = backend.turn_monitor(&target);
        }
        match self.turn_monitor.as_mut() {
            Some(monitor) => monitor.ended_turn(),
            None => backend.ended_turn(&target, &self.turn_file),
        }
    }
    fn observe_conversation(&mut self, client: &TmuxClient) {
        if self.record["lifetime"].is_null() {
            if let Some(native) = worker_conversation(client, &self.session) {
                self.conversation = native;
                self.record["native_session_id"] = json!(self.conversation);
                if self.record["retry"].as_u64().is_some_and(|retry| retry > 0) {
                    self.record["conversation_mode"] =
                        json!(if self.record["resume_session_id"] == self.conversation {
                            "resumed native conversation"
                        } else {
                            "fresh conversation (earlier context unavailable)"
                        });
                }
            }
        }
    }
}

/// What every process of an attempt's worker inherits, as `NAME=<attempt id>`.
const ATTEMPT_MARKER: &str = "OMAR_ATTEMPT";
/// How long a stopped worker's processes get to be seen gone.
const WORKER_STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a started worker gets to open its conversation.
const WORKER_START_TIMEOUT: Duration = Duration::from_secs(60);

/// How much of a window its invocations have used. Counts only the agent's
/// own invocations, and the tags they run in: another agent's tags never reach
/// it.
#[derive(Debug, Clone, Copy)]
struct Used {
    tasks: u32,
    tags: u32,
    last_tag: Tag,
}

impl Used {
    /// A window opened by an invocation at `tag`.
    fn first(tag: Tag) -> Self {
        Self {
            tasks: 1,
            tags: 1,
            last_tag: tag,
        }
    }

    /// Whether an invocation at `tag` falls outside the window. Invocations
    /// in a tag already counted share it: a `per tag` window ends only when
    /// the agent runs in a tag after it.
    fn spent(&self, lifetime: Lifetime, tag: Tag) -> bool {
        match lifetime.unit {
            LifetimeUnit::Task => self.tasks >= lifetime.count,
            LifetimeUnit::Tag => tag != self.last_tag && self.tags >= lifetime.count,
        }
    }

    /// Count an invocation at `tag` that is inside the window.
    fn take(&mut self, tag: Tag) {
        self.tasks += 1;
        if tag != self.last_tag {
            self.tags += 1;
            self.last_tag = tag;
        }
    }

    /// Whether no later invocation can join: the tasks are all used. Tags
    /// cannot say, since the agent may run again in the same one.
    fn exhausted(&self, lifetime: Lifetime) -> bool {
        lifetime.unit == LifetimeUnit::Task && self.tasks >= lifetime.count
    }
}

fn served_invocation(invocation: &InvocationSpec) -> Value {
    json!({
        "invocation_id": invocation.id,
        "reaction": invocation.reaction_id,
        "tag": {"timestamp": invocation.tag.timestamp, "microstep": invocation.tag.microstep},
    })
}

impl ScopedWorkers {
    fn new(
        state: &VmState,
        launcher: AgentLauncher,
        deployment: Arc<Mutex<deploy::DeploymentRecord>>,
        publisher: Option<crate::diagram::DiagramPublisher>,
    ) -> Result<Option<Self>> {
        let states: BTreeMap<String, AgentState> = state
            .agents
            .iter()
            .filter(|(_, agent)| !is_web_backend(&agent.backend))
            .map(|(name, agent)| (name.clone(), agent.clone()))
            .collect();
        let log = launcher.runtime_dir.join("attempts.jsonl");
        let run = deployment
            .lock()
            .map_err(|_| anyhow::anyhow!("deployment record lock poisoned"))?
            .deployment_id
            .clone();
        Ok((!states.is_empty()).then(|| Self {
            windows: states
                .keys()
                .map(|name| (name.clone(), Mutex::new(None)))
                .collect(),
            states,
            stopped: Mutex::new(BTreeSet::new()),
            seen: Mutex::new(reported_conversations(&log)),
            log,
            launcher,
            run,
            deployment,
            publisher,
        }))
    }

    /// Record `agent`'s session as one the run must clean up, or, with
    /// `None`, as gone.
    fn track(&self, agent: &str, session: Option<&str>) -> Result<()> {
        if session.is_some() {
            self.stopped.lock().unwrap().remove(agent);
        }
        let mut deployment = self
            .deployment
            .lock()
            .map_err(|_| anyhow::anyhow!("deployment record lock poisoned"))?;
        match session {
            Some(session) => deployment
                .sessions
                .insert(agent.to_owned(), session.to_owned()),
            None => deployment.sessions.remove(agent),
        };
        deployment.save(&self.launcher.runtime_dir)
    }

    fn record_stop(&self, agent: &str) -> Result<()> {
        if self.states[agent].lifetime.is_some() {
            self.track(agent, None)
        } else {
            self.stopped.lock().unwrap().insert(agent.to_owned());
            Ok(())
        }
    }

    fn cleanup_sessions(&self, sessions: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let stopped = self.stopped.lock().unwrap();
        sessions
            .iter()
            .filter(|(agent, _)| !stopped.contains(*agent))
            .map(|(agent, session)| (agent.clone(), session.clone()))
            .collect()
    }

    fn slot(&self, agent: &str) -> std::sync::MutexGuard<'_, Option<Window>> {
        self.windows[agent]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Which window `invocation` falls in: the attempt of `slot`'s open one
    /// and `false`, or, once the open one is spent, a new attempt and `true`.
    /// Counts only this agent's own invocations and the tags they run in. A
    /// window that is spent is stopped here, before anything of the new one
    /// starts.
    fn admit(
        &self,
        invocation: &InvocationSpec,
        slot: &mut Option<Window>,
    ) -> Result<(manager::TopologyAttempt, bool)> {
        let lifetime = self.states[&invocation.agent].lifetime;
        if let Some(window) = slot.as_mut() {
            anyhow::ensure!(
                self.launcher.client.has_session(&window.session)?
                    && !self
                        .launcher
                        .client
                        .session_process_tree(&window.session)?
                        .is_empty(),
                "agent '{}' task {}: worker died while idle ({})",
                invocation.agent,
                invocation.id,
                window.session
            );
            if lifetime.is_none_or(|lifetime| !window.used.spent(lifetime, invocation.tag)) {
                window.used.take(invocation.tag);
                return Ok((window.attempt.clone(), false));
            }
            self.close(slot).with_context(|| {
                format!(
                    "agent '{}' invocation {}: the worker of its previous conversation was not confirmed stopped",
                    invocation.agent, invocation.id
                )
            })?;
        }
        let backend = crate::backend::by_name(&self.states[&invocation.agent].backend)
            .context("agent with a lifetime has no backend")?;
        Ok((
            manager::TopologyAttempt {
                id: Uuid::new_v4().to_string(),
                key: Uuid::new_v4().to_string(),
                native_session: if lifetime.is_some() {
                    backend.new_conversation_id()
                } else {
                    String::new()
                },
            },
            true,
        ))
    }

    /// Run `invocation` in its window: start the window's worker when
    /// `fresh`, hand it `answer`, and check the worker still holds the
    /// conversation it started with before the writes are released.
    fn run(
        &self,
        invocation: &InvocationSpec,
        attempt: &manager::TopologyAttempt,
        fresh: bool,
        retries: u32,
        slot: &mut Option<Window>,
        answer: impl FnOnce(&mut Window) -> Result<(BTreeMap<String, Value>, bool)>,
    ) -> Result<BTreeMap<String, Value>> {
        let agent = &self.states[&invocation.agent];
        let backend = crate::backend::by_name(&agent.backend)
            .context("agent with a lifetime has no backend")?;
        if fresh {
            *slot = Some(self.open(invocation, attempt, retries)?);
        }
        let window = slot.as_mut().context("agent has no open window")?;
        if !fresh {
            window.record["invocations"]
                .as_array_mut()
                .context("window record has no invocations")?
                .push(served_invocation(invocation));
            self.record(&window.record);
            self.publish(&window.agent, &window.record);
        }
        let result = (|| -> Result<(BTreeMap<String, Value>, bool)> {
            if !fresh && agent.lifetime.is_some() {
                self.same_conversation(window, backend)?;
            }
            let (writes, timed_out) = answer(window)?;
            // A worker that ran out of time is busy and about to be stopped,
            // so what it holds is no longer asked.
            if !timed_out {
                if agent.lifetime.is_some() {
                    self.same_conversation(window, backend)?;
                } else {
                    window.observe_conversation(&self.launcher.client);
                }
            }
            Ok((writes, timed_out))
        })();
        let lifetime = agent.lifetime;
        match result {
            Ok((writes, true)) => {
                // The contract let it stay silent, but the worker is still on
                // the invocation, so no later one may join its window.
                window.failed = true;
                self.close(slot).with_context(|| {
                    format!(
                        "agent '{}' invocation {}: worker attempt {} was not confirmed stopped, so its effects are withheld",
                        invocation.agent, invocation.id, attempt.id
                    )
                })?;
                Ok(writes)
            }
            Ok((writes, false)) => {
                // No later invocation can join a window whose tasks are
                // spent, so its worker is stopped before the writes are
                // released, as the last thing it did.
                if lifetime.is_some_and(|lifetime| window.used.exhausted(lifetime)) {
                    self.close(slot).with_context(|| {
                        format!(
                            "agent '{}' invocation {}: worker attempt {} was not confirmed stopped, so its effects are withheld",
                            invocation.agent, invocation.id, attempt.id
                        )
                    })?;
                } else {
                    window.record["outcome"] = json!("open");
                    self.record(&window.record);
                    self.publish(&window.agent, &window.record);
                }
                Ok(writes)
            }
            Err(error) => {
                // A worker that failed an invocation is in an unknown state.
                window.failed = true;
                if let Some(credit) = error.downcast_ref::<crate::backend::OutOfCredit>() {
                    window.record["failure_reason"] = json!(credit.to_string());
                }
                if let Some(death) = error.downcast_ref::<WorkerDied>() {
                    window.record["outcome"] = json!("died");
                    window.record["death_reason"] = json!(death.reason);
                }
                let error = error.context(format!(
                    "agent '{}' invocation {}: worker attempt {}",
                    invocation.agent, invocation.id, attempt.id
                ));
                match self.close(slot) {
                    Ok(()) => Err(error),
                    Err(stop) => Err(stop.context(format!(
                        "agent '{}' invocation {}: worker attempt {} was not confirmed stopped, so its effects are withheld",
                        invocation.agent, invocation.id, attempt.id
                    ))),
                }
            }
        }
    }

    /// Start a window's worker for `invocation`, the first it serves, and
    /// prove its conversation new. On failure the worker is stopped.
    fn open(
        &self,
        invocation: &InvocationSpec,
        attempt: &manager::TopologyAttempt,
        retries: u32,
    ) -> Result<Window> {
        anyhow::ensure!(
            retries == 0 || !self.launcher.shutdown.load(Ordering::Acquire),
            "runtime is shutting down"
        );
        let agent = &self.states[&invocation.agent];
        let backend = crate::backend::by_name(&agent.backend)
            .context("agent with a lifetime has no backend")?;
        let client = &self.launcher.client;
        let session = client.session_for(&invocation.agent);
        let mut record = json!({
            "run": self.run,
            "logical_agent": invocation.agent,
            "attempt_id": attempt.id,
            "worker_session": session,
            "backend": agent.backend,
            "native_session_id": Value::Null,
            "verification": if agent.lifetime.is_some() { backend.freshness().name() } else { "unscoped" },
            "retry": retries,
            "resume_session_id": if agent.lifetime.is_none() && !attempt.native_session.is_empty() { Some(&attempt.native_session) } else { None },
            "lifetime": agent.lifetime,
            "invocations": [served_invocation(invocation)],
            "outcome": "started",
            "cleanup_status": "pending",
        });
        if retries > 0 {
            record["conversation_mode"] = json!(if agent.lifetime.is_some()
                || attempt.native_session.is_empty()
            {
                "fresh conversation (earlier context unavailable)"
            } else {
                "resumed native conversation"
            });
        }
        // Anything already in the session is some earlier worker, and taking
        // it over would be the reuse a new window rules out.
        if agent.lifetime.is_some() && client.has_session(&session)? {
            record["outcome"] = json!("refused");
            record["cleanup_status"] = json!("not_started");
            self.record(&record);
            bail!(
                "agent '{}' invocation {}: a worker is already running in {session}; refusing to reuse it",
                invocation.agent,
                invocation.id
            );
        }
        if agent.lifetime.is_some() && retries == 0 {
            self.record(&record);
        }
        if let Err(error) = self.track(&invocation.agent, Some(&session)) {
            record["outcome"] = json!("refused");
            record["cleanup_status"] = json!("not_started");
            self.record(&record);
            return Err(error.context(format!(
                "agent '{}' invocation {}: its worker could not be recorded for cleanup",
                invocation.agent, invocation.id
            )));
        }
        let mut tree = Vec::new();
        let mut sessions = BTreeSet::new();
        // Whatever the worker reported, kept even when it is refused.
        let mut reported = None;
        let since = std::time::SystemTime::now();
        let started = (|| -> Result<()> {
            if !client.has_session(&session)? {
                let command = self.launcher.launch(
                    &invocation.agent,
                    agent,
                    agent.lifetime.is_some().then_some(attempt),
                    (agent.lifetime.is_none() && !attempt.native_session.is_empty())
                        .then_some(attempt.native_session.as_str()),
                )?;
                if agent.lifetime.is_some() || attempt.native_session.is_empty() {
                    self.launcher
                        .wait_ready(&invocation.agent, agent, &command)?;
                }
            }
            if agent.lifetime.is_none() {
                tree = client.session_process_tree(&session)?;
                sessions.extend(crate::process::sessions(&tree));
                reported = worker_conversation(client, &session);
                return Ok(());
            }
            client.finish_backend_startup(&session, WORKER_START_TIMEOUT)?;
            self.opened_conversation(
                &session,
                backend,
                attempt,
                since,
                (&mut tree, &mut sessions),
                &mut reported,
            )
        })();
        record["native_session_id"] = json!(reported);
        // A minted id is on disk before the worker hears anything: the next
        // run's check is only as good as this log.
        let started = started.and_then(|()| match backend.freshness() {
            _ if agent.lifetime.is_none() => {
                self.record(&record);
                Ok(())
            }
            Freshness::Chosen => {
                self.record(&record);
                Ok(())
            }
            Freshness::Reported => self.append(&record).with_context(|| {
                format!(
                    "could not record the conversation it reported in {}",
                    self.log.display()
                )
            }),
        });
        match started {
            Ok(()) => {
                // Mission Control shows the window as soon as it is proved.
                self.publish(&invocation.agent, &record);
                Ok(Window {
                    turn_monitor: None,
                    turn_file: manager::topology_turn_file(
                        &self.launcher.omar_dir,
                        self.launcher.ea_id,
                        &self.launcher.team,
                        &invocation.agent,
                        agent.lifetime.map(|_| attempt.id.as_str()),
                    ),
                    agent: invocation.agent.clone(),
                    attempt: attempt.clone(),
                    session,
                    tree,
                    sessions,
                    since,
                    conversation: reported.unwrap_or_else(|| {
                        if agent.lifetime.is_none() {
                            attempt.native_session.clone()
                        } else {
                            String::new()
                        }
                    }),
                    used: Used::first(invocation.tag),
                    failed: false,
                    record,
                })
            }
            Err(error) => {
                let stopped = self.stop(
                    &session,
                    attempt,
                    (agent, since),
                    &mut tree,
                    &mut sessions,
                    false,
                );
                if stopped.is_ok() {
                    if let Err(error) = self.record_stop(&invocation.agent) {
                        eprintln!("warning: could not record {session} as stopped: {error:#}");
                    }
                }
                record["outcome"] = json!("failed");
                record["cleanup_status"] = json!(if stopped.is_ok() {
                    "stopped"
                } else {
                    "unconfirmed"
                });
                self.record(&record);
                self.publish(&invocation.agent, &record);
                let error = error.context(format!(
                    "agent '{}' invocation {}: worker attempt {} did not start a fresh conversation",
                    invocation.agent, invocation.id, attempt.id
                ));
                match stopped {
                    Ok(()) => Err(error),
                    Err(stop) => Err(stop.context(format!(
                        "agent '{}' invocation {}: worker attempt {} was not confirmed stopped, so its effects are withheld",
                        invocation.agent, invocation.id, attempt.id
                    ))),
                }
            }
        }
    }

    /// Whether the window's worker still holds the conversation it started
    /// with. A later invocation in a different conversation would have
    /// reached something the window did not promise.
    fn same_conversation(
        &self,
        window: &Window,
        backend: &dyn crate::backend::Backend,
    ) -> Result<()> {
        let client = &self.launcher.client;
        let pane_pid = client.get_pane_pid(&window.session)?;
        let stamp = client.session_delivery(&window.session);
        let target = crate::backend::Target {
            name: &window.session,
            pane_pid,
            stamp: stamp.as_deref(),
        };
        match backend.conversation_id(&target) {
            Some(native) if native == window.conversation => Ok(()),
            Some(native) => bail!(
                "its worker moved to conversation {native} from {}; the window's writes are refused",
                window.conversation
            ),
            None => bail!(
                "its worker no longer reports conversation {}; the window's writes are refused",
                window.conversation
            ),
        }
    }

    /// End `slot`'s window: stop its worker, see its processes gone, and
    /// record how it went.
    fn close(&self, slot: &mut Option<Window>) -> Result<()> {
        let Some(mut window) = slot.take() else {
            return Ok(());
        };
        let agent = &self.states[&window.agent];
        deploy::capture_session_log(
            &self.launcher.client,
            &window.agent,
            &window.session,
            &deploy::logs_dir(&self.launcher.runtime_dir),
        );
        let stopped = self.stop(
            &window.session,
            &window.attempt,
            (agent, window.since),
            &mut window.tree,
            &mut window.sessions,
            agent.lifetime.is_none() && window.record["outcome"] == "died",
        );
        if stopped.is_ok() {
            if let Err(error) = self.record_stop(&window.agent) {
                eprintln!(
                    "warning: could not record {} as stopped: {error:#}",
                    window.session
                );
            }
        }
        if window.record["outcome"] != "died" {
            window.record["outcome"] = json!(if window.failed { "failed" } else { "completed" });
        }
        if let Some(wait) = window.record["usage_waits"]
            .as_array_mut()
            .and_then(|v| v.last_mut())
            .filter(|v| v["ended_at"].is_null())
        {
            wait["ended_at"] = json!(chrono::Utc::now().to_rfc3339());
        }
        window.record["cleanup_status"] = json!(if stopped.is_ok() {
            "stopped"
        } else {
            "unconfirmed"
        });
        self.record(&window.record);
        self.publish(&window.agent, &window.record);
        stopped
    }

    /// Stop every open window, as the run ends.
    fn close_all(&self) {
        for (agent, window) in &self.windows {
            let mut slot = window
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Err(error) = self.close(&mut slot) {
                eprintln!("warning: could not stop the worker of agent '{agent}': {error:#}");
            }
        }
    }

    fn publish(&self, agent: &str, record: &Value) {
        if unscoped_without_restart(record) {
            return;
        }
        if let Some(publisher) = &self.publisher {
            publisher.agent_attempt(agent, record);
        }
    }

    /// Wait for the started worker to report the conversation it opened,
    /// into `reported`, and check that conversation is new.
    fn opened_conversation(
        &self,
        session: &str,
        backend: &dyn crate::backend::Backend,
        attempt: &manager::TopologyAttempt,
        since: std::time::SystemTime,
        (tree, sessions): (&mut Vec<(u32, String)>, &mut BTreeSet<u32>),
        reported: &mut Option<String>,
    ) -> Result<()> {
        let client = &self.launcher.client;
        let deadline = Instant::now() + WORKER_START_TIMEOUT;
        loop {
            let pane_pid = client.get_pane_pid(session)?;
            *tree = client.session_process_tree(session)?;
            sessions.extend(crate::process::sessions(tree));
            let stamp = client.session_delivery(session);
            let target = crate::backend::Target {
                name: session,
                pane_pid,
                stamp: stamp.as_deref(),
            };
            match backend.conversation_id(&target) {
                Some(native) => {
                    let verified = fresh_conversation(
                        backend.freshness(),
                        &attempt.native_session,
                        &native,
                        &self.seen,
                        || backend.prove_fresh(&target, &native, since, deadline),
                    );
                    *reported = Some(native);
                    return verified;
                }
                None if Instant::now() < deadline => thread::sleep(Duration::from_millis(250)),
                None => bail!(
                    "it never reported a conversation within {:?}",
                    WORKER_START_TIMEOUT
                ),
            }
        }
    }

    /// Stop the worker's process tree, and whatever it left running outside
    /// it (see [`crate::process::leftovers`], and on macOS
    /// [`crate::process::touching`] its instance's workspace since `since`),
    /// and see them gone; then remove the attempt's own files.
    fn stop(
        &self,
        session: &str,
        attempt: &manager::TopologyAttempt,
        (agent, since): (&AgentState, std::time::SystemTime),
        tree: &mut Vec<(u32, String)>,
        sessions: &mut BTreeSet<u32>,
        preserve_history: bool,
    ) -> Result<()> {
        let client = &self.launcher.client;
        if client.has_session(session)? {
            let live = client.session_process_tree(session)?;
            sessions.extend(crate::process::sessions(&live));
            tree.extend(live);
            if !preserve_history {
                crate::backend::kill_worker(client, session, true)?;
            } else {
                client.kill_session_tree(session)?;
            }
        }
        let marker = format!("{ATTEMPT_MARKER}={}", attempt.id);
        let deadline = Instant::now() + WORKER_STOP_TIMEOUT;
        #[cfg(target_os = "macos")]
        let workspace: Vec<PathBuf> = self
            .launcher
            .workspaces
            .get(&agent.instance)
            .map(|(worktree, temp)| vec![worktree.clone(), temp.clone()])
            .context("agent instance has no workspace")?;
        #[cfg(not(target_os = "macos"))]
        let _ = (agent, since);
        loop {
            let left = crate::process::leftovers(sessions, &marker);
            // macOS hides its own binaries' environments, the shells' among
            // them, so one that left the session is found by what it touches
            // instead: never this runtime or another live worker, whose
            // processes may share the workspace.
            #[cfg(target_os = "macos")]
            let left = {
                let spared = crate::process::descendants(
                    client
                        .other_pane_pids(session)?
                        .into_iter()
                        .chain([std::process::id()]),
                );
                let mut left = left;
                for found in crate::process::touching(&workspace, since, &spared)? {
                    if !left.contains(&found) {
                        left.push(found);
                    }
                }
                left
            };
            crate::process::signal_tree(&left, "-KILL");
            if !crate::process::tree_alive(tree)
                && !crate::process::tree_alive(&left)
                && !client.has_session(session)?
            {
                break;
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "worker processes of {session} were still running {:?} after they were stopped: {:?}",
                WORKER_STOP_TIMEOUT,
                left.iter().map(|(pid, _)| pid).collect::<Vec<_>>()
            );
            thread::sleep(Duration::from_millis(100));
        }
        let files =
            manager::attempt_mcp_dir(&self.launcher.omar_dir, self.launcher.ea_id, &attempt.id);
        if let Err(error) = fs::remove_dir_all(&files) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("warning: could not remove {}: {error}", files.display());
            }
        }
        Ok(())
    }

    /// Best effort: a record that cannot be written does not decide the run.
    fn record(&self, record: &Value) {
        if unscoped_without_restart(record) {
            return;
        }
        if let Err(error) = self.append(record) {
            eprintln!(
                "warning: could not record worker attempt in {}: {error}",
                self.log.display()
            );
        }
    }

    fn append(&self, record: &Value) -> std::io::Result<()> {
        let line = format!("{record}\n");
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .and_then(|mut file| file.write_all(line.as_bytes()))
    }
}

fn unscoped_without_restart(record: &Value) -> bool {
    record["verification"] == "unscoped"
        && record["retry"] == 0
        && record["outcome"] != "died"
        && record["reminders"].as_array().is_none_or(Vec::is_empty)
        && record["usage_waits"].as_array().is_none_or(Vec::is_empty)
        && record["failure_reason"].is_null()
}

/// Every conversation an attempt recorded in `log`, in this run or an earlier
/// one of the same deployment. No log is none; one that cannot be read, or
/// has a line that is not a record, is an error.
fn reported_conversations(log: &Path) -> Result<BTreeSet<String>, String> {
    let text = match fs::read_to_string(log) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(format!("could not read {}: {error}", log.display())),
    };
    let mut seen = BTreeSet::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let record: Value = serde_json::from_str(line).map_err(|error| {
            format!("{} has a line that is not a record: {error}", log.display())
        })?;
        if let Some(native) = record["native_session_id"].as_str() {
            seen.insert(native.to_owned());
        }
    }
    Ok(seen)
}

/// Whether `reported`, the conversation a newly started worker opened, is a
/// new one, by the backend's form of proof. Either way it joins `seen`, so no
/// later attempt can report it again; `prove` is the backend's own check,
/// run once the id itself has passed.
fn fresh_conversation(
    freshness: Freshness,
    chosen: &str,
    reported: &str,
    seen: &Mutex<Result<BTreeSet<String>, String>>,
    prove: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if freshness == Freshness::Chosen && reported != chosen {
        bail!("it opened conversation {reported}, not the new conversation {chosen} it was given");
    }
    if reported.trim().is_empty() {
        bail!("it reported an empty conversation id");
    }
    {
        let mut seen = seen
            .lock()
            .map_err(|_| anyhow::anyhow!("conversation log lock poisoned"))?;
        match &mut *seen {
            Ok(seen) => {
                if !seen.insert(reported.to_owned()) {
                    bail!("it opened conversation {reported}, which an earlier attempt already had");
                }
            }
            // A chosen id was minted for this attempt alone; only a minted one
            // rests on the log.
            Err(_) if freshness == Freshness::Chosen => {}
            Err(error) => bail!(
                "the conversations earlier attempts had are unknown, so {reported} cannot be shown new: {error}"
            ),
        }
    }
    prove().with_context(|| format!("conversation {reported} could not be shown new"))
}

/// A body reaches its own instance's state and no other's.
///
/// Codegen binds only the invoking instance's variables, so a body built here
/// cannot reach further. The check is what the VM trusts instead of the
/// binary, which it did not write.
fn state_writes_stay_in_instance(
    state: &VmState,
    invocation: &InvocationSpec,
    writes: &BTreeMap<String, Value>,
) -> Result<()> {
    match writes.keys().find(|name| {
        state.state_vars.contains_key(*name) && !invocation.state_values.contains_key(*name)
    }) {
        Some(name) => bail!(
            "reaction '{}' wrote state '{name}' outside its instance",
            invocation.reaction_id
        ),
        None => Ok(()),
    }
}

/// Sends a reaction to its compiled body when it has one, and to its agent
/// otherwise. One run may hold both kinds.
struct DispatchExecutor<'a, E: ReactionExecutor> {
    state: &'a VmState,
    code: Option<crate::reaction::Reactions>,
    /// The run-wide timeout, which bounds a body that set no deadline.
    timeout: Duration,
    run_started: Instant,
    agents: E,
    workspace_dirs: BTreeMap<String, (PathBuf, PathBuf)>,
}

impl<E: ReactionExecutor> ReactionExecutor for DispatchExecutor<'_, E> {
    fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
        if let Some(backend) = self
            .state
            .agents
            .get(&invocation.agent)
            .and_then(|agent| crate::backend::by_name(&agent.backend))
            .filter(|backend| backend.topology_only())
        {
            let rendered = render_prompt(&invocation.prompt, &prompt_values(&invocation))?;
            let mut clock = TaskClock::new();
            let budget = invocation.within.unwrap_or(self.timeout);
            let mut backoff = Duration::from_secs(5);
            let mut writes = None;
            for retries in 0..=MAX_TASK_RETRIES {
                let mut attempt = json!({
                    "attempt_id": Uuid::new_v4().to_string(), "backend": backend.kind().name(),
                    "retry": retries, "outcome": "started", "cleanup_status": "no_process",
                    "conversation_mode": "fresh HTTP invocation",
                });
                self.agents.record_attempt(&invocation, attempt.clone());
                let result = loop {
                    anyhow::ensure!(
                        self.run_started.elapsed() < self.timeout,
                        "run-wide timeout exceeded while waiting for usage limit"
                    );
                    let result = backend.invoke_topology(&crate::backend::TopologyInvocation {
                        effects: &invocation.allowed_effects,
                        triggers: &invocation.trigger_values,
                        instructions: &rendered,
                        timeout: budget
                            .saturating_sub(clock.elapsed())
                            .min(self.timeout.saturating_sub(self.run_started.elapsed())),
                        retries,
                    });
                    let limit = result
                        .as_ref()
                        .err()
                        .and_then(|error| error.downcast_ref::<crate::backend::UsageLimit>());
                    let Some(limit) = limit else {
                        break result;
                    };
                    clock.pause();
                    let wait = json!({ "task_id": invocation.id, "reaction": invocation.reaction_id, "started_at": chrono::Utc::now().to_rfc3339(), "resets_at": limit.resets_at });
                    attempt
                        .as_object_mut()
                        .unwrap()
                        .entry("usage_waits")
                        .or_insert_with(|| json!([]))
                        .as_array_mut()
                        .unwrap()
                        .push(wait);
                    self.agents.record_attempt(&invocation, attempt.clone());
                    let until = Instant::now() + limit.delay(backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(300));
                    while Instant::now() < until {
                        if self.run_started.elapsed() >= self.timeout {
                            attempt["usage_waits"]
                                .as_array_mut()
                                .unwrap()
                                .last_mut()
                                .unwrap()["ended_at"] = json!(chrono::Utc::now().to_rfc3339());
                            attempt["outcome"] = json!("failed");
                            self.agents.record_attempt(&invocation, attempt.clone());
                            bail!("run-wide timeout exceeded while waiting for usage limit");
                        }
                        thread::sleep(
                            WORKER_POLL.min(until.saturating_duration_since(Instant::now())),
                        );
                    }
                    clock.resume();
                    attempt["usage_waits"]
                        .as_array_mut()
                        .unwrap()
                        .last_mut()
                        .unwrap()["ended_at"] = json!(chrono::Utc::now().to_rfc3339());
                    self.agents.record_attempt(&invocation, attempt.clone());
                };
                match result {
                    Ok(answer) => {
                        attempt["outcome"] = json!("completed");
                        self.agents.record_attempt(&invocation, attempt);
                        writes = Some(answer);
                        break;
                    }
                    Err(error) => {
                        let retryable = backend.retryable_topology_error(&error);
                        attempt["outcome"] = json!(if retryable { "died" } else { "failed" });
                        attempt["death_reason"] = json!(error.to_string());
                        self.agents.record_attempt(&invocation, attempt);
                        if !retryable || retries == MAX_TASK_RETRIES || clock.elapsed() >= budget {
                            return Err(error.context(format!(
                                "agent '{}' task {} ({}): HTTP invocation failed after {retries} retries",
                                invocation.agent, invocation.id, invocation.reaction_id
                            )));
                        }
                    }
                }
            }
            let writes = writes.context("HTTP invocation produced no answer")?;
            validate_contract(&invocation.contract, &writes)?;
            for (port, value) in &writes {
                let port_type = self
                    .state
                    .ports
                    .get(port)
                    .context("backend wrote an unknown port")?;
                validate_value(&port_type.ty, value)?;
            }
            return Ok(writes);
        }
        if let Some(code) = &self.code {
            if code.handles(&invocation.reaction_id) {
                let deadline = invocation.within.unwrap_or(self.timeout);
                let Some(writes) = code.invoke_in(
                    self.state,
                    &invocation.reaction_id,
                    &invocation.trigger_values,
                    &invocation.state_values,
                    deadline,
                    self.workspace_dirs
                        .get(&self.state.reactions[&invocation.reaction_id].instance),
                )?
                else {
                    // Killed with nothing written, so its instance keeps the
                    // state it had before the invocation.
                    return expired(&invocation, deadline);
                };
                state_writes_stay_in_instance(self.state, &invocation, &writes)?;
                // State comes back beside the effects and is not one of them.
                let effects: BTreeMap<_, _> = writes
                    .iter()
                    .filter(|(name, _)| {
                        !self.state.state_vars.contains_key(*name)
                            && name.as_str() != crate::reaction::STOP
                    })
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect();
                validate_contract(&invocation.contract, &effects)?;
                return Ok(writes);
            }
        }
        self.agents.invoke(invocation)
    }
}

/// The host trusts only typed, contract-checked invocation results from the VM.
struct SandboxExecutor<'a, E> {
    state: &'a VmState,
    workers: BTreeMap<String, crate::sandbox::Worker>,
    workspaces: &'a BTreeMap<String, crate::workspace::Workspace>,
    root: &'a Path,
    timeout: Duration,
    host: E,
}
impl<E: ReactionExecutor> ReactionExecutor for SandboxExecutor<'_, E> {
    fn invoke(&self, mut invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
        let reaction = &self.state.reactions[&invocation.reaction_id];
        if self.workers.is_empty()
            || self
                .state
                .agents
                .get(&invocation.agent)
                .is_some_and(|a| is_web_backend(&a.backend))
        {
            return self.host.invoke(invocation);
        }
        let workspace = &self.workspaces[&reaction.instance];
        for (name, value) in &mut invocation.trigger_values {
            if let Some(port) = self.state.ports.get(name) {
                crate::sandbox::map_input_paths(value, &port.ty, workspace, self.root)?;
            }
        }
        let worker = self
            .workers
            .get(&reaction.instance)
            .context("missing team sandbox")?;
        let deadline = invocation.within.unwrap_or(self.timeout);
        let response = worker.request(
            serde_json::to_value(&invocation)?,
            deadline.saturating_add(Duration::from_secs(20)),
        )?;
        let writes: BTreeMap<String, Value> = serde_json::from_value(
            response
                .get("writes")
                .context("missing sandbox writes")?
                .clone(),
        )?;
        validate_sandbox_writes(self.state, &invocation, &writes)?;
        Ok(writes)
    }
}
fn validate_sandbox_writes(
    state: &VmState,
    invocation: &InvocationSpec,
    writes: &BTreeMap<String, Value>,
) -> Result<()> {
    let mut effects = BTreeMap::new();
    for (name, value) in writes {
        if invocation.state_values.contains_key(name)
            && state.reactions[&invocation.reaction_id].body.is_some()
        {
            validate_value(&state.state_vars[name].ty, value)?;
        } else {
            let ty = invocation
                .allowed_effects
                .get(name)
                .context("sandbox wrote outside invocation effects")?;
            validate_value(ty, value)?;
            effects.insert(name.clone(), value.clone());
        }
    }
    validate_contract(&invocation.contract, &effects)
}

/// A worker receives only its team's definitions and initial values. The host
/// retains routing and other teams' parameters/state, including nested teams.
fn sandbox_state(state: &VmState, instance: &str) -> VmState {
    let mut local = state.clone();
    local.instances.retain(|name, _| name == instance);
    local
        .agents
        .retain(|_, agent| agent.instance == instance && !is_web_backend(&agent.backend));
    local
        .reactions
        .retain(|_, reaction| reaction.instance == instance);
    let referenced: BTreeSet<_> = local
        .reactions
        .values()
        .flat_map(|reaction| reaction.triggers.iter().chain(&reaction.effects))
        .cloned()
        .collect();
    // Parents may consume child outputs; retain their schemas, not child values.
    local.ports.retain(|name, _| referenced.contains(name));
    local.timers.retain(|name, _| referenced.contains(name));
    local.state_vars.retain(|_, var| var.instance == instance);
    local.params.retain(|_, param| param.instance == instance);
    local.connections.clear();
    local
}

/// Internal VM-side service. No host paths other than the two mounts are used.
/// stdout is reserved for bounded protocol frames; backend panes/logs stay in VM.
pub fn sandbox_worker() -> Result<()> {
    let mut input = BufReader::new(std::io::stdin());
    let init: crate::sandbox::Init =
        serde_json::from_slice(&crate::sandbox::read_frame(&mut input)?)?;
    anyhow::ensure!(
        init.protocol == crate::sandbox::PROTOCOL,
        "sandbox protocol mismatch"
    );
    let local_root = dirs::home_dir()
        .context("sandbox home")?
        .join(".omar-sandbox");
    fs::create_dir_all(&local_root)?;
    let generated = local_root.join("generated");
    let runtime_dir = local_root.join("runtime");
    let worktree = init.workspace.worktree(&init.root);
    let config = TopologyRunConfig {
        ea_id: 0,
        omar_dir: &local_root,
        generated: &generated,
        base_prefix: "omar-team-",
        health_idle_warning: 15,
        sandbox_template: None,
        inputs: &[],
        replace: true,
        timeout: init.timeout,
        pace: Pace::Fast,
        diagram_address: None,
        diagram_ready: None,
        panel_ready: None,
        // The host checkpoints and owns the workspace's lifetime.
        checkpoint_policy: None,
        keep_workspaces: true,
        program_path: None,
        // The VM goes with the run; the host's shutdown stops it.
        shutdown: Arc::default(),
    };
    let server = InvocationServer::start()?;
    let client = TmuxClient::new("omar-team-");
    let workspaces = BTreeMap::from([(init.workspace.instance.clone(), init.workspace.clone())]);
    let mut sessions = BTreeMap::new();
    let result = (|| -> Result<()> {
        let code = crate::reaction::build(&init.state, &generated)?
            .map(|code| code.with_cleanup_directory(local_root.join("reactions")));
        let launcher = AgentLauncher::new(
            &init.state,
            &client,
            &runtime_dir,
            &server,
            &config,
            (&workspaces, &init.root),
            init.resumed_from.as_ref(),
        );
        spawn_topology_agents(&init.state, &launcher, config.replace, &mut sessions)?;
        // The worker starts with the run; the host bounds the run as well.
        let run_started = Instant::now();
        let executor = DispatchExecutor {
            state: &init.state,
            code,
            timeout: init.timeout,
            run_started,
            workspace_dirs: BTreeMap::from([(
                init.workspace.instance.clone(),
                (worktree.clone(), init.workspace.temp(&init.root)),
            )]),
            agents: AgentReactionExecutor {
                client: client.clone(),
                team: init.state.team.clone(),
                registry: server.registry.clone(),
                timeout: init.timeout,
                run_started,
                web: BTreeSet::new(),
                // `launch` refuses a sandboxed run with a `per` agent.
                scoped: None,
            },
        };
        crate::sandbox::write_frame(
            &mut std::io::stdout(),
            &json!({"protocol": crate::sandbox::PROTOCOL}),
        )?;
        loop {
            let bytes = crate::sandbox::read_frame(&mut input)?;
            let invocation: InvocationSpec = serde_json::from_slice(&bytes)?;
            let result = executor.invoke(invocation);
            let reply = match result {
                Ok(writes) => json!({"protocol": crate::sandbox::PROTOCOL, "writes": writes}),
                Err(error) => {
                    json!({"protocol": crate::sandbox::PROTOCOL, "error": format!("{error:#}")})
                }
            };
            crate::sandbox::write_frame(&mut std::io::stdout(), &reply)?;
        }
    })();
    let _ = deploy::teardown_sessions(&client, &sessions, &deploy::logs_dir(&runtime_dir));
    result
}

pub struct TopologyRunConfig<'a> {
    pub ea_id: crate::ea::EaId,
    pub omar_dir: &'a Path,
    /// Where this program's generated artifacts go, from `generated_dir`.
    pub generated: &'a Path,
    pub base_prefix: &'a str,
    pub health_idle_warning: i64,
    pub sandbox_template: Option<&'a str>,
    pub inputs: &'a [String],
    pub replace: bool,
    pub timeout: Duration,
    /// Runtime shutdown must not turn teardown into a worker restart.
    pub shutdown: Arc<AtomicBool>,
    /// How the run's logical clock is paced against the wall clock.
    pub pace: Pace,
    pub diagram_address: Option<std::net::SocketAddr>,
    /// Receives the bound diagram address once the server is up. `omar serve`
    /// needs it while the run is still in flight; `omar run` prints it instead.
    pub diagram_ready: Option<mpsc::Sender<std::net::SocketAddr>>,
    /// Receives the invocation service's address and token once it is up.
    ///
    /// A `web` agent has nothing spawned for it, so nothing is ever handed the
    /// credentials a pane's MCP sidecar gets from its context file. Without
    /// this the daemon cannot answer on a client's behalf, and a web-backed
    /// reaction would always sit until its deadline.
    pub panel_ready: Option<mpsc::Sender<PanelAccess>>,
    /// How often the run checkpoints on its own; `None` is the default hour.
    pub checkpoint_policy: Option<checkpoint::Policy>,
    /// Keep the instances' workspaces when the run ends; otherwise they go
    /// with it. A resume takes the paused run's choice.
    pub keep_workspaces: bool,
    /// The program file, recorded in checkpoints so a resume can place
    /// generated code beside it. `None` for a program that arrived over HTTP.
    pub program_path: Option<&'a Path>,
}

/// Where a run's invocations are answered, and the secret that authorises it.
///
/// Loopback-only and per run, like the service it points at. Held by the daemon
/// rather than handed to a browser: it authorises writing any effect of any
/// invocation in the run, which is not a capability a page should carry.
#[derive(Debug, Clone)]
pub struct PanelAccess {
    pub endpoint: String,
    pub token: String,
    /// The `web` agents in this run. Everything else answers through its own
    /// pane, and a panel has no business offering to answer for them.
    pub agents: BTreeSet<String>,
}

/// How a run ended when it did not fail: it drained its queue, or an
/// operator's stop closed it at a tag boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEnd {
    Completed,
    Stopped,
    /// Ended at a checkpoint it can continue from.
    Paused,
}

/// How a run ended, with the output ports and state variables it ended with.
pub struct RunOutcome {
    pub end: RunEnd,
    pub outputs: BTreeMap<String, Value>,
    pub state: BTreeMap<String, Value>,
    /// The live diagram as the run left it, when one was served: a paused
    /// run's picture outlives its diagram server.
    pub diagram: Option<crate::diagram::DiagramSnapshot>,
}

/// Advance the shared record and persist it, as one step.
fn advance_record(
    record: &Arc<Mutex<deploy::DeploymentRecord>>,
    dir: &Path,
    next: DeploymentState,
    detail: Option<&str>,
) -> Result<()> {
    let mut guard = record
        .lock()
        .map_err(|_| anyhow::anyhow!("deployment record lock poisoned"))?;
    guard.advance(next, detail)?;
    guard.save(dir)
}

/// Cleanup must use the deployment's launch identity, never ambient tmux
/// state. Unknown ownership prevents final snapshots.
fn cleanup_recorded_sessions(
    record: &Arc<Mutex<deploy::DeploymentRecord>>,
    sessions: &BTreeMap<String, String>,
    dir: &Path,
) -> Vec<String> {
    let guard = match record.lock() {
        Ok(guard) => guard,
        Err(_) => return vec!["deployment record lock poisoned".into()],
    };
    if !guard.sandboxes.is_empty() {
        return crate::sandbox::stop_all(&guard.sandboxes);
    }
    if sessions.is_empty() {
        return Vec::new();
    }
    match guard.session_client() {
        Ok(client) => deploy::teardown_sessions(&client, sessions, &deploy::logs_dir(dir)),
        Err(error) => vec![format!("{error:#}")],
    }
}

/// The failure funnel: keep pane output as logs, kill the sessions, record
/// FAILED. Best effort; the caller reports the error already on its way out.
fn fail_deployment(
    record: &Arc<Mutex<deploy::DeploymentRecord>>,
    dir: &Path,
    sessions: &BTreeMap<String, String>,
    error: &anyhow::Error,
) {
    let failures = cleanup_recorded_sessions(record, sessions, dir);
    for failure in &failures {
        eprintln!("warning: session not cleaned up: {failure}");
    }
    if let Ok(mut guard) = record.lock() {
        let message = format!("{error:#}");
        let _ = guard.advance(DeploymentState::Failed, Some(&message));
        guard.error = Some(message);
        guard.sessions_cleaned = failures.is_empty();
        let _ = guard.save(dir);
    }
    let _ = deploy::clear_stop(dir);
}

/// The run's checkpoint controller: physical-time scheduling, each team
/// instance's file version, and atomic publication, all keyed to one
/// deployment. The event loop asks it at tag boundaries; it never interrupts
/// a tag.
struct RunCheckpointer<'a> {
    omar_dir: &'a Path,
    dir: PathBuf,
    store: checkpoint::Store,
    record: Arc<Mutex<deploy::DeploymentRecord>>,
    workspaces: &'a BTreeMap<String, crate::workspace::Workspace>,
    program: Value,
    program_path: Option<String>,
    /// The program's source, read once at the start so an edit to the file
    /// after it was compiled does not reach the checkpoints.
    source: Option<checkpoint::Source>,
    team: String,
    ea_id: u32,
    pace: Pace,
    agents: BTreeMap<String, checkpoint::AgentContext>,
    schedule: Mutex<Schedule>,
}

struct Schedule {
    policy: checkpoint::Policy,
    /// When the next physical-time capture is due; none without a period.
    next_due: Option<Instant>,
    /// The tag the last capture of any kind described, so a logical cadence
    /// counts from it and a boundary revisited mid-wait is not captured twice.
    last_captured_tag: Option<(u64, u64)>,
    sequence: u64,
    parent: Option<String>,
}

impl Schedule {
    /// Whether an automatic capture is due at a boundary that completed
    /// `state.completed_tag`: the physical deadline passed, or the tags
    /// crossed into a new logical period since the last capture.
    fn due(&self, now: Instant, state: &ExecutionState) -> bool {
        if self.next_due.is_some_and(|at| now >= at) {
            return true;
        }
        let Some(completed) = state.completed_tag else {
            return false;
        };
        if self.last_captured_tag == Some(completed) {
            return false;
        }
        if self.policy.every_tag {
            return true;
        }
        let period = self.policy.logical_period_ns;
        if period == 0 {
            return false;
        }
        let last = self.last_captured_tag.map_or(0, |(t, _)| t / period);
        completed.0 / period > last
    }
}

impl<'a> RunCheckpointer<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        omar_dir: &'a Path,
        dir: &Path,
        record: Arc<Mutex<deploy::DeploymentRecord>>,
        workspaces: &'a BTreeMap<String, crate::workspace::Workspace>,
        state: &VmState,
        bytecode: &Bytecode,
        config: &TopologyRunConfig<'_>,
        resumed_from: Option<&checkpoint::Manifest>,
    ) -> Result<Self> {
        let store = checkpoint::Store::new(dir);
        // A fresh run starts its own lineage: an earlier run's checkpoints
        // are set aside, so a rollback can only choose from this run's.
        if resumed_from.is_none() {
            if let Some(archived) = store.archive()? {
                println!(
                    "Checkpoints of the previous run moved to {}",
                    archived.display()
                );
            }
        }
        // An operator's live change outlives the runner that saw it; a launch
        // override starts a fresh policy; otherwise the default hour.
        let policy = match (config.checkpoint_policy, deploy::read_policy(dir)?) {
            (Some(policy), _) => policy,
            (None, Some(saved)) if resumed_from.is_some() => saved,
            (None, _) => resumed_from.map(|m| m.policy).unwrap_or_default(),
        };
        deploy::write_policy(dir, policy)?;
        let (existing, problems) = store.list()?;
        for problem in problems {
            eprintln!("warning: ignoring unreadable checkpoint {problem}");
        }
        let sequence = existing.iter().map(|m| m.sequence).max().unwrap_or(0) + 1;
        let next_due = policy.physical().map(|period| Instant::now() + period);
        {
            let mut guard = record
                .lock()
                .map_err(|_| anyhow::anyhow!("deployment record lock poisoned"))?;
            guard.next_checkpoint_at = policy
                .physical()
                .map(|period| deploy::now_unix() + period.as_secs());
            guard.save(dir)?;
        }
        let program_path = config
            .program_path
            .map(|path| path.to_string_lossy().into_owned())
            .or_else(|| resumed_from.and_then(|m| m.program_path.clone()));
        let source = match config.program_path {
            Some(path) if path.extension().is_some_and(|e| e == "omar") => {
                Some(checkpoint::Source::read(path)?)
            }
            _ => match resumed_from {
                Some(manifest) => store
                    .source(&manifest.id)?
                    .map(|path| checkpoint::Source::read(&path))
                    .transpose()?,
                None => None,
            },
        };
        Ok(Self {
            omar_dir,
            dir: dir.to_path_buf(),
            store,
            record,
            workspaces,
            program: serde_json::to_value(bytecode)?,
            program_path,
            source,
            team: state.team.clone(),
            ea_id: config.ea_id,
            pace: config.pace,
            agents: state
                .agents
                .iter()
                .map(|(name, agent)| {
                    (
                        name.clone(),
                        checkpoint::AgentContext {
                            backend: agent.backend.clone(),
                            restoration: checkpoint::FRESH_CONVERSATION.to_string(),
                        },
                    )
                })
                .collect(),
            schedule: Mutex::new(Schedule {
                policy,
                next_due,
                last_captured_tag: resumed_from.and_then(|m| m.completed_tag),
                sequence,
                parent: resumed_from.map(|m| m.id.clone()),
            }),
        })
    }

    /// Pick up a period an operator changed while the run was going. The
    /// next deadline is one new period from the change.
    fn refresh_policy(&self, schedule: &mut Schedule) {
        let Ok(Some(saved)) = deploy::read_policy(&self.dir) else {
            return;
        };
        if saved == schedule.policy {
            return;
        }
        schedule.policy = saved;
        schedule.next_due = saved.physical().map(|period| Instant::now() + period);
        if let Ok(mut guard) = self.record.lock() {
            guard.next_checkpoint_at = saved
                .physical()
                .map(|period| deploy::now_unix() + period.as_secs());
            let _ = guard.save(&self.dir);
        }
    }
}

impl Checkpointer for RunCheckpointer<'_> {
    fn due(&self, state: &ExecutionState) -> bool {
        let Ok(mut schedule) = self.schedule.lock() else {
            return false;
        };
        self.refresh_policy(&mut schedule);
        schedule.due(Instant::now(), state)
    }

    fn capture(&self, state: &ExecutionState, trigger: Trigger) -> Result<String> {
        let mut schedule = self
            .schedule
            .lock()
            .map_err(|_| anyhow::anyhow!("checkpoint schedule lock poisoned"))?;
        let id = uuid::Uuid::new_v4().to_string();
        let kind = match trigger {
            Trigger::Automatic => "automatic",
            Trigger::Manual => "manual",
            Trigger::Pause => "pause",
            Trigger::Mutation => "mutation",
        };
        let label = format!("Checkpoint {} ({kind})", schedule.sequence);
        // Each instance's files, as a version its own history keeps. The
        // manifest names them; nothing is copied.
        let mut artifacts = BTreeMap::new();
        for (instance, workspace) in self.workspaces {
            let snapshot = workspace
                .snapshot(self.omar_dir, &label)
                .with_context(|| format!("snapshot instance '{instance}' files"))?;
            artifacts.insert(
                instance.clone(),
                checkpoint::ArtifactRef {
                    workspace_id: workspace.id.clone(),
                    snapshot_id: snapshot.id,
                    commit: snapshot.commit,
                },
            );
        }
        let deployment_id = self
            .record
            .lock()
            .map_err(|_| anyhow::anyhow!("deployment record lock poisoned"))?
            .deployment_id
            .clone();
        let manifest = checkpoint::Manifest {
            version: checkpoint::FORMAT,
            id: id.clone(),
            sequence: schedule.sequence,
            parent: schedule.parent.clone(),
            deployment_id,
            team: self.team.clone(),
            ea_id: self.ea_id,
            created_at: deploy::now_unix(),
            completed_tag: state.completed_tag,
            next_tag: state.next_tag(),
            trigger,
            policy: schedule.policy,
            pace: match self.pace {
                Pace::RealTime => "real_time".to_string(),
                Pace::Fast => "fast".to_string(),
            },
            program_sha256: checkpoint::sha256(&serde_json::to_vec(&self.program)?),
            program_path: self.program_path.clone(),
            state_sha256: checkpoint::sha256(&serde_json::to_vec_pretty(state)?),
            elapsed_ns: state.elapsed_ns,
            workspaces: artifacts,
            agents: self.agents.clone(),
            mutation: Vec::new(),
        };
        self.store
            .publish(&manifest, state, &self.program, self.source.as_ref())?;
        schedule.sequence += 1;
        schedule.parent = Some(id.clone());
        schedule.next_due = schedule
            .policy
            .physical()
            .map(|period| Instant::now() + period);
        schedule.last_captured_tag = state.completed_tag;
        {
            let mut guard = self
                .record
                .lock()
                .map_err(|_| anyhow::anyhow!("deployment record lock poisoned"))?;
            guard.checkpoint = Some(id.clone());
            guard.checkpoint_error = None;
            guard.next_checkpoint_at = schedule
                .policy
                .physical()
                .map(|period| deploy::now_unix() + period.as_secs());
            guard.save(&self.dir)?;
        }
        let at = state
            .completed_tag
            .map(|(t, m)| format!("({t}, {m})"))
            .unwrap_or_else(|| "start".to_string());
        println!("Checkpoint {id} published: {kind}, after tag {at}");
        Ok(id)
    }

    fn capture_failed(&self, error: &anyhow::Error, attempt: u32, held: bool) {
        eprintln!("warning: checkpoint attempt {attempt} failed: {error:#}");
        // Only a held failure is the operator's to act on; a waiting
        // `checkpoint create` must not read a retry in progress as final.
        if !held {
            return;
        }
        if let Ok(mut guard) = self.record.lock() {
            guard.checkpoint_error = Some(format!("{error:#}"));
            let _ = guard.save(&self.dir);
        }
    }
}

/// A checkpoint a run continues from, with its files already restored into
/// fresh workspaces.
struct ResumePlan {
    manifest: checkpoint::Manifest,
    state: ExecutionState,
    workspaces: BTreeMap<String, crate::workspace::Workspace>,
}

pub fn run_topology(bytecode: &Bytecode, config: TopologyRunConfig<'_>) -> Result<RunOutcome> {
    let state = verify(bytecode)?;
    if let Some(template) = config.sandbox_template {
        crate::sandbox::validate_template(template)?;
    }
    launch(&state, bytecode, config, None)
}

/// Continue a paused team from its resume point: the head a rollback chose,
/// else its latest checkpoint. Files come back as new workspaces restored
/// from the captured versions; agents start fresh conversations and are
/// told so; the clock continues where the checkpoint left it.
pub fn resume_topology(config: TopologyRunConfig<'_>, team: &str) -> Result<RunOutcome> {
    let runtime_dir = deploy::dir_for(config.omar_dir, config.ea_id, team);
    let record = deploy::DeploymentRecord::load(&runtime_dir)?
        .with_context(|| format!("no deployment '{team}'"))?;
    if record.is_active() && record.pid != std::process::id() && record.runner_alive() {
        bail!(
            "deployment '{team}' is {} (pid {}); pause or stop it first",
            record.state,
            record.pid
        );
    }
    // Only a paused run: a stopped one executed tags after its last
    // checkpoint, and continuing from there would replay them.
    anyhow::ensure!(
        record.state == DeploymentState::Paused,
        "deployment '{team}' is {}; only a paused run can resume",
        record.state
    );
    let store = checkpoint::Store::new(&runtime_dir);
    let point = store
        .resume_point()?
        .with_context(|| format!("deployment '{team}' has no complete checkpoint"))?;
    let (manifest, exec, program) = store.load(&point.id)?;
    let bytecode: Bytecode = serde_json::from_value(program)?;
    let state = verify(&bytecode)?;
    anyhow::ensure!(
        state.team == team,
        "checkpoint {} is for team {}, not {team}",
        manifest.id,
        state.team
    );
    // Every instance the program declares must have a captured version, and
    // nothing else may be listed: a checkpoint that disagrees with its own
    // program cannot stand in for the run.
    let expected: BTreeSet<String> = crate::workspace::instance_owners(&state)
        .into_keys()
        .collect();
    let captured: BTreeSet<String> = manifest.workspaces.keys().cloned().collect();
    anyhow::ensure!(
        expected == captured,
        "checkpoint {} covers instances {:?}, program declares {:?}",
        manifest.id,
        captured,
        expected
    );
    let mut workspaces = BTreeMap::new();
    let result = (|| -> Result<()> {
        for (instance, artifact) in &manifest.workspaces {
            let source = crate::workspace::Workspace::load(config.omar_dir, &artifact.workspace_id)
                .with_context(|| format!("instance '{instance}' workspace"))?;
            let restored = source
                .restore(config.omar_dir, &artifact.snapshot_id)
                .with_context(|| format!("restore instance '{instance}' files"))?;
            workspaces.insert(instance.clone(), restored);
        }
        Ok(())
    })();
    if let Err(error) = result {
        for workspace in workspaces.values() {
            if let Some(root) = workspace.worktree(config.omar_dir).parent() {
                let _ = fs::remove_dir_all(root);
            }
        }
        return Err(error);
    }
    // The paused run's choices carry over: a resume is the same run, so it
    // gets fresh sandboxes from the template the run pinned, or none.
    let template = record.sandbox_template.clone();
    if let Some(template) = &template {
        crate::sandbox::validate_template(template)?;
    }
    let config = TopologyRunConfig {
        keep_workspaces: record.keep_workspaces,
        sandbox_template: template.as_deref(),
        ..config
    };
    launch(
        &state,
        &bytecode,
        config,
        Some(ResumePlan {
            manifest,
            state: exec,
            workspaces,
        }),
    )
}

fn launch(
    state: &VmState,
    bytecode: &Bytecode,
    config: TopologyRunConfig<'_>,
    resume: Option<ResumePlan>,
) -> Result<RunOutcome> {
    let state = state.clone();
    let runtime_dir = deploy::dir_for(config.omar_dir, config.ea_id, &state.team);
    fs::create_dir_all(&runtime_dir)?;
    // One live run per team: its sessions are named by team and agent, so a
    // second run would be answered by the first run's panes.
    if let Some(mut existing) = deploy::DeploymentRecord::load(&runtime_dir)? {
        if existing.is_active() && existing.pid != std::process::id() && existing.runner_alive() {
            bail!(
                "deployment '{}' is {} (pid {}); stop it first",
                state.team,
                existing.state,
                existing.pid
            );
        }
        anyhow::ensure!(
            config.replace || existing.sandboxes.is_empty() || existing.sessions_cleaned,
            "deployment '{}' has unconfirmed sandbox cleanup; use --replace to clean it up first",
            state.team
        );
        if config.replace && !existing.sessions.is_empty() {
            existing.session_client()?;
        }
        if config.replace && !existing.sessions_cleaned {
            let failures = if existing.sandboxes.is_empty() {
                let old_client = existing.session_client()?;
                for session in existing.sessions.values() {
                    if old_client.has_session_for_cleanup(session)? {
                        old_client.ensure_session_not_attached(session)?;
                    }
                }
                deploy::teardown_sessions(
                    &old_client,
                    &existing.sessions,
                    &deploy::logs_dir(&runtime_dir),
                )
            } else {
                crate::sandbox::stop_all(&existing.sandboxes)
            };
            anyhow::ensure!(
                failures.is_empty(),
                "cannot replace deployment until old sessions are cleaned up: {}",
                failures.join("; ")
            );
            if existing.is_active() {
                existing.advance(
                    DeploymentState::Cancelled,
                    Some("cleaned up before replacement"),
                )?;
            }
            existing.sessions_cleaned = true;
            existing.save(&runtime_dir)?;
        } else if existing.is_active() {
            bail!("deployment '{}' has an unconfirmed crashed run; use --replace to clean it up first", state.team);
        }
    }
    // A stop left over from an earlier run must not end this one.
    deploy::clear_stop(&runtime_dir)?;
    let obsolete_invocations = runtime_dir.join("invocations");
    if obsolete_invocations.exists() {
        fs::remove_dir_all(&obsolete_invocations)?;
    }
    let client = TmuxClient::new(crate::ea::ea_prefix(config.ea_id, config.base_prefix));
    // A session of an agent with a lifetime is recorded only while a window
    // may have one: see `ScopedWorkers::track`.
    // A window's worker is started and stopped by the host, which a team in
    // a sandbox does not run its agents on.
    if config.sandbox_template.is_some() {
        if let Some((name, _)) = state
            .agents
            .iter()
            .find(|(_, agent)| agent.lifetime.is_some())
        {
            bail!("agent '{name}' has a `per` lifetime, which does not run in a sandbox yet; run with --no-sandbox");
        }
    }
    let planned_sessions: BTreeMap<String, String> = state
        .agents
        .iter()
        .filter(|(_, agent)| {
            config.sandbox_template.is_none()
                && needs_agent_process(&agent.backend)
                && agent.lifetime.is_none()
        })
        .map(|(name, _)| (name.clone(), client.session_for(name)))
        .collect();
    let record = Arc::new(Mutex::new(deploy::DeploymentRecord::create(
        &state.team,
        planned_sessions,
        config.timeout.as_secs(),
    )));
    let (resumed_from, start_state, restored) = match resume {
        Some(plan) => (Some(plan.manifest), Some(plan.state), Some(plan.workspaces)),
        None => (None, None, None),
    };
    {
        let mut guard = record
            .lock()
            .map_err(|_| anyhow::anyhow!("deployment record lock poisoned"))?;
        guard.resumed_from = resumed_from.as_ref().map(|m| m.id.clone());
        guard.keep_workspaces = config.keep_workspaces;
        guard.save(&runtime_dir)?;
    }
    let diagram_server = config
        .diagram_address
        .map(|address| DiagramServer::start(&state, address))
        .transpose()?;
    if let Some(server) = &diagram_server {
        println!("Diagram server: http://{}", server.address());
        if let Some(sender) = &config.diagram_ready {
            let _ = sender.send(server.address());
        }
    }
    let diagram_publisher = diagram_server.as_ref().map(DiagramServer::publisher);
    let noop_observer = NoopTopologyObserver;
    let observer: &dyn TopologyObserver = diagram_publisher
        .as_ref()
        .map(|publisher| publisher as &dyn TopologyObserver)
        .unwrap_or(&noop_observer);
    observer.run_started();

    // Setup can fail — a backend that never reaches its ready banner, a missing
    // input — and those failures are just as much a failed run as one in the
    // event loop. Report them on the stream too, or an observer sees it go
    // quiet with no reason given.
    advance_record(&record, &runtime_dir, DeploymentState::Deploying, None)?;
    // Only what this run spawned. A failure must not tear down a session it
    // refused to replace.
    let mut spawned: BTreeMap<String, String> = BTreeMap::new();
    type Prepared = (
        InvocationServer,
        BTreeMap<String, Value>,
        Option<crate::reaction::Reactions>,
        BTreeMap<String, crate::workspace::Workspace>,
        AgentLauncher,
        BTreeMap<String, crate::sandbox::Worker>,
    );
    let prepared = (|| -> Result<Prepared> {
        // Before anything is spawned: compiling the bodies is the step most
        // likely to fail, and it costs nothing to find out first.
        let reactions = if config.sandbox_template.is_none() {
            crate::reaction::build(&state, config.generated)?
        } else {
            None
        };
        let deployment_id = record
            .lock()
            .map_err(|_| anyhow::anyhow!("deployment record lock poisoned"))?
            .deployment_id
            .clone();
        // A resumed run's files were restored before launch; a fresh run's
        // instances get new, empty workspaces.
        let workspaces = match restored {
            Some(workspaces) => workspaces,
            None => crate::workspace::for_topology(
                config.omar_dir,
                config.ea_id,
                &deployment_id,
                &state,
            )?,
        };
        {
            let mut guard = record
                .lock()
                .map_err(|_| anyhow::anyhow!("deployment record lock poisoned"))?;
            guard.workspaces = workspaces
                .iter()
                .map(|(name, ws)| (name.clone(), ws.id.clone()))
                .collect();
            guard.save(&runtime_dir)?;
        }
        let invocation_server = InvocationServer::start()?;
        let launcher = AgentLauncher::new(
            &state,
            &client,
            &runtime_dir,
            &invocation_server,
            &config,
            (&workspaces, config.omar_dir),
            resumed_from.as_ref(),
        );
        let mut workers = BTreeMap::new();
        if let Some(template) = config.sandbox_template {
            for (instance, workspace) in &workspaces {
                let name = crate::sandbox::name(workspace);
                // Persist ownership before create; failures/crashes leave a
                // cleanup obligation instead of orphaning an unrecorded VM.
                {
                    let mut guard = record
                        .lock()
                        .map_err(|_| anyhow::anyhow!("deployment lock poisoned"))?;
                    guard.sandboxes.insert(instance.clone(), name.clone());
                    guard.sandbox_template = Some(template.to_owned());
                    guard.save(&runtime_dir)?;
                }
                crate::sandbox::create(
                    &name,
                    template,
                    &workspace.worktree(config.omar_dir),
                    &workspace.temp(config.omar_dir),
                )?;
                let worker = crate::sandbox::Worker::start(
                    &name,
                    &workspace.worktree(config.omar_dir),
                    &runtime_dir
                        .join("sandbox-logs")
                        .join(format!("{}.log", workspace.id)),
                )?;
                let init = crate::sandbox::Init {
                    protocol: crate::sandbox::PROTOCOL,
                    state: sandbox_state(&state, instance),
                    workspace: workspace.clone(),
                    root: config.omar_dir.to_path_buf(),
                    timeout: config.timeout,
                    resumed_from: resumed_from.clone(),
                };
                worker.request(serde_json::to_value(init)?, Duration::from_secs(180))?;
                observer.sandbox_started(instance, &name);
                workers.insert(instance.clone(), worker);
            }
        } else {
            spawn_topology_agents(&state, &launcher, config.replace, &mut spawned)?;
        }
        // A resumed run's inputs were consumed at its start tag; what it
        // continues with is in the checkpoint's queue.
        let inputs = if start_state.is_some() {
            BTreeMap::new()
        } else {
            parse_inputs(&state, config.inputs)?
        };
        Ok((
            invocation_server,
            inputs,
            reactions,
            workspaces,
            launcher,
            workers,
        ))
    })();
    let (invocation_server, inputs, reactions, workspaces, launcher, workers) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            observer.run_failed(&error.to_string());
            fail_deployment(&record, &runtime_dir, &spawned, &error);
            return Err(error);
        }
    };
    let web: BTreeSet<String> = state
        .agents
        .iter()
        .filter(|(_, agent)| is_web_backend(&agent.backend))
        .map(|(name, _)| name.clone())
        .collect();
    // Only when the program has one. A run with nothing web-backed gives the
    // daemon no panel to offer, rather than an empty one.
    if !web.is_empty() {
        if let Some(sender) = &config.panel_ready {
            let _ = sender.send(PanelAccess {
                endpoint: invocation_server.endpoint.clone(),
                token: invocation_server.token.clone(),
                agents: web.clone(),
            });
        }
    }
    let executor = DispatchExecutor {
        workspace_dirs: workspaces
            .iter()
            .map(|(name, ws)| {
                (
                    name.clone(),
                    (ws.worktree(config.omar_dir), ws.temp(config.omar_dir)),
                )
            })
            .collect(),
        state: &state,
        code: reactions,
        timeout: config.timeout,
        run_started: Instant::now(),
        agents: AgentReactionExecutor {
            client,
            team: state.team.clone(),
            registry: invocation_server.registry.clone(),
            timeout: config.timeout,
            run_started: Instant::now(),
            web,
            scoped: ScopedWorkers::new(
                &state,
                launcher,
                record.clone(),
                diagram_publisher.clone(),
            )?,
        },
    };
    let executor = SandboxExecutor {
        state: &state,
        workspaces: &workspaces,
        root: config.omar_dir,
        workers,
        timeout: config.timeout,
        host: executor,
    };
    let checkpointer = match RunCheckpointer::new(
        config.omar_dir,
        &runtime_dir,
        record.clone(),
        &workspaces,
        &state,
        bytecode,
        &config,
        resumed_from.as_ref(),
    ) {
        Ok(checkpointer) => checkpointer,
        Err(error) => {
            observer.run_failed(&error.to_string());
            fail_deployment(&record, &runtime_dir, &spawned, &error);
            return Err(error);
        }
    };
    if let Err(error) = advance_record(&record, &runtime_dir, DeploymentState::Running, None) {
        fail_deployment(&record, &runtime_dir, &spawned, &error);
        return Err(error);
    }
    // Flip RUNNING to STOPPING or PAUSING the moment a request lands, even
    // while the loop is blocked mid-tag, so an operator polling status sees
    // it acknowledged.
    let watcher_shutdown = Arc::new(AtomicBool::new(false));
    let watcher = {
        let record = record.clone();
        let dir = runtime_dir.clone();
        let shutdown = watcher_shutdown.clone();
        thread::spawn(move || {
            while !shutdown.load(Ordering::Acquire) {
                let acknowledged = match deploy::pending_request(&dir) {
                    Some(ControlOp::Stop) => Some((
                        DeploymentState::Stopping,
                        "stop requested; waiting for the current tag to close",
                    )),
                    Some(ControlOp::Pause) => Some((
                        DeploymentState::Pausing,
                        "pause requested; checkpointing at the next tag boundary",
                    )),
                    _ => None,
                };
                if let Some((next, detail)) = acknowledged {
                    if let Ok(mut guard) = record.lock() {
                        if guard.state == DeploymentState::Running
                            || (guard.state == DeploymentState::Pausing
                                && next == DeploymentState::Stopping)
                        {
                            let _ = guard.advance(next, Some(detail));
                            let _ = guard.save(&dir);
                        }
                    }
                    return;
                }
                thread::sleep(Duration::from_millis(250));
            }
        })
    };
    let start = match start_state {
        Some(saved) => LoopStart::Resume(saved),
        None => LoopStart::Fresh(inputs),
    };
    let outcome = run_event_loop_controlled(
        &state,
        start,
        &executor,
        observer,
        config.pace,
        Some(&runtime_dir),
        Some(&checkpointer),
    );
    // However the loop ended, no window's worker outlives it: a resume starts
    // new conversations, as a conversation cannot be rolled back.
    if let Some(scoped) = &executor.host.agents.scoped {
        scoped.close_all();
    }
    watcher_shutdown.store(true, Ordering::Release);
    let _ = watcher.join();
    let end = match outcome {
        Ok(end) => end,
        Err(error) => {
            observer.run_failed(&error.to_string());
            let sessions = record
                .lock()
                .map(|guard| guard.sessions.clone())
                .unwrap_or_default();
            let sessions = executor.host.agents.scoped.as_ref().map_or_else(
                || sessions.clone(),
                |workers| workers.cleanup_sessions(&sessions),
            );
            fail_deployment(&record, &runtime_dir, &sessions, &error);
            return Err(error);
        }
    };
    let sessions = record
        .lock()
        .map_err(|_| anyhow::anyhow!("deployment record lock poisoned"))?
        .sessions
        .clone();
    let sessions = executor.host.agents.scoped.as_ref().map_or_else(
        || sessions.clone(),
        |workers| workers.cleanup_sessions(&sessions),
    );
    if let Err(error) = write_json_atomic(&runtime_dir.join("state.json"), &state) {
        fail_deployment(&record, &runtime_dir, &sessions, &error);
        return Err(error);
    }
    if let LoopEnd::Paused(paused) = end {
        // The checkpoint is already published. What remains is to leave
        // nothing running: agents are torn down, and the record says the
        // run can continue from where it stands.
        if let Err(error) = write_json_atomic(&deploy::outputs_path(&runtime_dir), &paused.outputs)
        {
            fail_deployment(&record, &runtime_dir, &sessions, &error);
            return Err(error);
        }
        let cleanup_failures = cleanup_recorded_sessions(&record, &sessions, &runtime_dir);
        for failure in &cleanup_failures {
            eprintln!("warning: session not cleaned up: {failure}");
        }
        let checkpoint_id = {
            let mut guard = record
                .lock()
                .map_err(|_| anyhow::anyhow!("deployment record lock poisoned"))?;
            if guard.state == DeploymentState::Running {
                guard.advance(
                    DeploymentState::Pausing,
                    Some("pause honoured at a tag boundary"),
                )?;
            }
            let id = guard.checkpoint.clone().unwrap_or_default();
            guard.sessions_cleaned = cleanup_failures.is_empty();
            guard.state_vars = paused.state_vars.clone();
            guard.next_checkpoint_at = None;
            guard.advance(
                DeploymentState::Paused,
                Some(&format!("paused at checkpoint {id}")),
            )?;
            guard.save(&runtime_dir)?;
            id
        };
        deploy::clear_stop(&runtime_dir)?;
        observer.run_paused(&checkpoint_id);
        let next = paused
            .next_tag()
            .map(|(t, m)| format!("({t}, {m})"))
            .unwrap_or_else(|| "none".to_string());
        println!(
            "Topology '{}' paused at checkpoint {checkpoint_id}; next tag {next}; resume with `omar resume {}`",
            state.team, state.team
        );
        for (port, value) in &paused.outputs {
            println!("Output {port} = {value}");
        }
        return Ok(RunOutcome {
            end: RunEnd::Paused,
            outputs: paused.outputs,
            state: paused.state_vars,
            diagram: diagram_server.as_ref().map(DiagramServer::snapshot),
        });
    }
    let (settled, stopped) = match end {
        LoopEnd::Completed(settled) => (settled, false),
        LoopEnd::Stopped(settled) => (settled, true),
        LoopEnd::Paused(_) => unreachable!("handled above"),
    };
    let Settled {
        outputs,
        state_vars,
    } = settled;
    observer.run_completed(&outputs);
    if let Err(error) = write_json_atomic(&deploy::outputs_path(&runtime_dir), &outputs) {
        fail_deployment(&record, &runtime_dir, &sessions, &error);
        return Err(error);
    }
    let cleanup_failures = cleanup_recorded_sessions(&record, &sessions, &runtime_dir);
    for failure in &cleanup_failures {
        eprintln!("warning: session not cleaned up: {failure}");
    }
    {
        let mut guard = record
            .lock()
            .map_err(|_| anyhow::anyhow!("deployment record lock poisoned"))?;
        if stopped && guard.state == DeploymentState::Running {
            guard.advance(
                DeploymentState::Stopping,
                Some("stop honoured at a tag boundary"),
            )?;
        }
        let detail = if stopped {
            "stopped by request"
        } else {
            "run completed"
        };
        // The record keeps the state a run ended with, the way it keeps
        // the outputs, so a stopped run can be read back.
        guard.sessions_cleaned = cleanup_failures.is_empty();
        guard.state_vars = state_vars.clone();
        guard.next_checkpoint_at = None;
        guard.advance(DeploymentState::Terminated, Some(detail))?;
        guard.save(&runtime_dir)?;
    }
    deploy::clear_stop(&runtime_dir)?;
    if !cleanup_failures.is_empty() {
        // Something may still be writing: neither a final version nor a
        // removal can be trusted.
        eprintln!("warning: workspaces left as they are because agent cleanup failed");
    } else if config.keep_workspaces {
        // File versions are independent of runtime checkpoints. Agents have
        // been torn down; keep their final artifacts without reusing the directory.
        for workspace in workspaces.values() {
            if let Err(error) = workspace.snapshot(config.omar_dir, "Final topology files") {
                eprintln!(
                    "warning: could not snapshot workspace {}: {error:#}",
                    workspace.id
                );
            }
        }
    } else {
        // The run is over and nothing can resume it, so its files go with
        // it: the current workspaces and the ones its checkpoints kept for a
        // rollback. Copies an operator restored stay theirs.
        let record = record
            .lock()
            .map_err(|_| anyhow::anyhow!("deployment record lock poisoned"))?
            .clone();
        let (manifests, _) = checkpoint::Store::new(&runtime_dir).list()?;
        for workspace in
            crate::workspace::lineage(config.omar_dir, config.ea_id, &record, &manifests, false)?
        {
            if let Err(error) = workspace.remove(config.omar_dir) {
                eprintln!(
                    "warning: could not remove workspace {}: {error:#}",
                    workspace.id
                );
            }
        }
    }
    if stopped {
        println!("Topology '{}' stopped", state.team);
    } else {
        println!("Topology '{}' completed", state.team);
    }
    for (port, value) in &outputs {
        println!("Output {port} = {value}");
    }
    for (name, value) in &state_vars {
        println!("State {name} = {value}");
    }
    Ok(RunOutcome {
        end: if stopped {
            RunEnd::Stopped
        } else {
            RunEnd::Completed
        },
        outputs,
        state: state_vars,
        diagram: diagram_server.as_ref().map(DiagramServer::snapshot),
    })
}

const AGENT_PROTOCOL: &str = "You are an OMAR topology agent. Only act on OMAR INVOCATION messages. You cannot message other agents. For each invocation, you may use your normal tools to inspect, create, and edit files and run commands needed for the task. For topology communication, use omar_set_port only for the invocation's allowed output ports, and omar_set_state for your team's state, then call omar_complete to finish. Files and artifacts belong in your team instance workspace as described below. Port writes are buffered and repeated writes use last-writer-wins semantics.";

/// What it takes to start one topology agent's worker. Kept for the whole
/// run, because an agent with a `per` lifetime is started again for every
/// window.
#[derive(Clone)]
struct AgentLauncher {
    client: TmuxClient,
    team: String,
    runtime_dir: PathBuf,
    omar_dir: PathBuf,
    ea_id: crate::ea::EaId,
    base_prefix: String,
    health_idle_warning: i64,
    shutdown: Arc<AtomicBool>,
    endpoint: String,
    token: String,
    /// Each instance's worktree and temp directory.
    workspaces: BTreeMap<String, (PathBuf, PathBuf)>,
    /// What every agent of a resumed run is told; empty for a fresh run.
    resumed: String,
}

impl AgentLauncher {
    fn new(
        state: &VmState,
        client: &TmuxClient,
        runtime_dir: &Path,
        invocation_server: &InvocationServer,
        config: &TopologyRunConfig<'_>,
        workspace_access: (&BTreeMap<String, crate::workspace::Workspace>, &Path),
        resumed_from: Option<&checkpoint::Manifest>,
    ) -> Self {
        let (workspaces, workspace_root) = workspace_access;
        // No backend restores a conversation at a recorded turn, so a resumed
        // agent is told plainly that it continues a run it has no memory of.
        let resumed = resumed_from
            .map(|m| {
                let at = m
                    .completed_tag
                    .map(|(t, s)| format!("logical tag ({t}, {s})"))
                    .unwrap_or_else(|| "its start".to_string());
                format!(
                    "\n\nThis run resumed from checkpoint {} taken after {at}. Your earlier conversation was not restored; the files in your workspace are exactly as they were at that checkpoint. Continue from them.",
                    m.id
                )
            })
            .unwrap_or_default();
        Self {
            client: client.clone(),
            team: state.team.clone(),
            runtime_dir: runtime_dir.to_path_buf(),
            omar_dir: config.omar_dir.to_path_buf(),
            ea_id: config.ea_id,
            base_prefix: config.base_prefix.to_string(),
            health_idle_warning: config.health_idle_warning,
            shutdown: config.shutdown.clone(),
            endpoint: invocation_server.endpoint.clone(),
            token: invocation_server.token.clone(),
            workspaces: workspaces
                .iter()
                .map(|(name, ws)| {
                    (
                        name.clone(),
                        (ws.worktree(workspace_root), ws.temp(workspace_root)),
                    )
                })
                .collect(),
            resumed,
        }
    }

    /// Start `name`'s worker in its session and return the line it was
    /// launched with, which says how it proves readiness. `attempt` is set
    /// for an agent with a lifetime: the worker serves one window of it and
    /// opens the native conversation the attempt names.
    fn launch(
        &self,
        name: &str,
        agent: &AgentState,
        attempt: Option<&manager::TopologyAttempt>,
        resume: Option<&str>,
    ) -> Result<String> {
        let session = self.client.session_for(name);
        let agent_dir = self.runtime_dir.join("agents").join(name);
        fs::create_dir_all(&agent_dir)?;
        let prompt_file = agent_dir.join("system.md");
        let (worktree, temp) = self
            .workspaces
            .get(&agent.instance)
            .context("agent instance has no workspace")?;
        let workdir = worktree.to_str().context("workspace path is not UTF-8")?;
        // Nothing of an earlier window is put in front of it: what it knows
        // of the run is what the workspace holds.
        let scoped = match (attempt, agent.lifetime) {
            (Some(_), Some(lifetime)) => format!(
                "\n\nYour conversation is reset {lifetime}. Invocations within it share it. After a reset, earlier conversations are gone and you cannot see them; whatever later work needs from them is in the workspace files."
            ),
            _ => String::new(),
        };
        fs::write(&prompt_file, format!("{AGENT_PROTOCOL}\n\nYour team instance workspace is {workdir}. Put all persistent files and artifacts in this worktree. Use {} only for disposable files; temp is excluded from snapshots. Agents in your instance share this worktree; other instances have separate workspaces. File snapshots do not undo external actions.{}{scoped}\n", temp.display(), self.resumed))?;
        let backend = canonical_backend(&agent.backend);
        let base_command = crate::backend::resolve(backend)
            .map(|backend| backend.default_command().to_string())
            .map_err(anyhow::Error::msg)?;
        let context = McpLaunchContext {
            omar_dir: self.omar_dir.clone(),
            ea_id: self.ea_id,
            session_prefix: self.base_prefix.clone(),
            default_command: base_command.clone(),
            default_workdir: workdir.to_string(),
            health_idle_warning: self.health_idle_warning,
            agent_name: None,
            tmux_server: std::env::var("OMAR_TMUX_SERVER").ok(),
            // Topology agents answer invocations; they do not chat with the
            // operator, so they get no serve context.
            serve: None,
            topology: Some(TopologyMcpContext {
                team: self.team.clone(),
                agent: name.to_string(),
                endpoint: self.endpoint.clone(),
                token: self.token.clone(),
                attempt: attempt.cloned(),
                resume: resume.map(str::to_owned),
            }),
        };
        let command = manager::build_agent_command(&base_command, &prompt_file, &[], &context);
        let mut environment = vec![
            ("OMAR_WORKTREE".into(), workdir.into()),
            ("OMAR_TEMP".into(), temp.to_string_lossy().into_owned()),
            ("TMPDIR".into(), temp.to_string_lossy().into_owned()),
        ];
        // So what the attempt leaves running can be found once it stops.
        if let Some(attempt) = attempt {
            environment.push((ATTEMPT_MARKER.into(), attempt.id.clone()));
        }
        self.client.new_session_with_backend_env(
            &session,
            &command,
            Some(workdir),
            Some(backend),
            &environment,
        )?;
        Ok(command)
    }

    /// Wait for a launched worker's banner, when its backend paints one.
    fn wait_ready(&self, name: &str, agent: &AgentState, command: &str) -> Result<()> {
        let Some(backend) = crate::backend::by_name(canonical_backend(&agent.backend)) else {
            return Ok(());
        };
        if let crate::backend::Readiness::Banner(markers) = backend.readiness(command) {
            if !self.client.wait_for_markers(
                &self.client.session_for(name),
                markers,
                Duration::from_secs(60),
                Duration::from_millis(250),
            ) {
                bail!("agent '{}' did not become ready", name);
            }
        }
        Ok(())
    }
}

fn spawn_topology_agents(
    state: &VmState,
    launcher: &AgentLauncher,
    replace: bool,
    spawned: &mut BTreeMap<String, String>,
) -> Result<()> {
    let client = &launcher.client;
    // The line each agent was launched with, which is what says how it
    // proves readiness.
    let mut launched: BTreeMap<String, String> = BTreeMap::new();
    for (name, agent) in &state.agents {
        // A web agent is not spawned. There is no command to resolve, no pane
        // to put it in, and no readiness to wait for — the agent exists as a
        // name in the topology and an inbox in the registry.
        if !needs_agent_process(&agent.backend) {
            continue;
        }
        let session = client.session_for(name);
        if client.has_session(&session)? {
            if !replace {
                bail!(
                    "agent '{}' already exists; use --replace to restart it with scoped topology tools",
                    name
                );
            }
            client.ensure_session_not_attached(&session)?;
            crate::backend::kill_worker(client, &session, true)?;
        }
        // An agent with a lifetime has no worker until an invocation starts
        // its window.
        if agent.lifetime.is_some() {
            continue;
        }
        let command = launcher.launch(name, agent, None, None)?;
        spawned.insert(name.clone(), session);
        launched.insert(name.clone(), command);
    }

    for (name, command) in &launched {
        launcher.wait_ready(name, &state.agents[name], command)?;
    }
    Ok(())
}

/// Input ports nothing inside the topology writes to.
///
/// Reported by `/v1/programs/check` as a diagnostic, not as something to set: a
/// program that closes its loop has none, and one that has some has a port
/// nothing will ever drive. Naming them while the operator is still editing is
/// cheaper than discovering it from a run that sits still.
pub fn open_inputs(state: &VmState) -> Vec<String> {
    state
        .ports
        .iter()
        .filter(|(name, port)| {
            port.kind == PortKind::Input
                && !state
                    .connections
                    .iter()
                    .any(|connection| connection.target == **name)
        })
        .map(|(name, _)| name.clone())
        .collect()
}

pub fn parse_inputs(state: &VmState, raw_inputs: &[String]) -> Result<BTreeMap<String, Value>> {
    let mut inputs = BTreeMap::new();
    for raw in raw_inputs {
        let (name, raw_value) = raw
            .split_once('=')
            .with_context(|| format!("input '{raw}' must use NAME=VALUE"))?;
        let port = state
            .ports
            .get(name)
            .with_context(|| format!("unknown input port '{name}'"))?;
        if port.kind != PortKind::Input {
            bail!("port '{name}' is not an input");
        }
        let value = if port.ty == "path" {
            let path = fs::canonicalize(raw_value)
                .with_context(|| format!("input path '{}' does not exist", raw_value))?;
            Value::String(path.to_string_lossy().into_owned())
        } else {
            parse_input_value(&port.ty, raw_value)?
        };
        validate_value(&port.ty, &value)?;
        inputs.insert(name.to_string(), value);
    }
    for (name, port) in &state.ports {
        // An input fed by a connection gets its value from inside the topology,
        // so the operator has nothing to supply. Supplying one anyway is still
        // allowed: that is how a feedback loop is seeded.
        let connected = state
            .connections
            .iter()
            .any(|connection| connection.target == *name);
        if port.kind == PortKind::Input && !connected && !inputs.contains_key(name) {
            bail!("missing input '{name}'");
        }
    }
    Ok(inputs)
}

fn parse_input_value(ty: &str, value: &str) -> Result<Value> {
    // A refined string is still supplied as bare text on the command line;
    // whether it is one of the values the port admits is `validate_value`'s
    // answer, not the parser's.
    if string_enum(ty).is_some() {
        return Ok(Value::String(value.to_string()));
    }
    match ty {
        "bool" => Ok(Value::Bool(value.parse()?)),
        "int" => Ok(json!(value.parse::<i64>()?)),
        "float" => Ok(json!(value.parse::<f64>()?)),
        "string" | "path" | "bytes" => Ok(Value::String(value.to_string())),
        "signal" => Ok(Value::Null),
        _ => serde_json::from_str(value).context("complex input must be valid JSON"),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct Tag {
    timestamp: u64,
    microstep: u64,
}

impl Tag {
    const START: Self = Self {
        timestamp: 0,
        microstep: 0,
    };

    fn advance(self, delay: Delay) -> Result<Self> {
        if delay.time > 0 {
            return Ok(Self {
                timestamp: self
                    .timestamp
                    .checked_add(delay.time)
                    .context("logical timestamp overflow")?,
                microstep: 0,
            });
        }
        if delay.microstep {
            return Ok(Self {
                timestamp: self.timestamp,
                microstep: self
                    .microstep
                    .checked_add(1)
                    .context("logical microstep overflow")?,
            });
        }
        Ok(self)
    }
}

/// What one hop costs.
///
/// The distinction Lingua Franca draws, and the reason a fixpoint exists to be
/// reached: a plain connection is instantaneous, so a value written at a tag is
/// readable by the rest of that same tag. `after 0` costs a microstep, which is
/// how a loop closes without letting time pass. Anything larger is real time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Delay {
    /// Nanoseconds of logical time. Zero unless the hop was written with one.
    time: u64,
    /// Whether the hop still costs a microstep when no time passes.
    microstep: bool,
}

impl Delay {
    const INSTANT: Self = Self {
        time: 0,
        microstep: false,
    };
    const MICROSTEP: Self = Self {
        time: 0,
        microstep: true,
    };

    /// What `after n` costs: `after 0` buys a microstep, and anything larger
    /// buys that much logical time.
    fn after(time: u64) -> Self {
        if time == 0 {
            Self::MICROSTEP
        } else {
            Self {
                time,
                microstep: false,
            }
        }
    }

    /// A hop that settles within the tag it started in.
    fn is_instant(self) -> bool {
        self.time == 0 && !self.microstep
    }

    /// Two hops in a row: time adds, and a microstep anywhere costs one.
    fn then(self, other: Self) -> Result<Self> {
        Ok(Self {
            time: self
                .time
                .checked_add(other.time)
                .context("logical delay overflow")?,
            microstep: self.microstep || other.microstep,
        })
    }
}

/// What writing to `port` costs, before any connection is followed.
///
/// An action is the one port that carries a microstep of its own: that is what
/// makes it the way to schedule something later without naming a time, and what
/// keeps a self-loop through one from being a causality loop. Every other port
/// is instantaneous — a reaction's effect is readable at the tag it fired at.
fn port_delay(state: &VmState, port: &str) -> Delay {
    match state.ports.get(port) {
        Some(state) if state.kind == PortKind::Action => match state.delay {
            Some(time) if time > 0 => Delay {
                time,
                microstep: false,
            },
            _ => Delay::MICROSTEP,
        },
        _ => Delay::INSTANT,
    }
}

/// What a connection costs on top of its target port.
fn connection_delay(connection: &ConnectionState) -> Delay {
    match connection.delay {
        None => Delay::INSTANT,
        Some(time) => Delay::after(time),
    }
}

/// Write a value to a port, at this tag or a later one.
///
/// Returns whether this tag learned something, which is what tells the settling
/// loop it has more to do. An instantaneous hop joins the tag in progress; only
/// a hop that costs a microstep or real time goes on the queue.
fn deliver(
    state: &VmState,
    events: &mut BTreeMap<String, Value>,
    queue: &mut BTreeMap<Tag, BTreeMap<String, Value>>,
    current_tag: Tag,
    port: &str,
    value: Value,
    additional_delay: Delay,
) -> Result<bool> {
    if !state.ports.contains_key(port) {
        bail!("unknown scheduled port '{port}'");
    }
    let total = port_delay(state, port).then(additional_delay)?;
    if total.is_instant() {
        events.insert(port.to_string(), value);
        return Ok(true);
    }
    queue
        .entry(current_tag.advance(total)?)
        .or_default()
        .insert(port.to_string(), value);
    Ok(false)
}

/// Carry every connection whose source is present, until none is left to carry.
///
/// A connection fires at most once per tag — `carried` is what remembers — so
/// this terminates after at most one round per connection, and a chain of
/// instantaneous connections settles in the tag it started in.
fn settle_connections(
    state: &VmState,
    events: &mut BTreeMap<String, Value>,
    queue: &mut BTreeMap<Tag, BTreeMap<String, Value>>,
    tag: Tag,
    carried: &mut BTreeSet<usize>,
) -> Result<()> {
    loop {
        let mut learned = false;
        for (index, connection) in state.connections.iter().enumerate() {
            if carried.contains(&index) {
                continue;
            }
            let Some(value) = events.get(&connection.source).cloned() else {
                continue;
            };
            carried.insert(index);
            learned |= deliver(
                state,
                events,
                queue,
                tag,
                &connection.target,
                value,
                connection_delay(connection),
            )?;
        }
        if !learned {
            break;
        }
    }
    Ok(())
}

/// Reactions grouped so everything in a layer may run at once, and everything
/// in a later layer runs after.
///
/// Static: the order follows from the wiring, not from what happens to be
/// present. Walking it once per tag is what makes a reaction fire at most once
/// per tag, and only once every reaction that could decide one of its triggers
/// has had its turn — which is what "all triggers known" means operationally.
fn precedence_layers(state: &VmState) -> Result<Vec<Vec<String>>> {
    let mut indegree: BTreeMap<&str, usize> =
        state.reactions.keys().map(|id| (id.as_str(), 0)).collect();
    let mut outgoing: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for id in state.reactions.keys() {
        let after = must_follow(state, id);
        for later in &after {
            if let Some(count) = indegree.get_mut(later.as_str()) {
                *count += 1;
            }
        }
        outgoing.insert(id.as_str(), after);
    }

    let mut remaining: BTreeSet<&str> = state.reactions.keys().map(String::as_str).collect();
    let mut layers = Vec::new();
    while !remaining.is_empty() {
        let layer: Vec<&str> = remaining
            .iter()
            .copied()
            .filter(|id| indegree[id] == 0)
            .collect();
        if layer.is_empty() {
            bail!("causality loop among reactions: {remaining:?}");
        }
        for id in &layer {
            remaining.remove(id);
            for target in &outgoing[id] {
                if let Some(count) = indegree.get_mut(target.as_str()) {
                    *count -= 1;
                }
            }
        }
        layers.push(layer.into_iter().map(str::to_string).collect());
    }
    Ok(layers)
}

/// One logical tag a program would pass through.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimelineStep {
    pub timestamp: u64,
    pub microstep: u64,
    /// Ports and timers carrying a value at this tag, in name order.
    pub events: Vec<String>,
    /// Reactions whose triggers are present, in the order they would run.
    pub reactions: Vec<String>,
}

/// The tags a program would pass through, worked out without running it.
///
/// The determinism claim made visible: which tags a program reaches and what
/// fires at each are decided by the program, not by what the agents happen to
/// say. So they can be shown before anything is deployed.
///
/// `present` names the ports to treat as carrying a value at the first tag —
/// the inputs a run would be admitted with. Timers are always seeded; they are
/// what starts a program that starts itself. Presence is all that is needed:
/// a reaction fires on a trigger being there, whatever it holds.
///
/// Values are not worked out, only presence — an agent's answer is the one
/// thing here that is not determined. Two consequences worth knowing:
///
/// - a reaction whose contract offers alternatives (`-> (x|y)`) is shown as
///   writing *both*, because which one it picks is the agent's to decide. This
///   over-approximates, and never misses a step;
/// - it cannot know a value that decides a delay, and nothing in the language
///   has one, which is why this works at all.
///
/// Scheduling is not re-implemented here. `settle_connections`, `deliver` and
/// `precedence_layers` are the same ones the event loop walks, so what settles
/// at a tag — and which tag it settles at — cannot drift between what is shown
/// and what runs.
pub fn timeline(
    state: &VmState,
    present: &BTreeSet<String>,
    max_steps: usize,
) -> (Vec<TimelineStep>, bool) {
    let seed: BTreeMap<String, Value> = present
        .iter()
        .filter(|name| state.ports.contains_key(*name))
        .map(|name| (name.clone(), Value::Null))
        .collect();
    let mut queue = BTreeMap::from([(Tag::START, seed)]);
    for (name, timer) in state.timers.iter().filter(|(_, timer)| !timer.shutdown) {
        queue
            .entry(Tag {
                timestamp: timer.offset,
                microstep: 0,
            })
            .or_default()
            .insert(name.clone(), Value::Null);
    }

    // A program with a causality loop has no order to project; `verify` refuses
    // it before a run, and there is nothing to show for one here either.
    let Ok(layers) = precedence_layers(state) else {
        return (Vec::new(), false);
    };

    let mut steps = Vec::new();
    while let Some((tag, events)) = queue.pop_first() {
        if events.is_empty() {
            continue;
        }
        if steps.len() >= max_steps {
            return (steps, true);
        }

        let mut events = events;
        let mut carried = BTreeSet::new();
        let _ = settle_connections(state, &mut events, &mut queue, tag, &mut carried);

        for (name, timer) in &state.timers {
            if timer.period > 0 && events.contains_key(name) {
                let Some(next) = tag.timestamp.checked_add(timer.period) else {
                    continue;
                };
                queue
                    .entry(Tag {
                        timestamp: next,
                        microstep: 0,
                    })
                    .or_default()
                    .insert(name.clone(), Value::Null);
            }
        }

        // The same walk down the precedence order the event loop makes, with
        // every effect taken rather than the one an agent would have picked.
        let mut fired = Vec::new();
        for layer in &layers {
            let mut enabled: Vec<_> = layer
                .iter()
                .filter_map(|id| state.reactions.get_key_value(id))
                .filter(|(_, reaction)| {
                    reaction
                        .triggers
                        .iter()
                        .any(|trigger| events.contains_key(trigger))
                })
                .collect();
            if enabled.is_empty() {
                continue;
            }
            enabled.sort_by_key(|(_, reaction)| reaction.order);
            for (id, reaction) in &enabled {
                fired.push((*id).clone());
                for effect in &reaction.effects {
                    let _ = deliver(
                        state,
                        &mut events,
                        &mut queue,
                        tag,
                        effect,
                        Value::Null,
                        Delay::INSTANT,
                    );
                }
            }
            let _ = settle_connections(state, &mut events, &mut queue, tag, &mut carried);
        }

        steps.push(TimelineStep {
            timestamp: tag.timestamp,
            microstep: tag.microstep,
            events: events.keys().cloned().collect(),
            reactions: fired,
        });
    }
    (steps, false)
}

/// How a run's logical clock is held against the wall clock.
///
/// Real time by default: a tag at logical timestamp T does not run before T has
/// elapsed since the run began, which is what makes a delay a promise about
/// when rather than only about order. `Fast` runs the same program as quickly
/// as the work allows, which is what a test wants and what `--fast` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    RealTime,
    Fast,
}

/// What a run leaves behind: its outputs, and the last value of every state
/// variable.
struct Settled {
    outputs: BTreeMap<String, Value>,
    state_vars: BTreeMap<String, Value>,
}

/// How the loop ended: the queue drained, a stop closed the run at a tag
/// boundary, or a pause did with a checkpoint behind it. Either way what
/// settled so far comes along.
enum LoopEnd {
    Completed(Settled),
    Stopped(Settled),
    Paused(ExecutionState),
}

/// Where a loop begins: the start tag seeded with the run's inputs, or the
/// boundary a checkpoint captured.
enum LoopStart {
    Fresh(BTreeMap<String, Value>),
    Resume(ExecutionState),
}

/// What the loop asks of the run's checkpoint controller at a boundary.
/// Operator requests arrive through the deployment's control file; this is
/// only the part that needs the run's own state.
pub(crate) trait Checkpointer {
    /// Whether an automatic capture is due at this boundary, which completed
    /// `state.completed_tag`.
    fn due(&self, state: &ExecutionState) -> bool;
    /// Publish a checkpoint for `state`. Returns its id.
    fn capture(&self, state: &ExecutionState, trigger: Trigger) -> Result<String>;
    /// A capture attempt failed. `held` says the internal retries are spent
    /// and the run now waits at its boundary for an operator.
    fn capture_failed(&self, error: &anyhow::Error, attempt: u32, held: bool);
}

/// What a boundary decided.
enum Boundary {
    Continue,
    Stop,
    Pause(ExecutionState),
}

/// Bounded internal retries before a failed capture holds the run for an
/// operator's `checkpoint retry`.
const CAPTURE_RETRIES: u32 = 3;

/// Honour whatever is owed at a tag boundary: an operator's stop, a capture
/// that is due or was asked for, or a pause. Nothing is in flight here, so a
/// capture describes exactly the queue as it stands.
fn settle_boundary(
    deployment_dir: Option<&Path>,
    checkpointer: Option<&dyn Checkpointer>,
    observer: &dyn TopologyObserver,
    snapshot: &dyn Fn() -> ExecutionState,
) -> Result<Boundary> {
    let request = deployment_dir.and_then(deploy::pending_request);
    let mut trigger = match request {
        Some(ControlOp::Stop) => return Ok(Boundary::Stop),
        Some(ControlOp::Pause) => Some(Trigger::Pause),
        Some(ControlOp::Checkpoint) | Some(ControlOp::RetryCheckpoint) => Some(Trigger::Manual),
        None => None,
    };
    let Some(checkpointer) = checkpointer else {
        // A loop with nothing to checkpoint with cannot pause; the request
        // stays for a runner that can. Tests and projections land here.
        return Ok(Boundary::Continue);
    };
    if trigger.is_none() && checkpointer.due(&snapshot()) {
        trigger = Some(Trigger::Automatic);
    }
    let Some(mut trigger) = trigger else {
        return Ok(Boundary::Continue);
    };
    let mut attempt = 0;
    // The state that was captured is the state a pause hands back: one
    // snapshot, so what the checkpoint says and what the runner reports agree
    // to the nanosecond.
    let mut captured;
    loop {
        captured = snapshot();
        match checkpointer.capture(&captured, trigger) {
            Ok(id) => {
                let kind = match trigger {
                    Trigger::Automatic => "automatic",
                    Trigger::Manual => "manual",
                    Trigger::Pause => "pause",
                    Trigger::Mutation => "mutation",
                };
                observer.checkpoint_published(&id, kind, captured.completed_tag);
                break;
            }
            Err(error) => {
                attempt += 1;
                checkpointer.capture_failed(&error, attempt, attempt > CAPTURE_RETRIES);
                if attempt <= CAPTURE_RETRIES {
                    thread::sleep(Duration::from_secs(1 << (attempt - 1)));
                    continue;
                }
            }
        }
        // Held at the boundary: the previous checkpoint stands, the next tag
        // does not run, and other topologies are unaffected. Only an operator
        // moves this run again. The request that asked for this capture is
        // consumed first (its trigger is kept), or the hold would answer it
        // again at once and no retry could be admitted.
        if let Some(dir) = deployment_dir {
            if matches!(
                deploy::pending_request(dir),
                Some(ControlOp::Checkpoint | ControlOp::RetryCheckpoint | ControlOp::Pause)
            ) {
                deploy::clear_stop(dir)?;
            }
        }
        loop {
            thread::sleep(Duration::from_millis(250));
            match deployment_dir.and_then(deploy::pending_request) {
                Some(ControlOp::Stop) => return Ok(Boundary::Stop),
                Some(ControlOp::Pause) => {
                    trigger = Trigger::Pause;
                    break;
                }
                Some(ControlOp::Checkpoint) | Some(ControlOp::RetryCheckpoint) => break,
                None => {}
            }
        }
        attempt = 0;
    }
    if trigger == Trigger::Pause {
        return Ok(Boundary::Pause(captured));
    }
    if let Some(dir) = deployment_dir {
        // A manual request is answered; a stop or pause that lands later
        // must not be mistaken for it.
        if matches!(
            deploy::pending_request(dir),
            Some(ControlOp::Checkpoint) | Some(ControlOp::RetryCheckpoint)
        ) {
            deploy::clear_stop(dir)?;
        }
    }
    Ok(Boundary::Continue)
}

fn queued(queue: &BTreeMap<Tag, BTreeMap<String, Value>>) -> Vec<QueuedTag> {
    queue
        .iter()
        .map(|(tag, events)| QueuedTag {
            timestamp: tag.timestamp,
            microstep: tag.microstep,
            events: events.clone(),
        })
        .collect()
}

#[cfg(test)]
fn run_event_loop<E: ReactionExecutor>(
    state: &VmState,
    inputs: BTreeMap<String, Value>,
    executor: &E,
) -> Result<BTreeMap<String, Value>> {
    match run_event_loop_observed(
        state,
        inputs,
        executor,
        &NoopTopologyObserver,
        Pace::Fast,
        None,
    )? {
        LoopEnd::Completed(settled) | LoopEnd::Stopped(settled) => Ok(settled.outputs),
        LoopEnd::Paused(state) => Ok(state.outputs),
    }
}

#[cfg(test)]
fn run_event_loop_observed<E: ReactionExecutor>(
    state: &VmState,
    inputs: BTreeMap<String, Value>,
    executor: &E,
    observer: &dyn TopologyObserver,
    pace: Pace,
    deployment_dir: Option<&Path>,
) -> Result<LoopEnd> {
    run_event_loop_controlled(
        state,
        LoopStart::Fresh(inputs),
        executor,
        observer,
        pace,
        deployment_dir,
        None,
    )
}

fn run_event_loop_controlled<E: ReactionExecutor>(
    state: &VmState,
    start: LoopStart,
    executor: &E,
    observer: &dyn TopologyObserver,
    pace: Pace,
    deployment_dir: Option<&Path>,
    checkpointer: Option<&dyn Checkpointer>,
) -> Result<LoopEnd> {
    // When this process joined the run's logical clock. A resumed run has
    // already spent `base_elapsed` on that clock, so time is measured from
    // here plus that: the clock continues where the checkpoint left it, and
    // time spent paused is not on it.
    let origin = Instant::now();
    let (mut queue, mut outputs, mut store, base_elapsed, mut completed) = match start {
        LoopStart::Fresh(inputs) => {
            let mut queue = BTreeMap::from([(Tag::START, inputs)]);
            // Arm every timer for its first firing. A timer's value is the
            // timestamp it fired at, which is what makes `$(t)` in a prompt
            // worth reading. `shutdown` waits for the run's last tag.
            for (name, timer) in state.timers.iter().filter(|(_, timer)| !timer.shutdown) {
                queue
                    .entry(Tag {
                        timestamp: timer.offset,
                        microstep: 0,
                    })
                    .or_default()
                    .insert(name.clone(), json!(timer.offset));
            }
            // Every state variable starts where the program said and lives
            // here for the run: an invocation sees its instance's, and hands
            // back what it set.
            let store: BTreeMap<String, Value> = state
                .state_vars
                .iter()
                .map(|(name, var)| (name.clone(), var.initial.clone()))
                .collect();
            (queue, BTreeMap::new(), store, Duration::ZERO, None)
        }
        LoopStart::Resume(saved) => {
            // Timers are not re-armed from their declarations: their next
            // firings are in the saved queue, and a one-shot already consumed
            // has none there.
            let queue = saved
                .queue
                .into_iter()
                .map(|entry| {
                    (
                        Tag {
                            timestamp: entry.timestamp,
                            microstep: entry.microstep,
                        },
                        entry.events,
                    )
                })
                .collect();
            (
                queue,
                saved.outputs,
                saved.state_vars,
                Duration::from_nanos(saved.elapsed_ns),
                saved.completed_tag,
            )
        }
    };
    let elapsed = || origin.elapsed() + base_elapsed;
    // Where logical time zero stands on the wall clock, for anything that
    // counts down to a tag: agents are up, so from here the clock is real.
    observer
        .clock_started(deploy::now_unix_millis().saturating_sub(base_elapsed.as_millis() as u64));

    // No bound on how many tags a run may pass through. A loop that costs a
    // microstep somewhere -- through an action, or a connection written
    // `after 0` -- is a walk through superdense time rather than a knot to be
    // untied, and `verify` has already refused the ones that cost nothing.
    // What used to be a runaway guard mostly cut long-lived programs short —
    // and silently reported success when it did.
    //
    // The real limits are the two `checked_add`s in `Tag::advance`, which end a
    // run that exhausts u64 rather than wrapping it into the past.
    // Fixed before the first tag: precedence follows from the wiring, and the
    // wiring does not change while a run is in flight.
    let layers = precedence_layers(state)?;
    // A body called `request_stop()` at this tag.
    let mut stop_requested = false;
    // The queue holds only the `shutdown` tag: the run ends once it is over.
    let mut shutting_down = false;

    loop {
        // A tag boundary: nothing is in flight, so this is where an
        // operator's stop ends the run without abandoning any invocation's
        // contract, and where a checkpoint describes the run exactly. The
        // next tag is still queued, never popped before it runs, so a pause
        // here loses nothing.
        let settled = {
            let snapshot = || ExecutionState {
                version: checkpoint::FORMAT,
                completed_tag: completed,
                queue: queued(&queue),
                outputs: outputs.clone(),
                state_vars: store.clone(),
                elapsed_ns: elapsed().as_nanos() as u64,
            };
            // The last tag is not a boundary a pause can hold: resumed, it
            // would end the run without remembering it had begun to.
            if shutting_down {
                Boundary::Continue
            } else {
                settle_boundary(deployment_dir, checkpointer, observer, &snapshot)?
            }
        };
        match settled {
            Boundary::Continue => {}
            Boundary::Stop => {
                return Ok(LoopEnd::Stopped(Settled {
                    outputs,
                    state_vars: store,
                }))
            }
            Boundary::Pause(state) => return Ok(LoopEnd::Paused(state)),
        }
        let Some((&tag, next_events)) = queue.first_key_value() else {
            // Nothing left to happen is an end too, and `shutdown` hears of it.
            if shutting_down || !arm_shutdown(state, &mut queue, completed)? {
                break;
            }
            shutting_down = true;
            continue;
        };
        // A tag nothing is present at is not a moment the run passed through.
        // No reaction can fire at one -- enabling asks whether any trigger is
        // in `events`, which is false for every reaction when it is empty --
        // and no connection, output or timer can move either. Announcing it
        // would tell a client the run had advanced when nothing had. The only
        // way to reach one is a program admitted with no inputs, which seeds
        // the start tag with an empty map.
        if next_events.is_empty() {
            queue.pop_first();
            continue;
        }
        let due = Duration::from_nanos(tag.timestamp);
        if pace == Pace::RealTime {
            // Microsteps carry no time, so only the timestamp is owed. A tag
            // whose moment has already passed runs now rather than being
            // pushed further out: being late is not a reason to be later.
            // Sliced so a stop during a long wait is honoured within a beat,
            // and so a checkpoint that falls due mid-wait is taken at this
            // boundary, with the awaited tag still queued and unexecuted.
            while let Some(remaining) = due.checked_sub(elapsed()) {
                let settled = {
                    let snapshot = || ExecutionState {
                        version: checkpoint::FORMAT,
                        completed_tag: completed,
                        queue: queued(&queue),
                        outputs: outputs.clone(),
                        state_vars: store.clone(),
                        elapsed_ns: elapsed().as_nanos() as u64,
                    };
                    settle_boundary(deployment_dir, checkpointer, observer, &snapshot)?
                };
                match settled {
                    Boundary::Continue => {}
                    Boundary::Stop => {
                        return Ok(LoopEnd::Stopped(Settled {
                            outputs,
                            state_vars: store,
                        }))
                    }
                    Boundary::Pause(state) => return Ok(LoopEnd::Paused(state)),
                }
                thread::sleep(remaining.min(Duration::from_millis(250)));
            }
        }
        let Some((tag, events)) = queue.pop_first() else {
            break;
        };
        // Everything this tag knows. It grows as the tag settles: an
        // instantaneous connection or a reaction's effect joins it rather than
        // landing on a later tag, and the reactions downstream read it before
        // the tag is over.
        let mut events = events;
        let mut carried = BTreeSet::new();
        // How far past its logical time this tag actually ran. Zero while the
        // run is on or ahead of schedule, and growing only when the work
        // outlasts the gap it was given -- which is the one number that says
        // whether a program is keeping the promise its delays make.
        let lag = elapsed().saturating_sub(due).as_nanos() as u64;
        // Settle before observing, so what a tag reports includes the values
        // its connections carried into it rather than only what it was woken
        // with.
        settle_connections(state, &mut events, &mut queue, tag, &mut carried)?;
        observer.tag_advanced(tag.timestamp, tag.microstep, lag, &events);
        for (name, timer) in &state.timers {
            if timer.period > 0 && events.contains_key(name) {
                let next = tag
                    .timestamp
                    .checked_add(timer.period)
                    .context("logical timestamp overflow")?;
                queue
                    .entry(Tag {
                        timestamp: next,
                        microstep: 0,
                    })
                    .or_default()
                    .insert(name.clone(), json!(next));
            }
        }
        // One pass down the precedence order. A layer's reactions cannot
        // decide each other's triggers -- that is what put them in one layer --
        // so running them together is safe, and by the time a layer is reached
        // every reaction that could feed it has already had its turn.
        for layer in &layers {
            let mut enabled: Vec<_> = layer
                .iter()
                .filter_map(|id| state.reactions.get_key_value(id))
                .filter(|(_, reaction)| {
                    reaction
                        .triggers
                        .iter()
                        .any(|trigger| events.contains_key(trigger))
                })
                .collect();
            if enabled.is_empty() {
                continue;
            }
            enabled.sort_by_key(|(_, reaction)| reaction.order);

            let specs: Vec<_> = enabled
                .iter()
                .map(|entry| invocation_spec(state, *entry, &events, &store, tag))
                .collect::<Result<_>>()?;
            for spec in &specs {
                observer.reaction_started(
                    tag.timestamp,
                    tag.microstep,
                    &spec.reaction_id,
                    &spec.id.to_string(),
                );
            }
            let task_ids: Vec<_> = specs.iter().map(|spec| spec.id).collect();
            // An agent with a lifetime shares a conversation between its
            // invocations in a tag, so they run one after another in
            // declaration order, the later one seeing what the earlier left
            // in it; every other invocation runs beside the rest.
            let mut sequences: Vec<Vec<(usize, InvocationSpec)>> = Vec::new();
            for (index, spec) in specs.into_iter().enumerate() {
                let shared = state
                    .agents
                    .get(&spec.agent)
                    .is_some_and(|agent| agent.lifetime.is_some());
                match sequences
                    .iter_mut()
                    .find(|run| shared && run[0].1.agent == spec.agent)
                {
                    Some(run) => run.push((index, spec)),
                    None => sequences.push(vec![(index, spec)]),
                }
            }
            let mut results: Vec<(usize, Result<BTreeMap<String, Value>>)> =
                thread::scope(|scope| {
                    let handles: Vec<_> = sequences
                        .into_iter()
                        .map(|run| {
                            scope.spawn(move || {
                                // Stops at the first failure: nothing later
                                // in the sequence may start a worker or act
                                // in a run that is already failing.
                                let mut done = Vec::new();
                                for (index, spec) in run {
                                    let result = executor.invoke(spec);
                                    let failed = result.is_err();
                                    done.push((index, result));
                                    if failed {
                                        break;
                                    }
                                }
                                done
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .map(|handle| {
                            handle
                                .join()
                                .map_err(|_| anyhow::anyhow!("reaction executor panicked"))
                        })
                        .collect::<Result<Vec<_>>>()
                })?
                .into_iter()
                .flatten()
                .collect();
            results.sort_by_key(|(index, _)| *index);
            let results = results
                .into_iter()
                .map(|(_, result)| result)
                .collect::<Result<Vec<_>>>()?;

            // Declaration order decides who wins a port two reactions in this
            // layer both write, which is the rule the language states.
            let mut completed_writes: Vec<_> = enabled
                .iter()
                .zip(task_ids)
                .zip(results)
                .map(|(((id, reaction), task_id), mut writes)| {
                    stop_requested |= writes.remove(crate::reaction::STOP).is_some();
                    observer.reaction_completed(
                        tag.timestamp,
                        tag.microstep,
                        id,
                        &task_id.to_string(),
                        &writes,
                    );
                    (reaction.order, writes)
                })
                .collect();
            completed_writes.sort_by_key(|(order, _)| *order);
            for (_, writes) in completed_writes {
                for (port, value) in writes {
                    // A state variable's new value stays with its instance
                    // rather than travelling anywhere.
                    if state.state_vars.contains_key(&port) {
                        store.insert(port, value);
                        continue;
                    }
                    deliver(
                        state,
                        &mut events,
                        &mut queue,
                        tag,
                        &port,
                        value,
                        Delay::INSTANT,
                    )?;
                }
            }
            settle_connections(state, &mut events, &mut queue, tag, &mut carried)?;
        }

        // Read the outputs once the tag has settled, so a value a reaction
        // wrote at this tag counts as this tag's output.
        for (name, value) in &events {
            if state
                .ports
                .get(name)
                .is_some_and(|p| p.kind == PortKind::Output)
            {
                outputs.insert(name.clone(), value.clone());
            }
        }
        completed = Some((tag.timestamp, tag.microstep));
        if shutting_down {
            break;
        }
        // Whatever else was queued never happens: the run ends here, after
        // one more microstep for `shutdown`.
        if stop_requested {
            queue.clear();
            if !arm_shutdown(state, &mut queue, completed)? {
                break;
            }
            shutting_down = true;
        }
    }
    Ok(LoopEnd::Completed(Settled {
        outputs,
        state_vars: store,
    }))
}

/// Queue every `shutdown` trigger one microstep after the last tag, or at the
/// start of a run that passed through none. False when nothing reacts to it.
fn arm_shutdown(
    state: &VmState,
    queue: &mut BTreeMap<Tag, BTreeMap<String, Value>>,
    last: Option<(u64, u64)>,
) -> Result<bool> {
    let tag = match last {
        Some((timestamp, microstep)) => Tag {
            timestamp,
            microstep,
        }
        .advance(Delay::MICROSTEP)?,
        None => Tag::START,
    };
    let events: BTreeMap<_, _> = state
        .timers
        .iter()
        .filter(|(_, timer)| timer.shutdown)
        .map(|(name, _)| (name.clone(), json!(tag.timestamp)))
        .collect();
    if events.is_empty() {
        return Ok(false);
    }
    queue.insert(tag, events);
    Ok(true)
}

fn invocation_spec(
    state: &VmState,
    (reaction_id, reaction): (&String, &ReactionState),
    events: &BTreeMap<String, Value>,
    store: &BTreeMap<String, Value>,
    tag: Tag,
) -> Result<InvocationSpec> {
    let trigger_values = reaction
        .triggers
        .iter()
        .filter_map(|trigger| {
            events
                .get(trigger)
                .map(|value| (trigger.clone(), value.clone()))
        })
        .collect();
    let mut allowed_effects = BTreeMap::new();
    let mut effect_names = BTreeMap::new();
    for effect in &reaction.effects {
        let port = state
            .ports
            .get(effect)
            .with_context(|| format!("unknown effect port '{effect}'"))?;
        allowed_effects.insert(effect.clone(), port.ty.clone());
        // As the program spelled it, so the agent reads `Decision` rather
        // than the refinement it expands to.
        effect_names.insert(
            effect.clone(),
            port.declared.clone().unwrap_or_else(|| port.ty.clone()),
        );
    }
    let types = types_for_effects(state, &effect_names);
    // Only its own instance's: a body reaches state through `self`, a task
    // through `$(name)` and `omar_set_state`.
    let state_types = state
        .state_vars
        .iter()
        .filter(|(_, var)| var.instance == reaction.instance)
        .map(|(name, var)| (name.clone(), var.ty.clone()))
        .collect();
    let state_values = state
        .state_vars
        .iter()
        .filter(|(_, var)| var.instance == reaction.instance)
        .filter_map(|(name, _)| store.get(name).map(|value| (name.clone(), value.clone())))
        .collect();
    let instance_contexts = || {
        state
            .state_vars
            .iter()
            .filter(|(_, var)| var.instance == reaction.instance && !var.agents.is_empty())
    };
    let contexts = instance_contexts()
        .filter(|(_, var)| var.agents.contains(&reaction.agent))
        .map(|(name, var)| (name.clone(), var.directive.clone()))
        .collect();
    let context_names = instance_contexts().map(|(name, _)| name.clone()).collect();
    Ok(InvocationSpec {
        id: next_task_id()?,
        reaction_id: reaction_id.clone(),
        agent: reaction.agent.clone(),
        trigger_values,
        allowed_effects,
        effect_names,
        types,
        state_values,
        state_types,
        contexts,
        context_names,
        contract: reaction.contract.clone(),
        prompt: reaction.prompt.clone(),
        within: reaction.within.map(Duration::from_nanos),
        tag,
    })
}

// Process-wide allocation keeps IDs distinct across concurrent runs and never
// reuses a completed task's ID. Stay within JSON's exact integer range.
static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

fn next_task_id() -> Result<u64> {
    // Explicit CAS loop: `fetch_update` is deprecated on newer toolchains and
    // its replacement `try_update` postdates the minimum supported Rust.
    let mut id = NEXT_TASK_ID.load(Ordering::Relaxed);
    loop {
        if id >= 1_u64 << 53 {
            anyhow::bail!("task ID space exhausted");
        }
        match NEXT_TASK_ID.compare_exchange_weak(id, id + 1, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Ok(id),
            Err(current) => id = current,
        }
    }
}

/// The message an agent is handed, with `rendered` as its prompt.
///
/// The header is one `key: value` per line up to the first blank line, which
/// is what the stub agent parses. Effects are spelled as the program declared
/// them, and `types` maps each name used back to the type the runtime checks.
/// What the schema said about a type follows the header, where the agent
/// reads it as prose.
fn invocation_message(invocation: &InvocationSpec, rendered: &str) -> Result<String> {
    let mut header = format!(
        "OMAR INVOCATION\ntask_id: {}\ntriggers: {}\neffects: {}\n",
        invocation.id,
        serde_json::to_string(&invocation.trigger_values)?,
        serde_json::to_string(&invocation.effect_names)?,
    );
    let mut described = String::new();
    if !invocation.types.is_empty() {
        header.push_str(&format!(
            "types: {}\n",
            serde_json::to_string(&invocation.types)?
        ));
        for (name, declared) in &invocation.types {
            described.push_str(&describe_type(name, declared));
            described.push('\n');
        }
        described.push('\n');
    }
    header.push_str(&format!("contract: {}\n", invocation.contract));
    let mut state_note = String::new();
    // A context this agent shares is set out on its own line, with how to
    // keep it, and not repeated as state.
    if !invocation.contexts.is_empty() {
        header.push_str(&format!(
            "contexts: {}\n",
            serde_json::to_string(&context_view(
                &invocation.contexts,
                &invocation.state_values
            ))?
        ));
    }
    let state: BTreeMap<_, _> = invocation
        .state_values
        .iter()
        .filter(|(name, _)| !invocation.contexts.contains_key(*name))
        .collect();
    if !state.is_empty() {
        header.push_str(&format!("state: {}\n", serde_json::to_string(&state)?));
        state_note = " The state above is your team's, kept between its tasks: change any of it with omar_set_state(task_id, name, value) before omar_complete, if the next task should see something else.".to_string();
    }
    let context_note = if invocation.contexts.is_empty() {
        ""
    } else {
        " Each context above is a note your team keeps between its tasks: before omar_complete, set it to its full updated text with omar_set_state, or pass it in omar_complete's keep list if nothing changed."
    };
    Ok(format!(
        "{header}\n{described}{rendered}\n\nUse omar_set_port for each effect you choose, then call omar_complete exactly once. Pass the integer task_id above to both tools. For a signal effect, set its value to null.{state_note}{context_note} Do not address another agent directly."
    ))
}

/// One line saying what a named type admits and what its schema said.
fn describe_type(name: &str, declared: &TypeState) -> String {
    let mut line = match string_enum(&declared.ty) {
        Some(allowed) => {
            // Quoted by serde: an admitted value may contain a quote.
            let options: Vec<String> = allowed.iter().map(|o| json!(o).to_string()).collect();
            format!("Type {name} is one of {}.", options.join(", "))
        }
        None => format!("Type {name} is {}.", declared.ty),
    };
    if let Some(title) = declared
        .title
        .as_deref()
        .filter(|t| !t.is_empty() && *t != name)
    {
        line.push_str(&format!(" Title: {title}."));
    }
    if let Some(description) = declared.description.as_deref().filter(|d| !d.is_empty()) {
        line.push_str(&format!(" {}", description.trim()));
        if !description.trim().ends_with('.') {
            line.push('.');
        }
    }
    line
}

/// The named types the effects' spellings use.
///
/// By name, not by what the name expands to: two imports may define the same
/// values, and the agent is told the one the program wrote.
fn types_for_effects(
    state: &VmState,
    effect_names: &BTreeMap<String, String>,
) -> BTreeMap<String, TypeState> {
    let named: BTreeSet<&str> = effect_names
        .values()
        .map(|spelled| leaf_type(spelled))
        .collect();
    state
        .types
        .iter()
        .filter(|(name, _)| named.contains(name.as_str()))
        .map(|(name, defined)| (name.clone(), defined.clone()))
        .collect()
}

fn render_prompt(template: &str, values: &BTreeMap<String, Value>) -> Result<String> {
    let mut rendered = String::new();
    let mut remaining = template;
    while let Some(start) = remaining.find("$(") {
        rendered.push_str(&remaining[..start]);
        let expression = &remaining[start + 2..];
        let end = interpolation_end(expression).context("unterminated prompt interpolation")?;
        let identifier = interpolation_identifier(&expression[..end])?;
        match values.get(&identifier) {
            Some(Value::String(text)) => rendered.push_str(text),
            Some(other) => rendered.push_str(&other.to_string()),
            None => rendered.push_str("<absent>"),
        }
        remaining = &expression[end + 1..];
    }
    rendered.push_str(remaining);
    Ok(rendered)
}

fn interpolation_end(expression: &str) -> Option<usize> {
    let bytes = expression.as_bytes();
    let mut depth = 1usize;
    let mut index = 0usize;
    let mut block_comment = false;
    let mut line_comment = false;
    while index < bytes.len() {
        if line_comment {
            if bytes[index] == b'\n' {
                line_comment = false;
            }
            index += 1;
            continue;
        }
        if block_comment {
            if bytes.get(index..index + 2) == Some(b"*/") {
                block_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if bytes.get(index..index + 2) == Some(b"//") {
            line_comment = true;
            index += 2;
        } else if bytes.get(index..index + 2) == Some(b"/*") {
            block_comment = true;
            index += 2;
        } else if bytes[index] == b'(' {
            depth += 1;
            index += 1;
        } else if bytes[index] == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
            index += 1;
        } else {
            index += 1;
        }
    }
    None
}

fn interpolation_identifier(expression: &str) -> Result<String> {
    let mut clean = String::new();
    let mut chars = expression.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(ch) = chars.next() {
                if ch == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    break;
                }
            }
        } else if ch == '/' && chars.peek() == Some(&'/') {
            break;
        } else {
            clean.push(ch);
        }
    }
    clean
        .split_whitespace()
        .next()
        .map(str::to_string)
        .context("empty prompt interpolation")
}

pub(crate) fn valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `instance.member`, the name a team instance gives what it declares. Plain
/// identifiers are qualified names with nothing to qualify.
fn valid_qualified_name(value: &str) -> bool {
    !value.is_empty() && value.split('.').all(valid_identifier)
}

/// Members name the instance that declared them.
///
/// Empty is allowed so bytecode written before instances were carried through
/// still loads; naming one that was never declared is a real inconsistency.
fn check_instance(state: &VmState, kind: &str, name: &str, instance: &str) -> Result<()> {
    if !instance.is_empty() && !state.instances.contains_key(instance) {
        bail!("{kind} '{name}' names undeclared instance '{instance}'");
    }
    Ok(())
}

fn require_identifier(kind: &str, value: &str) -> Result<()> {
    if valid_qualified_name(value) {
        Ok(())
    } else {
        bail!("invalid {kind} name '{value}'")
    }
}

/// Whether this agent needs a launched coding-agent process.
fn needs_agent_process(backend: &str) -> bool {
    !is_web_backend(backend)
        && !crate::backend::by_name(backend).is_some_and(|backend| backend.topology_only())
}

/// Web agents are answered by a client through the invocation registry.
fn is_web_backend(backend: &str) -> bool {
    canonical_backend(backend) == "web"
}

fn canonical_backend(backend: &str) -> &str {
    if backend.eq_ignore_ascii_case("web") {
        return "web";
    }
    crate::backend::by_name(backend)
        .map(|backend| backend.kind().name())
        .unwrap_or(backend)
}

#[cfg(test)]
mod tests {
    #[test]
    fn sandbox_replies_cannot_write_unrelated_ports_or_wrong_types() {
        let state = hr_state();
        let reaction = state.reactions.get_key_value("reaction.1").unwrap();
        let invocation = invocation_spec(
            &state,
            reaction,
            &BTreeMap::from([("triage".into(), json!("ready"))]),
            &BTreeMap::new(),
            Tag::default(),
        )
        .unwrap();
        assert!(validate_sandbox_writes(
            &state,
            &invocation,
            &BTreeMap::from([("opinion1".into(), json!("ok"))])
        )
        .is_ok());
        assert!(validate_sandbox_writes(
            &state,
            &invocation,
            &BTreeMap::from([("opinion2".into(), json!("other team"))])
        )
        .is_err());
        assert!(validate_sandbox_writes(
            &state,
            &invocation,
            &BTreeMap::from([("opinion1".into(), json!(123))])
        )
        .is_err());
        assert!(validate_sandbox_writes(&state, &invocation, &BTreeMap::new()).is_err());
    }

    #[test]
    fn sandbox_initialization_does_not_disclose_other_teams_values() {
        let mut state = hr_state();
        state.state_vars.insert(
            "child.secret".into(),
            StateVarState {
                ty: "string".into(),
                initial: json!("private child state"),
                instance: "child".into(),
                agents: Vec::new(),
                directive: None,
            },
        );
        state.params.insert(
            "child.key".into(),
            ParamState {
                ty: "string".into(),
                value: json!("private child parameter"),
                instance: "child".into(),
            },
        );
        state.params.insert(
            "key".into(),
            ParamState {
                ty: "string".into(),
                value: json!("own parameter"),
                instance: "".into(),
            },
        );
        state.ports.get_mut("opinion1").unwrap().instance = "child".into();
        let parent = sandbox_state(&state, "");
        assert!(parent.ports.contains_key("opinion1"));
        let serialized = serde_json::to_string(&parent).unwrap();
        assert!(!serialized.contains("private child"));
        assert!(serialized.contains("own parameter"));
        assert!(parent.connections.is_empty());
        assert!(!parent.reactions.is_empty());
        let child = sandbox_state(&state, "child");
        assert_eq!(child.state_vars.len(), 1);
        assert_eq!(child.params.len(), 1);
        assert!(child.agents.is_empty());
        assert!(child.reactions.is_empty());
    }

    #[test]
    fn cleanup_refuses_unknown_recorded_tmux_identity() {
        let mut record = crate::deploy::DeploymentRecord::create("test", Default::default(), 10);
        record.tmux_server = None;
        let record = std::sync::Arc::new(std::sync::Mutex::new(record));
        let sessions = std::collections::BTreeMap::from([("worker".into(), "session".into())]);
        let directory = tempfile::tempdir().unwrap();
        let failures = super::cleanup_recorded_sessions(&record, &sessions, directory.path());
        assert_eq!(failures.len(), 1);
        assert!(!record.lock().unwrap().sessions_cleaned);
    }

    use super::*;
    use std::sync::Mutex;

    /// The outputs however the loop ended; these tests never request a stop.
    fn loop_outputs(end: LoopEnd) -> BTreeMap<String, Value> {
        match end {
            LoopEnd::Completed(settled) | LoopEnd::Stopped(settled) => settled.outputs,
            LoopEnd::Paused(state) => state.outputs,
        }
    }

    fn program() -> Bytecode {
        serde_json::from_str(
            r#"{
              "version": 1,
              "team": "Demo",
              "instructions": [
                {"op":"begin_plan","team":"Demo"},
                {"op":"spawn_agent","name":"worker","backend":"Codex"},
                {"op":"define_port","kind":"input","name":"request","type":"string"},
                {"op":"define_port","kind":"output","name":"done","type":"bool"},
                {"op":"install_reaction","id":"reaction.0","agent":"worker","triggers":["request"],"effects":["done"],"contract":"done","prompt":"Work"},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn generated_artifacts_belong_to_the_project_that_holds_src() {
        // Lingua Franca's layout: `src/` is the project's, so `src-gen` is a
        // sibling of it rather than a child.
        let dir = generated_dir(Path::new("/work/topology/src/RingCode.omar"));

        assert_eq!(dir, Path::new("/work/topology/src-gen/RingCode"));
    }

    #[test]
    fn a_program_outside_a_src_folder_generates_where_it_lives() {
        let dir = generated_dir(Path::new("/tmp/loose/RingCode.omar"));

        assert_eq!(dir, Path::new("/tmp/loose/src-gen/RingCode"));
    }

    #[test]
    fn each_program_in_a_src_folder_gets_its_own_generated_directory() {
        // One `src/` holds many programs, and a single crate directory would
        // have them overwrite each other's Cargo.toml.
        let one = generated_dir(Path::new("/work/topology/src/RingCode.omar"));
        let two = generated_dir(Path::new("/work/topology/src/RingLeader.omar"));

        assert_ne!(one, two);
        assert_eq!(one.parent(), two.parent());
    }

    #[test]
    fn rejects_compiled_json_as_a_user_program() {
        let directory = tempfile::tempdir().unwrap();
        let bytecode_path = directory.path().join("workflow.json");
        fs::write(&bytecode_path, serde_json::to_vec(&program()).unwrap()).unwrap();

        let error =
            load_program_with(&bytecode_path, Some(Path::new("missing-omarc"))).unwrap_err();

        assert!(error.to_string().contains("must use the .omar extension"));
    }

    #[cfg(unix)]
    #[test]
    fn compiles_omar_source_before_loading_it() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("workflow.omar");
        fs::write(&source_path, serde_json::to_vec(&program()).unwrap()).unwrap();
        let compiler_path = directory.path().join("omarc");
        // omarc [options] <input> <output>: the files are the last two arguments.
        fs::write(
            &compiler_path,
            "#!/bin/sh\nshift $(($# - 2))\ncp \"$1\" \"$2\"\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&compiler_path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&compiler_path, permissions).unwrap();

        let loaded = load_program_with(&source_path, Some(&compiler_path)).unwrap();

        assert_eq!(loaded.team, "Demo");
    }

    #[test]
    fn external_schema_compiles_and_enforces_the_imported_enum() {
        let compiler = Path::new(env!("CARGO_MANIFEST_DIR")).join("lang/.lake/build/bin/omarc");
        if !compiler.exists() {
            eprintln!("skipping: {} has not been built", compiler.display());
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let schema = directory.path().join("decision.json");
        let source = directory.path().join("review.omar");
        fs::write(
            &schema,
            r#"{"type":"string","enum":["approved","needs_revision"]}"#,
        )
        .unwrap();
        fs::write(
            &source,
            r#"
            type Decision from "decision.json"
            team Review[reviewer : Codex] {
                input request : string
                output decision : Decision
                task reviewer(request) -> decision "Review $(request)"
            }
            main { review = Review() }
        "#,
        )
        .unwrap();
        let bytecode = load_program_with(&source, Some(&compiler)).unwrap();
        let plan = verify(&bytecode).unwrap();
        let ty = &plan.ports["review.decision"].ty;
        assert_eq!(ty, r#"string in ["approved","needs_revision"]"#);
        assert_eq!(
            plan.ports["review.decision"].declared.as_deref(),
            Some("Decision")
        );
        // The name survives to the plan, so the agent can be told it.
        assert_eq!(
            plan.types["Decision"],
            TypeState {
                ty: ty.clone(),
                title: None,
                description: None,
            }
        );
        validate_value(ty, &json!("approved")).unwrap();
        let error = validate_value(ty, &json!("approved with revisions"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("needs_revision"), "{error}");
        assert!(validate_value(ty, &json!({"decision":"approved"})).is_err());
        // The bytecode embeds constraints; deployed validation needs no file.
        fs::remove_file(&schema).unwrap();
        validate_value(ty, &json!("needs_revision")).unwrap();
        let error = load_program_with(&source, Some(&compiler))
            .unwrap_err()
            .to_string();
        assert!(error.contains("decision.json"), "{error}");
        fs::write(
            &schema,
            r#"{"type":"string","enum":["approved"],"minLength":20}"#,
        )
        .unwrap();
        let error = load_program_with(&source, Some(&compiler))
            .unwrap_err()
            .to_string();
        assert!(error.contains("unsupported JSON Schema keyword"), "{error}");
    }

    #[test]
    fn a_staged_program_imports_from_its_own_directory_only() {
        let compiler = Path::new(env!("CARGO_MANIFEST_DIR")).join("lang/.lake/build/bin/omarc");
        if !compiler.exists() {
            eprintln!("skipping: {} has not been built", compiler.display());
            return;
        }
        let outside = tempfile::tempdir().unwrap();
        let elsewhere = outside.path().join("decision.json");
        fs::write(
            &elsewhere,
            r#"{"type":"string","enum":["approved","needs_revision"]}"#,
        )
        .unwrap();
        let directory = outside.path().join("staged");
        fs::create_dir_all(directory.join("schemas")).unwrap();
        fs::copy(&elsewhere, directory.join("schemas/decision.json")).unwrap();
        let program = |import: &str| {
            format!(
                r#"
                type Decision from "{import}"
                team Review[reviewer : Codex] {{
                    input request : string
                    output decision : Decision
                    task reviewer(request) -> decision "Review $(request)"
                }}
                main {{ review = Review() }}
            "#
            )
        };
        let source = directory.join("review.omar");

        // A staged program reads only what came with it.
        for import in [
            elsewhere.display().to_string(),
            "../decision.json".to_string(),
            "schemas/../../decision.json".to_string(),
            "schemas\\decision.json".to_string(),
        ] {
            fs::write(&source, program(&import)).unwrap();
            let error = load_program_with(&source, Some(&compiler))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("inside the program's directory"),
                "{import} gave {error}"
            );
        }
        for import in ["schemas/decision.json", "./schemas/decision.json"] {
            fs::write(&source, program(import)).unwrap();
            load_program_with(&source, Some(&compiler)).unwrap();
        }
    }

    #[test]
    fn a_program_s_imports_are_read_from_beside_it_to_travel_with_it() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("schemas")).unwrap();
        let decision = r#"{"type":"string","enum":["continue","stop"]}"#;
        let priority = r#"{"type":"string","enum":["high","low"]}"#;
        fs::write(directory.path().join("schemas/decision.json"), decision).unwrap();
        fs::write(directory.path().join("priority.json"), priority).unwrap();
        let program = directory.path().join("review.omar");
        let source = r#"
            // type Commented from "schemas/nowhere.json"
            type Decision from "./schemas/decision.json" // the route
            type   Priority   from   "priority.json"
            type Elsewhere from "/etc/hostname"
            type Above from "../decision.json"
            team Review[reviewer : Codex] {
                input request : string
                output decision : Decision
                task reviewer(request) -> decision "Review $(request)"
            }
            main { review = Review() }
        "#;

        assert_eq!(
            imports_of(source),
            vec![
                (
                    "Decision".to_string(),
                    "./schemas/decision.json".to_string()
                ),
                ("Priority".to_string(), "priority.json".to_string()),
                ("Elsewhere".to_string(), "/etc/hostname".to_string()),
                ("Above".to_string(), "../decision.json".to_string()),
            ]
        );
        // Imports outside the directory are left for the compiler to refuse.
        let files = imported_files(&program, source).unwrap();
        assert_eq!(
            files,
            BTreeMap::from([
                ("./schemas/decision.json".to_string(), decision.to_string()),
                ("priority.json".to_string(), priority.to_string()),
            ])
        );

        let error = imported_files(&program, r#"type Missing from "schemas/missing.json""#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("'Missing'"), "{error}");
        assert!(error.contains("schemas/missing.json"), "{error}");
        assert!(error.contains("review.omar"), "{error}");

        for local in ["decision.json", "./a/b.json", "a/./b.json"] {
            assert!(is_local_import(local), "{local}");
        }
        for foreign in [
            "",
            "/etc/hostname",
            "../x.json",
            "a/../../x.json",
            "a\\b.json",
        ] {
            assert!(!is_local_import(foreign), "{foreign}");
        }
    }

    #[test]
    fn verifies_initial_topology() {
        let state = verify(&program()).unwrap();
        assert_eq!(state.agents.len(), 1);
        assert_eq!(state.ports.len(), 2);
        assert_eq!(state.reactions.len(), 1);
    }

    /// A plan with one named enum type and a body reaction that reads or
    /// writes a port of it.
    fn body_with_enum(kind: &str, triggers: &str, effects: &str) -> Bytecode {
        serde_json::from_str(&format!(
            r#"{{
              "version": 1,
              "team": "Desk",
              "instructions": [
                {{"op":"begin_plan","team":"Desk"}},
                {{"op":"define_type","name":"Decision",
                  "type":"string in [\"continue\",\"stop\"]",
                  "description":"Whether to go on"}},
                {{"op":"define_port","kind":"input","name":"token","type":"string"}},
                {{"op":"define_port","kind":"{kind}","name":"decision",
                  "type":"string in [\"continue\",\"stop\"]","declared":"Decision"}},
                {{"op":"define_port","kind":"output","name":"memo","type":"string"}},
                {{"op":"install_reaction","id":"reaction.0","agent":"",
                 "triggers":{triggers},"effects":{effects},
                 "contract":"","prompt":"","body":"memo = Some(String::new());"}},
                {{"op":"commit_plan"}}
              ]
            }}"#
        ))
        .unwrap()
    }

    #[test]
    fn a_body_may_read_an_enum_port_but_not_write_one() {
        let plan = verify(&body_with_enum("input", r#"["decision"]"#, r#"["memo"]"#)).unwrap();
        assert_eq!(
            plan.types["Decision"].ty,
            r#"string in ["continue","stop"]"#
        );
        assert_eq!(
            plan.types["Decision"].description.as_deref(),
            Some("Whether to go on")
        );
        assert!(crate::reaction::supports_type(
            r#"string in ["continue","stop"]"#
        ));

        let error = verify(&body_with_enum("output", r#"["token"]"#, r#"["decision"]"#))
            .unwrap_err()
            .to_string();
        assert!(error.contains("writes 'decision'"), "{error}");
        assert!(error.contains("does not enforce"), "{error}");
    }

    #[test]
    fn a_port_takes_one_incoming_connection() {
        let mut program = body_with_enum("input", r#"["decision"]"#, r#"["memo"]"#);
        for (at, extra) in [
            r#"{"op":"define_port","kind":"input","name":"other","type":"string"}"#,
            r#"{"op":"connect_ports","source":"token","target":"memo"}"#,
            r#"{"op":"connect_ports","source":"other","target":"memo"}"#,
        ]
        .into_iter()
        .enumerate()
        {
            program
                .instructions
                .insert(6 + at, serde_json::from_str(extra).unwrap());
        }
        let error = verify(&program).unwrap_err().to_string();
        assert!(
            error.contains("port 'memo' already has an incoming connection from 'token'"),
            "{error}"
        );
    }

    #[test]
    fn rejects_a_duplicate_or_malformed_named_type() {
        for (extra, expected) in [
            (
                r#"{"op":"define_type","name":"Decision","type":"string"}"#,
                "duplicate type",
            ),
            (
                r#"{"op":"define_type","name":"Broken","type":"string in [oops"}"#,
                "invalid string refinement",
            ),
            (
                r#"{"op":"define_type","name":"Blank","type":" "}"#,
                "empty definition",
            ),
        ] {
            let mut program = body_with_enum("input", r#"["decision"]"#, r#"["memo"]"#);
            program
                .instructions
                .insert(2, serde_json::from_str(extra).unwrap());
            let error = verify(&program).unwrap_err().to_string();
            assert!(error.contains(expected), "{extra} gave {error}");
        }
    }

    #[test]
    fn a_declared_spelling_has_to_be_the_type() {
        for (declared, expected) in [
            ("Verdict", "declared as 'Verdict'"),
            ("list<Decision>", "declared as 'list<Decision>'"),
            ("string", "declared as 'string'"),
        ] {
            let mut program = body_with_enum("input", r#"["decision"]"#, r#"["memo"]"#);
            let Instruction::DefinePort {
                declared: spelled, ..
            } = &mut program.instructions[3]
            else {
                panic!("fixture moved");
            };
            *spelled = Some(declared.to_string());
            let error = verify(&program).unwrap_err().to_string();
            assert!(error.contains(expected), "{declared} gave {error}");
        }
    }

    #[test]
    fn two_imports_of_the_same_values_keep_their_own_names() {
        // `A` and `B` expand to one refinement. The agent is told the one the
        // program wrote on the port, not whichever sorts last.
        let bytecode: Bytecode = serde_json::from_str(
            r#"{
              "version": 1,
              "team": "Desk",
              "instructions": [
                {"op":"begin_plan","team":"Desk"},
                {"op":"define_type","name":"A","type":"string in [\"x\",\"y\"]","description":"the first"},
                {"op":"define_type","name":"B","type":"string in [\"x\",\"y\"]","description":"the second"},
                {"op":"spawn_agent","name":"clerk","backend":"Codex"},
                {"op":"define_port","kind":"input","name":"topic","type":"string"},
                {"op":"define_port","kind":"output","name":"out",
                 "type":"string in [\"x\",\"y\"]","declared":"B"},
                {"op":"install_reaction","id":"reaction.0","agent":"clerk",
                 "triggers":["topic"],"effects":["out"],"contract":"out","prompt":"p"},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap();
        let plan = verify(&bytecode).unwrap();
        let reaction = plan.reactions.get_key_value("reaction.0").unwrap();
        let spec = invocation_spec(
            &plan,
            reaction,
            &BTreeMap::from([("topic".to_string(), json!("t"))]),
            &BTreeMap::new(),
            Tag::default(),
        )
        .unwrap();
        assert_eq!(spec.effect_names["out"], "B");
        assert_eq!(spec.types.keys().collect::<Vec<_>>(), vec!["B"]);
        let message = invocation_message(&spec, "p").unwrap();
        assert!(message.contains(r#"effects: {"out":"B"}"#), "{message}");
        assert!(
            message.contains("Type B is one of \"x\", \"y\". the second."),
            "{message}"
        );
        assert!(!message.contains("the first"), "{message}");
    }

    #[test]
    fn an_agent_is_told_the_type_name_and_what_the_schema_said() {
        let decision = TypeState {
            ty: r#"string in ["continue","stop"]"#.into(),
            title: Some("Review decision".into()),
            description: Some("Whether the request may go on".into()),
        };
        let spec = InvocationSpec {
            id: 1,
            reaction_id: "reaction.0".into(),
            agent: "clerk".into(),
            trigger_values: BTreeMap::from([("topic".to_string(), json!("hi"))]),
            allowed_effects: BTreeMap::from([
                ("out".to_string(), decision.ty.clone()),
                ("all".to_string(), format!("list<{}>", decision.ty)),
                ("memo".to_string(), "string".to_string()),
            ]),
            effect_names: BTreeMap::from([
                ("out".to_string(), "Decision".to_string()),
                ("all".to_string(), "list<Decision>".to_string()),
                ("memo".to_string(), "string".to_string()),
            ]),
            types: BTreeMap::from([("Decision".to_string(), decision.clone())]),
            state_values: BTreeMap::new(),
            state_types: BTreeMap::new(),
            contexts: BTreeMap::new(),
            context_names: BTreeSet::new(),
            contract: "out".into(),
            prompt: "Decide.".into(),
            within: None,
            tag: Tag::default(),
        };
        let message = invocation_message(&spec, "Decide.").unwrap();
        let header: Vec<&str> = message
            .lines()
            .take_while(|line| !line.is_empty())
            .collect();
        assert_eq!(header[0], "OMAR INVOCATION");
        assert!(header.contains(&"task_id: 1"), "{message}");
        assert!(
            header
                .contains(&r#"effects: {"all":"list<Decision>","memo":"string","out":"Decision"}"#),
            "{message}"
        );
        assert!(
            header.contains(&r#"types: {"Decision":{"type":"string in [\"continue\",\"stop\"]","title":"Review decision","description":"Whether the request may go on"}}"#),
            "{message}"
        );
        assert!(header.contains(&"contract: out"), "{message}");
        // The prose the agent reads comes after the header, before the prompt.
        let body = &message[message.find("\n\n").unwrap() + 2..];
        assert!(
            body.starts_with(
                "Type Decision is one of \"continue\", \"stop\". Title: Review decision. Whether the request may go on.\n\nDecide."
            ),
            "{body}"
        );

        // Only the types the effects use are mentioned, and a program with
        // none reads as it always did.
        let plain = InvocationSpec {
            types: BTreeMap::new(),
            allowed_effects: BTreeMap::from([("memo".to_string(), "string".to_string())]),
            effect_names: BTreeMap::from([("memo".to_string(), "string".to_string())]),
            ..spec
        };
        let message = invocation_message(&plain, "Decide.").unwrap();
        assert!(!message.contains("types:"), "{message}");
        assert!(
            message.contains(
                "effects: {\"memo\":\"string\"}\ncontract: out\n\nDecide.\n\nUse omar_set_port"
            ),
            "{message}"
        );
    }

    #[test]
    fn only_types_the_effects_use_are_offered() {
        let plan = verify(&body_with_enum("output", r#"["token"]"#, r#"["memo"]"#)).unwrap();
        let reaction = plan.reactions.get_key_value("reaction.0").unwrap();
        let spec = invocation_spec(
            &plan,
            reaction,
            &BTreeMap::from([("token".to_string(), json!("t"))]),
            &BTreeMap::new(),
            Tag::default(),
        )
        .unwrap();
        assert!(spec.types.is_empty(), "{:?}", spec.types);
    }

    #[test]
    fn rejects_a_port_whose_refinement_does_not_parse() {
        // `omarc` cannot emit either of these. Hand-written bytecode can, and
        // the port is what is wrong -- not the first value written to it.
        for (ty, expected) in [
            (r#"string in [oops"#, "invalid string refinement"),
            (r#"string in [1,2]"#, "invalid string refinement"),
            (r#"string in []"#, "admits no value"),
            // `validate_value` looks inside these, so `verify` must too.
            (r#"list<string in [oops>"#, "invalid string refinement"),
            (r#"list<string in [oops"#, "invalid type"),
            (r#"option<string in ["a"]"#, "invalid type"),
            (r#"list<>"#, "invalid type"),
            (r#"option<>"#, "invalid type"),
            // One `>` too many leaves `string>`, which is not a type.
            (r#"list<string>>"#, "invalid type"),
            (r#"option<int>>"#, "invalid type"),
            (r#"list<option<int>>>"#, "invalid type"),
            (r#"list<strin>"#, "invalid type"),
            (r#"text"#, "invalid type"),
            (r#"string in ["a","a"]"#, "duplicate enum value"),
            (r#"list<string in ["a","b","a"]>"#, "duplicate enum value"),
            (r#"list<option<string in [oops>"#, "invalid type"),
            (r#"option<string in []>"#, "admits no value"),
            (
                r#"list<option<string in [1,2]>>"#,
                "invalid string refinement",
            ),
        ] {
            let mut program = program();
            program.instructions[2] = serde_json::from_str(&format!(
                r#"{{"op":"define_port","kind":"input","name":"request","type":{}}}"#,
                serde_json::to_string(ty).unwrap()
            ))
            .unwrap();
            let error = verify(&program).unwrap_err().to_string();
            assert!(error.contains(expected), "{ty} gave {error}");
        }
    }

    #[test]
    fn rejects_incomplete_topology() {
        let mut program = program();
        program.instructions.pop();
        assert!(verify(&program).is_err());
    }

    #[test]
    fn a_refined_string_admits_only_what_it_lists() {
        let ty = "string in [\"continue\",\"stop\"]";
        validate_value(ty, &json!("continue")).unwrap();
        validate_value(ty, &json!("stop")).unwrap();

        // The failure the refinement exists for: a value that is a perfectly
        // good string and not one of the answers the port accepts.
        let rejected = validate_value(ty, &json!("this is a terminal record, not a forward"))
            .unwrap_err()
            .to_string();
        assert!(rejected.contains("\"continue\""), "{rejected}");
        assert!(rejected.contains("\"stop\""), "{rejected}");

        assert!(validate_value(ty, &json!(1)).is_err());
        validate_value("list<string in [\"a\",\"b\"]>", &json!(["a", "b"])).unwrap();
        assert!(validate_value("list<string in [\"a\",\"b\"]>", &json!(["c"])).is_err());
    }

    #[test]
    fn enum_cli_inputs_are_bare_text_and_still_validate_membership() {
        let ty = r#"string in ["continue","stop"]"#;
        let value = parse_input_value(ty, "continue").unwrap();
        assert_eq!(value, json!("continue"));
        validate_value(ty, &value).unwrap();
        for raw in ["unknown", r#""continue""#] {
            let value = parse_input_value(ty, raw).unwrap();
            assert_eq!(value, json!(raw));
            assert!(validate_value(ty, &value).is_err());
        }
        let nested = "list<string in [\"continue\",\"stop\"]>";
        let value = parse_input_value(nested, r#"["continue","stop"]"#).unwrap();
        validate_value(nested, &value).unwrap();
        assert!(parse_input_value(nested, "continue").is_err());
    }

    #[test]
    fn a_refinement_reports_awkward_values_readably() {
        // An admitted value may contain a quote or a newline, and the agent
        // reads the rejection, so the options are quoted by serde.
        let ty = r#"string in ["say \"hi\"","two\nlines"]"#;
        validate_value(ty, &json!("say \"hi\"")).unwrap();
        let rejected = validate_value(ty, &json!("no")).unwrap_err().to_string();
        assert!(rejected.contains(r#""say \"hi\"""#), "{rejected}");
        assert!(rejected.contains(r#""two\nlines""#), "{rejected}");

        // `omarc` rejects an empty refinement; hand-written bytecode can still
        // carry one, and it is the port that is wrong, not the value.
        let empty = validate_value("string in []", &json!("anything"))
            .unwrap_err()
            .to_string();
        assert!(empty.contains("admits no value"), "{empty}");
    }

    #[test]
    fn validates_nested_list_and_option_values_recursively() {
        validate_value("list<option<int>>", &json!([1, null, 2])).unwrap();
        validate_value("option<list<bool>>", &Value::Null).unwrap();
        validate_value("option<list<bool>>", &json!([true, false])).unwrap();

        assert!(validate_value("list<option<int>>", &json!([1, "two"])).is_err());
        assert!(validate_value("option<int>", &json!("one")).is_err());
        assert!(validate_value("list<int>", &json!("not a list")).is_err());
    }

    #[test]
    fn effect_contract_rejects_writes_to_unmentioned_ports() {
        let writes = BTreeMap::from([
            ("declared".to_string(), json!(true)),
            ("omitted".to_string(), json!(true)),
        ]);
        let error = validate_contract("declared", &writes).unwrap_err();
        assert!(error
            .to_string()
            .contains("effect 'omitted' is not permitted"));
    }

    #[test]
    fn atomic_json_write_replaces_complete_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_json_atomic(&path, &json!({"revision": 1})).unwrap();
        write_json_atomic(&path, &json!({"revision": 2, "ready": true})).unwrap();

        let stored: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(stored, json!({"revision": 2, "ready": true}));
    }

    struct HrExecutor {
        calls: Mutex<Vec<String>>,
    }

    impl ReactionExecutor for HrExecutor {
        fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
            self.calls
                .lock()
                .unwrap()
                .push(invocation.reaction_id.clone());
            let writes = match invocation.reaction_id.as_str() {
                "reaction.0" => {
                    BTreeMap::from([("triage".to_string(), json!("strong systems candidate"))])
                }
                "reaction.1" => {
                    assert_eq!(
                        invocation.trigger_values.get("triage"),
                        Some(&json!("strong systems candidate"))
                    );
                    BTreeMap::from([("opinion1".to_string(), json!("strong engineer"))])
                }
                "reaction.2" => {
                    assert_eq!(
                        invocation.trigger_values.get("triage"),
                        Some(&json!("strong systems candidate"))
                    );
                    BTreeMap::from([("opinion2".to_string(), json!("good judgment"))])
                }
                "reaction.3" => {
                    assert_eq!(
                        invocation.trigger_values.get("opinion1"),
                        Some(&json!("strong engineer"))
                    );
                    assert_eq!(
                        invocation.trigger_values.get("opinion2"),
                        Some(&json!("good judgment"))
                    );
                    BTreeMap::from([("hired".to_string(), json!(true))])
                }
                other => panic!("unexpected reaction {other}"),
            };
            validate_contract(&invocation.contract, &writes)?;
            Ok(writes)
        }
    }

    fn hr_state() -> VmState {
        let mut state = VmState {
            version: 1,
            team: "HR".into(),
            state_vars: BTreeMap::new(),
            params: BTreeMap::new(),
            types: BTreeMap::new(),
            instances: BTreeMap::new(),
            timers: BTreeMap::new(),
            agents: BTreeMap::from([
                (
                    "manager".into(),
                    AgentState {
                        backend: "claude".into(),
                        instance: String::new(),
                        lifetime: None,
                    },
                ),
                (
                    "reviewer1".into(),
                    AgentState {
                        backend: "codex".into(),
                        instance: String::new(),
                        lifetime: None,
                    },
                ),
                (
                    "reviewer2".into(),
                    AgentState {
                        backend: "opencode".into(),
                        instance: String::new(),
                        lifetime: None,
                    },
                ),
            ]),
            ports: BTreeMap::new(),
            connections: Vec::new(),
            reactions: BTreeMap::new(),
        };
        for (name, kind, ty) in [
            ("resume", PortKind::Input, "path"),
            ("hired", PortKind::Output, "bool"),
            ("triage", PortKind::Action, "string"),
            ("opinion1", PortKind::Action, "string"),
            ("opinion2", PortKind::Action, "string"),
            ("log", PortKind::Action, "string"),
        ] {
            state.ports.insert(
                name.into(),
                PortState {
                    kind,
                    ty: ty.into(),
                    declared: None,
                    delay: None,
                    instance: String::new(),
                },
            );
        }
        for (order, id, agent, triggers, effects, contract) in [
            (
                0,
                "reaction.0",
                "manager",
                vec!["resume"],
                vec!["triage", "hired", "log"],
                "( triage | hired = false ) , log ?",
            ),
            (
                1,
                "reaction.1",
                "reviewer1",
                vec!["triage"],
                vec!["opinion1"],
                "opinion1",
            ),
            (
                2,
                "reaction.2",
                "reviewer2",
                vec!["triage"],
                vec!["opinion2"],
                "opinion2",
            ),
            (
                3,
                "reaction.3",
                "manager",
                vec!["opinion1", "opinion2"],
                vec!["hired"],
                "hired",
            ),
        ] {
            state.reactions.insert(
                id.into(),
                ReactionState {
                    order,
                    instance: String::new(),
                    agent: agent.into(),
                    triggers: triggers.into_iter().map(str::to_string).collect(),
                    effects: effects.into_iter().map(str::to_string).collect(),
                    contract: contract.into(),
                    prompt: "prompt".into(),
                    body: None,
                    within: None,
                },
            );
        }
        state
    }

    #[test]
    fn runs_hr_dataflow_to_completion() {
        let executor = HrExecutor {
            calls: Mutex::new(Vec::new()),
        };
        let outputs = run_event_loop(
            &hr_state(),
            BTreeMap::from([("resume".into(), json!("/tmp/resume.txt"))]),
            &executor,
        )
        .unwrap();
        assert_eq!(outputs, BTreeMap::from([("hired".into(), json!(true))]));

        let calls = executor.calls.lock().unwrap();
        assert_eq!(calls.first().map(String::as_str), Some("reaction.0"));
        assert_eq!(calls.last().map(String::as_str), Some("reaction.3"));
        assert!(calls[1..3].contains(&"reaction.1".to_string()));
        assert!(calls[1..3].contains(&"reaction.2".to_string()));
    }

    #[test]
    fn superdense_tags_advance_microstep_or_timestamp() {
        let tag = Tag {
            timestamp: 10,
            microstep: 7,
        };
        assert_eq!(
            tag.advance(Delay::after(0)).unwrap(),
            Tag {
                timestamp: 10,
                microstep: 8
            }
        );
        // A plain connection costs nothing, so it stays on the tag it started
        // on -- which is what lets a value be read by the rest of that tag.
        assert_eq!(tag.advance(Delay::INSTANT).unwrap(), tag);
        assert_eq!(
            tag.advance(Delay::after(2)).unwrap(),
            Tag {
                timestamp: 12,
                microstep: 0
            }
        );
    }

    /// A ring node: forward the token until it reaches the limit, then stop.
    /// Effects are qualified by instance, so this also proves the runtime
    /// carries qualified names through invocation and contract checking.
    struct RingExecutor {
        limit: i64,
        hops: Mutex<Vec<String>>,
    }

    impl ReactionExecutor for RingExecutor {
        fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
            self.hops
                .lock()
                .unwrap()
                .push(invocation.reaction_id.clone());
            let node = invocation
                .reaction_id
                .split('.')
                .next()
                .expect("reaction ids are qualified");
            let token = invocation
                .trigger_values
                .get(&format!("{node}.token"))
                .and_then(Value::as_i64)
                .expect("the token arrives under its qualified name");
            let writes = if token < self.limit {
                BTreeMap::from([(format!("{node}.out"), json!(token + 1))])
            } else {
                BTreeMap::from([(format!("{node}.done"), json!(token))])
            };
            validate_contract(&invocation.contract, &writes)?;
            Ok(writes)
        }
    }

    fn ring_bytecode() -> Bytecode {
        // As `omarc` elaborates tests/topology/src/Ring.omar: one team, three
        // instances, wired head to tail.
        serde_json::from_str(
            r#"{
              "version": 1,
              "team": "Ring",
              "instructions": [
                {"op":"begin_plan","team":"Ring"},
                {"op":"spawn_agent","name":"n1.agent","backend":"Codex"},
                {"op":"spawn_agent","name":"n2.agent","backend":"Codex"},
                {"op":"spawn_agent","name":"n3.agent","backend":"Codex"},
                {"op":"define_port","kind":"input","name":"n1.token","type":"int"},
                {"op":"define_port","kind":"output","name":"n1.out","type":"int"},
                {"op":"define_port","kind":"output","name":"n1.done","type":"int"},
                {"op":"define_port","kind":"input","name":"n2.token","type":"int"},
                {"op":"define_port","kind":"output","name":"n2.out","type":"int"},
                {"op":"define_port","kind":"output","name":"n2.done","type":"int"},
                {"op":"define_port","kind":"input","name":"n3.token","type":"int"},
                {"op":"define_port","kind":"output","name":"n3.out","type":"int"},
                {"op":"define_port","kind":"output","name":"n3.done","type":"int"},
                {"op":"connect_ports","source":"n1.out","target":"n2.token","delay":0},
                {"op":"connect_ports","source":"n2.out","target":"n3.token","delay":0},
                {"op":"connect_ports","source":"n3.out","target":"n1.token","delay":0},
                {"op":"install_reaction","id":"n1.reaction.0","agent":"n1.agent","triggers":["n1.token"],"effects":["n1.out","n1.done"],"contract":"( n1.out | n1.done )","prompt":"ring"},
                {"op":"install_reaction","id":"n2.reaction.0","agent":"n2.agent","triggers":["n2.token"],"effects":["n2.out","n2.done"],"contract":"( n2.out | n2.done )","prompt":"ring"},
                {"op":"install_reaction","id":"n3.reaction.0","agent":"n3.agent","triggers":["n3.token"],"effects":["n3.out","n3.done"],"contract":"( n3.out | n3.done )","prompt":"ring"},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn a_ring_of_instances_passes_a_token_until_a_node_stops_forwarding() {
        let state = verify(&ring_bytecode()).unwrap();
        let executor = RingExecutor {
            limit: 6,
            hops: Mutex::new(Vec::new()),
        };

        let outputs = run_event_loop(
            &state,
            BTreeMap::from([("n1.token".into(), json!(0))]),
            &executor,
        )
        .unwrap();

        // Three laps: each hop is a connection with no delay, which lands on
        // the next microstep, so the loop makes progress instead of deadlocking.
        let hops = executor.hops.lock().unwrap().clone();
        assert_eq!(
            hops,
            [
                "n1.reaction.0",
                "n2.reaction.0",
                "n3.reaction.0",
                "n1.reaction.0",
                "n2.reaction.0",
                "n3.reaction.0",
                "n1.reaction.0",
            ]
        );
        assert_eq!(outputs.get("n1.done"), Some(&json!(6)));
        assert_eq!(outputs.get("n2.done"), None);
    }

    #[test]
    fn a_connection_fed_input_needs_no_value_from_the_operator() {
        // n2 and n3 are fed by the ring; only the seed is the operator's to
        // give. Requiring all three would make instance wiring pointless.
        let state = verify(&ring_bytecode()).unwrap();

        let inputs = parse_inputs(&state, &["n1.token=0".to_string()]).unwrap();
        assert_eq!(inputs.get("n1.token"), Some(&json!(0)));
        assert_eq!(inputs.len(), 1);
    }

    #[test]
    fn agents_that_would_share_a_tmux_session_are_rejected() {
        // tmux flattens '.' to '_', so these two would answer for each other.
        let mut bytecode = ring_bytecode();
        bytecode.instructions.insert(
            2,
            Instruction::SpawnAgent {
                name: "n1_agent".to_string(),
                backend: "Codex".to_string(),
                instance: String::new(),
                lifetime: None,
            },
        );

        let error = verify(&bytecode).unwrap_err().to_string();

        assert!(error.contains("same tmux session"), "{error}");
    }

    #[test]
    fn a_nested_instance_names_the_instance_that_declared_it() {
        // A team can instantiate another team, so instances form a tree rather
        // than a list. The VM keeps one flat namespace, so the tree only
        // survives if each instance says who declared it.
        let bytecode: Bytecode = serde_json::from_str(
            r#"{
              "version": 1,
              "team": "Nest",
              "instructions": [
                {"op":"begin_plan","team":"Nest"},
                {"op":"declare_instance","name":"b","team":"B","parent":""},
                {"op":"declare_instance","name":"b.a","team":"A","parent":"b"},
                {"op":"spawn_agent","name":"b.a.agent","backend":"Codex","instance":"b.a"},
                {"op":"define_port","kind":"input","name":"b.a.inp","type":"int","instance":"b.a"},
                {"op":"define_port","kind":"output","name":"b.done","type":"int","instance":"b"},
                {"op":"install_reaction","id":"b.a.reaction.0","agent":"b.a.agent","triggers":["b.a.inp"],"effects":["b.done"],"contract":"b.done","prompt":"x","instance":"b.a"},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap();

        let state = verify(&bytecode).unwrap();

        assert_eq!(state.instances["b"].parent, "");
        assert_eq!(state.instances["b.a"].parent, "b");
        assert_eq!(state.instances["b.a"].team, "A");

        // A parent has to exist. Since one is always declared before its
        // children, this also rules out a cycle: neither end could come first.
        let orphan: Bytecode = serde_json::from_str(
            r#"{
              "version": 1,
              "team": "Orphan",
              "instructions": [
                {"op":"begin_plan","team":"Orphan"},
                {"op":"declare_instance","name":"b.a","team":"A","parent":"b"},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap();
        let error = verify(&orphan).unwrap_err().to_string();
        assert!(error.contains("undeclared parent 'b'"), "{error}");
    }

    /// One reaction under a contract of the caller's choosing, with or without
    /// a deadline the program set for itself.
    fn deadline_spec(contract: &str, within: Option<u64>) -> InvocationSpec {
        let within = match within {
            Some(nanos) => format!(r#","within":{nanos}"#),
            None => String::new(),
        };
        let bytecode: Bytecode = serde_json::from_str(&format!(
            r#"{{
              "version": 1,
              "team": "Desk",
              "instructions": [
                {{"op":"begin_plan","team":"Desk"}},
                {{"op":"spawn_agent","name":"clerk","backend":"Codex"}},
                {{"op":"define_port","kind":"input","name":"topic","type":"string"}},
                {{"op":"define_port","kind":"output","name":"memo","type":"string"}},
                {{"op":"install_reaction","id":"reaction.0","agent":"clerk",
                 "triggers":["topic"],"effects":["memo"],
                 "contract":"{contract}","prompt":"p"{within}}},
                {{"op":"commit_plan"}}
              ]
            }}"#
        ))
        .unwrap();
        let state = verify(&bytecode).unwrap();
        let (id, reaction) = state.reactions.get_key_value("reaction.0").unwrap();
        invocation_spec(
            &state,
            (id, reaction),
            &BTreeMap::from([("topic".to_string(), json!("x"))]),
            &BTreeMap::new(),
            Tag::default(),
        )
        .unwrap()
    }

    #[test]
    fn a_program_sets_its_own_deadline() {
        assert_eq!(
            deadline_spec("memo", Some(30_000_000_000)).within,
            Some(Duration::from_secs(30))
        );
        // Saying nothing leaves the run-wide timeout in charge rather than
        // inventing a deadline the program never asked for.
        assert_eq!(deadline_spec("memo", None).within, None);
    }

    #[test]
    fn claude_credit_checks_only_the_error_notice_and_its_wrapped_lines() {
        let screen = "❯ Explain insufficient credits errors.\n\
                      ⏺ The phrase insufficient credits concerns billing.\n\
                        ⎿  API Error: Service temporarily unavailable\n\
                      ❯ ";
        assert!(Window::claude_credit_error(screen).is_none());
        assert!(Window::claude_credit_error(
            "  ⎿  API Error: Service temporarily unavailable\n\
             ⏺ We should explain insufficient credits."
        )
        .is_none());
        for screen in [
            "  ⎿  API Error: Credit balance is too low",
            "  ⎿  API Error: 400 {\"error\":{\"message\":\"Credit balance is\n     too low\"}}\n❯ ",
            "API Error: insufficient\ncredits\n\n❯ ",
        ] {
            assert!(Window::claude_credit_error(screen).is_some(), "{screen}");
        }
    }

    #[test]
    fn usage_waits_pause_only_the_task_clock() {
        let mut clock = TaskClock::new();
        clock.started -= Duration::from_secs(20);
        clock.waiting = Some(clock.started + Duration::from_secs(3));
        assert_eq!(clock.elapsed(), Duration::from_secs(3));
        let wall_elapsed = clock.started.elapsed();
        clock.resume();
        assert!(clock.elapsed() < Duration::from_secs(4));
        assert!(wall_elapsed >= Duration::from_secs(20));
    }

    #[test]
    fn silence_completes_the_tag_when_the_contract_permits_it() {
        let spec = deadline_spec("memo ?", Some(1));
        assert_eq!(
            expired(&spec, Duration::from_secs(30)).unwrap(),
            BTreeMap::new()
        );
    }

    #[test]
    fn silence_stops_the_run_when_the_contract_requires_an_effect() {
        let spec = deadline_spec("memo", Some(1));
        let error = expired(&spec, Duration::from_secs(30))
            .unwrap_err()
            .to_string();
        // Names the contract, so an operator reads which promise went unkept
        // rather than only that something timed out, and names the invocation
        // so the line can be found in a log next to the rest of its run.
        assert!(error.contains("requires an effect"), "{error}");
        assert!(error.contains("memo"), "{error}");
        assert!(error.contains("reaction.0"), "{error}");
        assert!(error.contains(&spec.id.to_string()), "{error}");
    }

    /// Answers nothing, which is what an expired deadline does to a contract
    /// that permits silence.
    struct SilentExecutor;

    impl ReactionExecutor for SilentExecutor {
        fn invoke(&self, _invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
            Ok(BTreeMap::new())
        }
    }

    #[test]
    fn a_silent_reaction_leaves_what_it_feeds_unfired() {
        // Silence is absence, not a value: the port it would have written is
        // not present, so the reaction reading it never runs and the run ends
        // rather than stalling on a value that is never coming.
        let bytecode: Bytecode = serde_json::from_str(
            r#"{
              "version": 1,
              "team": "Desk",
              "instructions": [
                {"op":"begin_plan","team":"Desk"},
                {"op":"spawn_agent","name":"clerk","backend":"Codex"},
                {"op":"spawn_agent","name":"editor","backend":"Codex"},
                {"op":"define_port","kind":"input","name":"topic","type":"string"},
                {"op":"define_port","kind":"action","name":"memo","type":"string"},
                {"op":"define_port","kind":"output","name":"report","type":"string"},
                {"op":"install_reaction","id":"reaction.0","agent":"clerk",
                 "triggers":["topic"],"effects":["memo"],
                 "contract":"memo ?","prompt":"p","within":1},
                {"op":"install_reaction","id":"reaction.1","agent":"editor",
                 "triggers":["memo"],"effects":["report"],
                 "contract":"report","prompt":"p"},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap();
        let state = verify(&bytecode).unwrap();
        let outputs = run_event_loop(
            &state,
            BTreeMap::from([("topic".to_string(), json!("x"))]),
            &SilentExecutor,
        )
        .unwrap();
        assert!(outputs.is_empty(), "nothing downstream ran: {outputs:?}");
    }

    /// A two-input program with nothing feeding it: exactly the shape that has
    /// to sit still until the operator sends something.
    fn open_input_bytecode() -> Bytecode {
        serde_json::from_str(
            r#"{
              "version": 1,
              "team": "Desk",
              "instructions": [
                {"op":"begin_plan","team":"Desk"},
                {"op":"spawn_agent","name":"clerk","backend":"Codex"},
                {"op":"define_port","kind":"input","name":"topic","type":"string"},
                {"op":"define_port","kind":"input","name":"depth","type":"int"},
                {"op":"define_port","kind":"output","name":"memo","type":"string"},
                {"op":"install_reaction","id":"reaction.0","agent":"clerk",
                 "triggers":["topic","depth"],"effects":["memo"],
                 "contract":"memo","prompt":"p"},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap()
    }

    /// Records the tags a run actually passed through, so a projection can be
    /// held against the thing it claims to predict.
    struct RunTags {
        tags: Mutex<Vec<(u64, u64)>>,
    }

    impl TopologyObserver for RunTags {
        fn tag_advanced(
            &self,
            timestamp: u64,
            microstep: u64,
            _lag: u64,
            _ports: &BTreeMap<String, Value>,
        ) {
            self.tags.lock().unwrap().push((timestamp, microstep));
        }
    }

    fn tags_of_a_real_run<E: ReactionExecutor>(
        state: &VmState,
        inputs: BTreeMap<String, Value>,
        executor: &E,
    ) -> Vec<(u64, u64)> {
        let observer = RunTags {
            tags: Mutex::new(Vec::new()),
        };
        // Fast: this compares the tags a run passes through against the
        // projection of them, and waiting out real delays would say nothing
        // more about whether the two agree.
        run_event_loop_observed(state, inputs, executor, &observer, Pace::Fast, None).unwrap();
        let tags = observer.tags.lock().unwrap().clone();
        tags
    }

    fn timeline_tags(state: &VmState, present: &[&str]) -> Vec<(u64, u64)> {
        let present = present.iter().map(|name| name.to_string()).collect();
        let (steps, truncated) = timeline(state, &present, 256);
        assert!(!truncated, "the timeline ran out of room");
        steps
            .iter()
            .map(|step| (step.timestamp, step.microstep))
            .collect()
    }

    #[test]
    fn a_run_nothing_can_start_reaches_no_tag_at_all() {
        // The counterpart to the timeline showing nothing: a tag nothing is
        // present at is not a moment the run passed through, so the run does
        // not announce one. It used to announce (0, 0) — the start tag, seeded
        // whether or not anything was supplied — which told a client the run
        // had advanced when nothing had.
        let state = verify(&open_input_bytecode()).unwrap();
        let observer = RunTags {
            tags: Mutex::new(Vec::new()),
        };
        let outputs = loop_outputs(
            run_event_loop_observed(
                &state,
                BTreeMap::new(),
                &SilentExecutor,
                &observer,
                Pace::Fast,
                None,
            )
            .unwrap(),
        );
        assert!(observer.tags.lock().unwrap().is_empty(), "announced a tag");
        assert!(outputs.is_empty());
    }

    #[test]
    fn a_program_nothing_can_start_shows_nothing() {
        // No timer, and every input dangling: there is nothing to begin it, so
        // the timeline is empty rather than guessing at a first tag.
        let state = verify(&open_input_bytecode()).unwrap();
        let (steps, truncated) = timeline(&state, &BTreeSet::new(), 256);
        assert!(steps.is_empty(), "{steps:?}");
        assert!(!truncated);
    }

    #[test]
    fn the_timeline_hits_the_tags_the_run_does() {
        // The claim it makes: what a program does is decided by the program. If
        // these disagree, one of them is lying to the operator. A timer starts
        // both, which is the only thing that can start either.
        let state = verify(&timer_bytecode(3, 0)).unwrap();
        let executor = TimerExecutor {
            fired: Mutex::new(Vec::new()),
            stop_after: 0,
        };
        let real = tags_of_a_real_run(&state, BTreeMap::new(), &executor);
        assert!(!real.is_empty(), "the run reached no tags");
        assert_eq!(timeline_tags(&state, &[]), real);
    }

    #[test]
    fn a_periodic_projection_stops_rather_than_running_forever() {
        // A periodic timer has no end. A preview has to have one.
        let state = verify(&timer_bytecode(0, 10)).unwrap();
        let (steps, truncated) = timeline(&state, &BTreeSet::new(), 8);
        assert!(truncated);
        assert_eq!(steps.len(), 8);
        assert_eq!(steps[0].timestamp, 0);
    }

    #[test]
    fn direct_topology_backends_have_no_agent_process() {
        assert!(!needs_agent_process("Jev"));
        assert!(!needs_agent_process(" JeV "));
        assert!(!needs_agent_process("Web"));
        assert!(needs_agent_process("Codex"));
        assert!(needs_agent_process("ClaudeCode"));
        assert!(needs_agent_process("Stub"));
    }

    #[test]
    fn a_client_is_a_backend_however_it_is_spelled() {
        assert!(is_web_backend("Web"));
        assert!(is_web_backend("web"));
        assert!(!is_web_backend("ClaudeCode"));
        assert!(!is_web_backend("Stub"));
    }

    #[test]
    fn a_web_agent_is_answered_through_the_registry() {
        // Nothing is spawned for a web agent, so the registration is the whole
        // of the work item: a panel asks what it owes, answers it, and the tag
        // moves on exactly as it would for an agent in a pane.
        let server = InvocationServer::start().unwrap();
        let bytecode: Bytecode = serde_json::from_str(
            r#"{
              "version": 1,
              "team": "Desk",
              "instructions": [
                {"op":"begin_plan","team":"Desk"},
                {"op":"spawn_agent","name":"panel","backend":"Web"},
                {"op":"define_port","kind":"input","name":"topic","type":"string"},
                {"op":"define_port","kind":"output","name":"verdict","type":"string"},
                {"op":"install_reaction","id":"reaction.0","agent":"panel",
                 "triggers":["topic"],"effects":["verdict"],
                 "contract":"verdict","prompt":"Decide on $(topic)"},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap();
        let state = verify(&bytecode).unwrap();
        let executor = AgentReactionExecutor {
            client: TmuxClient::new("omar-test"),
            team: state.team.clone(),
            registry: server.registry.clone(),
            timeout: Duration::from_secs(10),
            run_started: Instant::now(),
            web: BTreeSet::from(["panel".to_string()]),
            scoped: None,
        };
        let context = TopologyMcpContext {
            resume: None,
            team: state.team.clone(),
            agent: "panel".into(),
            endpoint: server.endpoint.clone(),
            token: server.token.clone(),
            attempt: None,
        };

        let person = thread::spawn(move || {
            for _ in 0..200 {
                let waiting = pending_invocations(&context).unwrap();
                if let Some(item) = waiting["pending"].as_array().unwrap().first() {
                    let id = item["task_id"].as_u64().unwrap();
                    mcp_set_port(
                        &context,
                        json!({"task_id": id, "port": "verdict", "value": "ship"}),
                    )
                    .unwrap();
                    mcp_complete(&context, json!({"task_id": id})).unwrap();
                    return item.clone();
                }
                thread::sleep(Duration::from_millis(10));
            }
            panic!("no invocation was ever offered to the panel");
        });

        let outputs = run_event_loop(
            &state,
            BTreeMap::from([("topic".to_string(), json!("x"))]),
            &executor,
        )
        .unwrap();
        let offered = person.join().unwrap();

        assert_eq!(outputs["verdict"], json!("ship"));
        // The prompt reaches a person already interpolated, so the panel shows
        // the instruction an agent would have read rather than the template.
        assert_eq!(offered["prompt"], json!("Decide on x"));
        // What it may set and what it may see, and nothing else in the run.
        assert_eq!(offered["allowed_effects"], json!({"verdict": "string"}));
        assert_eq!(offered["trigger_values"], json!({"topic": "x"}));
    }

    #[test]
    fn a_bare_port_name_resolves_to_its_one_qualified_effect() {
        let allowed = BTreeMap::from([
            ("faq.objection".to_string(), "string".to_string()),
            ("faq.checked".to_string(), "string".to_string()),
        ]);
        let (port, _) = resolve_effect_port(&allowed, "objection").unwrap();
        assert_eq!(port, "faq.objection");
        let (port, _) = resolve_effect_port(&allowed, "faq.checked").unwrap();
        assert_eq!(port, "faq.checked");

        // An unknown name says which names would have worked.
        let err = resolve_effect_port(&allowed, "verdict")
            .unwrap_err()
            .to_string();
        assert!(err.contains("faq.checked, faq.objection"), "{err}");

        // A bare name two effects share is not guessed at.
        let ambiguous = BTreeMap::from([
            ("a.note".to_string(), "string".to_string()),
            ("b.note".to_string(), "string".to_string()),
        ]);
        assert!(resolve_effect_port(&ambiguous, "note").is_err());
    }

    #[test]
    fn one_agents_pending_work_is_not_anothers() {
        let server = InvocationServer::start().unwrap();
        server
            .registry
            .register(InvocationRecord {
                id: 3,
                team: "Desk".into(),
                agent: "panel".into(),
                reaction: "reaction.0".into(),
                contract: "verdict".into(),
                allowed_effects: BTreeMap::from([("verdict".into(), "string".into())]),
                trigger_values: BTreeMap::new(),
                prompt: "decide".into(),
                writes: BTreeMap::new(),
                completed: false,
                ..Default::default()
            })
            .unwrap();

        let mine = server
            .registry
            .execute("Desk", "panel", None, InvocationCommand::Pending)
            .unwrap();
        assert_eq!(mine["pending"].as_array().unwrap().len(), 1);

        // Asking as someone else returns nothing rather than someone else's
        // work: an invocation is addressed to one agent.
        let theirs = server
            .registry
            .execute("Desk", "clerk", None, InvocationCommand::Pending)
            .unwrap();
        assert!(theirs["pending"].as_array().unwrap().is_empty());
        server.registry.remove(&3);
    }

    /// A connection carrying `delay` nanoseconds, so the run reaches its
    /// output only at that logical timestamp — and under `Pace::RealTime` owes
    /// that much wall-clock time before it gets there.
    fn delayed_bytecode(delay: u64) -> Bytecode {
        serde_json::from_str(&format!(
            r#"{{
              "version": 1,
              "team": "Desk",
              "instructions": [
                {{"op":"begin_plan","team":"Desk"}},
                {{"op":"spawn_agent","name":"clerk","backend":"Codex"}},
                {{"op":"define_port","kind":"input","name":"topic","type":"string"}},
                {{"op":"define_port","kind":"action","name":"staged","type":"string"}},
                {{"op":"define_port","kind":"output","name":"memo","type":"string"}},
                {{"op":"connect_ports","source":"staged","target":"memo","delay":{delay}}},
                {{"op":"install_reaction","id":"reaction.0","agent":"clerk",
                 "triggers":["topic"],"effects":["staged"],
                 "contract":"staged","prompt":"p"}},
                {{"op":"commit_plan"}}
              ]
            }}"#
        ))
        .unwrap()
    }

    /// Writes its one effect and returns at once, so the only time the run
    /// takes is the time the topology asked for.
    struct PromptExecutor;

    impl ReactionExecutor for PromptExecutor {
        fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
            let writes = BTreeMap::from([("staged".to_string(), json!("done"))]);
            validate_contract(&invocation.contract, &writes)?;
            Ok(writes)
        }
    }

    /// The delay these tests are built around.
    ///
    /// Large enough that the gap between sleeping and not sleeping dwarfs any
    /// scheduling jitter a loaded CI host adds, which is what keeps a
    /// wall-clock assertion from being a coin flip.
    const TEST_DELAY: Duration = Duration::from_millis(300);

    #[test]
    fn real_time_owes_the_delay_a_program_asked_for() {
        let state = verify(&delayed_bytecode(TEST_DELAY.as_nanos() as u64)).unwrap();
        let started = Instant::now();
        let outputs = loop_outputs(
            run_event_loop_observed(
                &state,
                BTreeMap::from([("topic".to_string(), json!("x"))]),
                &PromptExecutor,
                &NoopTopologyObserver,
                Pace::RealTime,
                None,
            )
            .unwrap(),
        );
        let elapsed = started.elapsed();
        assert_eq!(outputs["memo"], json!("done"));
        assert!(
            elapsed >= TEST_DELAY,
            "a delay is a wait, not only an ordering: {elapsed:?}"
        );
    }

    #[test]
    fn fast_runs_the_same_program_without_the_wait() {
        let state = verify(&delayed_bytecode(TEST_DELAY.as_nanos() as u64)).unwrap();
        let started = Instant::now();
        let outputs = loop_outputs(
            run_event_loop_observed(
                &state,
                BTreeMap::from([("topic".to_string(), json!("x"))]),
                &PromptExecutor,
                &NoopTopologyObserver,
                Pace::Fast,
                None,
            )
            .unwrap(),
        );
        let elapsed = started.elapsed();
        // Same outputs, same tags, no wall clock. Only the waiting is skipped.
        assert_eq!(outputs["memo"], json!("done"));
        // Half the delay: a run that slept would take all of it, and one that
        // did not has nothing to do, so the slack is for the host rather than
        // for the behaviour under test.
        assert!(
            elapsed < TEST_DELAY / 2,
            "fast should not sleep: {elapsed:?}"
        );
    }

    /// Records the lag reported at every tag.
    struct LagRecorder {
        lags: Mutex<Vec<u64>>,
    }

    impl TopologyObserver for LagRecorder {
        fn tag_advanced(
            &self,
            _timestamp: u64,
            _microstep: u64,
            lag: u64,
            _ports: &BTreeMap<String, Value>,
        ) {
            self.lags.lock().unwrap().push(lag);
        }
    }

    #[test]
    fn lag_is_how_far_past_its_logical_time_a_tag_ran() {
        // A reaction that outlasts the gap to the next tag puts the run behind
        // by the difference, and that is the whole of what lag reports.
        const REACTION: Duration = Duration::from_millis(400);
        let state = verify(&delayed_bytecode(TEST_DELAY.as_nanos() as u64)).unwrap();
        struct SlowExecutor;
        impl ReactionExecutor for SlowExecutor {
            fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
                thread::sleep(REACTION);
                let writes = BTreeMap::from([("staged".to_string(), json!("done"))]);
                validate_contract(&invocation.contract, &writes)?;
                Ok(writes)
            }
        }
        let observer = LagRecorder {
            lags: Mutex::new(Vec::new()),
        };
        run_event_loop_observed(
            &state,
            BTreeMap::from([("topic".to_string(), json!("x"))]),
            &SlowExecutor,
            &observer,
            Pace::RealTime,
            None,
        )
        .unwrap();

        let lags = observer.lags.lock().unwrap().clone();
        // Tag 0 runs immediately, so nothing is owed yet.
        assert!(
            lags[0] < Duration::from_millis(50).as_nanos() as u64,
            "the first tag is not late: {lags:?}"
        );
        // The tag after the reaction was due at TEST_DELAY and reached at
        // REACTION, so it is behind by the difference.
        let behind = (REACTION - TEST_DELAY).as_nanos() as u64;
        let last = *lags.last().expect("a tag ran");
        assert!(
            last >= behind,
            "the run is at least {behind}ns behind: {lags:?}"
        );
        assert!(
            last < behind + TEST_DELAY.as_nanos() as u64,
            "and not a whole delay more than that: {lags:?}"
        );
    }

    #[test]
    fn fast_reports_a_run_that_is_ahead_of_its_schedule_as_unlagged() {
        // Nothing waits, so every tag is reached before its logical time and
        // the run is never behind. Saturating rather than signed: "ahead by
        // 300ms" is not a number an operator has a use for.
        let state = verify(&delayed_bytecode(TEST_DELAY.as_nanos() as u64)).unwrap();
        let observer = LagRecorder {
            lags: Mutex::new(Vec::new()),
        };
        run_event_loop_observed(
            &state,
            BTreeMap::from([("topic".to_string(), json!("x"))]),
            &PromptExecutor,
            &observer,
            Pace::Fast,
            None,
        )
        .unwrap();
        let lags = observer.lags.lock().unwrap().clone();
        assert_eq!(*lags.last().expect("a tag ran"), 0, "{lags:?}");
    }

    #[test]
    fn a_tag_already_late_is_not_made_later() {
        // The reaction outlasts the gap to the next tag, so that tag is due
        // before it is reached — and must run at once rather than waiting out
        // a schedule it has already missed.
        const REACTION: Duration = Duration::from_millis(400);
        let state = verify(&delayed_bytecode(TEST_DELAY.as_nanos() as u64)).unwrap();
        struct SlowExecutor;
        impl ReactionExecutor for SlowExecutor {
            fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
                thread::sleep(REACTION);
                let writes = BTreeMap::from([("staged".to_string(), json!("done"))]);
                validate_contract(&invocation.contract, &writes)?;
                Ok(writes)
            }
        }
        let started = Instant::now();
        run_event_loop_observed(
            &state,
            BTreeMap::from([("topic".to_string(), json!("x"))]),
            &SlowExecutor,
            &NoopTopologyObserver,
            Pace::RealTime,
            None,
        )
        .unwrap();
        let elapsed = started.elapsed();
        // Correct is the reaction alone; pushing the missed tag out would add
        // the whole delay on top. The bound sits halfway between the two, so
        // neither a slow host nor the bug can be mistaken for the other.
        assert!(
            elapsed < REACTION + TEST_DELAY / 2,
            "a missed tag should not be pushed further out: {elapsed:?}"
        );
    }

    /// Records the tag every invocation arrived at, so a timer's schedule can
    /// be read off the run rather than inferred from its outputs.
    struct TimerExecutor {
        fired: Mutex<Vec<u64>>,
        /// Firings after which this executor stops the run.
        ///
        /// A periodic timer has no end of its own, so something has to say
        /// when. It used to be the tag cap, which stopped every run at 1024
        /// whether or not that was the program's intent.
        stop_after: usize,
    }

    impl ReactionExecutor for TimerExecutor {
        fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
            let at = invocation
                .trigger_values
                .values()
                .next()
                .and_then(Value::as_u64)
                .expect("a timer carries the time it fired at");
            let count = {
                let mut fired = self.fired.lock().unwrap();
                fired.push(at);
                fired.len()
            };
            if self.stop_after > 0 && count >= self.stop_after {
                bail!("enough");
            }
            let writes = BTreeMap::from([("note".into(), json!("tick"))]);
            validate_contract(&invocation.contract, &writes)?;
            Ok(writes)
        }
    }

    fn timer_bytecode(offset: u64, period: u64) -> Bytecode {
        serde_json::from_str(&format!(
            r#"{{
              "version": 1,
              "team": "Poller",
              "instructions": [
                {{"op":"begin_plan","team":"Poller"}},
                {{"op":"spawn_agent","name":"agent","backend":"Codex"}},
                {{"op":"define_port","kind":"output","name":"note","type":"string"}},
                {{"op":"declare_timer","name":"t","offset":{offset},"period":{period}}},
                {{"op":"install_reaction","id":"reaction.0","agent":"agent","triggers":["t"],"effects":["note"],"contract":"note","prompt":"tick at $(t)"}},
                {{"op":"commit_plan"}}
              ]
            }}"#
        ))
        .unwrap()
    }

    #[test]
    fn exhausting_a_tag_field_ends_the_run_rather_than_wrapping_it() {
        // With no cap on tags, these two are the only limits left. Wrapping
        // either would put an event in the run's own past, where the queue
        // would hand it back before things that already happened.
        let last_microstep = Tag {
            timestamp: 7,
            microstep: u64::MAX,
        };
        let error = last_microstep
            .advance(Delay::after(0))
            .unwrap_err()
            .to_string();
        assert!(error.contains("microstep overflow"), "{error}");

        let last_timestamp = Tag {
            timestamp: u64::MAX,
            microstep: 0,
        };
        let error = last_timestamp
            .advance(Delay::after(1))
            .unwrap_err()
            .to_string();
        assert!(error.contains("timestamp overflow"), "{error}");

        // And one short of each still advances, so the guard is at the edge
        // rather than a step inside it.
        assert_eq!(
            Tag {
                timestamp: 7,
                microstep: u64::MAX - 1,
            }
            .advance(Delay::after(0))
            .unwrap(),
            Tag {
                timestamp: 7,
                microstep: u64::MAX,
            }
        );
    }

    /// Every program in the corpus still verifies.
    ///
    /// Rejecting a causality loop is a rejection the compiler cannot make on
    /// its own — it needs the whole elaborated topology — so without this a
    /// program that stops verifying is only found when someone deploys it.
    /// Skipped where `omarc` has not been built, which is why CI builds it.
    #[test]
    fn the_topology_corpus_verifies() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let omarc = root.join("lang/.lake/build/bin/omarc");
        if !omarc.exists() {
            eprintln!("skipping: {} has not been built", omarc.display());
            return;
        }

        let mut checked = 0;
        let mut sources: Vec<_> = std::fs::read_dir(root.join("tests/topology/src"))
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|kind| kind == "omar"))
            .collect();
        sources.sort();

        for source in sources {
            let bytecode_path = std::env::temp_dir().join(format!(
                "omar-verify-{}.json",
                source.file_stem().unwrap().to_string_lossy()
            ));
            let compiled = std::process::Command::new(&omarc)
                .arg(&source)
                .arg(&bytecode_path)
                .output()
                .unwrap();
            assert!(
                compiled.status.success(),
                "{} did not compile: {}",
                source.display(),
                String::from_utf8_lossy(&compiled.stderr)
            );

            let bytecode: Bytecode =
                serde_json::from_str(&std::fs::read_to_string(&bytecode_path).unwrap()).unwrap();
            verify(&bytecode)
                .unwrap_or_else(|error| panic!("{} does not verify: {error}", source.display()));
            let _ = std::fs::remove_file(&bytecode_path);
            checked += 1;
        }
        assert!(checked > 0, "the corpus is empty");
    }

    /// A parent triggered by two children at different depths fires once.
    ///
    /// This is the whole of the fixpoint rule in one program. `deep` is three
    /// instantaneous hops from `start` and `shallow` is one, so under a
    /// semantics where a hop costs a microstep the watcher would be woken by
    /// `shallow`, run with `deep` absent, and then be woken again — reading a
    /// half-built picture the first time and reporting twice. Because no hop
    /// costs anything, both land at one tag and the watcher runs after both.
    #[test]
    fn a_parent_reading_two_depths_fires_once_with_both_in_hand() {
        let bytecode: Bytecode = serde_json::from_str(
            r#"{
              "version": 1,
              "team": "Depths",
              "instructions": [
                {"op":"begin_plan","team":"Depths"},
                {"op":"spawn_agent","name":"near","backend":"Stub"},
                {"op":"spawn_agent","name":"far","backend":"Stub"},
                {"op":"spawn_agent","name":"deeper","backend":"Stub"},
                {"op":"spawn_agent","name":"watcher","backend":"Stub"},
                {"op":"define_port","kind":"input","name":"start","type":"int"},
                {"op":"define_port","kind":"output","name":"shallow","type":"int"},
                {"op":"define_port","kind":"input","name":"shallow_in","type":"int"},
                {"op":"define_port","kind":"output","name":"middle","type":"int"},
                {"op":"define_port","kind":"input","name":"middle_in","type":"int"},
                {"op":"define_port","kind":"output","name":"deep","type":"int"},
                {"op":"define_port","kind":"input","name":"deep_in","type":"int"},
                {"op":"define_port","kind":"output","name":"verdict","type":"int"},
                {"op":"connect_ports","source":"shallow","target":"shallow_in"},
                {"op":"connect_ports","source":"middle","target":"middle_in"},
                {"op":"connect_ports","source":"deep","target":"deep_in"},
                {"op":"install_reaction","id":"reaction.0","agent":"near",
                 "triggers":["start"],"effects":["shallow"],
                 "contract":"shallow","prompt":"p"},
                {"op":"install_reaction","id":"reaction.1","agent":"far",
                 "triggers":["shallow_in"],"effects":["middle"],
                 "contract":"middle","prompt":"p"},
                {"op":"install_reaction","id":"reaction.2","agent":"deeper",
                 "triggers":["middle_in"],"effects":["deep"],
                 "contract":"deep","prompt":"p"},
                {"op":"install_reaction","id":"reaction.3","agent":"watcher",
                 "triggers":["shallow_in","deep_in"],"effects":["verdict"],
                 "contract":"verdict","prompt":"p"},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap();
        let state = verify(&bytecode).unwrap();

        /// Records what the watcher was holding each time it ran.
        #[derive(Default)]
        struct Watch {
            saw: Mutex<Vec<BTreeMap<String, Value>>>,
        }
        impl ReactionExecutor for Watch {
            fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
                if invocation.reaction_id == "reaction.3" {
                    self.saw
                        .lock()
                        .unwrap()
                        .push(invocation.trigger_values.clone());
                }
                Ok(invocation
                    .allowed_effects
                    .keys()
                    .map(|port| (port.clone(), json!(1)))
                    .collect())
            }
        }

        let executor = Watch::default();
        run_event_loop(
            &state,
            BTreeMap::from([("start".to_string(), json!(1))]),
            &executor,
        )
        .unwrap();

        let saw = executor.saw.lock().unwrap();
        assert_eq!(saw.len(), 1, "the watcher ran more than once: {saw:?}");
        assert_eq!(
            saw[0].keys().collect::<Vec<_>>(),
            vec!["deep_in", "shallow_in"],
            "the watcher ran without both of its triggers"
        );
    }

    /// The two kinds of connection, told apart by the tag they land on.
    #[test]
    fn a_plain_connection_is_instant_and_after_zero_costs_a_microstep() {
        let program = |delay: &str| {
            format!(
                r#"{{
                  "version": 1,
                  "team": "Hop",
                  "instructions": [
                    {{"op":"begin_plan","team":"Hop"}},
                    {{"op":"spawn_agent","name":"downstream","backend":"Stub"}},
                    {{"op":"define_port","kind":"input","name":"source","type":"int"}},
                    {{"op":"define_port","kind":"input","name":"target","type":"int"}},
                    {{"op":"define_port","kind":"output","name":"done","type":"int"}},
                    {{"op":"connect_ports","source":"source","target":"target"{delay}}},
                    {{"op":"install_reaction","id":"reaction.0","agent":"downstream",
                     "triggers":["target"],"effects":["done"],
                     "contract":"done","prompt":"p"}},
                    {{"op":"commit_plan"}}
                  ]
                }}"#
            )
        };

        /// Reports the tag the downstream reaction was invoked at.
        #[derive(Default)]
        struct At {
            tag: Mutex<Option<(u64, u64)>>,
        }
        impl TopologyObserver for At {
            fn reaction_started(&self, timestamp: u64, microstep: u64, _: &str, _: &str) {
                *self.tag.lock().unwrap() = Some((timestamp, microstep));
            }
        }
        struct Yes;
        impl ReactionExecutor for Yes {
            fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
                Ok(invocation
                    .allowed_effects
                    .keys()
                    .map(|port| (port.clone(), json!(1)))
                    .collect())
            }
        }

        let ran_at = |delay: &str| {
            let bytecode: Bytecode = serde_json::from_str(&program(delay)).unwrap();
            let state = verify(&bytecode).unwrap();
            let observer = At::default();
            run_event_loop_observed(
                &state,
                BTreeMap::from([("source".to_string(), json!(1))]),
                &Yes,
                &observer,
                Pace::Fast,
                None,
            )
            .unwrap();
            let tag = *observer.tag.lock().unwrap();
            tag.expect("the downstream reaction never ran")
        };

        // No `after`: the value is readable at the tag it was written.
        assert_eq!(ran_at(""), (0, 0));
        // `after 0`: one microstep later, no time passed.
        assert_eq!(ran_at(r#","delay":0"#), (0, 1));
        // `after n`: n nanoseconds later, back at microstep zero.
        assert_eq!(ran_at(r#","delay":5"#), (5, 0));
    }

    /// A cycle that costs nothing to go round is refused before it runs.
    #[test]
    fn a_loop_of_instant_hops_is_refused() {
        let program = |delay: &str| {
            format!(
                r#"{{
                  "version": 1,
                  "team": "Knot",
                  "instructions": [
                    {{"op":"begin_plan","team":"Knot"}},
                    {{"op":"spawn_agent","name":"left","backend":"Stub"}},
                    {{"op":"spawn_agent","name":"right","backend":"Stub"}},
                    {{"op":"define_port","kind":"output","name":"a","type":"int"}},
                    {{"op":"define_port","kind":"input","name":"a_in","type":"int"}},
                    {{"op":"define_port","kind":"output","name":"b","type":"int"}},
                    {{"op":"define_port","kind":"input","name":"b_in","type":"int"}},
                    {{"op":"connect_ports","source":"a","target":"a_in"{delay}}},
                    {{"op":"connect_ports","source":"b","target":"b_in"}},
                    {{"op":"install_reaction","id":"reaction.0","agent":"left",
                     "triggers":["a_in"],"effects":["b"],"contract":"b","prompt":"p"}},
                    {{"op":"install_reaction","id":"reaction.1","agent":"right",
                     "triggers":["b_in"],"effects":["a"],"contract":"a","prompt":"p"}},
                    {{"op":"commit_plan"}}
                  ]
                }}"#
            )
        };

        let verified = |delay: &str| {
            let bytecode: Bytecode = serde_json::from_str(&program(delay)).unwrap();
            verify(&bytecode)
        };

        // Every hop instantaneous: neither reaction can go first.
        let error = verified("").unwrap_err().to_string();
        assert!(error.contains("causality loop"), "{error}");

        // Delay the backwards edge so data dependencies agree with the
        // instance's definition order within each tag.
        verified(r#","delay":0"#).expect("a microstep breaks the loop");
        // As does real time.
        verified(r#","delay":5"#).expect("a delay breaks the loop");
    }

    #[test]
    fn a_zero_delay_loop_runs_past_where_the_tag_cap_used_to_be() {
        // A graph cycle is not a causality loop when it costs a microstep to go
        // round -- here through an action -- so this is a walk through
        // superdense time. It used to be cut off at 1024 tags with an error;
        // now it ends when the work says so.
        let bytecode: Bytecode = serde_json::from_str(
            r#"{
              "version": 1,
              "team": "Loop",
              "instructions": [
                {"op":"begin_plan","team":"Loop"},
                {"op":"spawn_agent","name":"counter","backend":"Codex"},
                {"op":"define_port","kind":"input","name":"start","type":"int"},
                {"op":"define_port","kind":"action","name":"next","type":"int"},
                {"op":"define_port","kind":"output","name":"result","type":"int"},
                {"op":"install_reaction","id":"reaction.0","agent":"counter",
                 "triggers":["start","next"],"effects":["next","result"],
                 "contract":"( next | result )","prompt":"p"},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap();
        let state = verify(&bytecode).unwrap();

        /// Feeds itself until it has gone well past the old bound, then exits
        /// through the other alternative of its contract.
        struct Spinner {
            seen: Mutex<u64>,
        }
        impl ReactionExecutor for Spinner {
            fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
                let mut seen = self.seen.lock().unwrap();
                *seen += 1;
                let writes = if *seen >= 4096 {
                    BTreeMap::from([("result".to_string(), json!(*seen))])
                } else {
                    BTreeMap::from([("next".to_string(), json!(*seen))])
                };
                validate_contract(&invocation.contract, &writes)?;
                Ok(writes)
            }
        }

        let executor = Spinner {
            seen: Mutex::new(0),
        };
        let outputs = run_event_loop(
            &state,
            BTreeMap::from([("start".to_string(), json!(0))]),
            &executor,
        )
        .unwrap();
        assert_eq!(outputs["result"], json!(4096));
    }

    #[test]
    fn a_timer_with_no_period_fires_once_at_its_offset() {
        let state = verify(&timer_bytecode(3, 0)).unwrap();
        let executor = TimerExecutor {
            fired: Mutex::new(Vec::new()),
            // A one-shot ends on its own; nothing has to stop it.
            stop_after: 0,
        };

        let outputs = run_event_loop(&state, BTreeMap::new(), &executor).unwrap();

        // Once, at the offset — not at tag 0, and not again afterwards.
        assert_eq!(*executor.fired.lock().unwrap(), vec![3]);
        assert_eq!(outputs, BTreeMap::from([("note".into(), json!("tick"))]));
    }

    #[test]
    fn a_periodic_timer_re_arms_itself_at_every_period() {
        let state = verify(&timer_bytecode(0, 10)).unwrap();
        let executor = TimerExecutor {
            fired: Mutex::new(Vec::new()),
            // Well past the 1024 tags that used to end every run, so this also
            // says the bound is gone rather than merely raised.
            stop_after: 1500,
        };

        // A periodic timer has no end of its own. Nothing in the runtime
        // invents one, so it runs until the work does something about it.
        let error = run_event_loop(&state, BTreeMap::new(), &executor).unwrap_err();
        assert!(error.to_string().contains("enough"), "{error}");

        let fired = executor.fired.lock().unwrap().clone();
        assert_eq!(&fired[..4], &[0, 10, 20, 30], "fired at {fired:?}");
        assert_eq!(fired.len(), 1500, "kept its period the whole way");
        // Every firing is one period on from the last, all the way out.
        assert_eq!(*fired.last().unwrap(), 10 * 1499);
    }

    /// A clock that ticks every 10ns, and a reaction to `shutdown`.
    fn shutdown_bytecode(period: u64) -> Bytecode {
        serde_json::from_str(&format!(
            r#"{{
              "version": 1,
              "team": "Smoke",
              "instructions": [
                {{"op":"begin_plan","team":"Smoke"}},
                {{"op":"spawn_agent","name":"agent","backend":"Codex"}},
                {{"op":"define_port","kind":"output","name":"note","type":"string"}},
                {{"op":"declare_timer","name":"t","offset":5,"period":{period}}},
                {{"op":"declare_timer","name":"shutdown","offset":0,"period":0,"shutdown":true}},
                {{"op":"install_reaction","id":"reaction.0","agent":"agent","triggers":["t"],"effects":["note"],"contract":"note","prompt":"tick"}},
                {{"op":"install_reaction","id":"reaction.1","agent":"agent","triggers":["shutdown"],"effects":["note"],"contract":"note","prompt":"bye"}},
                {{"op":"commit_plan"}}
              ]
            }}"#
        ))
        .unwrap()
    }

    /// Records which trigger fired when, and asks to stop at a given firing.
    struct StopExecutor {
        fired: Mutex<Vec<(String, u64)>>,
        stop_at: usize,
    }

    impl ReactionExecutor for StopExecutor {
        fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
            let (trigger, at) = invocation.trigger_values.iter().next().unwrap();
            let count = {
                let mut fired = self.fired.lock().unwrap();
                fired.push((trigger.clone(), at.as_u64().unwrap()));
                fired.len()
            };
            let mut writes = BTreeMap::from([("note".into(), json!(trigger))]);
            if count == self.stop_at {
                writes.insert(crate::reaction::STOP.into(), json!(true));
            }
            Ok(writes)
        }
    }

    #[test]
    fn a_requested_stop_ends_the_run_after_one_shutdown_microstep() {
        let state = verify(&shutdown_bytecode(10)).unwrap();
        let executor = StopExecutor {
            fired: Mutex::new(Vec::new()),
            stop_at: 3,
        };
        // A periodic timer never ends on its own; the request is what ends it.
        let outputs = run_event_loop(&state, BTreeMap::new(), &executor).unwrap();
        let fired = executor.fired.lock().unwrap().clone();
        let at = |name: &str, time| (name.to_string(), time);
        assert_eq!(
            fired,
            vec![at("t", 5), at("t", 15), at("t", 25), at("shutdown", 25)]
        );
        assert_eq!(outputs["note"], json!("shutdown"));
    }

    #[test]
    fn a_run_that_runs_out_of_events_still_shuts_down() {
        let state = verify(&shutdown_bytecode(0)).unwrap();
        let executor = StopExecutor {
            fired: Mutex::new(Vec::new()),
            stop_at: 0,
        };
        run_event_loop(&state, BTreeMap::new(), &executor).unwrap();
        let fired = executor.fired.lock().unwrap().clone();
        assert_eq!(
            fired,
            vec![("t".to_string(), 5), ("shutdown".to_string(), 5)]
        );
    }

    #[test]
    fn a_stop_with_nothing_reacting_to_shutdown_ends_at_its_tag() {
        let state = verify(&timer_bytecode(0, 10)).unwrap();
        let executor = StopExecutor {
            fired: Mutex::new(Vec::new()),
            stop_at: 2,
        };
        run_event_loop(&state, BTreeMap::new(), &executor).unwrap();
        assert_eq!(executor.fired.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_timer_cannot_share_a_name_with_a_port_or_be_written_to() {
        let shadowing: Bytecode = serde_json::from_str(
            r#"{
              "version": 1,
              "team": "Clash",
              "instructions": [
                {"op":"begin_plan","team":"Clash"},
                {"op":"define_port","kind":"input","name":"t","type":"int"},
                {"op":"declare_timer","name":"t","offset":1,"period":0},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap();
        let error = verify(&shadowing).unwrap_err().to_string();
        assert!(error.contains("a trigger has one name"), "{error}");

        let written: Bytecode = serde_json::from_str(
            r#"{
              "version": 1,
              "team": "Written",
              "instructions": [
                {"op":"begin_plan","team":"Written"},
                {"op":"spawn_agent","name":"agent","backend":"Codex"},
                {"op":"define_port","kind":"input","name":"go","type":"int"},
                {"op":"declare_timer","name":"t","offset":1,"period":0},
                {"op":"install_reaction","id":"reaction.0","agent":"agent","triggers":["go"],"effects":["t"],"contract":"t","prompt":"x"},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap();
        let error = verify(&written).unwrap_err().to_string();
        assert!(error.contains("cannot write to timer"), "{error}");
    }

    struct SuperdenseExecutor {
        calls: Mutex<Vec<String>>,
    }

    impl ReactionExecutor for SuperdenseExecutor {
        fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
            self.calls
                .lock()
                .unwrap()
                .push(invocation.reaction_id.clone());
            let writes = match invocation.reaction_id.as_str() {
                "reaction.0" => {
                    BTreeMap::from([("immediate".into(), json!(7)), ("fixed".into(), json!(7))])
                }
                "reaction.1" => BTreeMap::from([("fixed_result".into(), json!(7))]),
                "reaction.2" => BTreeMap::from([("connected_result".into(), json!(7))]),
                other => panic!("unexpected reaction {other}"),
            };
            validate_contract(&invocation.contract, &writes)?;
            Ok(writes)
        }
    }

    #[test]
    fn fixed_and_connection_delays_order_reactions_by_timestamp() {
        let bytecode: Bytecode = serde_json::from_str(
            r#"{
              "version": 1,
              "team": "Superdense",
              "instructions": [
                {"op":"begin_plan","team":"Superdense"},
                {"op":"spawn_agent","name":"producer","backend":"Codex"},
                {"op":"spawn_agent","name":"fixed_consumer","backend":"Codex"},
                {"op":"spawn_agent","name":"connected_consumer","backend":"Codex"},
                {"op":"define_port","kind":"input","name":"start","type":"int"},
                {"op":"define_port","kind":"output","name":"fixed_result","type":"int"},
                {"op":"define_port","kind":"output","name":"connected_result","type":"int"},
                {"op":"define_port","kind":"action","name":"immediate","type":"int"},
                {"op":"define_port","kind":"action","name":"fixed","type":"int","delay":2},
                {"op":"define_port","kind":"action","name":"connected","type":"int","delay":1},
                {"op":"connect_ports","source":"immediate","target":"connected","delay":3},
                {"op":"install_reaction","id":"reaction.0","agent":"producer","triggers":["start"],"effects":["immediate","fixed"],"contract":"immediate , fixed","prompt":"produce"},
                {"op":"install_reaction","id":"reaction.1","agent":"fixed_consumer","triggers":["fixed"],"effects":["fixed_result"],"contract":"fixed_result","prompt":"fixed"},
                {"op":"install_reaction","id":"reaction.2","agent":"connected_consumer","triggers":["connected"],"effects":["connected_result"],"contract":"connected_result","prompt":"connected"},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap();
        let state = verify(&bytecode).unwrap();
        let executor = SuperdenseExecutor {
            calls: Mutex::new(Vec::new()),
        };
        let outputs = run_event_loop(
            &state,
            BTreeMap::from([("start".into(), json!(7))]),
            &executor,
        )
        .unwrap();

        assert_eq!(
            outputs,
            BTreeMap::from([
                ("connected_result".into(), json!(7)),
                ("fixed_result".into(), json!(7)),
            ])
        );
        assert_eq!(
            *executor.calls.lock().unwrap(),
            vec!["reaction.0", "reaction.1", "reaction.2"]
        );

        // Delays are where a re-implementation would drift first: a fixed port
        // delay and a connection delay both move a tag, and the projection has
        // to move it the same way. It shares `enqueue_event` with the loop so
        // that it cannot, and this is what says so.
        let real = tags_of_a_real_run(
            &state,
            BTreeMap::from([("start".into(), json!(7))]),
            &SuperdenseExecutor {
                calls: Mutex::new(Vec::new()),
            },
        );
        assert_eq!(timeline_tags(&state, &["start"]), real);
    }

    struct OrTriggerExecutor {
        invocations: Mutex<Vec<InvocationSpec>>,
    }

    impl ReactionExecutor for OrTriggerExecutor {
        fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
            self.invocations.lock().unwrap().push(invocation);
            Ok(BTreeMap::from([("hired".to_string(), json!(true))]))
        }
    }

    #[test]
    fn reaction_fires_when_any_declared_trigger_is_present() {
        let mut state = hr_state();
        state.reactions.retain(|id, _| id.as_str() == "reaction.3");
        let executor = OrTriggerExecutor {
            invocations: Mutex::new(Vec::new()),
        };

        let outputs = run_event_loop(
            &state,
            BTreeMap::from([("opinion1".into(), json!("strong engineer"))]),
            &executor,
        )
        .unwrap();

        assert_eq!(outputs, BTreeMap::from([("hired".into(), json!(true))]));
        let invocations = executor.invocations.lock().unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(
            invocations[0].trigger_values,
            BTreeMap::from([("opinion1".into(), json!("strong engineer"))])
        );
    }

    #[test]
    fn reaction_fires_once_when_multiple_triggers_share_a_tag() {
        let mut state = hr_state();
        state.reactions.retain(|id, _| id.as_str() == "reaction.3");
        let executor = OrTriggerExecutor {
            invocations: Mutex::new(Vec::new()),
        };

        run_event_loop(
            &state,
            BTreeMap::from([
                ("opinion1".into(), json!("strong engineer")),
                ("opinion2".into(), json!("good judgment")),
            ]),
            &executor,
        )
        .unwrap();

        let invocations = executor.invocations.lock().unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].trigger_values.len(), 2);
    }

    #[test]
    fn absent_trigger_interpolation_is_explicit() {
        let values = BTreeMap::from([("left".into(), json!("ready"))]);
        assert_eq!(
            render_prompt("left=$(left), right=$(right)", &values).unwrap(),
            "left=ready, right=<absent>"
        );
    }

    fn independent_team_reactions() -> VmState {
        let mut state = hr_state();
        state
            .reactions
            .retain(|id, _| matches!(id.as_str(), "reaction.1" | "reaction.2"));
        state
    }

    #[test]
    fn different_agents_share_files_in_prompt_definition_order() {
        struct Files {
            path: PathBuf,
            active: std::sync::atomic::AtomicBool,
            order: Mutex<Vec<String>>,
        }
        impl ReactionExecutor for Files {
            fn invoke(&self, spec: InvocationSpec) -> Result<BTreeMap<String, Value>> {
                use std::sync::atomic::Ordering;
                assert!(
                    !self.active.swap(true, Ordering::SeqCst),
                    "same-team invocations overlap"
                );
                self.order.lock().unwrap().push(spec.reaction_id.clone());
                if spec.reaction_id == "reaction.2" {
                    thread::sleep(Duration::from_millis(20));
                    fs::write(&self.path, "first invocation finished")?;
                } else {
                    assert_eq!(fs::read_to_string(&self.path)?, "first invocation finished");
                }
                self.active.store(false, Ordering::SeqCst);
                Ok(BTreeMap::new())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let executor = Files {
            path: dir.path().join("artifact"),
            active: std::sync::atomic::AtomicBool::new(false),
            order: Mutex::new(Vec::new()),
        };
        let mut state = independent_team_reactions();
        // Key/agent names must not determine order: the definition index does.
        state.reactions.get_mut("reaction.1").unwrap().order = 30;
        state.reactions.get_mut("reaction.2").unwrap().order = 20;
        run_event_loop(
            &state,
            BTreeMap::from([("triage".into(), json!("ready"))]),
            &executor,
        )
        .unwrap();
        assert_eq!(
            *executor.order.lock().unwrap(),
            vec!["reaction.2", "reaction.1"]
        );
    }

    #[test]
    fn separate_team_instances_can_still_execute_in_parallel() {
        struct Parallel {
            entered: Mutex<usize>,
            ready: std::sync::Condvar,
        }
        impl ReactionExecutor for Parallel {
            fn invoke(&self, _: InvocationSpec) -> Result<BTreeMap<String, Value>> {
                let mut count = self.entered.lock().unwrap();
                *count += 1;
                self.ready.notify_all();
                let (count, _) = self
                    .ready
                    .wait_timeout_while(count, Duration::from_secs(5), |count| *count < 2)
                    .unwrap();
                anyhow::ensure!(*count == 2, "separate instances were serialized");
                Ok(BTreeMap::new())
            }
        }
        let mut state = independent_team_reactions();
        state.reactions.get_mut("reaction.1").unwrap().instance = "parent".into();
        state.reactions.get_mut("reaction.2").unwrap().instance = "parent.child".into();
        let executor = Parallel {
            entered: Mutex::new(0),
            ready: std::sync::Condvar::new(),
        };
        run_event_loop(
            &state,
            BTreeMap::from([("triage".into(), json!("ready"))]),
            &executor,
        )
        .unwrap();
    }

    #[test]
    fn instance_order_uses_a_linear_chain_even_with_shared_agents_and_effects() {
        let mut state = independent_team_reactions();
        let template = state.reactions["reaction.1"].clone();
        state.reactions.clear();
        let count = 128;
        let ids: Vec<_> = (0..count)
            .map(|order| format!("reaction.{}", count - order))
            .collect();
        for (order, id) in ids.iter().enumerate() {
            let mut reaction = template.clone();
            reaction.order = order;
            state.reactions.insert(id.clone(), reaction);
        }
        // All reactions share an agent and an action effect, which must not
        // reintroduce redundant same-instance ordering edges.
        let edge_count: usize = ids.iter().map(|id| must_follow(&state, id).len()).sum();
        assert_eq!(edge_count, count - 1);
        assert_eq!(
            precedence_layers(&state).unwrap(),
            ids.iter().map(|id| vec![id.clone()]).collect::<Vec<_>>()
        );
        reject_causality_loops(&state).unwrap();

        // A backwards data dependency still closes a cycle through the chain,
        // even when the source and consumer are not adjacent definitions.
        state.ports.get_mut("opinion1").unwrap().kind = PortKind::Input;
        for reaction in state.reactions.values_mut() {
            reaction.effects.clear();
        }
        state
            .reactions
            .get_mut(ids.last().unwrap())
            .unwrap()
            .effects = vec!["opinion1".into()];
        state.reactions.get_mut(&ids[0]).unwrap().triggers = vec!["opinion1".into()];
        assert!(precedence_layers(&state).is_err());
        assert!(reject_causality_loops(&state).is_err());
    }

    #[test]
    fn backwards_same_tag_dependency_conflicts_with_team_definition_order() {
        let mut state = independent_team_reactions();
        state.ports.get_mut("opinion2").unwrap().kind = PortKind::Input;
        state.reactions.get_mut("reaction.1").unwrap().triggers = vec!["opinion2".into()];
        assert!(precedence_layers(&state).is_err());
        assert!(reject_causality_loops(&state).is_err());
    }

    #[test]
    fn precedence_orders_overlapping_effects_and_same_agent() {
        let mut state = hr_state();
        state.reactions.get_mut("reaction.2").unwrap().effects = vec!["opinion1".into()];
        state.reactions.get_mut("reaction.2").unwrap().agent = "reviewer1".into();
        assert!(must_follow(&state, "reaction.1").contains("reaction.2"));

        let layers = precedence_layers(&state).unwrap();
        let layer_of = |id: &str| {
            layers
                .iter()
                .position(|layer| layer.iter().any(|entry| entry == id))
                .unwrap()
        };
        assert!(layer_of("reaction.1") < layer_of("reaction.2"));
    }

    /// Two agents of one instance and the state they share. Each reaction
    /// triggers on `go` and on `again`, which the first raises once, so the
    /// run has two tags.
    fn shared_notes_bytecode() -> Bytecode {
        serde_json::from_str(
            r#"{
              "version": 1,
              "team": "Shared",
              "instructions": [
                {"op":"begin_plan","team":"Shared"},
                {"op":"declare_instance","name":"t","parent":"","team":"T"},
                {"op":"spawn_agent","instance":"t","name":"t.a","backend":"stub"},
                {"op":"spawn_agent","instance":"t","name":"t.b","backend":"stub"},
                {"op":"define_port","instance":"t","kind":"input","name":"t.go","type":"int"},
                {"op":"define_port","instance":"t","kind":"action","name":"t.again","type":"int"},
                {"op":"declare_state","instance":"t","name":"t.notes","type":"string","initial":""},
                {"op":"install_reaction","instance":"t","id":"t.reaction.0","agent":"t.a",
                  "triggers":["t.go","t.again"],"effects":["t.again"],
                  "contract":"t.again ?","prompt":"a reads [$(t.notes)]"},
                {"op":"install_reaction","instance":"t","id":"t.reaction.1","agent":"t.b",
                  "triggers":["t.go","t.again"],"effects":[],
                  "contract":"","prompt":"b reads [$(t.notes)]"},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap()
    }

    /// Answers each invocation through the registry, the way an agent in a
    /// pane does over MCP: it writes the state by its bare name and
    /// completes. Pauses the run from inside the first invocation when given
    /// a deployment directory.
    struct NoteTakers {
        registry: InvocationRegistry,
        prompts: Mutex<Vec<String>>,
        pause: Option<PathBuf>,
    }
    impl ReactionExecutor for NoteTakers {
        fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
            let record = invocation_record("Shared", &invocation)?;
            let turn = {
                let mut prompts = self.prompts.lock().unwrap();
                prompts.push(record.prompt.clone());
                prompts.len()
            };
            if let (1, Some(dir)) = (turn, &self.pause) {
                deploy::request(dir, ControlOp::Pause).unwrap();
            }
            let completion = self.registry.register(record)?;
            let agent = invocation.agent.clone();
            let who = agent.trim_start_matches("t.");
            self.registry.execute(
                "Shared",
                &agent,
                None,
                InvocationCommand::SetState(SetStateArgs {
                    task_id: invocation.id,
                    name: "notes".into(),
                    value: json!(format!("{who} was here")),
                }),
            )?;
            if invocation.trigger_values.contains_key("t.go") && who == "a" {
                self.registry.execute(
                    "Shared",
                    &agent,
                    None,
                    InvocationCommand::SetPort(SetPortArgs {
                        task_id: invocation.id,
                        port: "again".into(),
                        value: json!(1),
                    }),
                )?;
            }
            self.registry.execute(
                "Shared",
                &agent,
                None,
                InvocationCommand::Complete(CompleteArgs {
                    task_id: invocation.id,
                    keep: vec![],
                }),
            )?;
            let writes = completion.try_recv()?;
            self.registry.remove(&invocation.id);
            Ok(writes)
        }
    }

    /// Every task of an instance reads its state in the prompt and writes it
    /// with `omar_set_state`, no contract saying so: a later task sees it at
    /// the same tag, the next tag sees it, and a pause and resume keep it.
    #[test]
    fn tasks_share_their_instances_state() {
        let state = verify(&shared_notes_bytecode()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let first = NoteTakers {
            registry: InvocationRegistry::default(),
            prompts: Mutex::new(Vec::new()),
            pause: Some(dir.path().to_path_buf()),
        };
        let end = run_event_loop_controlled(
            &state,
            LoopStart::Fresh(BTreeMap::from([("t.go".to_string(), json!(1))])),
            &first,
            &NoopTopologyObserver,
            Pace::Fast,
            Some(dir.path()),
            Some(&Recorder::new()),
        )
        .unwrap();
        let LoopEnd::Paused(captured) = end else {
            panic!("the pause request was not honoured");
        };
        // `b` is declared after `a`, so it runs after it and reads its write.
        assert_eq!(
            *first.prompts.lock().unwrap(),
            ["a reads []", "b reads [a was here]"]
        );
        assert_eq!(captured.state_vars["t.notes"], json!("b was here"));

        deploy::clear_stop(dir.path()).unwrap();
        let second = NoteTakers {
            registry: InvocationRegistry::default(),
            prompts: Mutex::new(Vec::new()),
            pause: None,
        };
        let end = run_event_loop_controlled(
            &state,
            LoopStart::Resume(captured),
            &second,
            &NoopTopologyObserver,
            Pace::Fast,
            Some(dir.path()),
            Some(&Recorder::new()),
        )
        .unwrap();
        let LoopEnd::Completed(settled) = end else {
            panic!("the resumed run should finish");
        };
        // The next tag, in a fresh process, starts from what the last one left.
        assert_eq!(
            *second.prompts.lock().unwrap(),
            ["a reads [b was here]", "b reads [a was here]"]
        );
        assert_eq!(settled.state_vars["t.notes"], json!("b was here"));
    }

    /// A task's state write goes through `omar_set_state`, never
    /// `omar_set_port`; it is checked like an effect's, stays private until
    /// completion, and reaches only its own instance's state. The agent is
    /// told what the state holds, in its message and by `omar_pending`.
    #[test]
    fn a_tasks_state_write_is_checked_and_scoped_to_its_instance() {
        let registry = InvocationRegistry::default();
        let record = InvocationRecord {
            id: 9,
            team: "Shared".into(),
            agent: "t.a".into(),
            reaction: "t.reaction.0".into(),
            contract: "t.out".into(),
            allowed_effects: BTreeMap::from([("t.out".into(), "string".into())]),
            state_values: BTreeMap::from([("t.notes".into(), json!("old"))]),
            state_types: BTreeMap::from([("t.notes".into(), "string".into())]),
            ..Default::default()
        };
        let completion = registry.register(record).unwrap();
        let set_port = |port: &str, value: Value| {
            registry.execute(
                "Shared",
                "t.a",
                None,
                InvocationCommand::SetPort(SetPortArgs {
                    task_id: 9,
                    port: port.into(),
                    value,
                }),
            )
        };
        let set = |name: &str, value: Value| {
            registry.execute(
                "Shared",
                "t.a",
                None,
                InvocationCommand::SetState(SetStateArgs {
                    task_id: 9,
                    name: name.into(),
                    value,
                }),
            )
        };
        let pending = registry
            .execute("Shared", "t.a", None, InvocationCommand::Pending)
            .unwrap();
        assert_eq!(pending["pending"][0]["state"]["t.notes"], json!("old"));
        assert_eq!(
            pending["pending"][0]["state_types"]["t.notes"],
            json!("string")
        );
        // State is not a port, and the refusal names the tool that sets it.
        for name in ["notes", "t.notes"] {
            let refused = set_port(name, json!("x")).unwrap_err();
            assert!(
                format!("{refused:#}").contains("omar_set_state"),
                "{refused:#}"
            );
        }
        // Nor is a port state.
        assert!(set("out", json!("x")).is_err());
        let wrong = set("notes", json!(3)).unwrap_err();
        assert!(format!("{wrong:#}").contains("invalid value"), "{wrong:#}");
        // Another instance's state is not this invocation's to write.
        let elsewhere = set("u.notes", json!("x")).unwrap_err();
        assert!(
            format!("{elsewhere:#}").contains("t.notes"),
            "{elsewhere:#}"
        );
        set("notes", json!("new")).unwrap();
        set_port("out", json!("done")).unwrap();
        assert!(completion.try_recv().is_err(), "private until complete");
        registry
            .execute(
                "Shared",
                "t.a",
                None,
                InvocationCommand::Complete(CompleteArgs {
                    task_id: 9,
                    keep: vec![],
                }),
            )
            .unwrap();
        let writes = completion.try_recv().unwrap();
        assert_eq!(writes["t.notes"], json!("new"));
        assert_eq!(writes["t.out"], json!("done"));

        let spec = InvocationSpec {
            id: 9,
            reaction_id: "t.reaction.0".into(),
            agent: "t.a".into(),
            trigger_values: BTreeMap::new(),
            allowed_effects: BTreeMap::new(),
            effect_names: BTreeMap::new(),
            types: BTreeMap::new(),
            state_values: BTreeMap::from([("t.notes".into(), json!("old"))]),
            state_types: BTreeMap::from([("t.notes".into(), "string".into())]),
            contexts: BTreeMap::new(),
            context_names: BTreeSet::new(),
            contract: String::new(),
            prompt: "Notes: $(t.notes)".into(),
            within: None,
            tag: Tag::default(),
        };
        let record = invocation_record("Shared", &spec).unwrap();
        assert_eq!(record.prompt, "Notes: old");
        let message = invocation_message(&spec, &record.prompt).unwrap();
        assert!(
            message.contains("state: {\"t.notes\":\"old\"}\n"),
            "{message}"
        );
        assert!(
            message.contains("change any of it with omar_set_state"),
            "{message}"
        );
    }

    /// Three agents of one instance; `a` and `b` share the context `ops`,
    /// `c` does not. All run at the one tag `go` arrives, in that order.
    fn shared_context_bytecode() -> Bytecode {
        serde_json::from_str(
            r#"{
              "version": 1,
              "team": "Ctx",
              "instructions": [
                {"op":"begin_plan","team":"Ctx"},
                {"op":"declare_instance","name":"t","parent":"","team":"T"},
                {"op":"spawn_agent","instance":"t","name":"t.a","backend":"stub"},
                {"op":"spawn_agent","instance":"t","name":"t.b","backend":"stub"},
                {"op":"spawn_agent","instance":"t","name":"t.c","backend":"stub"},
                {"op":"define_port","instance":"t","kind":"input","name":"t.go","type":"int"},
                {"op":"declare_state","instance":"t","name":"t.ops","type":"string","initial":"",
                  "agents":["t.a","t.b"],"directive":"List decisions, newest last."},
                {"op":"install_reaction","instance":"t","id":"t.reaction.0","agent":"t.a",
                  "triggers":["t.go"],"effects":[],"contract":"","prompt":"Plan."},
                {"op":"install_reaction","instance":"t","id":"t.reaction.1","agent":"t.b",
                  "triggers":["t.go"],"effects":[],"contract":"","prompt":"Build."},
                {"op":"install_reaction","instance":"t","id":"t.reaction.2","agent":"t.c",
                  "triggers":["t.go"],"effects":[],"contract":"","prompt":"Review."},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap()
    }

    /// Answers through the registry as an agent does over MCP. `a` records a
    /// decision in the context; `b` first tries to complete without touching
    /// it, then keeps it; `c` shares none. Each one's pending entry, message
    /// and refusals are kept.
    struct ContextKeepers {
        registry: InvocationRegistry,
        seen: Mutex<Vec<(String, Value, String)>>,
        refusals: Mutex<Vec<String>>,
    }
    impl ReactionExecutor for ContextKeepers {
        fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
            let record = invocation_record("Ctx", &invocation)?;
            let message = invocation_message(&invocation, &record.prompt)?;
            let completion = self.registry.register(record)?;
            let agent = invocation.agent.clone();
            let pending = self
                .registry
                .execute("Ctx", &agent, None, InvocationCommand::Pending)?;
            self.seen
                .lock()
                .unwrap()
                .push((agent.clone(), pending["pending"][0].clone(), message));
            let complete = |keep: &[&str]| {
                self.registry.execute(
                    "Ctx",
                    &agent,
                    None,
                    InvocationCommand::Complete(CompleteArgs {
                        task_id: invocation.id,
                        keep: keep.iter().map(|k| k.to_string()).collect(),
                    }),
                )
            };
            let refused = |result: Result<Value>| {
                let error = result.expect_err("completion should be refused");
                self.refusals.lock().unwrap().push(format!("{error:#}"));
            };
            match agent.as_str() {
                "t.a" => {
                    refused(complete(&[]));
                    self.registry.execute(
                        "Ctx",
                        &agent,
                        None,
                        InvocationCommand::SetState(SetStateArgs {
                            task_id: invocation.id,
                            name: "ops".into(),
                            value: json!("Use SQLite."),
                        }),
                    )?;
                    complete(&[])?;
                }
                "t.b" => {
                    refused(complete(&["style"]));
                    complete(&["ops"])?;
                }
                _ => {
                    refused(complete(&["ops"]));
                    complete(&[])?;
                }
            }
            let writes = completion.try_recv()?;
            self.registry.remove(&invocation.id);
            Ok(writes)
        }
    }

    /// A context reaches the tasks of the agents that share it in the
    /// invocation itself — a `contexts:` header line and `omar_pending` —
    /// and the prompt is the program's own. Completion is refused until each
    /// shared context is set or kept, and the invocation stays open to fix
    /// that; what one writes reaches the next. An agent that does not share
    /// it sees it only as the team's state, and cannot keep it.
    #[test]
    fn a_context_is_handed_to_its_agents_and_kept_by_them() {
        let state = verify(&shared_context_bytecode()).unwrap();
        let keepers = ContextKeepers {
            registry: InvocationRegistry::default(),
            seen: Mutex::new(Vec::new()),
            refusals: Mutex::new(Vec::new()),
        };
        let end = run_event_loop_controlled(
            &state,
            LoopStart::Fresh(BTreeMap::from([("t.go".to_string(), json!(1))])),
            &keepers,
            &NoopTopologyObserver,
            Pace::Fast,
            None,
            Some(&Recorder::new()),
        )
        .unwrap();
        let LoopEnd::Completed(settled) = end else {
            panic!("the run should finish");
        };
        assert_eq!(settled.state_vars["t.ops"], json!("Use SQLite."));
        let seen = keepers.seen.lock().unwrap();
        let [(_, a, a_message), (_, b, b_message), (_, c, c_message)] = &seen[..] else {
            panic!("expected three invocations, got {}", seen.len());
        };
        // The prompt is exactly the program's; the context is data beside it.
        assert_eq!(a["prompt"], json!("Plan."));
        assert_eq!(
            a["contexts"]["t.ops"],
            json!({"directive": "List decisions, newest last.", "value": ""})
        );
        assert!(
            a_message.contains(
                "contexts: {\"t.ops\":{\"directive\":\"List decisions, newest last.\",\"value\":\"\"}}\n"
            ),
            "{a_message}"
        );
        assert!(
            a_message.contains("before omar_complete, set it to its full updated text with omar_set_state, or pass it in omar_complete's keep list"),
            "{a_message}"
        );
        // `b` runs after `a` at the same tag, so it starts from `a`'s note.
        assert_eq!(b["prompt"], json!("Build."));
        assert_eq!(b["contexts"]["t.ops"]["value"], json!("Use SQLite."));
        // Set out on its own line, so not repeated as state.
        for message in [a_message, b_message] {
            assert!(!message.contains("state: "), "{message}");
        }
        // `c` shares no context: nothing to keep, and `ops` is just the
        // team's state, which every task may read.
        assert_eq!(c["prompt"], json!("Review."));
        assert_eq!(c["contexts"], json!({}));
        assert!(!c_message.contains("contexts: "), "{c_message}");
        assert!(
            c_message.contains("state: {\"t.ops\":\"Use SQLite.\"}\n"),
            "{c_message}"
        );
        let refusals = keepers.refusals.lock().unwrap();
        assert_eq!(
            refusals[0],
            "not complete: set `ops` with omar_set_state, or keep it: omar_complete(task_id, keep: [\"ops\"])"
        );
        assert!(
            refusals[1].contains(
                "keep names 'style', which is not a context this task shares; it shares: ops"
            ),
            "{}",
            refusals[1]
        );
        assert!(
            refusals[2].contains("keep names 'ops', but this task shares no context"),
            "{}",
            refusals[2]
        );
    }

    /// A context write over the limit is refused by `omar_set_state` with what
    /// to do about it, whoever writes it; one at the limit is taken.
    #[test]
    fn a_context_write_over_its_limit_is_refused() {
        let registry = InvocationRegistry::default();
        let completion = registry
            .register(InvocationRecord {
                id: 12,
                team: "Ctx".into(),
                agent: "t.c".into(),
                reaction: "t.reaction.2".into(),
                state_types: BTreeMap::from([("t.ops".into(), "string".into())]),
                context_names: BTreeSet::from(["t.ops".to_string()]),
                ..Default::default()
            })
            .unwrap();
        let set = |value: String| {
            registry.execute(
                "Ctx",
                "t.c",
                None,
                InvocationCommand::SetState(SetStateArgs {
                    task_id: 12,
                    name: "ops".into(),
                    value: json!(value),
                }),
            )
        };
        let refused = set("x".repeat(CONTEXT_LIMIT + 1)).unwrap_err();
        assert!(
            format!("{refused:#}").contains("over its 16384-byte limit; condense it"),
            "{refused:#}"
        );
        set("x".repeat(CONTEXT_LIMIT)).unwrap();
        registry
            .execute(
                "Ctx",
                "t.c",
                None,
                InvocationCommand::Complete(CompleteArgs {
                    task_id: 12,
                    keep: vec![],
                }),
            )
            .unwrap();
        assert_eq!(
            completion.try_recv().unwrap()["t.ops"]
                .as_str()
                .unwrap()
                .len(),
            CONTEXT_LIMIT
        );
    }

    /// A context is text, shared by agents of its own instance.
    #[test]
    fn a_context_must_be_text_shared_by_its_own_agents() {
        let with = |declaration: &str| {
            let mut bytecode = shared_context_bytecode();
            let at = bytecode
                .instructions
                .iter()
                .position(|i| matches!(i, Instruction::DeclareState { .. }))
                .unwrap();
            bytecode.instructions[at] = serde_json::from_str(declaration).unwrap();
            verify(&bytecode).map(|_| ()).unwrap_err().to_string()
        };
        assert!(with(r#"{"op":"declare_state","instance":"t","name":"t.ops","type":"int","initial":0,"agents":["t.a"]}"#)
            .contains("a context is a string"));
        assert!(with(r#"{"op":"declare_state","instance":"t","name":"t.ops","type":"string","initial":"","agents":["t.z"]}"#)
            .contains("not an agent of its instance"));
    }

    #[test]
    fn integer_task_ids_are_exact_and_owned_by_the_caller() {
        let registry = InvocationRegistry::default();
        let record = InvocationRecord {
            id: 7,
            team: "Desk".into(),
            agent: "worker".into(),
            reaction: "reaction.0".into(),
            contract: "answer".into(),
            allowed_effects: BTreeMap::from([("answer".into(), "string".into())]),
            trigger_values: BTreeMap::new(),
            prompt: "answer".into(),
            writes: BTreeMap::new(),
            completed: false,
            ..Default::default()
        };
        let completion = registry.register(record.clone()).unwrap();
        let set = |id| {
            InvocationCommand::SetPort(SetPortArgs {
                task_id: id,
                port: "answer".into(),
                value: json!("hello"),
            })
        };
        // Even with only one pending task, an incorrect ID is never guessed.
        assert!(registry.execute("Desk", "worker", None, set(8)).is_err());
        assert!(registry
            .execute(
                "Desk",
                "worker",
                None,
                InvocationCommand::Complete(CompleteArgs {
                    task_id: 8,
                    keep: vec![]
                })
            )
            .is_err());
        // A real ID is not a credential: both commands still check ownership.
        for (team, agent) in [("Desk", "other"), ("Other", "worker")] {
            assert!(registry.execute(team, agent, None, set(7)).is_err());
            assert!(registry
                .execute(
                    team,
                    agent,
                    None,
                    InvocationCommand::Complete(CompleteArgs {
                        task_id: 7,
                        keep: vec![]
                    })
                )
                .is_err());
        }
        let pending = registry
            .execute("Desk", "worker", None, InvocationCommand::Pending)
            .unwrap();
        assert_eq!(pending["pending"][0]["task_id"], json!(7));
        assert!(pending["pending"][0].get("invocation_id").is_none());
        // Multiple pending tasks remain individually addressable.
        let second = registry
            .register(InvocationRecord { id: 8, ..record })
            .unwrap();
        registry.execute("Desk", "worker", None, set(7)).unwrap();
        registry
            .execute(
                "Desk",
                "worker",
                None,
                InvocationCommand::Complete(CompleteArgs {
                    task_id: 7,
                    keep: vec![],
                }),
            )
            .unwrap();
        assert_eq!(completion.try_recv().unwrap()["answer"], json!("hello"));
        assert!(second.try_recv().is_err());
        registry.remove(&7);
        assert!(registry.execute("Desk", "worker", None, set(7)).is_err());
    }

    #[test]
    fn task_ids_are_json_integers_and_not_legacy_strings() {
        let valid = json!({"task_id": 7, "port": "answer", "value": "hello"});
        assert_eq!(
            serde_json::from_value::<SetPortArgs>(valid)
                .unwrap()
                .task_id,
            7
        );
        for id in [json!("7"), json!(7.5), json!(-1), Value::Null] {
            assert!(serde_json::from_value::<SetPortArgs>(
                json!({"task_id": id, "port": "answer", "value": "hello"})
            )
            .is_err());
            assert!(serde_json::from_value::<CompleteArgs>(json!({"task_id": id})).is_err());
        }
        assert!(
            serde_json::from_value::<CompleteArgs>(json!({"invocation_id": "old-uuid"})).is_err()
        );
        let first = next_task_id().unwrap();
        let second = next_task_id().unwrap();
        assert!(first > 0 && second > first && second < (1_u64 << 53));
    }

    #[test]
    fn reactive_scoped_port_writes_are_last_writer_wins() {
        let server = InvocationServer::start().unwrap();
        let context = TopologyMcpContext {
            resume: None,
            team: "Demo".into(),
            agent: "worker".into(),
            endpoint: server.endpoint.clone(),
            token: server.token.clone(),
            attempt: None,
        };
        let record = InvocationRecord {
            id: 1,
            team: "Demo".into(),
            agent: "worker".into(),
            reaction: "reaction.0".into(),
            contract: "opinion".into(),
            allowed_effects: BTreeMap::from([("opinion".into(), "string".into())]),
            trigger_values: BTreeMap::new(),
            prompt: "p".into(),
            writes: BTreeMap::new(),
            completed: false,
            ..Default::default()
        };
        let completion = server.registry.register(record).unwrap();

        for value in ["first", "second"] {
            mcp_set_port(
                &context,
                json!({"task_id":1, "port":"opinion", "value":value}),
            )
            .unwrap();
        }
        mcp_complete(&context, json!({"task_id":1})).unwrap();

        let writes = completion.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(writes["opinion"], json!("second"));
        server.registry.remove(&1);
    }

    #[test]
    fn a_rejected_refinement_can_be_corrected_over_the_invocation_protocol() {
        let server = InvocationServer::start().unwrap();
        let context = TopologyMcpContext {
            resume: None,
            team: "Recovery".into(),
            agent: "worker".into(),
            endpoint: server.endpoint.clone(),
            token: server.token.clone(),
            attempt: None,
        };
        let completion = server
            .registry
            .register(InvocationRecord {
                id: 4,
                team: "Recovery".into(),
                agent: "worker".into(),
                reaction: "reaction.0".into(),
                contract: "decision".into(),
                allowed_effects: BTreeMap::from([(
                    "decision".into(),
                    r#"string in ["continue","stop"]"#.into(),
                )]),
                trigger_values: BTreeMap::new(),
                prompt: "Choose a permitted decision.".into(),
                writes: BTreeMap::new(),
                completed: false,
                ..Default::default()
            })
            .unwrap();

        // Exercise the real socket path, not just validate_value.
        let error = mcp_set_port(
            &context,
            json!({
                "task_id": 4, "port": "decision",
                "value": "this is a terminal record, not a forward"
            }),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("decision"), "{error}");
        assert!(error.contains("expected one of"), "{error}");
        assert!(error.contains("\"continue\""), "{error}");
        assert!(error.contains("\"stop\""), "{error}");

        // Rejection must not satisfy the required output or release a result.
        let error = mcp_complete(&context, json!({"task_id": 4}))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("effect contract 'decision' is not satisfied"),
            "{error}"
        );
        assert!(matches!(
            completion.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(!server.registry.answered(&4));

        let response = mcp_set_port(
            &context,
            json!({
                "task_id": 4, "port": "decision", "value": "continue"
            }),
        )
        .unwrap();
        assert_eq!(response["status"], json!("buffered"));
        assert!(matches!(
            completion.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        let response = mcp_complete(&context, json!({"task_id": 4})).unwrap();
        assert_eq!(response["status"], json!("complete"));
        assert_eq!(
            completion.recv_timeout(Duration::from_secs(1)).unwrap(),
            BTreeMap::from([("decision".into(), json!("continue"))])
        );
        assert!(server.registry.answered(&4));
        server.registry.remove(&4);
    }

    #[test]
    fn reactive_invocations_enforce_owner_and_contract() {
        let server = InvocationServer::start().unwrap();
        let context = TopologyMcpContext {
            resume: None,
            team: "Demo".into(),
            agent: "worker".into(),
            endpoint: server.endpoint.clone(),
            token: server.token.clone(),
            attempt: None,
        };
        let record = InvocationRecord {
            id: 2,
            team: "Demo".into(),
            agent: "worker".into(),
            reaction: "reaction.0".into(),
            contract: "opinion".into(),
            allowed_effects: BTreeMap::from([("opinion".into(), "string".into())]),
            trigger_values: BTreeMap::new(),
            prompt: "p".into(),
            writes: BTreeMap::new(),
            completed: false,
            ..Default::default()
        };
        let completion = server.registry.register(record).unwrap();

        let mut wrong_owner = context.clone();
        wrong_owner.agent = "intruder".into();
        let owner_error = mcp_set_port(
            &wrong_owner,
            json!({"task_id":2, "port":"opinion", "value":"no"}),
        )
        .unwrap_err();
        assert!(owner_error.to_string().contains("does not belong"));

        let contract_error = mcp_complete(&context, json!({"task_id":2})).unwrap_err();
        assert!(
            format!("{contract_error:#}").contains("effect contract 'opinion' is not satisfied")
        );

        mcp_set_port(
            &context,
            json!({"task_id":2, "port":"opinion", "value":"yes"}),
        )
        .unwrap();
        mcp_complete(&context, json!({"task_id":2})).unwrap();
        assert_eq!(
            completion.recv_timeout(Duration::from_secs(1)).unwrap(),
            BTreeMap::from([("opinion".into(), json!("yes"))])
        );
        server.registry.remove(&2);
    }

    /// Answers its invocation, and files a stop request while doing so.
    struct StopWhileAnsweringExecutor {
        dir: std::path::PathBuf,
        calls: Mutex<usize>,
    }

    impl ReactionExecutor for StopWhileAnsweringExecutor {
        fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
            *self.calls.lock().unwrap() += 1;
            // Filed once; a second invocation finds it pending and is refused,
            // which is the rule, not a failure of this executor.
            let _ = crate::deploy::request_stop(&self.dir);
            let port = invocation.allowed_effects.keys().next().unwrap().clone();
            Ok(BTreeMap::from([(port, json!("ping"))]))
        }
    }

    /// `a` fires `reaction.0` at one tag; its action effect schedules
    /// `reaction.1` a microstep later, so the run crosses a tag boundary.
    fn two_tag_bytecode() -> Bytecode {
        serde_json::from_str(
            r#"{
              "version": 1,
              "team": "Boundary",
              "instructions": [
                {"op":"begin_plan","team":"Boundary"},
                {"op":"spawn_agent","name":"worker","backend":"stub"},
                {"op":"define_port","kind":"input","name":"a","type":"string"},
                {"op":"define_port","kind":"action","name":"x","type":"string"},
                {"op":"define_port","kind":"output","name":"out","type":"string"},
                {"op":"install_reaction","id":"reaction.0","agent":"worker","triggers":["a"],"effects":["x"],"contract":"x","prompt":"First"},
                {"op":"install_reaction","id":"reaction.1","agent":"worker","triggers":["x"],"effects":["out"],"contract":"out","prompt":"Second"},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn a_stop_request_ends_the_run_at_the_next_tag_boundary() {
        let state = verify(&two_tag_bytecode()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let executor = StopWhileAnsweringExecutor {
            dir: dir.path().to_path_buf(),
            calls: Mutex::new(0),
        };
        let inputs = BTreeMap::from([("a".to_string(), json!("go"))]);
        let end = run_event_loop_observed(
            &state,
            inputs.clone(),
            &executor,
            &NoopTopologyObserver,
            Pace::Fast,
            Some(dir.path()),
        )
        .unwrap();
        assert!(matches!(end, LoopEnd::Stopped(_)));
        assert_eq!(*executor.calls.lock().unwrap(), 1);

        // The same program drains both tags when nothing watches for a stop.
        let unwatched = tempfile::tempdir().unwrap();
        let executor = StopWhileAnsweringExecutor {
            dir: unwatched.path().to_path_buf(),
            calls: Mutex::new(0),
        };
        let end = run_event_loop_observed(
            &state,
            inputs,
            &executor,
            &NoopTopologyObserver,
            Pace::Fast,
            None,
        )
        .unwrap();
        match end {
            LoopEnd::Completed(settled) => {
                assert_eq!(settled.outputs.get("out"), Some(&json!("ping")))
            }
            LoopEnd::Stopped(_) | LoopEnd::Paused(_) => panic!("nothing requested a stop"),
        }
        assert_eq!(*executor.calls.lock().unwrap(), 2);
    }

    /// A program of one instance whose reaction keeps a count in state. It
    /// re-triggers itself through an action until the count reaches three.
    fn counter_bytecode(extra: &str) -> Bytecode {
        serde_json::from_str(&format!(
            r#"{{
              "version": 1,
              "team": "Counter",
              "instructions": [
                {{"op":"begin_plan","team":"Counter"}},
                {{"op":"declare_instance","name":"c","parent":"","team":"Counter"}},
                {{"op":"define_port","instance":"c","kind":"input","name":"c.tick","type":"int"}},
                {{"op":"define_port","instance":"c","kind":"action","name":"c.again","type":"int"}},
                {{"op":"define_port","instance":"c","kind":"output","name":"c.total","type":"int"}},
                {{"op":"declare_state","instance":"c","name":"c.count","type":"int","initial":0}},
                {{"op":"install_reaction","instance":"c","id":"c.reaction.0","agent":"",
                  "triggers":["c.tick","c.again"],"effects":["c.total","c.again"],
                  "contract":"c.total , c.again ?","prompt":"","body":"unused"}},
                {extra}
                {{"op":"commit_plan"}}
              ]
            }}"#
        ))
        .unwrap()
    }

    /// Counts invocations; pauses the run from inside the first one, the way
    /// an operator's request lands while a tag is in flight.
    struct PausingCounter {
        calls: Mutex<usize>,
        dir: Option<PathBuf>,
    }
    impl ReactionExecutor for PausingCounter {
        fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
            let calls = {
                let mut calls = self.calls.lock().unwrap();
                *calls += 1;
                *calls
            };
            if calls == 1 {
                if let Some(dir) = &self.dir {
                    deploy::request(dir, ControlOp::Pause).unwrap();
                }
            }
            let count = invocation.state_values["c.count"].as_i64().unwrap() + 1;
            let mut writes = BTreeMap::from([
                ("c.count".to_string(), json!(count)),
                ("c.total".to_string(), json!(count * 10)),
            ]);
            if count < 3 {
                writes.insert("c.again".to_string(), json!(count));
            }
            Ok(writes)
        }
    }

    /// Remembers what it captured; never due on its own unless told.
    struct Recorder {
        captures: Mutex<Vec<(Trigger, ExecutionState)>>,
        due_once: Mutex<bool>,
        fail_first: Mutex<u32>,
        failures: Mutex<u32>,
        expect_hold: Mutex<bool>,
    }
    impl Recorder {
        fn new() -> Self {
            Self {
                captures: Mutex::new(Vec::new()),
                due_once: Mutex::new(false),
                fail_first: Mutex::new(0),
                failures: Mutex::new(0),
                expect_hold: Mutex::new(false),
            }
        }
    }
    impl Checkpointer for Recorder {
        fn due(&self, _state: &ExecutionState) -> bool {
            std::mem::take(&mut *self.due_once.lock().unwrap())
        }
        fn capture(&self, state: &ExecutionState, trigger: Trigger) -> Result<String> {
            let mut fail = self.fail_first.lock().unwrap();
            if *fail > 0 {
                *fail -= 1;
                bail!("disk full");
            }
            self.captures.lock().unwrap().push((trigger, state.clone()));
            Ok(format!("cp-{}", self.captures.lock().unwrap().len()))
        }
        fn capture_failed(&self, _error: &anyhow::Error, _attempt: u32, held: bool) {
            assert!(
                !held || *self.expect_hold.lock().unwrap(),
                "one failure is retried internally, never held"
            );
            *self.failures.lock().unwrap() += 1;
        }
    }

    /// A pause at a tag boundary captures the run exactly, and a loop resumed
    /// from that capture runs only what was left: same final outputs as an
    /// uninterrupted run, no invocation repeated, none lost.
    #[test]
    fn a_resumed_loop_continues_exactly_where_the_pause_left_it() {
        let state = verify(&counter_bytecode("")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let first = PausingCounter {
            calls: Mutex::new(0),
            dir: Some(dir.path().to_path_buf()),
        };
        let recorder = Recorder::new();
        let end = run_event_loop_controlled(
            &state,
            LoopStart::Fresh(BTreeMap::from([("c.tick".to_string(), json!(1))])),
            &first,
            &NoopTopologyObserver,
            Pace::Fast,
            Some(dir.path()),
            Some(&recorder),
        )
        .unwrap();
        let LoopEnd::Paused(captured) = end else {
            panic!("the pause request was not honoured");
        };
        assert_eq!(
            *first.calls.lock().unwrap(),
            1,
            "paused after the first tag"
        );
        assert_eq!(captured.completed_tag, Some((0, 0)));
        assert_eq!(captured.state_vars["c.count"], json!(1));
        assert_eq!(captured.outputs["c.total"], json!(10));
        // The re-trigger through the action is queued, not lost: it is the
        // whole of the run's remaining work.
        assert_eq!(captured.queue.len(), 1);
        assert_eq!(captured.next_tag(), Some((0, 1)));
        assert!(captured.queue[0].events.contains_key("c.again"));
        let captures = recorder.captures.lock().unwrap();
        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0].0, Trigger::Pause);
        assert_eq!(captures[0].1, captured);
        drop(captures);

        // The operator's request is consumed by the runner that paused; a
        // fresh process continues from the capture alone.
        deploy::clear_stop(dir.path()).unwrap();
        let second = PausingCounter {
            calls: Mutex::new(0),
            dir: None,
        };
        let end = run_event_loop_controlled(
            &state,
            LoopStart::Resume(captured),
            &second,
            &NoopTopologyObserver,
            Pace::Fast,
            Some(dir.path()),
            Some(&Recorder::new()),
        )
        .unwrap();
        let LoopEnd::Completed(settled) = end else {
            panic!("the resumed run should drain its queue");
        };
        assert_eq!(settled.outputs["c.total"], json!(30));
        assert_eq!(settled.state_vars["c.count"], json!(3));
        assert_eq!(*second.calls.lock().unwrap(), 2, "two tags were left");
    }

    /// A timer's next firings travel in the queue, so a resumed run picks up
    /// the cadence without re-arming from the declaration or firing a backlog.
    #[test]
    fn a_resumed_timer_keeps_its_cadence_and_repeats_nothing() {
        struct Ticks {
            fired: Mutex<Vec<u64>>,
            dir: PathBuf,
            pause_at: usize,
            stop_at: usize,
        }
        impl ReactionExecutor for Ticks {
            fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
                let at = invocation.trigger_values["t"].as_u64().unwrap();
                let count = {
                    let mut fired = self.fired.lock().unwrap();
                    fired.push(at);
                    fired.len()
                };
                if count == self.pause_at {
                    deploy::request(&self.dir, ControlOp::Pause).unwrap();
                }
                if count == self.stop_at {
                    deploy::request_stop(&self.dir).unwrap();
                }
                Ok(BTreeMap::from([("note".into(), json!("tick"))]))
            }
        }
        let state = verify(&timer_bytecode(5, 10)).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ticks = Ticks {
            fired: Mutex::new(Vec::new()),
            dir: dir.path().to_path_buf(),
            pause_at: 2,
            stop_at: 0,
        };
        let end = run_event_loop_controlled(
            &state,
            LoopStart::Fresh(BTreeMap::new()),
            &ticks,
            &NoopTopologyObserver,
            Pace::Fast,
            Some(dir.path()),
            Some(&Recorder::new()),
        )
        .unwrap();
        let LoopEnd::Paused(captured) = end else {
            panic!("expected a pause");
        };
        assert_eq!(*ticks.fired.lock().unwrap(), vec![5, 15]);
        assert_eq!(captured.completed_tag, Some((15, 0)));
        assert_eq!(
            captured.next_tag(),
            Some((25, 0)),
            "the next firing is queued"
        );
        deploy::clear_stop(dir.path()).unwrap();
        let later = Ticks {
            fired: Mutex::new(Vec::new()),
            dir: dir.path().to_path_buf(),
            pause_at: 0,
            stop_at: 2,
        };
        let end = run_event_loop_controlled(
            &state,
            LoopStart::Resume(captured),
            &later,
            &NoopTopologyObserver,
            Pace::Fast,
            Some(dir.path()),
            Some(&Recorder::new()),
        )
        .unwrap();
        assert!(matches!(end, LoopEnd::Stopped(_)));
        assert_eq!(*later.fired.lock().unwrap(), vec![25, 35]);
        deploy::clear_stop(dir.path()).unwrap();
    }

    /// An automatic deadline and a manual request both capture at a boundary
    /// and let the run go on; a capture that fails is retried, and the
    /// manual request file is cleared once answered.
    #[test]
    fn captures_at_boundaries_do_not_interrupt_the_run_and_failures_are_retried() {
        let state = verify(&counter_bytecode("")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let recorder = Recorder::new();
        *recorder.due_once.lock().unwrap() = true;
        *recorder.fail_first.lock().unwrap() = 1;
        deploy::request(dir.path(), ControlOp::Checkpoint).unwrap();
        let executor = PausingCounter {
            calls: Mutex::new(0),
            dir: None,
        };
        let end = run_event_loop_controlled(
            &state,
            LoopStart::Fresh(BTreeMap::from([("c.tick".to_string(), json!(1))])),
            &executor,
            &NoopTopologyObserver,
            Pace::Fast,
            Some(dir.path()),
            Some(&recorder),
        )
        .unwrap();
        assert!(matches!(end, LoopEnd::Completed(_)));
        assert_eq!(
            *executor.calls.lock().unwrap(),
            3,
            "the run went on to its end"
        );
        let captures = recorder.captures.lock().unwrap();
        // The manual request was answered at the first boundary, before any
        // tag; the automatic deadline fell due at the next one.
        assert_eq!(captures[0].0, Trigger::Manual);
        assert_eq!(captures[0].1.completed_tag, None);
        assert_eq!(captures[0].1.next_tag(), Some((0, 0)));
        assert_eq!(captures[1].0, Trigger::Automatic);
        assert_eq!(captures[1].1.completed_tag, Some((0, 0)));
        assert_eq!(captures.len(), 2);
        assert_eq!(
            *recorder.failures.lock().unwrap(),
            1,
            "one failed attempt, then success"
        );
        assert_eq!(
            deploy::pending_request(dir.path()),
            None,
            "answered requests are cleared"
        );
    }

    /// A cadence decides at a boundary: physical by the clock, every tag by a
    /// new completed tag, logical by the tags crossing a period; and never
    /// twice for one tag, which a wait revisits every slice.
    #[test]
    fn cadences_fall_due_on_the_clock_on_every_tag_or_on_logical_periods() {
        let at = |tag: Option<(u64, u64)>| ExecutionState {
            version: checkpoint::FORMAT,
            completed_tag: tag,
            queue: Vec::new(),
            outputs: BTreeMap::new(),
            state_vars: BTreeMap::new(),
            elapsed_ns: 0,
        };
        let now = Instant::now();
        let mut schedule = Schedule {
            policy: checkpoint::Policy::OFF,
            next_due: None,
            last_captured_tag: None,
            sequence: 1,
            parent: None,
        };
        assert!(!schedule.due(now, &at(Some((5, 0)))), "off never falls due");
        schedule.next_due = Some(now + Duration::from_secs(60));
        assert!(!schedule.due(now, &at(None)));
        assert!(
            schedule.due(now + Duration::from_secs(61), &at(None)),
            "physical: by the clock"
        );
        schedule.next_due = None;
        schedule.policy.every_tag = true;
        assert!(!schedule.due(now, &at(None)), "no tag completed yet");
        assert!(schedule.due(now, &at(Some((1, 0)))));
        schedule.last_captured_tag = Some((1, 0));
        assert!(
            !schedule.due(now, &at(Some((1, 0)))),
            "a boundary revisited mid-wait"
        );
        assert!(
            schedule.due(now, &at(Some((1, 1)))),
            "a microstep is a tag too"
        );
        schedule.policy = checkpoint::Policy {
            logical_period_ns: 10,
            ..checkpoint::Policy::OFF
        };
        schedule.last_captured_tag = None;
        assert!(!schedule.due(now, &at(Some((9, 0)))));
        assert!(
            schedule.due(now, &at(Some((10, 0)))),
            "first period crossed"
        );
        schedule.last_captured_tag = Some((10, 0));
        assert!(!schedule.due(now, &at(Some((19, 3)))));
        assert!(
            schedule.due(now, &at(Some((25, 0)))),
            "a later period, however far"
        );
        schedule.last_captured_tag = Some((25, 0));
        assert!(!schedule.due(now, &at(Some((29, 0)))));
    }

    /// Once its retries are spent a capture holds the run, and only a new
    /// request moves it: the one that asked for the capture is consumed, so
    /// the hold neither answers it again nor blocks the retry.
    #[test]
    fn a_held_capture_waits_for_a_fresh_retry_request() {
        let state = verify(&counter_bytecode("")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let recorder = Recorder::new();
        *recorder.fail_first.lock().unwrap() = CAPTURE_RETRIES + 1;
        *recorder.expect_hold.lock().unwrap() = true;
        deploy::request(dir.path(), ControlOp::Checkpoint).unwrap();
        let path = dir.path().to_path_buf();
        let operator = thread::spawn(move || {
            // The hold consumed the manual request; a retry is admitted.
            let deadline = Instant::now() + Duration::from_secs(30);
            while deploy::pending_request(&path).is_some() {
                assert!(Instant::now() < deadline, "the request was never consumed");
                thread::sleep(Duration::from_millis(20));
            }
            deploy::request(&path, ControlOp::RetryCheckpoint).unwrap();
        });
        let executor = PausingCounter {
            calls: Mutex::new(0),
            dir: None,
        };
        let end = run_event_loop_controlled(
            &state,
            LoopStart::Fresh(BTreeMap::from([("c.tick".to_string(), json!(1))])),
            &executor,
            &NoopTopologyObserver,
            Pace::Fast,
            Some(dir.path()),
            Some(&recorder),
        )
        .unwrap();
        operator.join().unwrap();
        assert!(matches!(end, LoopEnd::Completed(_)));
        assert_eq!(*recorder.failures.lock().unwrap(), CAPTURE_RETRIES + 1);
        let captures = recorder.captures.lock().unwrap();
        assert_eq!(captures.len(), 1, "the retry captured once");
        assert_eq!(
            captures[0].0,
            Trigger::Manual,
            "the original trigger survives the hold"
        );
        assert_eq!(deploy::pending_request(dir.path()), None);
    }

    /// State a reaction hands back stays with its instance: it reaches the
    /// next invocation, never a port, and its last value leaves with the run.
    #[test]
    fn state_rides_from_one_invocation_to_the_next() {
        struct Counting;
        impl ReactionExecutor for Counting {
            fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
                let count = invocation.state_values["c.count"].as_i64().unwrap() + 1;
                let mut writes = BTreeMap::from([
                    ("c.count".to_string(), json!(count)),
                    ("c.total".to_string(), json!(count * 10)),
                ]);
                if count < 3 {
                    writes.insert("c.again".to_string(), json!(count));
                }
                Ok(writes)
            }
        }
        let state = verify(&counter_bytecode("")).unwrap();
        let end = run_event_loop_observed(
            &state,
            BTreeMap::from([("c.tick".to_string(), json!(1))]),
            &Counting,
            &NoopTopologyObserver,
            Pace::Fast,
            None,
        )
        .unwrap();
        let LoopEnd::Completed(settled) = end else {
            panic!("nothing requested a stop");
        };
        assert_eq!(
            settled.outputs,
            BTreeMap::from([("c.total".to_string(), json!(30))])
        );
        assert_eq!(
            settled.state_vars,
            BTreeMap::from([("c.count".to_string(), json!(3))])
        );
    }

    /// Both stateful and stateless reactions share their instance's workspace.
    #[test]
    fn reactions_of_an_instance_run_in_definition_order() {
        let second = r#"{"op":"define_port","instance":"c","kind":"output","name":"c.other","type":"int"},
            {"op":"install_reaction","instance":"c","id":"c.reaction.1","agent":"",
             "triggers":["c.tick"],"effects":["c.other"],"contract":"c.other","prompt":"","body":"unused"},"#;
        let stateful = verify(&counter_bytecode(second)).unwrap();
        assert_eq!(
            must_follow(&stateful, "c.reaction.0"),
            BTreeSet::from(["c.reaction.1".to_string()])
        );
        assert!(must_follow(&stateful, "c.reaction.1").is_empty());

        let mut bytecode = counter_bytecode(second);
        bytecode
            .instructions
            .retain(|i| !matches!(i, Instruction::DeclareState { .. }));
        let stateless = verify(&bytecode).unwrap();
        assert!(must_follow(&stateless, "c.reaction.0").contains("c.reaction.1"));
    }

    #[test]
    fn verify_checks_a_state_declaration() {
        let broken = |line: &str| {
            let mut bytecode = counter_bytecode("");
            let declared = bytecode
                .instructions
                .iter()
                .position(|i| matches!(i, Instruction::DeclareState { .. }))
                .unwrap();
            bytecode.instructions[declared] = serde_json::from_str(line).unwrap();
            verify(&bytecode).unwrap_err().to_string()
        };
        assert!(broken(
            r#"{"op":"declare_state","instance":"c","name":"c.count","type":"int","initial":"0"}"#
        )
        .contains("starts as"));
        assert!(broken(
            r#"{"op":"declare_state","instance":"c","name":"c.tick","type":"int","initial":0}"#
        )
        .contains("is also a port"));
        assert!(broken(
            r#"{"op":"declare_state","instance":"c","name":"c.count","type":"float","initial":0}"#
        )
        .contains("unsupported type"));
    }

    /// Just after the last state declaration, so an inserted declaration is
    /// part of the plan rather than trailing its commit.
    fn declaration_point(bytecode: &Bytecode) -> usize {
        bytecode
            .instructions
            .iter()
            .rposition(|i| matches!(i, Instruction::DeclareState { .. }))
            .map(|at| at + 1)
            .expect("the counter declares state")
    }

    /// A body binds a local per name it reaches, so a type the generator
    /// cannot carry, or a name Rust has taken, is an error about the program.
    /// Caught here a bad program is refused when it is read; caught by cargo it
    /// would be admitted and then fail a run.
    #[test]
    fn verify_refuses_a_body_naming_what_rust_cannot_bind() {
        let with_port = |kind: &str, name: &str, ty: &str| {
            let mut bytecode = counter_bytecode("");
            let at = declaration_point(&bytecode);
            bytecode.instructions.insert(
                at,
                serde_json::from_str(&format!(
                    r#"{{"op":"define_port","instance":"c","kind":"{kind}","name":"{name}","type":"{ty}"}}"#
                ))
                .unwrap(),
            );
            for instruction in &mut bytecode.instructions {
                if let Instruction::InstallReaction { triggers, .. } = instruction {
                    triggers.push(name.to_string());
                }
            }
            verify(&bytecode).unwrap_err().to_string()
        };
        // An untyped action is a signal, which no Rust local can hold.
        assert!(with_port("action", "c.go", "signal").contains("a body carries int"));
        // `type` is a name Rust has taken, and the body did not write `r#type`.
        assert!(with_port("input", "c.type", "int").contains("Rust keyword"));
        // `t` is the wire map every getter reads, and `None` is what an effect
        // starts as; a local of either name would shadow them under the body.
        assert!(with_port("input", "c.t", "int").contains("generated crate uses"));
        assert!(with_port("input", "c.None", "int").contains("generated crate uses"));
        // `_` discards rather than names, so it cannot be read or written.
        assert!(with_port("input", "c._", "int").contains("discards a value"));
    }

    /// A timer trigger becomes a local like any other, so its name has to be
    /// one Rust will take — the type never varies, but the name does.
    #[test]
    fn verify_refuses_a_body_triggered_by_a_timer_rust_has_named() {
        let mut bytecode = counter_bytecode("");
        let at = declaration_point(&bytecode);
        bytecode.instructions.insert(
            at,
            serde_json::from_str(
                r#"{"op":"declare_timer","instance":"c","name":"c.match","offset":1,"period":0}"#,
            )
            .unwrap(),
        );
        for instruction in &mut bytecode.instructions {
            if let Instruction::InstallReaction { triggers, .. } = instruction {
                triggers.push("c.match".to_string());
            }
        }

        assert!(verify(&bytecode)
            .unwrap_err()
            .to_string()
            .contains("Rust keyword"));
    }

    /// A claim is created and then filled in, so a reader can find it empty
    /// while the run that made it is still writing. Taking it over then would
    /// let two programs compile into one directory, which is the thing the
    /// claim exists to stop.
    #[test]
    fn a_claim_still_being_written_is_waited_for_not_taken() {
        let dir = tempfile::tempdir().unwrap();
        let claim = dir.path().join(".source");
        // Created, as the winner creates it, but not yet written.
        fs::write(&claim, "").unwrap();

        let writing = claim.clone();
        let winner = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            fs::write(&writing, "/somewhere/else/foo.omar").unwrap();
        });
        let held = read_claim(&claim);
        winner.join().unwrap();

        assert_eq!(held, "/somewhere/else/foo.omar");
    }

    /// Each declaration checks the namespaces already filled in, which is
    /// enough only for the order the compiler happens to emit. The VM's
    /// namespace is flat, so a collision is a collision whichever came first.
    #[test]
    fn verify_refuses_one_name_declared_twice_in_any_order() {
        let collide = |first: &str, second: &str| {
            let mut bytecode = counter_bytecode("");
            let at = declaration_point(&bytecode);
            // Inserted in this order, so each arrives before the other's check.
            bytecode
                .instructions
                .insert(at, serde_json::from_str(second).unwrap());
            bytecode
                .instructions
                .insert(at, serde_json::from_str(first).unwrap());
            verify(&bytecode).unwrap_err().to_string()
        };
        let param =
            r#"{"op":"declare_param","instance":"c","name":"c.dup","type":"int","value":1}"#;
        let state =
            r#"{"op":"declare_state","instance":"c","name":"c.dup","type":"int","initial":0}"#;

        // The order the compiler emits: the parameter checks state and catches it.
        assert!(collide(state, param).contains("is also a port, timer or state"));
        // The order it does not: state checks ports and timers, and not params.
        assert!(collide(param, state).contains("a name means one thing"));
    }

    /// A parameter is a constant the instantiation chose, so the VM carries it
    /// per instance rather than letting the compiler paste it into the body.
    #[test]
    fn a_parameter_belongs_to_the_instance_that_was_given_it() {
        let mut bytecode = counter_bytecode("");
        let at = declaration_point(&bytecode);
        bytecode.instructions.insert(
            at,
            serde_json::from_str(
                r#"{"op":"declare_param","instance":"c","name":"c.idx","type":"int","value":7}"#,
            )
            .unwrap(),
        );
        let state = verify(&bytecode).unwrap();

        let param = state.params.get("c.idx").expect("the parameter is carried");
        assert_eq!(param.value, json!(7));
        assert_eq!(param.instance, "c");
    }

    #[test]
    fn verify_checks_a_parameter_declaration() {
        let broken = |line: &str| {
            let mut bytecode = counter_bytecode("");
            let at = declaration_point(&bytecode);
            bytecode
                .instructions
                .insert(at, serde_json::from_str(line).unwrap());
            verify(&bytecode).unwrap_err().to_string()
        };
        // A body names a parameter the way it names a port or its state, so
        // the name has to mean one thing.
        assert!(broken(
            r#"{"op":"declare_param","instance":"c","name":"c.tick","type":"int","value":1}"#
        )
        .contains("is also a port"));
        assert!(broken(
            r#"{"op":"declare_param","instance":"c","name":"c.count","type":"int","value":1}"#
        )
        .contains("is also a port"));
        assert!(broken(
            r#"{"op":"declare_param","instance":"c","name":"c.idx","type":"int","value":"one"}"#
        )
        .contains("was given"));
    }

    /// A body answers for itself, so naming an agent or a prompt beside one
    /// says two things at once and the VM refuses to pick.
    #[test]
    fn verify_refuses_a_body_that_also_names_an_agent_or_prompt() {
        let with = |field: &str, value: &str| {
            let mut bytecode = counter_bytecode("");
            let installed = bytecode
                .instructions
                .iter()
                .position(|i| matches!(i, Instruction::InstallReaction { .. }))
                .unwrap();
            let Instruction::InstallReaction { agent, prompt, .. } =
                &mut bytecode.instructions[installed]
            else {
                unreachable!("the position above matched one");
            };
            match field {
                "agent" => *agent = value.to_string(),
                _ => *prompt = value.to_string(),
            }
            verify(&bytecode).unwrap_err().to_string()
        };
        assert!(with("agent", "writer").contains("names no agent or prompt"));
        assert!(with("prompt", "decide").contains("names no agent or prompt"));

        // A deadline is not one of the exclusive fields: a body may bound its
        // own worst case the way a prompt does.
        let mut bounded = counter_bytecode("");
        let installed = bounded
            .instructions
            .iter()
            .position(|i| matches!(i, Instruction::InstallReaction { .. }))
            .unwrap();
        if let Instruction::InstallReaction { within, .. } = &mut bounded.instructions[installed] {
            *within = Some(5_000_000_000);
        }
        let state = verify(&bounded).unwrap();
        assert_eq!(state.reactions["c.reaction.0"].within, Some(5_000_000_000));
    }

    /// Codegen binds only the invoking instance's variables, so this refuses
    /// what only a binary the VM did not write could send.
    #[test]
    fn a_body_may_not_write_another_instances_state() {
        let second = r#"{"op":"declare_instance","name":"d","parent":"","team":"Counter"},
            {"op":"declare_state","instance":"d","name":"d.count","type":"int","initial":0},"#;
        let state = verify(&counter_bytecode(second)).unwrap();
        let entry = state.reactions.get_key_value("c.reaction.0").unwrap();
        let spec = invocation_spec(
            &state,
            entry,
            &BTreeMap::from([("c.tick".to_string(), json!(1))]),
            &BTreeMap::from([
                ("c.count".to_string(), json!(0)),
                ("d.count".to_string(), json!(0)),
            ]),
            Tag::default(),
        )
        .unwrap();
        assert_eq!(spec.state_values.keys().collect::<Vec<_>>(), ["c.count"]);

        let own = BTreeMap::from([("c.count".to_string(), json!(1))]);
        assert!(state_writes_stay_in_instance(&state, &spec, &own).is_ok());

        let reached = BTreeMap::from([("d.count".to_string(), json!(1))]);
        let error = state_writes_stay_in_instance(&state, &spec, &reached)
            .unwrap_err()
            .to_string();
        assert!(error.contains("outside its instance"), "{error}");
        assert!(error.contains("d.count"), "{error}");
    }

    /// A body that sleeps past its own deadline, so the wait has to end it.
    fn slow_bytecode(contract: &str) -> Bytecode {
        serde_json::from_str(&format!(
            r#"{{
              "version": 1,
              "team": "Slow",
              "instructions": [
                {{"op":"begin_plan","team":"Slow"}},
                {{"op":"declare_instance","name":"s","parent":"","team":"Slow"}},
                {{"op":"define_port","instance":"s","kind":"input","name":"s.tick","type":"int"}},
                {{"op":"define_port","instance":"s","kind":"output","name":"s.out","type":"int"}},
                {{"op":"install_reaction","instance":"s","id":"s.reaction.0","agent":"",
                  "triggers":["s.tick"],"effects":["s.out"],"contract":"{contract}","prompt":"",
                  "body":"std::thread::sleep(std::time::Duration::from_secs(30)); out = Some(1);",
                  "within":200000000}},
                {{"op":"commit_plan"}}
              ]
            }}"#
        ))
        .unwrap()
    }

    /// The smoke-test shape: a body records what it saw and asks to stop, and
    /// a `shutdown` body checks the record. A failed check fails the run.
    #[test]
    #[ignore = "shells out to cargo; run with --ignored"]
    fn a_shutdown_body_decides_whether_the_run_passed() {
        struct Unused;
        impl ReactionExecutor for Unused {
            fn invoke(&self, _: InvocationSpec) -> Result<BTreeMap<String, Value>> {
                panic!("every reaction has a body, so no agent is asked");
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let run = |expected: i64| {
            let bytecode: Bytecode = serde_json::from_str(&format!(
                r#"{{
                  "version": 1,
                  "team": "Smoke",
                  "instructions": [
                    {{"op":"begin_plan","team":"Smoke"}},
                    {{"op":"declare_instance","name":"s","parent":"","team":"Smoke"}},
                    {{"op":"declare_timer","instance":"s","name":"s.tick","offset":1,"period":1}},
                    {{"op":"declare_timer","instance":"s","name":"s.shutdown","offset":0,"period":0,"shutdown":true}},
                    {{"op":"declare_state","instance":"s","name":"s.ticks","type":"int","initial":0}},
                    {{"op":"install_reaction","instance":"s","id":"s.reaction.0","agent":"",
                      "triggers":["s.tick"],"effects":[],"contract":"","prompt":"",
                      "body":"self.ticks += 1; if self.ticks == 3 {{ request_stop(); }}"}},
                    {{"op":"install_reaction","instance":"s","id":"s.reaction.1","agent":"",
                      "triggers":["s.shutdown"],"effects":[],"contract":"","prompt":"",
                      "body":"assert_eq!(self.ticks, {expected}, \"ticks before shutdown\");"}},
                    {{"op":"commit_plan"}}
                  ]
                }}"#
            ))
            .unwrap();
            let state = verify(&bytecode).unwrap();
            let code = crate::reaction::build(&state, dir.path()).unwrap().unwrap();
            let executor = DispatchExecutor {
                workspace_dirs: BTreeMap::new(),
                state: &state,
                code: Some(code),
                timeout: Duration::from_secs(30),
                run_started: Instant::now(),
                agents: Unused,
            };
            run_event_loop(&state, BTreeMap::new(), &executor)
        };

        // The periodic timer would tick forever; the request ends it at three.
        run(3).unwrap();
        let error = format!("{:#}", run(4).unwrap_err());
        assert!(error.contains("ticks before shutdown"), "{error}");
    }

    /// The generated directory belongs to the program, so two runs of it share
    /// one `main.rs` and one cargo target. If they build at once, neither may
    /// end up publishing the other's build under its own source's name.
    #[test]
    #[ignore = "shells out to cargo; run with --ignored"]
    fn two_runs_building_at_once_each_get_their_own_body() {
        let dir = tempfile::tempdir().unwrap();
        let bodied = |body: &str| {
            let mut bytecode = counter_bytecode("");
            for instruction in &mut bytecode.instructions {
                if let Instruction::InstallReaction {
                    body: source,
                    triggers,
                    effects,
                    contract,
                    ..
                } = instruction
                {
                    *source = Some(body.to_string());
                    *triggers = vec!["c.tick".to_string()];
                    *effects = vec!["c.total".to_string()];
                    *contract = "c.total".to_string();
                }
            }
            verify(&bytecode).unwrap()
        };
        // Two different sources, so two different binaries.
        let one = bodied("total = Some(1);");
        let two = bodied("total = Some(2);");

        let at = dir.path().to_path_buf();
        let here = at.clone();
        let built = std::thread::scope(|scope| {
            let first = scope.spawn(|| crate::reaction::build(&one, &here).unwrap().unwrap());
            let second = scope.spawn(|| crate::reaction::build(&two, &at).unwrap().unwrap());
            (first.join().unwrap(), second.join().unwrap())
        });

        let (first, second) = built;
        assert_ne!(first.binary(), second.binary(), "one source, one name");
        let read = |handle: &crate::reaction::Reactions| fs::read(handle.binary()).unwrap();
        assert_ne!(
            read(&first),
            read(&second),
            "a name was published for a build it did not come from"
        );
    }

    /// A build that fails leaves its source behind, so the next attempt sees a
    /// source matching what it would generate. What it must not see is the
    /// binary the *previous* source built, or a broken program would quietly
    /// run the code it replaced.
    #[test]
    #[ignore = "shells out to cargo; run with --ignored"]
    fn a_failed_build_does_not_leave_the_last_body_runnable() {
        let dir = tempfile::tempdir().unwrap();
        let build = |body: &str| {
            let mut bytecode = counter_bytecode("");
            for instruction in &mut bytecode.instructions {
                if let Instruction::InstallReaction {
                    body: source,
                    triggers,
                    effects,
                    contract,
                    ..
                } = instruction
                {
                    // The counter triggers and writes `c.again`, and one local
                    // cannot be both. Only the build matters here.
                    *source = Some(body.to_string());
                    *triggers = vec!["c.tick".to_string()];
                    *effects = vec!["c.total".to_string()];
                    *contract = "c.total".to_string();
                }
            }
            let state = verify(&bytecode).unwrap();
            crate::reaction::build(&state, dir.path())
        };

        // A body that compiles publishes a binary named for its own source.
        let good = build("self.count += 1;").unwrap().unwrap();
        assert!(good.binary().exists());

        // A body that does not compile publishes nothing, and leaves the
        // binary an earlier source built alone — another run may be running it.
        assert!(build("nonexistent_fn();").is_err());
        assert!(
            good.binary().exists(),
            "a failed build took an unrelated binary"
        );

        // And the same broken body still fails rather than hitting a cache.
        assert!(build("nonexistent_fn();").is_err());
    }

    /// A body can leave a descendant behind, and that descendant inherits the
    /// streams. Waiting for them to end is then a wait with no deadline — so
    /// the invocation gives the text a moment and gives up, rather than
    /// outliving the body it was supposed to bound.
    #[cfg(unix)]
    #[test]
    #[ignore = "shells out to cargo; run with --ignored"]
    fn a_descendant_holding_the_streams_does_not_outlast_the_deadline() {
        struct Unused;
        impl ReactionExecutor for Unused {
            fn invoke(&self, _: InvocationSpec) -> Result<BTreeMap<String, Value>> {
                panic!("the reaction has a body, so no agent is asked");
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let mut bytecode = slow_bytecode("s.out");
        for instruction in &mut bytecode.instructions {
            if let Instruction::InstallReaction { body, .. } = instruction {
                // Answers at once, but leaves something holding the pipes.
                *body = Some(
                    "std::process::Command::new(\"sleep\").arg(\"30\").spawn().ok(); \
                     out = Some(1);"
                        .to_string(),
                );
            }
        }
        let state = verify(&bytecode).unwrap();
        let code = crate::reaction::build(&state, dir.path()).unwrap().unwrap();
        let spec = invocation_spec(
            &state,
            state.reactions.get_key_value("s.reaction.0").unwrap(),
            &BTreeMap::from([("s.tick".to_string(), json!(1))]),
            &BTreeMap::new(),
            Tag::default(),
        )
        .unwrap();

        let started = std::time::Instant::now();
        let outcome = DispatchExecutor {
            workspace_dirs: BTreeMap::new(),
            state: &state,
            code: Some(code),
            timeout: Duration::from_secs(30),
            run_started: Instant::now(),
            agents: Unused,
        }
        .invoke(spec);

        // The contract requires an effect and the deadline is 200ms, so this
        // fails — the point is that it fails promptly rather than waiting for
        // the descendant.
        assert!(outcome.is_err());
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "waited {:?} for a descendant to let go",
            started.elapsed()
        );
    }

    /// An overrunning body is killed with nothing written, and what that means
    /// is read off the contract, exactly as a silent agent's expiry is.
    #[test]
    #[ignore = "shells out to cargo; run with --ignored"]
    fn a_body_that_overruns_expires_on_its_contract() {
        struct Unused;
        impl ReactionExecutor for Unused {
            fn invoke(&self, _: InvocationSpec) -> Result<BTreeMap<String, Value>> {
                panic!("the reaction has a body, so no agent is asked");
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let run = |contract: &str| {
            let state = verify(&slow_bytecode(contract)).unwrap();
            let code = crate::reaction::build(&state, dir.path()).unwrap().unwrap();
            let spec = invocation_spec(
                &state,
                state.reactions.get_key_value("s.reaction.0").unwrap(),
                &BTreeMap::from([("s.tick".to_string(), json!(1))]),
                &BTreeMap::new(),
                Tag::default(),
            )
            .unwrap();
            DispatchExecutor {
                workspace_dirs: BTreeMap::new(),
                state: &state,
                code: Some(code),
                timeout: Duration::from_secs(30),
                run_started: Instant::now(),
                agents: Unused,
            }
            .invoke(spec)
        };

        // `s.out ?` is a promise the body may keep by staying silent, so the
        // tag completes with no writes.
        assert_eq!(run("s.out ?").unwrap(), BTreeMap::new());

        // `s.out` was required and never arrived. There is no value to invent.
        let error = run("s.out").unwrap_err().to_string();
        assert!(error.contains("requires an effect"), "{error}");
        assert!(error.contains("its body"), "{error}");
    }

    /// `coder : Claude per tag` as `omarc` emits it, at `version`.
    fn lifetime_program(version: u32, backend: &str, lifetime: Option<Value>) -> Bytecode {
        let mut agent =
            json!({"op":"spawn_agent","instance":"t","name":"t.coder","backend":backend});
        if let Some(lifetime) = lifetime {
            agent["lifetime"] = lifetime;
        }
        serde_json::from_value(json!({
            "version": version,
            "team": "Scoped",
            "instructions": [
                {"op":"begin_plan","team":"Scoped"},
                {"op":"declare_instance","name":"t","parent":"","team":"T"},
                agent,
                {"op":"commit_plan"}
            ]
        }))
        .unwrap()
    }

    fn per(unit: &str, count: u32) -> Option<Value> {
        Some(json!({"unit": unit, "count": count}))
    }

    #[test]
    fn a_lifetime_is_carried_and_none_is_not_written() {
        let program = lifetime_program(2, "Claude", per("tag", 3));
        let state = verify(&program).unwrap();
        let lifetime = state.agents["t.coder"].lifetime.unwrap();
        assert_eq!(
            lifetime,
            Lifetime {
                unit: LifetimeUnit::Tag,
                count: 3
            }
        );
        assert_eq!(lifetime.to_string(), "per 3 tags");
        // Round trips through the bytecode a checkpoint keeps as program.json.
        let kept: Bytecode =
            serde_json::from_value(serde_json::to_value(&program).unwrap()).unwrap();
        assert_eq!(kept.instructions, program.instructions);
        assert_eq!(
            serde_json::to_value(&kept.instructions[2]).unwrap()["lifetime"],
            json!({"unit":"tag","count":3})
        );
        assert_eq!(
            serde_json::to_value(&state.agents["t.coder"]).unwrap()["lifetime"],
            json!({"unit":"tag","count":3})
        );
        for (count, unit, spelled) in [
            (1, "task", "per task"),
            (1, "tag", "per tag"),
            (5, "task", "per 5 tasks"),
        ] {
            let state = verify(&lifetime_program(2, "Stub", per(unit, count))).unwrap();
            assert_eq!(
                state.agents["t.coder"].lifetime.unwrap().to_string(),
                spelled
            );
        }
        // Without one the agent's bytecode and state are as they always were.
        let plain = lifetime_program(1, "Claude", None);
        let state = verify(&plain).unwrap();
        assert_eq!(state.agents["t.coder"].lifetime, None);
        assert!(serde_json::to_value(&plain.instructions[2])
            .unwrap()
            .get("lifetime")
            .is_none());
        assert!(serde_json::to_value(&state.agents["t.coder"])
            .unwrap()
            .get("lifetime")
            .is_none());
    }

    #[test]
    fn a_lifetime_is_refused_where_it_could_be_dropped_or_unkept() {
        // Version 1 is what a runtime that predates lifetimes reads, ignoring
        // `lifetime`: such a program would keep the conversation.
        let error = verify(&lifetime_program(1, "Claude", per("tag", 1)))
            .unwrap_err()
            .to_string();
        assert!(error.contains("needs bytecode version 2"), "{error}");
        // A backend that cannot promise a fresh conversation is refused before
        // anything starts, naming what can.
        for backend in ["Web", "Agy", "Antigravity", "Cursor", "Nope"] {
            let error = verify(&lifetime_program(2, backend, per("task", 5)))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(&format!(
                    "agent 't.coder' resets its conversation (per 5 tasks), but backend '{backend}' cannot start a fresh conversation"
                )) && error.contains("supported: claude, codex, opencode, pi, jev, stub"),
                "{backend}: {error}"
            );
        }
        for backend in ["Claude", "Codex", "OpenCode", "Pi", "Jev", "Stub"] {
            verify(&lifetime_program(2, backend, per("tag", 1))).unwrap();
        }
        // Never a count of nothing.
        let error = verify(&lifetime_program(2, "Claude", per("tag", 0)))
            .unwrap_err()
            .to_string();
        assert!(error.contains("at least 1"), "{error}");
        // A lifetime this runtime does not know is an error, not the default.
        for lifetime in [
            json!({"unit":"week","count":1}),
            json!({"unit":"tag"}),
            json!({"unit":"tag","count":1,"extra":true}),
            json!("tag"),
        ] {
            let unknown = serde_json::from_value::<Bytecode>(json!({
                "version": 2, "team": "Scoped", "instructions": [
                    {"op":"begin_plan","team":"Scoped"},
                    {"op":"spawn_agent","name":"coder","backend":"Claude","lifetime":lifetime},
                    {"op":"commit_plan"}
                ]
            }));
            assert!(unknown.is_err(), "{lifetime}");
        }
        // Nor does a newer version pass.
        assert!(verify(&lifetime_program(3, "Claude", per("tag", 1))).is_err());
    }

    /// Which window each of an agent's invocations, at `tags`, falls in: the
    /// runtime's own rule, driven the way `admit` drives it.
    fn windows_of(lifetime: Lifetime, tags: &[(u64, u64)]) -> Vec<usize> {
        let tag = |(timestamp, microstep)| Tag {
            timestamp,
            microstep,
        };
        let mut windows = Vec::new();
        let mut open: Option<(usize, Used)> = None;
        for &at in tags {
            let at = tag(at);
            let window = match open {
                Some((window, mut used)) if !used.spent(lifetime, at) => {
                    used.take(at);
                    open = Some((window, used));
                    window
                }
                _ => {
                    let window = windows.last().map_or(0, |last| last + 1);
                    open = Some((window, Used::first(at)));
                    window
                }
            };
            windows.push(window);
        }
        windows
    }

    fn life(unit: LifetimeUnit, count: u32) -> Lifetime {
        Lifetime { unit, count }
    }

    #[test]
    fn per_task_gives_every_invocation_a_window_of_its_own() {
        // Two in one tag are still two windows.
        assert_eq!(
            windows_of(life(LifetimeUnit::Task, 1), &[(1, 0), (1, 0), (2, 0)]),
            [0, 1, 2]
        );
        assert!(Used::first(Tag::default()).exhausted(life(LifetimeUnit::Task, 1)));
    }

    #[test]
    fn per_n_tasks_ends_a_window_after_that_many_invocations() {
        // Three invocations, two windows.
        assert_eq!(
            windows_of(life(LifetimeUnit::Task, 2), &[(1, 0), (1, 0), (2, 0)]),
            [0, 0, 1]
        );
        let mut used = Used::first(Tag::default());
        assert!(!used.exhausted(life(LifetimeUnit::Task, 2)));
        used.take(Tag::default());
        assert!(used.exhausted(life(LifetimeUnit::Task, 2)));
    }

    #[test]
    fn per_tag_shares_one_window_between_invocations_in_a_tag() {
        // The same agent twice in one tag (a loop's second reaction arrives at
        // the same tag) shares a conversation; the next tag starts another.
        assert_eq!(
            windows_of(
                life(LifetimeUnit::Tag, 1),
                &[(1, 0), (1, 0), (2, 0), (2, 0)]
            ),
            [0, 0, 1, 1]
        );
        // A tag is a timestamp and a microstep: a loop through `after 0`
        // reaches a new tag on every pass.
        assert_eq!(
            windows_of(life(LifetimeUnit::Tag, 1), &[(1, 0), (1, 1), (1, 2)]),
            [0, 1, 2]
        );
        // Tags cannot say a window is over: the agent may run in it again.
        assert!(!Used::first(Tag::default()).exhausted(life(LifetimeUnit::Tag, 1)));
    }

    #[test]
    fn per_n_tags_counts_only_the_tags_the_agent_runs_in() {
        // Other agents' tags (here 2, 3, 4, 6, 7, 8) never reach this one, so
        // they do not shorten its window: it runs at 1, 5 and 9.
        assert_eq!(
            windows_of(life(LifetimeUnit::Tag, 2), &[(1, 0), (5, 0), (9, 0)]),
            [0, 0, 1]
        );
        assert_eq!(
            windows_of(
                life(LifetimeUnit::Tag, 2),
                &[(1, 0), (1, 0), (2, 0), (3, 0), (3, 0), (4, 0)]
            ),
            [0, 0, 0, 1, 1, 1]
        );
    }

    /// A conversation a backend minted is taken only when it is new: not
    /// empty, not one any attempt reported before, and passing the backend's
    /// own proof. Whatever is refused is never taken later either.
    #[test]
    fn a_reported_conversation_is_refused_unless_it_is_shown_new() {
        let seen = Mutex::new(Ok(BTreeSet::new()));
        let fresh = |reported: &str, seen: &Mutex<Result<BTreeSet<String>, String>>| {
            fresh_conversation(Freshness::Reported, "chosen", reported, seen, || Ok(()))
        };
        fresh("first", &seen).unwrap();
        let reused = fresh("first", &seen).unwrap_err().to_string();
        assert!(
            reused.contains("an earlier attempt already had"),
            "{reused}"
        );
        for empty in ["", "  "] {
            let error = fresh(empty, &seen).unwrap_err().to_string();
            assert!(error.contains("empty conversation id"), "{error}");
        }
        let unproved = fresh_conversation(Freshness::Reported, "chosen", "forked", &seen, || {
            bail!("it was forked from another thread")
        })
        .unwrap_err();
        assert!(
            format!("{unproved:#}").contains("it was forked from another thread"),
            "{unproved:#}"
        );
        assert!(fresh("forked", &seen).is_err());
        // Nor is a minted id taken when the earlier ones are unknown.
        let unknown = Mutex::new(Err("unreadable log".to_string()));
        let error = fresh("second", &unknown).unwrap_err().to_string();
        assert!(error.contains("unreadable log"), "{error}");
    }

    /// A chosen conversation must be the one given; its proof still runs; and
    /// it closes the id to a later minted report.
    #[test]
    fn a_chosen_conversation_must_be_the_one_given() {
        let seen = Mutex::new(Ok(BTreeSet::new()));
        let error = fresh_conversation(Freshness::Chosen, "given", "other", &seen, || Ok(()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("not the new conversation given"), "{error}");
        assert!(
            fresh_conversation(Freshness::Chosen, "given", "given", &seen, || bail!("no")).is_err()
        );
        assert!(fresh_conversation(Freshness::Reported, "x", "given", &seen, || Ok(())).is_err());
        // An unreadable log does not stand in the way of an id minted for
        // this attempt alone.
        let unknown = Mutex::new(Err("unreadable log".to_string()));
        fresh_conversation(Freshness::Chosen, "new", "new", &unknown, || Ok(())).unwrap();
    }

    /// The conversations a resumed run must not take again are every one its
    /// log recorded; a log that cannot be read is an error, not empty.
    #[test]
    fn earlier_conversations_are_read_from_the_attempt_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("attempts.jsonl");
        assert_eq!(reported_conversations(&log), Ok(BTreeSet::new()));
        fs::write(
            &log,
            "{\"native_session_id\":null,\"outcome\":\"started\"}\n\
             {\"native_session_id\":\"a\",\"outcome\":\"started\"}\n\
             {\"native_session_id\":\"a\",\"outcome\":\"completed\"}\n\
             {\"native_session_id\":\"b\",\"outcome\":\"failed\"}\n",
        )
        .unwrap();
        assert_eq!(
            reported_conversations(&log),
            Ok(BTreeSet::from(["a".to_string(), "b".to_string()]))
        );
        let seen = Mutex::new(reported_conversations(&log));
        assert!(fresh_conversation(Freshness::Reported, "x", "b", &seen, || Ok(())).is_err());
        fs::write(&log, "{\"native_session_id\":\"a\"}\n{\"native_sess").unwrap();
        assert!(reported_conversations(&log).is_err());
    }

    #[test]
    fn a_dead_tasks_buffered_ports_and_state_are_discarded_and_late_calls_refused() {
        let registry = InvocationRegistry::default();
        let mut spec = InvocationSpec {
            id: 1,
            agent: "coder".into(),
            reaction_id: "work".into(),
            contract: "out".into(),
            allowed_effects: BTreeMap::from([("out".into(), "string".into())]),
            state_types: BTreeMap::from([("state".into(), "string".into())]),
            ..Default::default()
        };
        let dead = registry
            .register(invocation_record("team", &spec).unwrap())
            .unwrap();
        registry
            .execute(
                "team",
                "coder",
                None,
                InvocationCommand::SetPort(SetPortArgs {
                    task_id: 1,
                    port: "out".into(),
                    value: json!("dead"),
                }),
            )
            .unwrap();
        registry
            .execute(
                "team",
                "coder",
                None,
                InvocationCommand::SetState(SetStateArgs {
                    task_id: 1,
                    name: "state".into(),
                    value: json!("dead state"),
                }),
            )
            .unwrap();
        registry.remove(&1);
        assert!(dead.recv().is_err());
        spec.id = 2;
        let retry = registry
            .register(invocation_record("team", &spec).unwrap())
            .unwrap();
        assert!(registry
            .execute(
                "team",
                "coder",
                None,
                InvocationCommand::Complete(CompleteArgs {
                    task_id: 1,
                    keep: Vec::new(),
                })
            )
            .is_err());
        assert!(registry
            .execute(
                "team",
                "coder",
                None,
                InvocationCommand::Complete(CompleteArgs {
                    task_id: 2,
                    keep: Vec::new(),
                })
            )
            .is_err());
        registry
            .execute(
                "team",
                "coder",
                None,
                InvocationCommand::SetPort(SetPortArgs {
                    task_id: 2,
                    port: "out".into(),
                    value: json!("retry"),
                }),
            )
            .unwrap();
        registry
            .execute(
                "team",
                "coder",
                None,
                InvocationCommand::Complete(CompleteArgs {
                    task_id: 2,
                    keep: Vec::new(),
                }),
            )
            .unwrap();
        assert_eq!(
            retry.recv().unwrap(),
            BTreeMap::from([("out".into(), json!("retry"))])
        );
    }

    #[test]
    fn worker_attempt_records_label_only_retries_and_start_them_once() {
        let dir = tempfile::tempdir().unwrap();
        let client = TmuxClient::on_server(
            "restart-test",
            Some(format!("omar-test-{}", Uuid::new_v4())),
        );
        let session = client.session_for("coder");
        struct Cleanup(TmuxClient, String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.kill_session(&self.1);
            }
        }
        let _cleanup = Cleanup(client.clone(), session.clone());
        client
            .new_session(&session, "sleep 60", dir.path().to_str())
            .unwrap();
        let agent = AgentState {
            backend: "Stub".into(),
            instance: "".into(),
            lifetime: None,
        };
        let workers = ScopedWorkers {
            states: BTreeMap::from([("coder".into(), agent)]),
            windows: BTreeMap::new(),
            stopped: Mutex::new(BTreeSet::new()),
            launcher: AgentLauncher {
                client: client.clone(),
                team: "test".into(),
                runtime_dir: dir.path().into(),
                omar_dir: dir.path().into(),
                ea_id: 0,
                base_prefix: "restart-test".into(),
                health_idle_warning: 0,
                shutdown: Arc::new(AtomicBool::new(false)),
                endpoint: "".into(),
                token: "".into(),
                workspaces: BTreeMap::from([("".into(), (dir.path().into(), dir.path().into()))]),
                resumed: "".into(),
            },
            run: "test".into(),
            deployment: Arc::new(Mutex::new(deploy::DeploymentRecord::create(
                "test",
                BTreeMap::new(),
                60,
            ))),
            log: dir.path().join("attempts.jsonl"),
            seen: Mutex::new(Ok(BTreeSet::new())),
            publisher: None,
        };
        let invocation = InvocationSpec {
            agent: "coder".into(),
            ..Default::default()
        };
        let original = manager::TopologyAttempt {
            id: "original".into(),
            key: "original".into(),
            native_session: "".into(),
        };
        let window = workers.open(&invocation, &original, 0).unwrap();
        assert!(window.record.get("conversation_mode").is_none());
        assert_eq!(window.record["worker_session"], session);
        let retry = manager::TopologyAttempt {
            id: "retry".into(),
            key: "retry".into(),
            native_session: "native".into(),
        };
        let mut window = workers.open(&invocation, &retry, 1).unwrap();
        assert_eq!(
            window.record["conversation_mode"],
            "resumed native conversation"
        );
        let lines: Vec<Value> = fs::read_to_string(&workers.log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            lines
                .iter()
                .filter(|record| record["attempt_id"] == "retry" && record["outcome"] == "started")
                .count(),
            1
        );
        client.kill_session(&session).unwrap();
        workers.launcher.shutdown.store(true, Ordering::Release);
        let error = workers
            .open(&invocation, &retry, 1)
            .err()
            .expect("shutdown must refuse a retry");
        assert!(error.to_string().contains("runtime is shutting down"));
        let mut executor = AgentReactionExecutor {
            client,
            team: "test".into(),
            registry: InvocationRegistry::default(),
            timeout: Duration::from_secs(60),
            run_started: Instant::now(),
            web: BTreeSet::new(),
            scoped: None,
        };
        let error = executor.watch_worker(&mut window, None).unwrap_err();
        assert_eq!(error.to_string(), "worker process exited");
        struct BlockedWorker;
        impl crate::backend::TurnMonitor for BlockedWorker {
            fn ended_turn(&mut self) -> Option<String> {
                None
            }
            fn blocked_input(&self) -> Option<String> {
                Some("question".into())
            }
            fn answer_input(&mut self, _: &str, _: &str) -> Result<bool> {
                panic!("a dead worker must be restarted before answering its question")
            }
        }
        window.turn_monitor = Some(Box::new(BlockedWorker));
        let mut turns = TaskTurnWatch {
            idle: Some(("input:question".into(), Instant::now() - TURN_GRACE * 2)),
            ..Default::default()
        };
        let mut clock = TaskClock::new();
        let error = executor
            .watch_worker(&mut window, Some((&invocation, &mut turns, &mut clock)))
            .unwrap_err();
        assert_eq!(error.to_string(), "worker process exited");
        executor.scoped = Some(workers);
        let error = executor.watch_worker(&mut window, None).unwrap_err();
        assert_eq!(error.to_string(), "runtime is shutting down");
        assert!(error.downcast_ref::<WorkerDied>().is_none());
        let workers = executor.scoped.as_ref().unwrap();
        let mut slot = Some(window);
        workers.close(&mut slot).unwrap();
        assert_eq!(
            workers.deployment.lock().unwrap().sessions.get("coder"),
            Some(&session)
        );
        let sessions = workers.deployment.lock().unwrap().sessions.clone();
        assert!(workers.cleanup_sessions(&sessions).is_empty());
        workers.track("coder", Some(&session)).unwrap();
        assert_eq!(workers.cleanup_sessions(&sessions), sessions);
    }

    #[test]
    fn turn_recovery_waits_for_grace_and_ignores_busy_or_earlier_tasks() {
        let now = Instant::now();
        let mut watch = TaskTurnWatch {
            seen: Some("earlier-task".into()),
            ..Default::default()
        };
        assert!(!watch.stopped(Some("earlier-task".into()), now));
        assert!(!watch.stopped(Some("current-task".into()), now));
        assert!(!watch.stopped(Some("current-task".into()), now + TURN_GRACE / 2));
        // Activity during grace cancels the candidate, even on a quiet screen.
        assert!(!watch.stopped(None, now + TURN_GRACE));
        assert!(!watch.stopped(Some("current-task".into()), now + TURN_GRACE));
        assert!(watch.stopped(Some("current-task".into()), now + TURN_GRACE * 2));
        watch.seen = watch.idle.take().map(|(id, _)| id);
        watch.reminded = true;
        assert!(!watch.stopped(Some("current-task".into()), now + TURN_GRACE * 3));
        assert!(!watch.stopped(Some("reminder-turn".into()), now + TURN_GRACE * 3));
        assert!(watch.stopped(Some("reminder-turn".into()), now + TURN_GRACE * 4));
        assert!(watch.reminded);
    }

    #[test]
    fn a_task_gets_exactly_two_retries_and_its_failure_names_the_worker_and_task() {
        let task = InvocationSpec {
            id: 42,
            agent: "p.coder".into(),
            reaction_id: "p.work".into(),
            ..Default::default()
        };
        let death = WorkerDied {
            reason: "worker process exited".into(),
            conversation: "native".into(),
        };
        assert!(retry_task(&task, 0, &death).is_ok());
        assert!(retry_task(&task, 1, &death).is_ok());
        let error = retry_task(&task, 2, &death).unwrap_err().to_string();
        for expected in [
            "p.coder",
            "task 42",
            "p.work",
            "after 2 retries",
            "worker process exited",
        ] {
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn only_the_attempt_an_invocation_was_started_for_may_answer_it() {
        let registry = InvocationRegistry::default();
        let completion = registry
            .register(InvocationRecord {
                id: 9,
                team: "Scoped".into(),
                agent: "t.coder".into(),
                reaction: "t.reaction.0".into(),
                contract: "t.out".into(),
                allowed_effects: BTreeMap::from([("t.out".into(), "string".into())]),
                trigger_values: BTreeMap::new(),
                prompt: "go".into(),
                writes: BTreeMap::new(),
                completed: false,
                attempt: Some("second".into()),
                ..Default::default()
            })
            .unwrap();
        let set = || {
            InvocationCommand::SetPort(SetPortArgs {
                task_id: 9,
                port: "t.out".into(),
                value: json!("v"),
            })
        };
        let complete = || {
            InvocationCommand::Complete(CompleteArgs {
                task_id: 9,
                keep: vec![],
            })
        };
        // The worker of an earlier attempt has the same name, and is refused;
        // so is a call that names no attempt at all.
        for stale in [Some("first"), None] {
            let error = registry
                .execute("Scoped", "t.coder", stale, set())
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("belongs to another worker attempt") && !error.contains("second"),
                "{error}"
            );
            assert!(registry
                .execute("Scoped", "t.coder", stale, complete())
                .is_err());
            let pending = registry
                .execute("Scoped", "t.coder", stale, InvocationCommand::Pending)
                .unwrap();
            assert_eq!(pending["pending"], json!([]));
        }
        assert!(completion.try_recv().is_err(), "a stale attempt published");
        registry
            .execute("Scoped", "t.coder", Some("second"), set())
            .unwrap();
        registry
            .execute("Scoped", "t.coder", Some("second"), complete())
            .unwrap();
        assert_eq!(completion.try_recv().unwrap()["t.out"], json!("v"));
        // A duplicate completion publishes nothing more.
        assert_eq!(
            registry
                .execute("Scoped", "t.coder", Some("second"), complete())
                .unwrap()["status"],
            json!("already_complete")
        );
        assert!(completion.try_recv().is_err());
        // Nor can anything answer once the invocation is over.
        registry.remove(&9);
        assert!(registry
            .execute("Scoped", "t.coder", Some("second"), complete())
            .is_err());
    }
}
