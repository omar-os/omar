//! Local Docker Sandboxes execution. Only explicit workspace directories cross
//! the VM boundary; control messages use sbx exec's pipes, not a host listener.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use crate::backend::Connection;

pub const FRAME_LIMIT: usize = 8 * 1024 * 1024;
pub const PROTOCOL: u32 = 1;

/// The published OMAR kit (`sandbox/omar-sandbox`), used by `enabled = true`
/// and `--sandbox` unless a run or `[sandbox].template` names another. To
/// change the kit: edit it, push, and pin the digest the `Sandbox Kit`
/// workflow prints here.
pub const DEFAULT_TEMPLATE: &str = "docker.io/lsk567/omar-sandbox@sha256:ca4c0ce361eb5c65f8c9dea7f9b399e73c185831955cc34d4f7377bbe306dcc9";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// A kit image pinned by SHA-256 digest, or an absolute kit directory.
    pub template: Option<String>,
    /// `true`: teams run in sandboxes, from `template` or OMAR's published
    /// kit. Unset or `false`: on the host, unless a template is named.
    pub enabled: Option<bool>,
}

impl Config {
    /// Whether a run that says nothing runs its teams in sandboxes.
    pub fn on_by_default(&self) -> bool {
        self.enabled != Some(false) && (self.template.is_some() || self.enabled == Some(true))
    }
}

pub fn validate_template(template: &str) -> Result<()> {
    if Path::new(template).is_absolute() {
        anyhow::ensure!(
            Path::new(template).is_dir(),
            "sandbox kit directory {template} does not exist"
        );
        return Ok(());
    }
    let hash = template
        .rsplit_once("@sha256:")
        .filter(|(name, _)| !name.is_empty())
        .map(|(_, hash)| hash);
    anyhow::ensure!(
        hash.is_some_and(|h| h.len() == 64 && h.bytes().all(|c| c.is_ascii_hexdigit())),
        "sandbox template must be pinned by IMAGE@sha256:<digest> or be an absolute kit directory"
    );
    Ok(())
}

/// The template a run's teams execute in, or `None` for the host. Sandboxes
/// are opt-in: the run asking (`sandbox`, or naming a template), a configured
/// template, or `enabled = true` requires them, and an unready host is an
/// error. `disabled` (the run's `--no-sandbox`) wins.
pub fn select(
    sandbox: bool,
    requested: Option<&str>,
    disabled: bool,
    config: &Config,
    backends: &[&str],
) -> Result<Option<String>> {
    let asked = sandbox || requested.is_some();
    let configured = config.enabled != Some(false)
        && (config.template.is_some() || config.enabled == Some(true));
    if disabled || !(asked || configured) {
        return Ok(None);
    }
    let template = requested
        .or(config.template.as_deref())
        .unwrap_or(DEFAULT_TEMPLATE);
    validate_template(template)?;
    ready(backends)?;
    Ok(Some(template.to_owned()))
}

/// Services whose key, in sbx's store and approved for kits, lets a backend
/// reach its models from a sandbox. Any one suffices.
fn services(backend: &str) -> Result<&'static [&'static str]> {
    use crate::backend::Kind;
    const MODELS: &[&str] = &[
        "anthropic",
        "openai",
        "google",
        "openrouter",
        "xai",
        "mistral",
        "groq",
    ];
    if backend.eq_ignore_ascii_case("web") {
        return Ok(&[]);
    }
    let kind = crate::backend::by_name(backend)
        .with_context(|| format!("unknown backend '{backend}'"))?
        .kind();
    Ok(match kind {
        Kind::Claude => &["anthropic"],
        Kind::Codex => &["openai"],
        Kind::Cursor => &["cursor"],
        Kind::Antigravity => &["google"],
        Kind::Opencode | Kind::Pi => MODELS,
        Kind::Stub => &[],
        Kind::Jev => bail!("Jev agents cannot run in sandboxes yet"),
    })
}

/// Docker Sandboxes can run these backends here: sbx installed, signed in,
/// virtualization usable, a worker binary to install, and a key for each
/// backend connected for sandboxes (`omar connect <backend> --sandbox`).
fn ready(backends: &[&str]) -> Result<()> {
    let needs = backends
        .iter()
        .map(|backend| Ok((*backend, services(backend)?)))
        .collect::<Result<Vec<_>>>()?;
    worker_binary()?;
    let mut cmd = command();
    cmd.args(["diagnose", "--json"]);
    let report: Value = serde_json::from_slice(
        &checked(cmd, Duration::from_secs(30))
            .context("Docker Sandboxes (sbx) is not installed or not working")?,
    )?;
    for check in report["checks"].as_array().into_iter().flatten() {
        let name = check["name"].as_str().unwrap_or_default();
        if check["status"] == "fail"
            && matches!(
                name,
                "Virtualization" | "Authentication" | "Global network policy"
            )
        {
            bail!(
                "Docker Sandboxes {name}: {} {}",
                check["message"].as_str().unwrap_or_default(),
                check["hint"].as_str().unwrap_or_default()
            );
        }
    }
    if needs.iter().all(|(_, services)| services.is_empty()) {
        return Ok(());
    }
    let keys = stored_keys()?;
    for (backend, services) in needs {
        if !services.is_empty() && usable(services, &keys).is_none() {
            bail!("{}", not_connected(backend));
        }
    }
    Ok(())
}

/// Why a backend's agents cannot start in a sandbox, and what fixes it.
/// Named as `omar connect` takes it, however the program spells the backend.
fn not_connected(backend: &str) -> String {
    let kind = crate::backend::by_name(backend).map(|b| b.kind());
    let backend = kind.map_or(backend, |kind| kind.name());
    let why = match kind {
        // The one refusal an operator would not expect: `omar connect claude`
        // with a subscription works on the host.
        Some(crate::backend::Kind::Claude) => {
            " (it needs an Anthropic API key: a Claude subscription cannot be used in sandboxes)"
        }
        _ => "",
    };
    format!("{backend} is not connected for sandboxes{why}; run `omar connect {backend} --sandbox`")
}

/// A key OMAR's kit can hand a backend in a sandbox: the sbx service it is
/// stored under, and the hosts the kit injects it into
/// (`sandbox/omar-sandbox/omar-sandbox.yaml`), which sbx's
/// `credentials.yaml` must approve before a third-party kit may use it.
struct Key {
    service: &'static str,
    api_key: &'static [&'static str],
    oauth: &'static [&'static str],
}

const KEYS: &[Key] = &[
    Key {
        service: "anthropic",
        api_key: &["api.anthropic.com"],
        oauth: &[],
    },
    Key {
        service: "openai",
        api_key: &["api.openai.com"],
        oauth: &["auth.openai.com", "chatgpt.com"],
    },
    Key {
        service: "cursor",
        api_key: &["api2.cursor.sh", "api3.cursor.sh", "repo42.cursor.sh"],
        oauth: &[],
    },
    Key {
        service: "google",
        api_key: &["generativelanguage.googleapis.com"],
        oauth: &[],
    },
    Key {
        service: "openrouter",
        api_key: &["openrouter.ai"],
        oauth: &[],
    },
    Key {
        service: "xai",
        api_key: &["api.x.ai"],
        oauth: &[],
    },
    Key {
        service: "mistral",
        api_key: &["api.mistral.ai"],
        oauth: &[],
    },
    Key {
        service: "groq",
        api_key: &["api.groq.com"],
        oauth: &[],
    },
];

/// What sbx holds: each stored service, `true` when it is an OAuth sign-in,
/// and which services `credentials.yaml` approves.
struct Keys {
    stored: Vec<(String, bool)>,
    bound: Vec<String>,
}

fn stored_keys() -> Result<Keys> {
    let mut cmd = command();
    cmd.args(["secret", "ls", "--global", "--json"]);
    let listed: Value = serde_json::from_slice(&checked(cmd, Duration::from_secs(30))?)?;
    let stored = listed["secrets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|secret| {
            let name = secret["name"].as_str()?;
            let oauth = secret["secret"]
                .as_str()
                .unwrap_or_default()
                .contains("oauth");
            Some((name.to_owned(), oauth))
        })
        .collect();
    Ok(Keys {
        stored,
        bound: bindings(),
    })
}

/// The first of `services` with a stored, approved key, and how to say it.
/// An OAuth sign-in counts only for codex: the kit points only codex at
/// ChatGPT's backend; opencode and pi in a sandbox call the OpenAI API.
fn usable(services: &[&str], keys: &Keys) -> Option<String> {
    let codex = services == ["openai"];
    services.iter().find_map(|service| {
        let (_, oauth) = keys.stored.iter().find(|(name, _)| name == service)?;
        let bound = keys.bound.iter().any(|bound| bound == service);
        (bound && (codex || !oauth)).then(|| match (*service, oauth) {
            ("openai", true) => "ChatGPT".to_owned(),
            (service, true) => format!("{service} sign-in"),
            (service, false) => format!("{service} key"),
        })
    })
}

/// Whether each backend can reach its models from a sandbox, as sbx's own
/// store says: the counterpart of the host sign-in `omar backends` shows.
pub fn connections(backends: &[&dyn crate::backend::Backend]) -> Vec<Connection> {
    let keys = match stored_keys() {
        Ok(keys) => keys,
        Err(error) => {
            let installed = command()
                .arg("version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok();
            let status = if installed {
                Connection::unknown(format!("{error:#}"))
            } else {
                Connection::NotInstalled
            };
            return vec![status; backends.len()];
        }
    };
    backends
        .iter()
        .map(|backend| match services(backend.kind().name()) {
            Ok(services) if !services.is_empty() => match usable(services, &keys) {
                Some(via) => Connection::connected(via),
                None => Connection::SignedOut,
            },
            _ => Connection::unknown("cannot run in sandboxes"),
        })
        .collect()
}

/// `omar connect <backend> --sandbox`: give sbx a key the backend's agents
/// use from a sandbox, approve OMAR's kit to use it, and prove it works the
/// way a host sign-in is proved, by a test prompt, here inside a throwaway
/// sandbox. The key goes into sbx's store through sbx's own prompt; OMAR
/// never sees it.
pub fn connect(backend: &dyn crate::backend::Backend, config: &Config) -> Result<()> {
    let name = backend.kind().name();
    let services = services(name)?;
    anyhow::ensure!(!services.is_empty(), "{name} needs no key in sandboxes");
    anyhow::ensure!(
        command()
            .arg("version")
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|s| s.success()),
        "Docker Sandboxes (sbx) is not installed: https://docs.docker.com/ai/sandboxes/"
    );
    let mut cmd = command();
    cmd.args(["diagnose", "--json"]);
    let signed_out = checked(cmd, Duration::from_secs(30))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|report| {
            report["checks"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|check| check["name"] == "Authentication" && check["status"] == "fail")
        });
    if signed_out {
        println!("Signing in to Docker Sandboxes.");
        interactive(&["login"])?;
    }
    let template = config.template.as_deref().unwrap_or(DEFAULT_TEMPLATE);
    validate_template(template)?;
    let check = |backend| {
        println!("Checking {name} with a test prompt in a sandbox (starts one)…");
        check(backend, template)
    };
    // A key stored with sbx directly, not yet approved for OMAR's kit.
    bind(services)?;
    let mut checked = false;
    if usable(services, &stored_keys()?).is_some() {
        match check(backend) {
            Ok(()) => checked = true,
            Err(error) => println!("{error}\nGiving sbx a new key for {name}."),
        }
    }
    if !checked {
        let (args, note): (&[&str], &str) = match backend.kind() {
            crate::backend::Kind::Claude => (
                &["secret", "set", "anthropic"],
                "Paste an Anthropic API key (console.anthropic.com). A Claude subscription cannot be used in sandboxes.",
            ),
            crate::backend::Kind::Codex => (
                &["secret", "set", "openai", "--oauth"],
                "Sign in with ChatGPT. Over SSH, forward port 1455 (ssh -L 1455:localhost:1455) or curl the callback URL on this machine.",
            ),
            crate::backend::Kind::Cursor => (
                &["secret", "set", "cursor"],
                "Paste a Cursor API key (cursor.com/dashboard → Integrations).",
            ),
            crate::backend::Kind::Antigravity => (
                &["secret", "set", "google"],
                "Paste a Gemini API key (aistudio.google.com/apikey).",
            ),
            _ => (
                &["secret", "set"],
                "Pick a model provider and paste its API key.",
            ),
        };
        println!("{note}");
        interactive(args)?;
        bind(services)?;
    }
    let Some(via) = usable(services, &stored_keys()?) else {
        bail!("sbx holds no key {name} can use; run `omar connect {name} --sandbox` again");
    };
    if !checked {
        check(backend).map_err(|error| {
            anyhow::anyhow!("{error}; run `omar connect {name} --sandbox` to give sbx a new key")
        })?;
    }
    println!("✓ {name} connected for sandboxes ({via})");
    Ok(())
}

/// An sbx command on the operator's terminal.
fn interactive(args: &[&str]) -> Result<()> {
    let mut cmd = command();
    let status = cmd
        .args(args)
        .stdin(Stdio::inherit())
        .status()
        .context("start sbx")?;
    anyhow::ensure!(status.success(), "`sbx {}` {status}", args.join(" "));
    Ok(())
}

/// Approve OMAR's kit to use the stored keys for `services` it is not yet
/// approved for, at the hosts the kit sends each to: what sbx would ask on
/// a first interactive `sbx create`, which OMAR's runs never are.
fn bind(services: &[&str]) -> Result<()> {
    let keys = stored_keys()?;
    let path = credentials_path().context("no home directory for sbx's credentials.yaml")?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut added = String::new();
    for key in KEYS {
        let Some((_, oauth)) = keys.stored.iter().find(|(name, _)| name == key.service) else {
            continue;
        };
        if !services.contains(&key.service) || keys.bound.iter().any(|b| b == key.service) {
            continue;
        }
        let (kind, domains) = if *oauth {
            ("oauth", key.oauth)
        } else {
            ("apiKey", key.api_key)
        };
        added.push_str(&format!(
            "  {}:\n    {kind}:\n      domains: [{}]\n",
            key.service,
            domains.join(", ")
        ));
        println!(
            "Approved OMAR's sandbox kit to use your {} {kind} at {}",
            key.service,
            domains.join(", ")
        );
    }
    if added.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, with_bindings(&text, &added))
        .with_context(|| format!("writing {}", path.display()))
}

/// `text` with `entries` added at the top of its `bindings:` map.
fn with_bindings(text: &str, entries: &str) -> String {
    let mut out = String::new();
    let mut placed = false;
    for line in text.lines() {
        out.push_str(line);
        out.push('\n');
        if !placed && line.trim_end() == "bindings:" {
            out.push_str(entries);
            placed = true;
        }
    }
    if !placed {
        out.push_str("bindings:\n");
        out.push_str(entries);
    }
    out
}

/// [`check`] from `config`'s template, for Mission Control.
pub fn check_connected(
    backend: &dyn crate::backend::Backend,
    config: &Config,
) -> Result<(), String> {
    let template = config.template.as_deref().unwrap_or(DEFAULT_TEMPLATE);
    validate_template(template).map_err(|error| format!("{error:#}"))?;
    check(backend, template)
}

/// The sign-in check (`connection::check_sign_in`) run in a throwaway
/// sandbox from `template`: the backend's CLI, as its agents will run, with
/// the key the proxy injects.
fn check(backend: &dyn crate::backend::Backend, template: &str) -> Result<(), String> {
    let name = backend.kind().name();
    let command_line = backend.default_command();
    let program = command_line
        .split_whitespace()
        .next()
        .unwrap_or(command_line);
    let Some(args) = backend.sign_in_check(program) else {
        return Ok(());
    };
    let sandbox = format!("omar-check-{}", uuid::Uuid::new_v4());
    let dir = std::env::temp_dir().join(&sandbox);
    let result = (|| {
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let mut cmd = command();
        cmd.args([
            "create", "--name", &sandbox, "--skills", "off", "--pull", "missing", template,
        ])
        .arg(&dir);
        checked(cmd, Duration::from_secs(900)).map_err(|error| format!("{error:#}"))?;
        let mut cmd = command();
        cmd.args(["exec", "--workdir"])
            .arg(&dir)
            .args([&sandbox, program])
            .args(&args);
        crate::backend::connection::run_checked(cmd)
    })();
    let mut cmd = command();
    cmd.args(["rm", "--force", &sandbox]);
    let _ = checked(cmd, Duration::from_secs(60));
    let _ = std::fs::remove_dir_all(&dir);
    result.map_err(|error| format!("{name}'s test prompt failed in a sandbox: {error}"))
}

/// Services approved for third-party kits in sbx's `credentials.yaml`: the
/// keys directly under `bindings:`.
fn bindings() -> Vec<String> {
    let text = credentials_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    parse_bindings(&text)
}

fn credentials_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .map(|dir| dir.join("sbx/credentials.yaml"))
}

fn parse_bindings(text: &str) -> Vec<String> {
    let mut services = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let content = line.trim();
        if content.is_empty() || content.starts_with('#') {
            continue;
        }
        if indent == 0 {
            inside = content == "bindings:";
        } else if inside && indent == 2 {
            if let Some(name) = content
                .strip_suffix(':')
                .or_else(|| content.split_once(": ").map(|(name, _)| name))
            {
                services.push(name.trim_matches(['"', '\'']).to_owned());
            }
        }
    }
    services
}

/// The Linux `omar` a sandbox runs as its worker: this binary on Linux, the
/// release's `omar-linux` beside it elsewhere.
fn worker_binary() -> Result<PathBuf> {
    let path = match std::env::var_os("OMAR_SANDBOX_WORKER_BIN") {
        Some(path) => PathBuf::from(path),
        None if cfg!(target_os = "linux") => std::env::current_exe()?,
        None => std::env::current_exe()?.with_file_name("omar-linux"),
    };
    anyhow::ensure!(
        path.is_file(),
        "no Linux omar binary for the sandbox worker at {}",
        path.display()
    );
    Ok(path)
}

/// Source path inputs refer to the seeded copy, never to another host mount.
pub fn map_input_paths(
    value: &mut Value,
    ty: &str,
    workspace: &crate::workspace::Workspace,
    root: &Path,
) -> Result<()> {
    if let Some(inner) = ty.strip_prefix("option<").and_then(|s| s.strip_suffix('>')) {
        if !value.is_null() {
            map_input_paths(value, inner, workspace, root)?;
        }
    } else if let Some(inner) = ty.strip_prefix("list<").and_then(|s| s.strip_suffix('>')) {
        for value in value.as_array_mut().context("expected path list")? {
            map_input_paths(value, inner, workspace, root)?;
        }
    } else if ty == "path" {
        let path = PathBuf::from(value.as_str().context("expected path")?);
        anyhow::ensure!(
            !path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
            "sandbox path inputs must not contain parent traversal"
        );
        let worktree = workspace.worktree(root);
        if path.is_relative() {
            *value = Value::String(worktree.join(path).to_string_lossy().into_owned());
            return Ok(());
        }
        anyhow::ensure!(
            path.starts_with(&worktree) || path.starts_with(workspace.temp(root)),
            "sandbox path input is outside its workspace; explicit artifact transfer is required"
        );
    }
    Ok(())
}

pub fn name(workspace: &crate::workspace::Workspace) -> String {
    format!("omar-team-{}", workspace.id)
}

fn command() -> Command {
    let mut command =
        Command::new(std::env::var_os("OMAR_SBX_BIN").unwrap_or_else(|| "sbx".into()));
    command.env_remove("SSH_AUTH_SOCK").stdin(Stdio::null());
    command
}

pub fn read_frame(reader: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((FRAME_LIMIT + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= FRAME_LIMIT,
        "sandbox frame exceeds size limit"
    );
    anyhow::ensure!(
        bytes.last() == Some(&b'\n'),
        "sandbox control stream closed or truncated"
    );
    Ok(bytes)
}

pub fn write_frame(writer: &mut impl Write, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    anyhow::ensure!(
        bytes.len() < FRAME_LIMIT,
        "sandbox frame exceeds size limit"
    );
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

/// Bound control commands too: a stopped daemon must not hang deployment cleanup.
fn checked(mut command: Command, timeout: Duration) -> Result<Vec<u8>> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start sbx; install Docker Sandboxes and run sbx login first")?;
    let stdout = child.stdout.take().context("sbx stdout")?;
    let stderr = child.stderr.take().context("sbx stderr")?;
    let collect = |stream: Box<dyn Read + Send>| {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stream.take(1024 * 1024 + 1).read_to_end(&mut bytes);
            let _ = sender.send(result.map(|_| bytes));
        });
        receiver
    };
    let output = collect(Box::new(stdout));
    let errors = collect(Box::new(stderr));
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let bytes = errors.recv_timeout(timeout.saturating_sub(started.elapsed()))??;
            anyhow::ensure!(
                status.success(),
                "sbx failed: {}",
                String::from_utf8_lossy(&bytes)
            );
            let bytes = output.recv_timeout(timeout.saturating_sub(started.elapsed()))??;
            anyhow::ensure!(bytes.len() <= 1024 * 1024, "sbx output too large");
            return Ok(bytes);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("sbx command timed out");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub fn create(name: &str, template: &str, worktree: &Path, temp: &Path) -> Result<()> {
    validate_template(template)?;
    anyhow::ensure!(
        worktree.is_absolute() && temp.is_absolute(),
        "sandbox mounts must be absolute"
    );
    let mut cmd = command();
    cmd.args([
        "create", "--name", name, "--skills", "off", "--pull", "missing", "--cpus", "2",
        "--memory", "2g", template,
    ])
    .arg(worktree)
    .arg(temp);
    // A kit directory builds on first use.
    checked(cmd, Duration::from_secs(900))?;
    // The worker is this runtime's own build, so its protocol always matches.
    // Root-owned: agents in the VM cannot replace it.
    let mut cmd = command();
    cmd.args(["exec", "-i", "-u", "root", name, "sh", "-c"])
        .arg("cat > /usr/local/bin/omar.part && chmod 0755 /usr/local/bin/omar.part && mv /usr/local/bin/omar.part /usr/local/bin/omar")
        .stdin(std::fs::File::open(worker_binary()?)?);
    checked(cmd, Duration::from_secs(120)).map(|_| ())
}

pub fn stop_all(names: &std::collections::BTreeMap<String, String>) -> Vec<String> {
    cleanup(names, Duration::from_secs(60), |args, timeout| {
        let mut cmd = command();
        cmd.args(args);
        checked(cmd, timeout)
    })
}

/// One deployment-wide budget and, on failure, one inventory check. A stalled
/// daemon must not make cleanup take a fresh minute for every team instance.
fn cleanup(
    names: &std::collections::BTreeMap<String, String>,
    budget: Duration,
    run: impl Fn(&[&str], Duration) -> Result<Vec<u8>>,
) -> Vec<String> {
    let started = Instant::now();
    let mut failures = std::collections::BTreeMap::new();
    for name in names.values() {
        let result = (|| -> Result<()> {
            // Persisted identities must never address arbitrary user sandboxes.
            let id = name
                .strip_prefix("omar-team-")
                .context("invalid sandbox name")?;
            anyhow::ensure!(
                uuid::Uuid::parse_str(id)?.to_string() == id,
                "invalid sandbox ID"
            );
            let left = budget.saturating_sub(started.elapsed());
            anyhow::ensure!(!left.is_zero(), "deployment sandbox cleanup timed out");
            run(&["stop", name], left)?;
            Ok(())
        })();
        if let Err(error) = result {
            failures.insert(name.clone(), format!("{name}: {error:#}"));
        }
    }
    let left = budget.saturating_sub(started.elapsed());
    if !failures.is_empty() && !left.is_zero() {
        // Creation may fail before allocation, or an operator may remove a VM.
        // Only a successful inventory proves absence, never a daemon/auth error.
        if let Ok(output) = run(&["ls", "--quiet"], left) {
            if let Ok(list) = std::str::from_utf8(&output) {
                failures.retain(|name, _| list.lines().any(|line| line.trim() == name));
            }
        }
    }
    failures.into_values().collect()
}

type Request = (Value, mpsc::Sender<Result<Value>>);

pub struct Worker {
    process: Mutex<Process>,
}
struct Process {
    child: Child,
    requests: mpsc::Sender<Request>,
    failed: bool,
}
impl Worker {
    pub fn start(name: &str, worktree: &Path, log: &Path) -> Result<Self> {
        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut child = command()
            .args(["exec", "-i", "--workdir"])
            .arg(worktree)
            .args([name, "/usr/local/bin/omar", "sandbox-worker"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(log)?)
            .spawn()
            .context("start sandbox worker")?;
        let mut input = child.stdin.take().context("sandbox stdin")?;
        let mut output = BufReader::new(child.stdout.take().context("sandbox stdout")?);
        let (send, receive) = mpsc::channel::<Request>();
        std::thread::spawn(move || {
            while let Ok((request, response)) = receive.recv() {
                let result = (|| {
                    write_frame(&mut input, &request)?;
                    Ok(serde_json::from_slice(&read_frame(&mut output)?)?)
                })();
                let failed = result.is_err();
                if response.send(result).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            process: Mutex::new(Process {
                child,
                requests: send,
                failed: false,
            }),
        })
    }
    pub fn request(&self, request: Value, timeout: Duration) -> Result<Value> {
        let mut process = self
            .process
            .lock()
            .map_err(|_| anyhow::anyhow!("sandbox worker lock poisoned"))?;
        anyhow::ensure!(!process.failed, "sandbox worker is unavailable");
        let (send, receive) = mpsc::channel();
        process
            .requests
            .send((request, send))
            .context("sandbox worker stopped")?;
        let result = receive
            .recv_timeout(timeout)
            .context("sandbox worker timed out or disconnected")
            .and_then(|r| r);
        if result.is_err() {
            process.failed = true;
            let _ = process.child.kill();
        }
        let response = result?;
        if let Some(error) = response.get("error") {
            bail!("sandbox worker: {error}");
        }
        anyhow::ensure!(
            response.get("protocol").and_then(Value::as_u64) == Some(PROTOCOL as u64),
            "sandbox worker protocol mismatch; rebuild the template with this OMAR version"
        );
        Ok(response)
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Serialize, Deserialize)]
pub struct Init {
    pub protocol: u32,
    pub state: crate::topology::VmState,
    pub workspace: crate::workspace::Workspace,
    pub root: PathBuf,
    pub timeout: Duration,
    /// Set when the run resumed, so agents are told their conversation is new.
    pub resumed_from: Option<crate::checkpoint::Manifest>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unpinned_templates_and_truncated_or_oversized_frames() {
        assert!(validate_template("omar:latest").is_err());
        assert!(validate_template(&format!("omar@sha256:{}", "a".repeat(64))).is_ok());
        assert!(read_frame(&mut &b"{}"[..]).is_err());
        let data = vec![b'x'; FRAME_LIMIT + 1];
        assert!(read_frame(&mut data.as_slice()).is_err());
        assert_eq!(read_frame(&mut &b"{}\n"[..]).unwrap(), b"{}\n");
    }
    #[test]
    fn paths_are_remapped_only_to_the_owning_workspace() {
        let root = Path::new("/omar");
        let workspace = crate::workspace::Workspace {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            ea_id: 0,
            deployment_id: "test".into(),
            instance: "parent.child".into(),
            parent_instance: Some("parent".into()),
            restored_from: None,
        };
        let own = workspace.temp(root).join("scratch");
        let file = workspace.worktree(root).join("file");
        let mut paths = serde_json::json!([file, null, own]);
        map_input_paths(&mut paths, "list<option<path>>", &workspace, root).unwrap();
        assert_eq!(paths[0], file.to_str().unwrap());
        assert!(paths[1].is_null());
        assert_eq!(paths[2], own.to_str().unwrap());
        let mut relative = serde_json::json!("artifact");
        map_input_paths(&mut relative, "path", &workspace, root).unwrap();
        assert_eq!(
            relative,
            workspace.worktree(root).join("artifact").to_str().unwrap()
        );
        for path in [
            "/source/file",
            "/source/../secret",
            "/omar/workspace-history/key",
            "/omar/workspaces/another/worktree/file",
        ] {
            assert!(
                map_input_paths(&mut serde_json::json!(path), "path", &workspace, root).is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn reads_bound_services_and_their_backend_needs() {
        let text = "# approvals\nbindings:\n  anthropic:\n    apiKey:\n      domains: [api.anthropic.com]\n  \"openai\": {oauth: {domains: [chatgpt.com]}}\nother:\n  google:\n";
        assert_eq!(parse_bindings(text), ["anthropic", "openai"]);
        assert_eq!(services("claude").unwrap(), ["anthropic"]);
        assert!(services("opencode").unwrap().contains(&"openrouter"));
        assert!(services("stub").unwrap().is_empty());
        assert!(services("jev").is_err());
    }
    #[test]
    fn a_backend_is_connected_for_sandboxes_by_a_stored_approved_key() {
        let keys = |stored: &[(&str, bool)], bound: &[&str]| Keys {
            stored: stored.iter().map(|(n, o)| (n.to_string(), *o)).collect(),
            bound: bound.iter().map(|b| b.to_string()).collect(),
        };
        let claude = services("claude").unwrap();
        let codex = services("codex").unwrap();
        let pi = services("pi").unwrap();
        // Stored but not approved for the kit: not yet.
        assert_eq!(usable(claude, &keys(&[("anthropic", false)], &[])), None);
        assert_eq!(
            usable(claude, &keys(&[("anthropic", false)], &["anthropic"])).as_deref(),
            Some("anthropic key")
        );
        // ChatGPT's sign-in reaches codex only.
        let chatgpt = keys(&[("openai", true)], &["openai"]);
        assert_eq!(usable(codex, &chatgpt).as_deref(), Some("ChatGPT"));
        assert_eq!(usable(pi, &chatgpt), None);
        assert_eq!(
            usable(
                pi,
                &keys(&[("openai", true), ("groq", false)], &["openai", "groq"])
            )
            .as_deref(),
            Some("groq key")
        );
        // A subscription sign-in on the host is no help; the error says so.
        assert!(not_connected("claude").contains("Claude subscription cannot be used"));
        assert!(not_connected("codex").ends_with("run `omar connect codex --sandbox`"));
        // Programs spell it ClaudeCode; `omar connect` takes claude.
        assert!(not_connected("ClaudeCode").starts_with("claude is not connected"));
        assert!(not_connected("ClaudeCode").ends_with("`omar connect claude --sandbox`"));
    }

    #[test]
    fn approving_a_key_adds_it_to_the_bindings_sbx_reads() {
        let entry = "  anthropic:\n    apiKey:\n      domains: [api.anthropic.com]\n";
        let fresh = with_bindings("", entry);
        assert_eq!(parse_bindings(&fresh), ["anthropic"]);
        let existing =
            "# mine\nbindings:\n  openai:\n    oauth:\n      domains: [chatgpt.com]\nother: 1\n";
        let both = with_bindings(existing, entry);
        assert_eq!(parse_bindings(&both), ["anthropic", "openai"]);
        assert!(both.starts_with("# mine\n") && both.ends_with("other: 1\n"));
        assert_eq!(
            parse_bindings(&with_bindings("other: 1\n", entry)),
            ["anthropic"]
        );
    }

    #[test]
    fn approved_hosts_are_the_ones_the_kit_sends_each_key_to() {
        let kit = include_str!("../sandbox/omar-sandbox/omar-sandbox.yaml");
        for key in KEYS {
            assert!(
                kit.contains(&format!("service: {}", key.service)),
                "{}",
                key.service
            );
            for domain in key.api_key {
                assert!(kit.contains(&format!("domain: {domain},")), "{domain}");
            }
            for host in key.oauth {
                assert!(kit.contains(&format!("{host}:443")), "{host}");
            }
        }
        for backend in ["claude", "codex", "cursor", "opencode", "pi", "agy"] {
            for service in services(backend).unwrap() {
                assert!(KEYS.iter().any(|key| key.service == *service), "{service}");
            }
        }
    }

    #[test]
    fn sandboxes_are_opt_in() {
        let off = Config {
            enabled: Some(false),
            ..Config::default()
        };
        assert_eq!(
            select(false, None, false, &Config::default(), &["claude"]).unwrap(),
            None
        );
        assert_eq!(select(false, None, false, &off, &["claude"]).unwrap(), None);
        let on = Config {
            enabled: Some(true),
            ..Config::default()
        };
        assert_eq!(select(false, None, true, &on, &["claude"]).unwrap(), None);
        assert_eq!(select(true, None, true, &on, &["claude"]).unwrap(), None);
        // Asked for: an unknown backend is refused before sbx is consulted,
        // even where the config turns sandboxes off.
        assert!(select(true, None, false, &off, &["no-such-backend"]).is_err());
        assert!(validate_template("/nonexistent/kit").is_err());
        assert!(validate_template(DEFAULT_TEMPLATE).is_ok());
    }
    #[test]
    fn worker_timeout_disables_further_requests() {
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let (send, _receive) = mpsc::channel();
        let worker = Worker {
            process: Mutex::new(Process {
                child,
                requests: send,
                failed: false,
            }),
        };
        let error = worker
            .request(Value::Null, Duration::from_millis(10))
            .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(worker
            .request(Value::Null, Duration::from_secs(1))
            .unwrap_err()
            .to_string()
            .contains("unavailable"));
    }
    #[test]
    fn cleanup_uses_one_budget_and_one_inventory() {
        let names: std::collections::BTreeMap<_, _> = (0..3)
            .map(|n| (n.to_string(), format!("omar-team-{}", uuid::Uuid::new_v4())))
            .collect();
        let calls = std::cell::RefCell::new(Vec::new());
        let failed = cleanup(&names, Duration::from_secs(1), |args, _| {
            calls.borrow_mut().push(args[0].to_string());
            if args[0] == "stop" {
                bail!("not found or daemon unavailable");
            }
            Ok(format!("{}\n", names["1"]).into_bytes())
        });
        assert_eq!(failed.len(), 1);
        assert!(failed[0].contains(&names["1"]));
        assert_eq!(*calls.borrow(), ["stop", "stop", "stop", "ls"]);

        let count = std::cell::Cell::new(0);
        let started = Instant::now();
        let failed = cleanup(&names, Duration::from_millis(10), |_, _| {
            count.set(count.get() + 1);
            std::thread::sleep(Duration::from_millis(15));
            bail!("daemon stalled")
        });
        assert_eq!(count.get(), 1);
        assert_eq!(failed.len(), 3);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
