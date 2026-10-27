//! Keys for backends run inside Docker Sandboxes (`sbx`). A sandboxed agent
//! never sees a host sign-in: sbx's proxy injects a key stored in sbx, and
//! only into domains approved for third-party kits in its `credentials.yaml`.
use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use super::{connection, Backend, Connection};

/// Opencode and Pi can use any of these model providers.
pub(crate) const MODEL_SERVICES: &[&str] = &[
    "anthropic",
    "openai",
    "google",
    "openrouter",
    "xai",
    "mistral",
    "groq",
];

/// The domains OMAR's sandbox kit injects each service's API key into.
const KEY_DOMAINS: &[(&str, &[&str])] = &[
    ("anthropic", &["api.anthropic.com"]),
    ("openai", &["api.openai.com"]),
    (
        "cursor",
        &["api2.cursor.sh", "api3.cursor.sh", "repo42.cursor.sh"],
    ),
    ("google", &["generativelanguage.googleapis.com"]),
    ("openrouter", &["openrouter.ai"]),
    ("xai", &["api.x.ai"]),
    ("mistral", &["api.mistral.ai"]),
    ("groq", &["api.groq.com"]),
];
/// ChatGPT sign-in for Codex: token refresh, then the Codex API.
const OPENAI_OAUTH_DOMAINS: &[&str] = &["auth.openai.com", "chatgpt.com"];

/// What sbx holds: services with a stored global secret, and services
/// approved for kits.
pub struct Keys {
    pub stored: Vec<String>,
    pub approved: Vec<String>,
}

/// `None` when sbx is not installed or does not answer.
pub fn keys() -> Option<Keys> {
    let output = connection::run("sbx", &["secret", "ls", "--global", "--json"])?;
    if !output.success {
        return None;
    }
    let listed: serde_json::Value = serde_json::from_str(&output.stdout).ok()?;
    let stored = listed["secrets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|secret| secret["name"].as_str().map(str::to_owned))
        .collect();
    let approved = credentials_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| parse_bindings(&text))
        .unwrap_or_default();
    Some(Keys { stored, approved })
}

/// Whether `backend` can run in a sandbox with the keys sbx holds.
pub fn status(backend: &dyn Backend, keys: Option<&Keys>) -> Connection {
    let Some(services) = backend.sandbox_services() else {
        return Connection::Unsupported;
    };
    if services.is_empty() {
        return Connection::Connected { via: None };
    }
    let Some(keys) = keys else {
        return Connection::NotInstalled;
    };
    let usable = |service: &&&str| {
        keys.stored.iter().any(|s| s == **service) && keys.approved.iter().any(|s| s == **service)
    };
    match services.iter().find(usable) {
        Some(service) => Connection::connected(*service),
        None => Connection::SignedOut,
    }
}

/// Every named backend can run sandboxed now; otherwise what to do about
/// the first that cannot. Checked when a run is submitted, before any VM.
#[allow(dead_code)] // Team sandboxes (#277) call it.
pub fn ready(names: &[&str]) -> Result<()> {
    let backends = names
        .iter()
        .map(|name| super::by_name(name).with_context(|| format!("unknown backend '{name}'")))
        .collect::<Result<Vec<_>>>()?;
    let needs_keys = backends
        .iter()
        .any(|backend| backend.sandbox_services() != Some(&[]));
    let keys = if needs_keys {
        Some(keys().context(
            "Docker Sandboxes (sbx) is not installed or not answering: https://docs.docker.com/ai/sandboxes/",
        )?)
    } else {
        None
    };
    for backend in backends {
        match status(backend, keys.as_ref()) {
            Connection::Connected { .. } => {}
            Connection::Unsupported => {
                bail!(
                    "{} agents cannot run in sandboxes yet",
                    backend.kind().name()
                )
            }
            _ => bail!(missing_key(backend, &connection::connection(backend))),
        }
    }
    Ok(())
}

/// Why a backend has no sandbox key, given how its host CLI is signed in.
fn missing_key(backend: &dyn Backend, host: &Connection) -> String {
    let name = backend.kind().name();
    match host {
        Connection::Connected { via: Some(via) } if super::claude::is_subscription(via) => format!(
            "{name} is signed in with a {via} subscription, which sandboxes cannot use; \
             give sandboxes an API key with `omar connect {name} --sandbox`"
        ),
        _ => format!("{name} has no key in Docker Sandboxes; run `omar connect {name} --sandbox`"),
    }
}

/// sbx's credential bindings file.
fn credentials_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .map(|dir| dir.join("sbx").join("credentials.yaml"))
}

/// Approve `service` for kits, unless it already is. Returns whether the
/// file changed.
pub fn approve(service: &str, oauth: bool) -> Result<bool> {
    let path = credentials_path().context("no home directory for sbx's credentials.yaml")?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let Some(updated) = with_binding(&text, service, oauth)? else {
        return Ok(false);
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, updated).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

/// `text` with a binding for `service` added, or `None` if it has one.
fn with_binding(text: &str, service: &str, oauth: bool) -> Result<Option<String>> {
    if parse_bindings(text).iter().any(|s| s == service) {
        return Ok(None);
    }
    let domains = KEY_DOMAINS
        .iter()
        .find(|(name, _)| *name == service)
        .map(|(_, domains)| *domains)
        .with_context(|| format!("OMAR's sandbox kit has no use for an sbx '{service}' key"))?;
    let mut entry = format!(
        "  {service}:\n    apiKey:\n      domains: [{}]\n",
        domains.join(", ")
    );
    if oauth {
        entry += &format!(
            "    oauth:\n      domains: [{}]\n",
            OPENAI_OAUTH_DOMAINS.join(", ")
        );
    }
    let mut lines: Vec<&str> = text.lines().collect();
    Ok(Some(
        match lines.iter().position(|line| *line == "bindings:") {
            Some(at) => {
                lines.insert(at + 1, entry.trim_end());
                lines.join("\n") + "\n"
            }
            None => {
                let mut out = text.to_owned();
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out + "bindings:\n" + &entry
            }
        },
    ))
}

/// Services approved for kits: the keys directly under `bindings:`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{of, Kind};

    fn keys(stored: &[&str], approved: &[&str]) -> Keys {
        Keys {
            stored: stored.iter().map(|s| s.to_string()).collect(),
            approved: approved.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_sandbox_key_counts_once_stored_and_approved() {
        let claude = of(Kind::Claude);
        assert_eq!(status(claude, None), Connection::NotInstalled);
        assert_eq!(
            status(claude, Some(&keys(&["anthropic"], &[]))),
            Connection::SignedOut
        );
        assert_eq!(
            status(claude, Some(&keys(&["anthropic"], &["anthropic"]))),
            Connection::connected("anthropic")
        );
        // Pi takes whichever model provider is there.
        assert_eq!(
            status(of(Kind::Pi), Some(&keys(&["groq"], &["groq", "openai"]))),
            Connection::connected("groq")
        );
        assert_eq!(
            status(of(Kind::Stub), None),
            Connection::Connected { via: None }
        );
        assert_eq!(status(of(Kind::Jev), None), Connection::Unsupported);
    }

    #[test]
    fn a_claude_subscription_is_told_to_bring_an_api_key() {
        let claude = of(Kind::Claude);
        assert_eq!(
            missing_key(claude, &Connection::connected("Claude Max")),
            "claude is signed in with a Claude Max subscription, which sandboxes cannot use; \
             give sandboxes an API key with `omar connect claude --sandbox`"
        );
        assert_eq!(
            missing_key(
                claude,
                &Connection::connected("API key (ANTHROPIC_API_KEY)")
            ),
            "claude has no key in Docker Sandboxes; run `omar connect claude --sandbox`"
        );
    }

    #[test]
    fn approving_a_service_keeps_the_operators_bindings() {
        let theirs = "# mine\nbindings:\n  github:\n    apiKey:\n      domains: [api.github.com]\n";
        let updated = with_binding(theirs, "anthropic", false).unwrap().unwrap();
        assert_eq!(parse_bindings(&updated), ["anthropic", "github"]);
        assert!(updated.starts_with("# mine\n"));
        assert!(updated.contains("  anthropic:\n    apiKey:\n      domains: [api.anthropic.com]\n"));
        assert!(with_binding(&updated, "anthropic", false)
            .unwrap()
            .is_none());

        let fresh = with_binding("", "openai", true).unwrap().unwrap();
        assert_eq!(
            fresh,
            "bindings:\n  openai:\n    apiKey:\n      domains: [api.openai.com]\n    oauth:\n      domains: [auth.openai.com, chatgpt.com]\n"
        );
        assert!(with_binding("", "github", false).is_err());
    }

    #[test]
    fn every_sandbox_service_has_domains() {
        for kind in crate::backend::ASSISTANT {
            for service in of(kind).sandbox_services().unwrap() {
                assert!(
                    KEY_DOMAINS.iter().any(|(name, _)| name == service),
                    "{service}"
                );
            }
        }
    }
}
