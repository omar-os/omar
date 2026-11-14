//! `omar connect`: install a backend's CLI with its vendor's own installer,
//! then let the CLI sign itself in. Credentials stay where each CLI keeps
//! them; OMAR never stores or sends one. Status comes from each CLI, except
//! pi's, which has no status command: OMAR lists the provider names in its
//! `auth.json` and keeps nothing else.
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, ensure, Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::Stylize;
use crossterm::{cursor, terminal, QueueableCommand};

use super::{connection, Backend, Connection, SignInHome};

/// Connect `backend` on this machine; `replace` signs in again even when
/// its sign-in works.
pub fn host(backend: &dyn Backend, replace: bool) -> Result<()> {
    connect(backend, replace)
}

/// `omar connect` on a terminal: pick a backend with the arrow keys, connect,
/// replace or disconnect its sign-in, or connect it for sandboxes, and come
/// back to the list for the next one.
pub fn wizard(sandbox: &crate::sandbox::Config) -> Result<()> {
    let mut selected = 0;
    loop {
        println!("{}", "Checking backends…".dim());
        let (statuses, sandboxes) = connection::assistants_and_sandboxes();
        let mut stdout = std::io::stdout();
        stdout.queue(cursor::MoveToPreviousLine(1))?;
        stdout.queue(terminal::Clear(terminal::ClearType::FromCursorDown))?;
        let outcome = match pick(&statuses, &sandboxes, selected)? {
            Choice::Quit => return Ok(()),
            Choice::Connect(index, replace) => {
                selected = index;
                connect(statuses[index].0, replace)
            }
            Choice::Disconnect(index) => {
                selected = index;
                disconnect(statuses[index].0)
            }
            Choice::Sandbox(index) => {
                selected = index;
                crate::sandbox::connect(statuses[index].0, sandbox, false)
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
    /// Connect; `true` replaces a sign-in it already has.
    Connect(usize, bool),
    Disconnect(usize),
    Sandbox(usize),
    Quit,
}

/// Whether sbx is here, so sandboxes are worth showing at all.
fn sandboxed(sandboxes: &[Connection]) -> bool {
    sandboxes
        .iter()
        .any(|sandbox| *sandbox != Connection::NotInstalled)
}

/// What the picker asks before acting on a connected backend.
#[derive(Clone, Copy, PartialEq)]
enum Ask {
    Replace,
    SignOut,
}

/// What to do next. Drawn inline, so what each sign-in prints stays in the
/// scrollback above the next list.
fn pick(
    statuses: &[(&dyn Backend, Connection)],
    sandboxes: &[Connection],
    selected: usize,
) -> Result<Choice> {
    let _raw = RawMode::enable()?;
    let mut stdout = std::io::stdout();
    let mut selected = selected.min(statuses.len().saturating_sub(1));
    let mut confirming = None;
    // How wide each drawn line was; a resize redraws.
    let mut drawn: Vec<usize> = Vec::new();
    loop {
        let lines = menu(statuses, sandboxes, selected, confirming);
        up_over(&mut stdout, &drawn)?;
        for line in &lines {
            write!(stdout, "{line}\r\n")?;
        }
        stdout.flush()?;
        drawn = lines.iter().map(|line| visible_width(line)).collect();
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        // Only `y` answers a question; any other key drops it.
        let answering = confirming.take();
        let connected = matches!(statuses[selected].1, Connection::Connected { .. });
        let choice = match key.code {
            KeyCode::Char('y') if answering == Some(Ask::Replace) => {
                Choice::Connect(selected, true)
            }
            KeyCode::Char('y') if answering == Some(Ask::SignOut) => Choice::Disconnect(selected),
            _ if answering.is_some() => continue,
            KeyCode::Char('d') => {
                confirming = connected.then_some(Ask::SignOut);
                continue;
            }
            KeyCode::Char('s') if sandboxed(sandboxes) => Choice::Sandbox(selected),
            KeyCode::Up | KeyCode::Char('k') => {
                selected = selected.checked_sub(1).unwrap_or(statuses.len() - 1);
                continue;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                selected = (selected + 1) % statuses.len();
                continue;
            }
            // A working sign-in is only replaced once asked.
            KeyCode::Enter if connected => {
                confirming = Some(Ask::Replace);
                continue;
            }
            KeyCode::Enter => Choice::Connect(selected, false),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Choice::Quit,
            KeyCode::Esc | KeyCode::Char('q') => Choice::Quit,
            _ => continue,
        };
        // Leave one line behind saying what was picked.
        up_over(&mut stdout, &drawn)?;
        let summary = match choice {
            Choice::Connect(index, false) => {
                format!("Connecting {}", statuses[index].0.kind().name())
            }
            Choice::Connect(index, true) => {
                format!("Replacing {}'s sign-in", statuses[index].0.kind().name())
            }
            Choice::Disconnect(index) => {
                format!("Disconnecting {}", statuses[index].0.kind().name())
            }
            Choice::Sandbox(index) => {
                format!(
                    "Connecting {} for sandboxes",
                    statuses[index].0.kind().name()
                )
            }
            Choice::Quit => "Done".to_string(),
        };
        write!(stdout, "{} {summary}\r\n", "◇".green())?;
        stdout.flush()?;
        return Ok(choice);
    }
}

/// Move back to the top of lines of these widths and clear them. A line
/// wider than the terminal wraps onto more rows, at today's width.
fn up_over(stdout: &mut std::io::Stdout, widths: &[usize]) -> Result<()> {
    let columns = terminal::size().map(|(columns, _)| columns).unwrap_or(80);
    let rows = rows(widths, columns);
    if rows > 0 {
        stdout.queue(cursor::MoveToPreviousLine(rows))?;
    }
    stdout.queue(terminal::Clear(terminal::ClearType::FromCursorDown))?;
    Ok(())
}

fn rows(widths: &[usize], columns: u16) -> u16 {
    let columns = usize::from(columns.max(1));
    let rows: usize = widths
        .iter()
        .map(|width| width.div_ceil(columns).max(1))
        .sum();
    rows.try_into().unwrap_or(u16::MAX)
}

/// Columns a styled line takes: its characters, without colour codes.
fn visible_width(line: &str) -> usize {
    let mut width = 0;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // CSI: ESC [ … final byte in @..~
            chars.find(|c| ('@'..='~').contains(c) && *c != '[');
        } else {
            width += 1;
        }
    }
    width
}

/// The picker's lines, `selected` marked; `confirming` asks before replacing
/// or signing out its sign-in.
fn menu(
    statuses: &[(&dyn Backend, Connection)],
    sandboxes: &[Connection],
    selected: usize,
    confirming: Option<Ask>,
) -> Vec<String> {
    let sandboxed = sandboxed(sandboxes);
    let keys = if sandboxed {
        "↑/↓ move · enter connect · s sandboxes · d disconnect · q quit"
    } else {
        "↑/↓ move · enter connect · d disconnect · q quit"
    };
    let mut lines = vec![format!(
        "{} Connect an agent backend  {}",
        "◆".cyan(),
        keys.dim()
    )];
    for (index, (backend, status)) in statuses.iter().enumerate() {
        let name = format!("{:<10}", backend.kind().name());
        let label = connection::label(status);
        let label = match status {
            Connection::Connected { .. } => label.green(),
            _ => label.yellow(),
        };
        let label = match sandboxes.get(index) {
            Some(sandbox) if sandboxed => {
                format!("{label}{}", connection::sandbox_note(sandbox, true).dim())
            }
            _ => label.to_string(),
        };
        lines.push(if index == selected {
            format!("{} {} {name}{label}", "│".cyan(), "●".cyan())
        } else {
            format!("{} {} {}{label}", "│".cyan(), "○".dim(), name.dim())
        });
    }
    let name = statuses[selected].0.kind().name();
    lines.push(match confirming {
        Some(Ask::Replace) => format!("{} Replace {name}'s sign-in? {}", "└".cyan(), "y / n".dim()),
        Some(Ask::SignOut) => format!("{} Sign {name} out? {}", "└".cyan(), "y / n".dim()),
        None => format!("{}", "└".cyan()),
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
fn connect(backend: &dyn Backend, replace: bool) -> Result<()> {
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
    let mut failed = String::new();
    // Stored is not working: a rejected key still reads as connected, and
    // signing in again is how it is replaced.
    let mut checked = false;
    if matches!(status, Connection::Connected { .. }) && !replace {
        match check(backend) {
            Ok(()) => checked = true,
            Err(error) => println!("{error}\nSigning {name} in again."),
        }
    }
    // Lives until the end, so the status and check below see the new home.
    let staged = match backend.sign_in_home() {
        Some(home) if replace && status != Connection::SignedOut => {
            Some(Staged::begin(home, name)?)
        }
        _ => None,
    };
    if !checked {
        let login = backend
            .login(headless())
            .with_context(|| format!("{name} has no sign-in OMAR can start"))?;
        if let Some(note) = login.note {
            println!("{note}");
        }
        // The CLI's own flow, on the operator's terminal.
        let exit = Command::new(program)
            .args(login.args)
            .status()
            .with_context(|| format!("starting `{program}`"))?;
        if !exit.success() {
            failed = format!(" (its sign-in {exit})");
        }
        status = connection::connection(backend);
    }
    match status {
        Connection::Connected { via } => {
            if !checked {
                check(backend).map_err(|error| {
                    anyhow::anyhow!("{error}; run `omar connect {name}` to sign in again")
                })?;
            }
            if let Some(staged) = &staged {
                staged.keep(name)?;
            }
            println!(
                "✓ {name} connected{}",
                via.map(|v| format!(" ({v})")).unwrap_or_default()
            );
            if checked {
                println!("To sign in another way: omar connect {name} --replace");
            }
        }
        Connection::Unknown { reason } => {
            bail!("could not confirm {name} is signed in{failed}: {reason}")
        }
        _ => bail!("{name} is still not signed in; run `omar connect {name}` again"),
    }
    if let Some(dir) = found_in(program, &std::env::var_os("PATH").unwrap_or_default()) {
        if found_in(program, &shell_path()).is_none() {
            add_to_shell_path(&dir, name);
        }
    }
    Ok(())
}

/// A replace signing in under a fresh home, for a CLI whose login drops the
/// old sign-in as it starts: the old one stays until [`Staged::keep`]. The
/// CLI finds the fresh home through its environment variable, set for this
/// process and every command it starts until dropped.
struct Staged {
    home: SignInHome,
    fresh: PathBuf,
    previous: Option<OsString>,
}

impl Staged {
    fn begin(home: SignInHome, name: &str) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let fresh = crate::sessions::state_root().join("sign-in").join(name);
        // Left by a replace that was killed.
        let _ = std::fs::remove_dir_all(&fresh);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&fresh)
            .with_context(|| format!("cannot make {}", fresh.display()))?;
        let previous = std::env::var_os(home.var);
        std::env::set_var(home.var, &fresh);
        println!("Your current {name} sign-in stays until the new one works.");
        Ok(Staged {
            home,
            fresh,
            previous,
        })
    }

    /// Put the new sign-in in place of the old one.
    fn keep(&self, name: &str) -> Result<()> {
        for file in self.home.files {
            ensure!(
                self.fresh.join(file).is_file(),
                "{name} signed in, but not into {file}, so OMAR cannot move it; your earlier sign-in is unchanged"
            );
        }
        std::fs::create_dir_all(&self.home.dir)?;
        for file in self.home.files {
            // Copied beside the old file, then renamed over it in one step.
            let next = self.home.dir.join(format!("{file}.omar-new"));
            std::fs::copy(self.fresh.join(file), &next)
                .with_context(|| format!("cannot write {}", next.display()))?;
            std::fs::rename(&next, self.home.dir.join(file))
                .with_context(|| format!("cannot replace {file} in {}", self.home.dir.display()))?;
        }
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.home.var, value),
            None => std::env::remove_var(self.home.var),
        }
        let _ = std::fs::remove_dir_all(&self.fresh);
    }
}

/// `connection::check_sign_in`, said as it happens; the error names the
/// environment variable the CLI signs in with, if one is set.
fn check(backend: &dyn Backend) -> Result<(), String> {
    let name = backend.kind().name();
    println!("Checking {name} with a test prompt…");
    connection::check_sign_in(backend).map_err(|error| {
        let with = connection::env_credential(backend.credential_vars())
            .map(|var| format!(" with ${var}"))
            .unwrap_or_default();
        format!("{name} is signed in{with}, but its test prompt failed: {error}")
    })
}

/// Have `backend`'s CLI sign itself out, then check that it took.
pub fn disconnect(backend: &dyn Backend) -> Result<()> {
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
        Connection::SignedOut | Connection::NotInstalled => println!("✓ {name} disconnected"),
        Connection::Unknown { reason } => {
            bail!("could not confirm {name} is signed out: {reason}")
        }
        // A credential in the environment outlives any sign-out.
        Connection::Connected { .. } => match connection::env_credential(backend.credential_vars())
        {
            Some(var) => bail!("{name} still connects with ${var}; unset it to disconnect"),
            None => bail!("{name} is still signed in"),
        },
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
    fn a_redraw_moves_up_over_every_wrapped_row() {
        use crossterm::style::Stylize;
        let line = format!("{} {}", "│".cyan(), "codex".dim());
        assert_eq!(visible_width(&line), 7);
        // The heading is 78 columns: one row at 80, two at 60.
        let lines = menu_widths();
        assert_eq!(rows(&lines, 80), lines.len() as u16);
        assert_eq!(rows(&lines, 60), lines.len() as u16 + 1);
        // A blank line still takes a row; an exact fit takes one.
        assert_eq!(rows(&[0, 60], 60), 2);
    }

    fn menu_widths() -> Vec<usize> {
        let statuses: Vec<_> = crate::backend::ASSISTANT
            .iter()
            .map(|kind| (crate::backend::of(*kind), Connection::SignedOut))
            .collect();
        let sandboxes = vec![Connection::NotInstalled; statuses.len()];
        menu(&statuses, &sandboxes, 0, None)
            .iter()
            .map(|line| visible_width(line))
            .collect()
    }

    #[test]
    fn install_dirs_come_after_the_operators_own() {
        // Reads HOME, which other tests repoint at a temp dir.
        let _env = crate::test_env_lock();
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
