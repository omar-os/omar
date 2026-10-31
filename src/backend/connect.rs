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
    connect(backend, &with_install_path())
}

/// `omar connect` on a terminal: pick a backend with the arrow keys, connect
/// it, and come back to the list for the next one.
pub fn wizard() -> Result<()> {
    let original = with_install_path();
    let mut selected = 0;
    loop {
        println!("{}", "Checking backends…".dim());
        let statuses = connection::assistants();
        let mut stdout = std::io::stdout();
        stdout.queue(cursor::MoveToPreviousLine(1))?;
        stdout.queue(terminal::Clear(terminal::ClearType::FromCursorDown))?;
        let Some(index) = pick(&statuses, selected)? else {
            return Ok(());
        };
        selected = index;
        if let Err(error) = connect(statuses[index].0, &original) {
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

/// The index chosen, or `None` to quit. Drawn inline, so what each sign-in
/// prints stays in the scrollback above the next list.
fn pick(statuses: &[(&dyn Backend, Connection)], selected: usize) -> Result<Option<usize>> {
    let _raw = RawMode::enable()?;
    let mut stdout = std::io::stdout();
    let mut selected = selected.min(statuses.len().saturating_sub(1));
    let mut drawn = 0;
    loop {
        let lines = menu(statuses, selected);
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
        let choice = match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                selected = selected.checked_sub(1).unwrap_or(statuses.len() - 1);
                continue;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                selected = (selected + 1) % statuses.len();
                continue;
            }
            KeyCode::Enter => Some(selected),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => None,
            KeyCode::Esc | KeyCode::Char('q') => None,
            _ => continue,
        };
        // Leave one line behind saying what was picked.
        stdout.queue(cursor::MoveToPreviousLine(drawn))?;
        stdout.queue(terminal::Clear(terminal::ClearType::FromCursorDown))?;
        let summary = match choice {
            Some(index) => format!("Connecting {}", statuses[index].0.kind().name()),
            None => "Done".to_string(),
        };
        write!(stdout, "{} {summary}\r\n", "◇".green())?;
        stdout.flush()?;
        return Ok(choice);
    }
}

/// The picker's lines, `selected` marked.
fn menu(statuses: &[(&dyn Backend, Connection)], selected: usize) -> Vec<String> {
    let mut lines = vec![format!(
        "{} Connect an agent backend  {}",
        "◆".cyan(),
        "↑/↓ move · enter connect · q quit".dim()
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
    lines.push(format!("{}", "└".cyan()));
    lines
}

/// Add the installers' directories to `PATH` and return what it was.
fn with_install_path() -> OsString {
    let original = std::env::var_os("PATH").unwrap_or_default();
    // Installers drop binaries here, often before the shell's PATH has them.
    std::env::set_var("PATH", with_install_dirs(&original));
    original
}

/// Install `backend` if needed and sign it in; `original` is the operator's
/// own `PATH`, before the installers' directories were added.
fn connect(backend: &dyn Backend, original: &OsString) -> Result<()> {
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
        if found_in(program, original).is_none() {
            println!(
                "Add {} to PATH so OMAR finds {name} from any shell.",
                dir.display()
            );
        }
    }
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
