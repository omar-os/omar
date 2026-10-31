//! `omar connect`: install a backend's CLI with its vendor's own installer,
//! then let the CLI sign itself in. Credentials stay where each CLI keeps
//! them; OMAR never reads or stores one.
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, ensure, Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::Stylize;
use crossterm::{cursor, terminal, QueueableCommand};

use super::{connection, Backend, Connection};

/// Connect `backend` on this machine.
pub fn host(backend: &dyn Backend) -> Result<()> {
    connect(backend)
}

/// `omar connect` on a terminal: pick a backend with the arrow keys, connect
/// or disconnect it, and come back to the list for the next one.
pub fn wizard() -> Result<()> {
    let mut selected = 0;
    loop {
        println!("{}", "Checking backends…".dim());
        let statuses = connection::assistants();
        let mut stdout = std::io::stdout();
        stdout.queue(cursor::MoveToPreviousLine(1))?;
        stdout.queue(terminal::Clear(terminal::ClearType::FromCursorDown))?;
        let outcome = match pick(&statuses, selected)? {
            Choice::Quit => return Ok(()),
            Choice::Connect(index) => {
                selected = index;
                connect(statuses[index].0)
            }
            Choice::Disconnect(index) => {
                selected = index;
                disconnect(statuses[index].0)
            }
        };
        if let Err(error) = outcome {
            println!("{} {error:#}", "✗".red());
        }
        println!();
    }
}

/// Raw mode for the picker, undone however it exits.
struct RawMode;

impl RawMode {
    fn enable() -> Result<Self> {
        terminal::enable_raw_mode()?;
        std::io::stdout().queue(cursor::Hide)?.flush()?;
        Ok(RawMode)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = std::io::stdout()
            .queue(cursor::Show)
            .and_then(|out| out.flush());
        let _ = terminal::disable_raw_mode();
    }
}

enum Choice {
    Connect(usize),
    Disconnect(usize),
    Quit,
}

/// What to do next. Drawn inline, so what each sign-in prints stays in the
/// scrollback above the next list.
fn pick(statuses: &[(&dyn Backend, Connection)], selected: usize) -> Result<Choice> {
    let _raw = RawMode::enable()?;
    let mut stdout = std::io::stdout();
    let mut selected = selected.min(statuses.len().saturating_sub(1));
    let mut confirming = false;
    let mut drawn = 0;
    loop {
        let lines = menu(statuses, selected, confirming);
        if drawn > 0 {
            stdout.queue(cursor::MoveToPreviousLine(drawn))?;
        }
        stdout.queue(terminal::Clear(terminal::ClearType::FromCursorDown))?;
        for line in &lines {
            write!(stdout, "{line}\r\n")?;
        }
        stdout.flush()?;
        drawn = lines.len() as u16;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        // Only `y` answers the sign-out question; any other key drops it.
        let answering = std::mem::take(&mut confirming);
        let choice = match key.code {
            KeyCode::Char('y') if answering => Choice::Disconnect(selected),
            _ if answering => continue,
            KeyCode::Char('d') => {
                confirming = matches!(statuses[selected].1, Connection::Connected { .. });
                continue;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                selected = selected.checked_sub(1).unwrap_or(statuses.len() - 1);
                continue;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                selected = (selected + 1) % statuses.len();
                continue;
            }
            KeyCode::Enter => Choice::Connect(selected),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Choice::Quit,
            KeyCode::Esc | KeyCode::Char('q') => Choice::Quit,
            _ => continue,
        };
        // Leave one line behind saying what was picked.
        stdout.queue(cursor::MoveToPreviousLine(drawn))?;
        stdout.queue(terminal::Clear(terminal::ClearType::FromCursorDown))?;
        let summary = match choice {
            Choice::Connect(index) => format!("Connecting {}", statuses[index].0.kind().name()),
            Choice::Disconnect(index) => {
                format!("Disconnecting {}", statuses[index].0.kind().name())
            }
            Choice::Quit => "Done".to_string(),
        };
        write!(stdout, "{} {summary}\r\n", "◇".green())?;
        stdout.flush()?;
        return Ok(choice);
    }
}

/// The picker's lines, `selected` marked; `confirming` asks before signing
/// it out.
fn menu(statuses: &[(&dyn Backend, Connection)], selected: usize, confirming: bool) -> Vec<String> {
    let mut lines = vec![format!(
        "{} Connect an agent backend  {}",
        "◆".cyan(),
        "↑/↓ move · enter connect · d disconnect · q quit".dim()
    )];
    for (index, (backend, status)) in statuses.iter().enumerate() {
        let name = format!("{:<10}", backend.kind().name());
        let label = connection::label(status);
        let label = match status {
            Connection::Connected { .. } => label.green(),
            _ => label.yellow(),
        };
        lines.push(if index == selected {
            format!("{} {} {name}{label}", "│".cyan(), "●".cyan())
        } else {
            format!("{} {} {}{label}", "│".cyan(), "○".dim(), name.dim())
        });
    }
    lines.push(if confirming {
        let name = statuses[selected].0.kind().name();
        format!("{} Sign {name} out? {}", "└".cyan(), "y / n".dim())
    } else {
        format!("{}", "└".cyan())
    });
    lines
}

/// `PATH` as the operator's shell gave it, before [`extend_path`].
static SHELL_PATH: std::sync::OnceLock<OsString> = std::sync::OnceLock::new();

/// Let OMAR find CLIs in the directories vendor installers use, even when
/// the operator's shell doesn't have them on `PATH` yet. Call once, first.
pub fn extend_path() {
    let original = std::env::var_os("PATH").unwrap_or_default();
    std::env::set_var("PATH", with_install_dirs(&original));
    let _ = SHELL_PATH.set(original);
}

fn shell_path() -> OsString {
    SHELL_PATH
        .get()
        .cloned()
        .unwrap_or_else(|| std::env::var_os("PATH").unwrap_or_default())
}

/// Install `backend` if needed and sign it in.
fn connect(backend: &dyn Backend) -> Result<()> {
    let name = backend.kind().name();
    let mut status = connection::connection(backend);
    if status == Connection::NotInstalled {
        install(backend)?;
        status = connection::connection(backend);
        ensure!(
            status != Connection::NotInstalled,
            "{name} installed, but `{}` still does not run; open a new shell and run `omar connect {name}` again",
            program(backend)
        );
    }
    // Resolved after installing: an installer can change which one runs.
    let program = program(backend);
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
        if found_in(program, &shell_path()).is_none() {
            add_to_shell_path(&dir, name);
        }
    }
    Ok(())
}

/// Have `backend`'s CLI sign itself out, then check that it took.
fn disconnect(backend: &dyn Backend) -> Result<()> {
    let name = backend.kind().name();
    let logout = backend
        .logout()
        .with_context(|| format!("{name} has no sign-out OMAR can start"))?;
    if let Some(note) = logout.note {
        println!("{note}");
    }
    let program = program(backend);
    Command::new(program)
        .args(logout.args)
        .status()
        .with_context(|| format!("starting `{program}`"))?;
    match connection::connection(backend) {
        // A credential in the environment outlives any sign-out.
        Connection::Connected { via: Some(via) } if std::env::var_os(&via).is_some() => {
            bail!("{name} still connects with ${via}; unset it to disconnect")
        }
        Connection::Connected { .. } => bail!("{name} is still signed in"),
        _ => println!("✓ {name} disconnected"),
    }
    Ok(())
}

/// Put `dir` on the operator's shell `PATH` from the next shell on, through
/// the startup file their shell reads; say how when the shell is unknown.
fn add_to_shell_path(dir: &Path, name: &str) {
    let home = dirs::home_dir().unwrap_or_default();
    let shown = match dir.strip_prefix(&home) {
        Ok(rest) => format!("$HOME/{}", rest.display()),
        Err(_) => dir.display().to_string(),
    };
    let rc = match std::env::var("SHELL")
        .unwrap_or_default()
        .rsplit('/')
        .next()
    {
        Some("bash") => Some(home.join(".bashrc")),
        Some("zsh") => Some(
            std::env::var_os("ZDOTDIR")
                .map(PathBuf::from)
                .unwrap_or(home.clone())
                .join(".zshrc"),
        ),
        _ => None,
    };
    let line = format!("export PATH=\"$PATH:{shown}\"");
    let Some(rc) = rc else {
        println!("Add {} to PATH so your shell finds {name}.", dir.display());
        return;
    };
    let existing = std::fs::read_to_string(&rc).unwrap_or_default();
    if !existing.contains(&line) {
        let appended = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&rc)
            .and_then(|mut file| {
                let gap = if existing.is_empty() || existing.ends_with('\n') {
                    ""
                } else {
                    "\n"
                };
                writeln!(file, "{gap}\n# Added by omar connect\n{line}")
            });
        if appended.is_err() {
            println!("Add {} to PATH so your shell finds {name}.", dir.display());
            return;
        }
    }
    println!(
        "Added {} to PATH in {}; open a new shell, or run: source {}",
        dir.display(),
        rc.display(),
        rc.display()
    );
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
    fn every_assistant_can_be_installed_signed_in_and_out() {
        for kind in super::super::ASSISTANT {
            let backend = super::super::of(kind);
            assert!(backend.install_command().is_some(), "{kind:?}");
            assert!(backend.login(false).is_some(), "{kind:?}");
            assert!(backend.login(true).is_some(), "{kind:?}");
            assert!(backend.logout().is_some(), "{kind:?}");
        }
    }
}
