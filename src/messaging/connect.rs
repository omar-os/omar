//! One saved setup per platform; one fixed local runtime shared by both.
use super::{config::Config, credential_store, credentials, runtime::OmarRuntime};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    time::Duration,
};

pub const RUNTIME_URL: &str = "http://127.0.0.1:45073";

fn directory() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("Cannot locate user configuration directory")?
        .join("omar/messaging"))
}

fn prompt(label: &str) -> Result<String> {
    ensure!(
        io::stdin().is_terminal(),
        "First-time messaging setup needs an interactive terminal"
    );
    eprint!("{label}: ");
    io::stderr().flush()?;
    let mut value = String::new();
    ensure!(
        io::stdin().read_line(&mut value)? > 0,
        "Setup cancelled; no connection started"
    );
    Ok(value.trim().to_string())
}

fn ids(label: &str) -> Result<Vec<String>> {
    let input = prompt(label)?;
    let values: Vec<_> = input
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    ensure!(!values.is_empty(), "At least one approved ID is required");
    Ok(values)
}

fn legacy(cwd: &Path, platform: &str) -> Option<PathBuf> {
    let names: &[&str] = if platform == "slack" {
        &[
            "config.workflows.local.json",
            "config.research.local.json",
            "config.local.json",
        ]
    } else {
        &["config.local.json"]
    };
    names
        .iter()
        .map(|n| cwd.join("integrations").join(platform).join(n))
        .find(|p| p.is_file())
}

fn import(path: &Path, platform: &str) -> Result<Value> {
    let path = std::fs::canonicalize(path)?;
    let mut value: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    ensure!(
        value.is_object(),
        "Invalid existing messaging configuration"
    );
    let account = value
        .get("accountId")
        .or_else(|| value.get("workspaceId"))
        .cloned()
        .context("Missing account ID")?;
    let conversations = value
        .get("allowedConversations")
        .or_else(|| value.get("allowedChannels"))
        .cloned();
    value.as_object_mut().unwrap().retain(|key, _| {
        matches!(
            key.as_str(),
            "accountId" | "allowedUsers" | "allowedConversations"
        )
    });
    value["accountId"] = account;
    if platform == "slack" {
        value["allowedConversations"] = conversations.context("Missing Slack channel allowlist")?;
    } else {
        value
            .as_object_mut()
            .unwrap()
            .remove("allowedConversations");
    }
    value.as_object_mut().unwrap().remove("workspaceId");
    value.as_object_mut().unwrap().remove("allowedChannels");
    value["platform"] = json!(platform);
    value["runtimeUrl"] = json!(RUNTIME_URL);
    value["autoChat"] = json!(true);
    Ok(value)
}

async fn setup(platform: &str, path: &Path) -> Result<Config> {
    if path.exists() {
        // Validate protection before loading any saved setup.
        let _: Value = credential_store::read(path)?;
        let config = Config::load(path)?;
        ensure!(
            config.platform == platform && config.auto_chat && config.runtime_url == RUNTIME_URL,
            "Saved connect setup must use {RUNTIME_URL} with autoChat enabled"
        );
        return Ok(config);
    }
    let cwd = std::env::current_dir()?;
    let value = if let Some(existing) = legacy(&cwd, platform) {
        eprintln!(
            "Reusing existing {platform} settings from {}.",
            existing.display()
        );
        import(&existing, platform)?
    } else {
        let account = match platform {
            "slack" => super::slack::account_id().await?,
            "discord" => super::discord::account_id().await?,
            _ => anyhow::bail!("Unsupported messaging platform"),
        };
        let users = ids("Approved user IDs (comma-separated)")?;
        let channels = if platform == "slack" {
            ids("Approved Slack channel IDs (comma-separated)")?
        } else {
            Vec::new()
        };
        json!({"platform":platform,"accountId":account,"allowedUsers":users,
            "allowedConversations":channels,"runtimeUrl":RUNTIME_URL,"autoChat":true})
    };
    // Validate before saving a setup that subsequent launches would trust.
    let scratch = path.with_extension(format!("{}.json", uuid::Uuid::new_v4()));
    credential_store::save(&scratch, &value)?;
    let result = Config::load(&scratch);
    let _ = credential_store::remove(&scratch);
    let config = result?;
    credential_store::save(path, &value)?;
    eprintln!("Saved {platform} setup to {}.", path.display());
    Ok(config)
}

fn runtime_session(sessions: &[crate::sessions::Session]) -> Result<String> {
    let matches: Vec<_> = sessions
        .iter()
        .filter(|s| s.url.trim_end_matches('/') == RUNTIME_URL)
        .collect();
    ensure!(
        matches.len() <= 1,
        "Multiple saved runtimes use port 45073; select the intended runtime before connecting"
    );
    if let Some(session) = matches.first() {
        ensure!(
            matches!(session.state.as_str(), "stopped" | "failed" | "stale"),
            "Runtime '{}' is {}; check it with `omar ls` before connecting",
            session.name,
            session.state
        );
        return Ok(session.name.clone());
    }
    ensure!(!sessions.iter().any(|s|s.name=="messaging"),
        "Saved runtime 'messaging' uses another address; resolve it before connecting on port 45073");
    Ok("messaging".into())
}

async fn health(base: &str) -> Result<()> {
    let health = OmarRuntime::new(base.into())?.call("/health", None).await?;
    ensure!(
        health["status"] == "ok"
            && health["access"] == "full"
            && health["protocol_version"].is_number(),
        "Port 45073 is not a writable OMAR runtime"
    );
    Ok(())
}

async fn ensure_runtime() -> Result<()> {
    match tokio::net::TcpStream::connect("127.0.0.1:45073").await {
        Ok(_) => match health(RUNTIME_URL).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                // `down` may acknowledge before its HTTP listener closes. Only
                // proceed if the port actually becomes free; never replace a service.
                tokio::time::sleep(Duration::from_millis(250)).await;
                match tokio::net::TcpStream::connect("127.0.0.1:45073").await {
                    Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {}
                    _ => {
                        return Err(error)
                            .context("Port 45073 is occupied; no service was stopped or replaced")
                    }
                }
            }
        },
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {}
        Err(error) => return Err(error).context("Cannot check runtime port 45073"),
    }
    let sessions = tokio::task::spawn_blocking(crate::sessions::discover).await??;
    let name = runtime_session(&sessions)?;
    eprintln!("Starting OMAR runtime '{name}' at {RUNTIME_URL}.");
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command
        .args(["-s", &name, "up", "--port", "45073", "--no-ea", "--json"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    // Do not pin provider secrets into the runtime's saved environment.
    for key in ["SLACK_BOT_TOKEN", "SLACK_APP_TOKEN", "DISCORD_BOT_TOKEN"] {
        command.env_remove(key);
    }
    let output = tokio::time::timeout(Duration::from_secs(90), command.output())
        .await
        .context("Messaging runtime startup timed out")??;
    // A simultaneous Slack/Discord startup may have won the named-session claim.
    if !output.status.success() {
        health(RUNTIME_URL).await.with_context(|| {
            format!(
                "Could not start the messaging runtime: {}",
                String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(1000)
                    .collect::<String>()
            )
        })?;
    } else {
        health(RUNTIME_URL).await?;
    }
    Ok(())
}

struct ConnectorLock(std::fs::File);
impl Drop for ConnectorLock {
    fn drop(&mut self) {
        // A concurrently spawned child can briefly inherit this descriptor
        // before exec closes it. Unlock explicitly when this connector ends.
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            unsafe {
                libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

fn connector_lock(folder: &Path, platform: &str) -> Result<ConnectorLock> {
    credential_store::private_directory(folder, true)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(folder.join(format!("{platform}.lock")))?;
        let meta = file.metadata()?;
        ensure!(
            meta.is_file()
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.nlink() == 1
                && meta.permissions().mode() & 0o077 == 0,
            "Unsafe messaging connection lock"
        );
        use std::os::fd::AsRawFd;
        ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "A {platform} connect command is already running; use its terminal or stop it first"
        );
        Ok(ConnectorLock(file))
    }
    #[cfg(not(unix))]
    anyhow::bail!("Messaging connect currently requires Unix; use WSL on Windows")
}

pub async fn run(platform: &str) -> Result<()> {
    ensure!(
        matches!(platform, "slack" | "discord"),
        "Unsupported messaging platform"
    );
    let folder = directory()?;
    let _connection = connector_lock(&folder, platform)?;
    credentials::ensure(platform)?;
    let path = folder.join(format!("{platform}.json"));
    let config = setup(platform, &path).await?;
    ensure_runtime().await?;
    match platform {
        "slack" => super::slack::run(config).await,
        "discord" => super::discord::run(config).await,
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests;
