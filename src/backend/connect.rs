//! `omar connect`: install a backend's CLI with its vendor's own installer,
//! then let the CLI sign itself in. With `--sandbox`, give Docker Sandboxes
//! a key of their own instead: a host sign-in never crosses into one.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, ensure, Context, Result};

use super::{connection, sbx, Backend, Connection, Kind};

/// Connect `backend` on this machine.
pub fn host(backend: &dyn Backend) -> Result<()> {
    let name = backend.kind().name();
    let program = program(backend);
    let original = std::env::var_os("PATH").unwrap_or_default();
    // Installers drop binaries here, often before the shell's PATH has them.
    std::env::set_var("PATH", with_install_dirs(&original));

    let mut status = connection::connection(backend);
    if status == Connection::NotInstalled {
        install(backend)?;
        status = connection::connection(backend);
        ensure!(
            status != Connection::NotInstalled,
            "{name} installed, but `{program}` still does not run; open a new shell and run `omar connect {name}` again"
        );
    }
    if !matches!(status, Connection::Connected { .. }) {
        let login = backend
            .login(headless())
            .with_context(|| format!("{name} has no sign-in OMAR can start"))?;
        if let Some(note) = login.note {
            println!("{note}");
        }
        // The CLI's own flow, on the operator's terminal.
        Command::new(program)
            .args(login.args)
            .status()
            .with_context(|| format!("starting `{program}`"))?;
        status = connection::connection(backend);
    }
    match status {
        Connection::Connected { via } => {
            println!(
                "✓ {name} connected{}",
                via.map(|v| format!(" ({v})")).unwrap_or_default()
            );
        }
        Connection::Unknown { reason } => println!("? {name}: {reason}"),
        _ => bail!("{name} is still not signed in; run `omar connect {name}` again"),
    }
    if let Some(dir) = found_in(program, &std::env::var_os("PATH").unwrap_or_default()) {
        if found_in(program, &original).is_none() {
            println!(
                "Add {} to PATH so OMAR finds {name} from any shell.",
                dir.display()
            );
        }
    }
    Ok(())
}

/// Store a key in sbx for `backend`'s sandboxed agents, and approve it for
/// OMAR's kit. `service` picks the provider for Opencode and Pi; `api_key`
/// makes Codex use an OpenAI key over a ChatGPT sign-in.
pub fn sandbox(backend: &dyn Backend, service: Option<&str>, api_key: bool) -> Result<()> {
    let name = backend.kind().name();
    let Some(services) = backend.sandbox_services() else {
        bail!("{name} agents cannot run in sandboxes yet");
    };
    let Some(default) = services.first() else {
        println!("✓ {name} needs no key in sandboxes");
        return Ok(());
    };
    let service = service.unwrap_or(default);
    ensure!(
        services.contains(&service),
        "{name} in a sandbox uses one of: {}",
        services.join(", ")
    );
    let oauth = backend.kind() == Kind::Codex && !api_key;
    let keys = sbx::keys().context(
        "Docker Sandboxes (sbx) is not installed or not answering: https://docs.docker.com/ai/sandboxes/",
    )?;
    if keys.stored.iter().any(|s| s == service) {
        println!("Using the {service} key already in sbx.");
    } else {
        let mut set = Command::new("sbx");
        set.args(["secret", "set", service]);
        if oauth {
            // Separate from the host's codex sign-in: two holders of one
            // refresh token keep invalidating each other.
            println!("Sign in to ChatGPT again, for sandboxes. If the link cannot reach this machine, use `omar connect codex --sandbox --api-key`.");
            set.arg("--oauth");
        }
        let status = set.status().context("starting `sbx secret set`")?;
        ensure!(status.success(), "`sbx secret set {service}` failed");
    }
    if sbx::approve(service, oauth)? {
        println!("Approved {service} for OMAR's sandbox kit in sbx's credentials.yaml.");
    }
    println!("✓ {name} can run in sandboxes ({service})");
    Ok(())
}

/// The executable the backend's default command runs.
fn program(backend: &dyn Backend) -> &'static str {
    let command = backend.default_command();
    command.split_whitespace().next().unwrap_or(command)
}

fn install(backend: &dyn Backend) -> Result<()> {
    let name = backend.kind().name();
    let script = backend
        .install_command()
        .with_context(|| format!("OMAR cannot install {name}"))?;
    if script.starts_with("npm ")
        && found_in("npm", &std::env::var_os("PATH").unwrap_or_default()).is_none()
    {
        bail!("{name} installs with npm; install Node.js first: https://nodejs.org");
    }
    println!("Installing {name}: {script}");
    let status = Command::new("sh")
        .args(["-c", script])
        .status()
        .context("starting sh")?;
    ensure!(status.success(), "installing {name} failed");
    if backend.kind() == Kind::Cursor {
        super::cursor::link_cursor_command()?;
    }
    Ok(())
}

/// `PATH` with the directories vendor installers use appended, so a binary
/// already on the operator's PATH still wins.
fn with_install_dirs(path: &OsString) -> OsString {
    let mut dirs: Vec<PathBuf> = std::env::split_paths(path).collect();
    if let Some(home) = dirs::home_dir() {
        for dir in [home.join(".local/bin"), home.join(".opencode/bin")] {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    std::env::join_paths(dirs).unwrap_or_else(|_| path.clone())
}

/// The directory on `path` holding an executable `program`.
fn found_in(program: &str, path: &OsString) -> Option<PathBuf> {
    std::env::split_paths(path).find(|dir| is_executable(&dir.join(program)))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// No browser can open here: an SSH session, or Linux with no display.
fn headless() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some()
        || (cfg!(target_os = "linux")
            && std::env::var_os("DISPLAY").is_none()
            && std::env::var_os("WAYLAND_DISPLAY").is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_dirs_come_after_the_operators_own() {
        let home = dirs::home_dir().unwrap();
        let path = with_install_dirs(&OsString::from("/usr/bin"));
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert_eq!(dirs[0], PathBuf::from("/usr/bin"));
        assert!(dirs.contains(&home.join(".local/bin")));
        assert!(dirs.contains(&home.join(".opencode/bin")));
        // Already present: not added twice.
        assert_eq!(with_install_dirs(&path), path);
    }

    #[test]
    fn every_assistant_can_be_installed_and_signed_in() {
        for kind in super::super::ASSISTANT {
            let backend = super::super::of(kind);
            assert!(backend.install_command().is_some(), "{kind:?}");
            assert!(backend.login(false).is_some(), "{kind:?}");
            assert!(backend.login(true).is_some(), "{kind:?}");
        }
    }
}
