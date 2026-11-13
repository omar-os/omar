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
/// error. `disabled` (the run's `--no-sandbox`) wins. Beside it, what
/// getting ready did, for the operator to be told.
pub fn select(
    sandbox: bool,
    requested: Option<&str>,
    disabled: bool,
    config: &Config,
    backends: &[&str],
) -> Result<(Option<String>, Vec<String>)> {
    let asked = sandbox || requested.is_some();
    let configured = config.enabled != Some(false)
        && (config.template.is_some() || config.enabled == Some(true));
    if disabled || !(asked || configured) {
        return Ok((None, Vec::new()));
    }
    let template = requested
        .or(config.template.as_deref())
        .unwrap_or(DEFAULT_TEMPLATE);
    validate_template(template)?;
    let notes = ready(backends)?;
    Ok((Some(template.to_owned()), notes))
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
/// backend connected for sandboxes (`omar connect <backend> --sandbox`). A
/// backend that signs in here with an API key is connected with it on the
/// way; what that did is returned, to be said.
fn ready(backends: &[&str]) -> Result<Vec<String>> {
    let needs = backends
        .iter()
        .map(|backend| Ok((*backend, services(backend)?)))
        .collect::<Result<Vec<_>>>()?;
    worker_binary()?;
    let report = diagnose().context("Docker Sandboxes (sbx) is not installed or not working")?;
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
    let mut notes = Vec::new();
    if needs.iter().all(|(_, services)| services.is_empty()) {
        return Ok(notes);
    }
    let mut keys = stored_keys()?;
    for (backend, services) in needs {
        if services.is_empty() || usable(services, &keys).is_some() {
            continue;
        }
        let found = crate::backend::by_name(backend).and_then(|b| host_key(b).map(|key| (b, key)));
        let Some((found, key)) = found else {
            bail!("{}", not_connected(backend));
        };
        store(key.service, &key.key)?;
        bind(services)?;
        keys = stored_keys()?;
        notes.push(reusing(found.kind().name(), &key));
    }
    Ok(notes)
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
    /// Where the host's CLIs read this key from the environment.
    env: &'static [&'static str],
}

const KEYS: &[Key] = &[
    Key {
        service: "anthropic",
        api_key: &["api.anthropic.com"],
        oauth: &[],
        env: &["ANTHROPIC_API_KEY"],
    },
    Key {
        service: "openai",
        api_key: &["api.openai.com"],
        oauth: &["auth.openai.com", "chatgpt.com"],
        env: &["OPENAI_API_KEY"],
    },
    Key {
        service: "cursor",
        api_key: &["api2.cursor.sh", "api3.cursor.sh", "repo42.cursor.sh"],
        oauth: &[],
        env: &["CURSOR_API_KEY"],
    },
    Key {
        service: "google",
        api_key: &["generativelanguage.googleapis.com"],
        oauth: &[],
        env: &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
    },
    Key {
        service: "openrouter",
        api_key: &["openrouter.ai"],
        oauth: &[],
        env: &["OPENROUTER_API_KEY"],
    },
    Key {
        service: "xai",
        api_key: &["api.x.ai"],
        oauth: &[],
        env: &["XAI_API_KEY"],
    },
    Key {
        service: "mistral",
        api_key: &["api.mistral.ai"],
        oauth: &[],
        env: &["MISTRAL_API_KEY"],
    },
    Key {
        service: "groq",
        api_key: &["api.groq.com"],
        oauth: &[],
        env: &["GROQ_API_KEY"],
    },
];

/// An API key the host's sign-in already has, which a sandbox can use as it
/// is: the sbx service it goes under, the key, and where it was found.
struct HostKey {
    service: &'static str,
    key: String,
    from: String,
}

/// The API key `backend` signs in with on this machine, if it signs in with
/// one: from the environment, which every CLI puts first, else from the
/// CLI's own store. A subscription is no key; it cannot cross into a
/// sandbox, though Codex's ChatGPT can be signed in there afresh.
fn host_key(backend: &dyn crate::backend::Backend) -> Option<HostKey> {
    use crate::backend::Kind;
    let name = backend.kind().name();
    let services = services(name).ok()?;
    let entry = |service: &str| KEYS.iter().find(|key| key.service == service);
    for service in services {
        for var in entry(service).map_or(&[][..], |key| key.env) {
            if let Some(key) = std::env::var(var).ok().filter(|key| !key.trim().is_empty()) {
                return Some(HostKey {
                    service: entry(service)?.service,
                    key,
                    from: format!("${var}"),
                });
            }
        }
    }
    let read = |path: PathBuf| -> Option<Value> {
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    };
    match backend.kind() {
        Kind::Codex => {
            let auth = read(crate::backend::codex::codex_home().ok()??.join("auth.json"))?;
            let key = auth["OPENAI_API_KEY"]
                .as_str()
                .filter(|key| !key.is_empty())?;
            Some(HostKey {
                service: "openai",
                key: key.to_owned(),
                from: "codex's API key sign-in".to_owned(),
            })
        }
        // A provider each keeps beside its OAuth sign-ins: pi's `api_key`
        // and opencode's `api` entries.
        Kind::Pi | Kind::Opencode => {
            let path = if backend.kind() == Kind::Pi {
                crate::backend::pi::agent_dir()?.join("auth.json")
            } else {
                std::env::var_os("XDG_DATA_HOME")
                    .map(PathBuf::from)
                    .or_else(|| dirs::home_dir().map(|home| home.join(".local").join("share")))?
                    .join("opencode")
                    .join("auth.json")
            };
            let auth = read(path)?;
            services.iter().find_map(|service| {
                let stored = &auth[*service];
                matches!(stored["type"].as_str(), Some("api_key" | "api")).then_some(())?;
                let key = stored["key"].as_str().filter(|key| !key.is_empty())?;
                Some(HostKey {
                    service: entry(service)?.service,
                    key: key.to_owned(),
                    from: format!("{name}'s {service} key"),
                })
            })
        }
        _ => None,
    }
}

/// What reusing a host key is called, said as it happens.
fn reusing(name: &str, key: &HostKey) -> String {
    format!(
        "Using {name}'s API key from this machine ({}) for sandboxes.",
        key.from
    )
}

/// Give sbx `key` for `service` through its stdin, so it is never on a
/// command line. sbx asks before replacing one, so a stored one goes first,
/// and with it the kit's approval, which was for that kind of key.
fn store(service: &str, key: &str) -> Result<()> {
    if stored_keys()?
        .stored
        .iter()
        .any(|(name, _)| name == service)
    {
        remove(service)?;
    }
    let mut child = command()
        .args(["secret", "set", service])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("start sbx")?;
    child
        .stdin
        .take()
        .context("sbx's stdin")?
        .write_all(key.as_bytes())?;
    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "`sbx secret set {service}` {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

/// Remove sbx's key for `service`, and OMAR's kit's approval to use it.
fn remove(service: &str) -> Result<()> {
    let mut cmd = command();
    cmd.args(["secret", "rm", service, "-f"]);
    checked(cmd, Duration::from_secs(30))?;
    unbind(service)
}

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

/// Whether Docker Sandboxes (sbx) is here at all.
pub fn installed() -> bool {
    command()
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
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
/// sandbox. A key that already works is kept unless `replace`. The host's
/// own API key is reused; a subscription cannot be, so sbx's own prompt asks
/// for a key (or, for Codex, a ChatGPT sign-in of the sandboxes' own), which
/// OMAR never sees.
pub fn connect(
    backend: &dyn crate::backend::Backend,
    config: &Config,
    replace: bool,
) -> Result<()> {
    let name = backend.kind().name();
    let services = services(name)?;
    anyhow::ensure!(!services.is_empty(), "{name} needs no key in sandboxes");
    anyhow::ensure!(
        installed(),
        "Docker Sandboxes (sbx) is not installed: https://docs.docker.com/ai/sandboxes/"
    );
    let signed_out = diagnose().ok().is_some_and(|report| {
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
    if !replace && usable(services, &stored_keys()?).is_some() {
        match check(backend) {
            Ok(()) => checked = true,
            Err(Failed::Sandbox(error)) => bail!("{error}"),
            Err(Failed::Prompt(error)) => println!("{error}\nGiving sbx a new key for {name}."),
        }
    }
    // Set when the host signs in another way, so it keeps two sign-ins.
    let mut host = None;
    if !checked {
        match host_key(backend) {
            Some(key) => {
                println!("{}", reusing(name, &key));
                store(key.service, &key.key)?;
            }
            None => host = sign_in(backend)?,
        }
        // The new key may be of another kind than the approval it replaces.
        for service in services {
            unbind(service)?;
        }
        bind(services)?;
    }
    let Some(via) = usable(services, &stored_keys()?) else {
        bail!("sbx holds no key {name} can use; run `omar connect {name} --sandbox` again");
    };
    if !checked {
        match check(backend) {
            Ok(()) => {}
            Err(Failed::Sandbox(error)) => bail!("{error}"),
            Err(Failed::Prompt(error)) => {
                bail!(
                    "{error}; run `omar connect {name} --sandbox --replace` to give sbx a new key"
                )
            }
        }
    }
    println!("✓ {name} connected for sandboxes ({via})");
    if let Some(host) = host {
        println!("{name} now has two sign-ins: {host} on this machine, {via} in sandboxes.");
    } else if checked {
        println!("To give sandboxes another key: omar connect {name} --sandbox --replace");
    }
    Ok(())
}

/// Ask for a sandbox sign-in through sbx's own prompt, after saying why the
/// host's cannot be reused; the host's sign-in, when it has one.
fn sign_in(backend: &dyn crate::backend::Backend) -> Result<Option<String>> {
    use crate::backend::Kind;
    let name = backend.kind().name();
    // `Some(None)`: signed in, but the CLI does not say how.
    let host = match crate::backend::connection::connection(backend) {
        Connection::Connected { via } => Some(via),
        _ => None,
    };
    match (&host, backend.kind()) {
        (Some(Some(via)), Kind::Codex) => {
            println!("{name} is signed in here with {via}; sandboxes need a sign-in of their own.")
        }
        (Some(Some(via)), _) if via.contains("key") => {
            println!("OMAR cannot read the key {name} signs in with here; give sandboxes one.")
        }
        (Some(Some(via)), _) => println!(
            "{name} is signed in here with {via}, which sandboxes cannot use; they need an API key."
        ),
        (Some(None), _) => {
            println!("{name}'s sign-in here is not an API key OMAR can reuse; sandboxes need one.")
        }
        (None, _) => println!("{name} has no API key on this machine for sandboxes to reuse."),
    }
    let chatgpt = backend.kind() == Kind::Codex && {
        print!("Sign in with ChatGPT for sandboxes, or paste an OpenAI API key? [C/k] ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        !answer.trim().eq_ignore_ascii_case("k")
    };
    let (args, note): (&[&str], &str) = match backend.kind() {
        Kind::Claude => (
            &["secret", "set", "anthropic"],
            "Paste an Anthropic API key (console.anthropic.com).",
        ),
        Kind::Codex if chatgpt => (
            &["secret", "set", "openai", "--oauth"],
            "Sign in with ChatGPT. Over SSH, forward port 1455 (ssh -L 1455:localhost:1455) or curl the callback URL on this machine.",
        ),
        Kind::Codex => (
            &["secret", "set", "openai"],
            "Paste an OpenAI API key (platform.openai.com/api-keys).",
        ),
        Kind::Cursor => (
            &["secret", "set", "cursor"],
            "Paste a Cursor API key (cursor.com/dashboard → Integrations).",
        ),
        Kind::Antigravity => (
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
    Ok(host.map(|via| via.unwrap_or_else(|| "its sign-in".to_owned())))
}

/// `omar disconnect <backend> --sandbox`: remove the key sbx gives its
/// agents, and the kit's approval to use it. Backends that shared that key
/// lose it too, and are named.
pub fn disconnect(backend: &dyn crate::backend::Backend) -> Result<()> {
    let name = backend.kind().name();
    let services = services(name)?;
    let keys = stored_keys()?;
    let Some(service) = services.iter().find(|service| {
        keys.stored.iter().any(|(stored, _)| stored == *service)
            && keys.bound.iter().any(|bound| bound == *service)
    }) else {
        bail!("{name} is not connected for sandboxes");
    };
    let assistants: Vec<_> = crate::backend::ASSISTANT
        .iter()
        .map(|kind| crate::backend::of(*kind))
        .collect();
    let before = connections(&assistants);
    remove(service)?;
    println!("✓ {name} disconnected from sandboxes (sbx's {service} key removed)");
    for ((other, was), now) in assistants.iter().zip(before).zip(connections(&assistants)) {
        let other = other.kind().name();
        if other != name
            && matches!(was, Connection::Connected { .. })
            && !matches!(now, Connection::Connected { .. })
        {
            println!("{other} used that key too; it is no longer connected for sandboxes.");
        }
    }
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

/// Withdraw OMAR's kit's approval to use the stored key for `service`.
fn unbind(service: &str) -> Result<()> {
    let Some(path) = credentials_path() else {
        return Ok(());
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let kept = without_binding(&text, service);
    if kept != text {
        std::fs::write(&path, kept).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// `text` without `service`'s entry in its `bindings:` map: its line and the
/// lines indented under it.
fn without_binding(text: &str, service: &str) -> String {
    let mut out = String::new();
    let mut in_bindings = false;
    let mut skipping: Option<usize> = None;
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        if let Some(depth) = skipping {
            if line.trim().is_empty() || indent > depth {
                continue;
            }
            skipping = None;
        }
        if indent == 0 && !line.trim().is_empty() {
            in_bindings = line.trim_end() == "bindings:";
        } else if in_bindings && line.trim_start().trim_end() == format!("{service}:") {
            skipping = Some(indent);
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
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

/// The sign-in check (`connection::check_sign_in`) run in a throwaway
/// sandbox from `template`: the backend's CLI, as its agents will run, with
/// the key the proxy injects.
fn check(backend: &dyn crate::backend::Backend, template: &str) -> Result<(), Failed> {
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
        let start = |error: String| {
            Failed::Sandbox(format!(
                "could not start a sandbox to check {name}: {error}"
            ))
        };
        std::fs::create_dir_all(&dir).map_err(|error| start(error.to_string()))?;
        let mut cmd = command();
        cmd.args([
            "create", "--name", &sandbox, "--skills", "off", "--pull", "missing", template,
        ])
        .arg(&dir);
        checked(cmd, Duration::from_secs(900)).map_err(|error| start(format!("{error:#}")))?;
        let mut cmd = command();
        cmd.args(["exec", "--workdir"])
            .arg(&dir)
            .args([&sandbox, program])
            .args(&args);
        crate::backend::connection::run_checked(cmd).map_err(|error| {
            Failed::Prompt(format!("{name}'s test prompt failed in a sandbox: {error}"))
        })
    })();
    let mut cmd = command();
    cmd.args(["rm", "--force", &sandbox]);
    let _ = checked(cmd, Duration::from_secs(60));
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// Why a sandbox check failed: sbx could not start the sandbox, which no new
/// key fixes, or the backend's test prompt failed in it, which one may.
enum Failed {
    Sandbox(String),
    Prompt(String),
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Failed::Sandbox(why) | Failed::Prompt(why)) = self;
        f.write_str(why)
    }
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

/// `sbx diagnose --json`'s report. sbx exits 1 when any check fails, and
/// the report says which, so it is read either way.
fn diagnose() -> Result<Value> {
    let mut cmd = command();
    cmd.args(["diagnose", "--json"]);
    let (_, output, errors) = run(cmd, Duration::from_secs(30))?;
    serde_json::from_slice(&output)
        .with_context(|| format!("sbx diagnose failed: {}", String::from_utf8_lossy(&errors)))
}

/// Bound control commands too: a stopped daemon must not hang deployment cleanup.
fn checked(command: Command, timeout: Duration) -> Result<Vec<u8>> {
    let (success, output, errors) = run(command, timeout)?;
    anyhow::ensure!(success, "sbx failed: {}", String::from_utf8_lossy(&errors));
    Ok(output)
}

/// sbx's exit status, stdout and stderr, within `timeout`.
fn run(mut command: Command, timeout: Duration) -> Result<(bool, Vec<u8>, Vec<u8>)> {
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
            let errors = errors.recv_timeout(timeout.saturating_sub(started.elapsed()))??;
            let bytes = output.recv_timeout(timeout.saturating_sub(started.elapsed()))??;
            anyhow::ensure!(bytes.len() <= 1024 * 1024, "sbx output too large");
            return Ok((status.success(), bytes, errors));
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
        let response = self.exchange(request, timeout)?;
        if let Some(error) = response.get("error") {
            bail!("sandbox worker: {error}");
        }
        Ok(response)
    }
    /// One request and its reply, an `error` reply included: a failed
    /// invocation's reply still carries the check runs it made.
    pub fn exchange(&self, request: Value, timeout: Duration) -> Result<Value> {
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
    fn withdrawing_a_key_removes_only_its_binding() {
        let text = "# mine\nbindings:\n  anthropic:\n    apiKey:\n      domains: [api.anthropic.com]\n  openai:\n    oauth:\n      domains: [chatgpt.com]\nother:\n  anthropic: kept\n";
        let kept = without_binding(text, "anthropic");
        assert_eq!(parse_bindings(&kept), ["openai"]);
        assert!(kept.starts_with("# mine\nbindings:\n  openai:\n"), "{kept}");
        // Only under `bindings:`.
        assert!(kept.ends_with("other:\n  anthropic: kept\n"), "{kept}");
        assert_eq!(without_binding(&kept, "groq"), kept);
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
            (None, Vec::new())
        );
        assert_eq!(
            select(false, None, false, &off, &["claude"]).unwrap(),
            (None, Vec::new())
        );
        let on = Config {
            enabled: Some(true),
            ..Config::default()
        };
        assert_eq!(
            select(false, None, true, &on, &["claude"]).unwrap(),
            (None, Vec::new())
        );
        assert_eq!(
            select(true, None, true, &on, &["claude"]).unwrap(),
            (None, Vec::new())
        );
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
