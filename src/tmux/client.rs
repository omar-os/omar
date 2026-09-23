use anyhow::{anyhow, Context, Result};
use std::io::Write;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use super::Session;

/// Bound how long a newly launched backend may take to publish its channel.
#[derive(Debug, Clone)]
pub struct DeliveryOptions {
    pub startup_timeout: Duration,
    pub poll_interval: Duration,
}

impl Default for DeliveryOptions {
    fn default() -> Self {
        Self {
            startup_timeout: Duration::from_secs(15),
            poll_interval: Duration::from_millis(100),
        }
    }
}

fn tail_pane_lines(output: String, lines: i32) -> String {
    if lines <= 0 {
        return output;
    }

    let limit = lines as usize;
    let had_trailing_newline = output.ends_with('\n');
    let mut selected: Vec<&str> = output.lines().rev().take(limit).collect();
    selected.reverse();

    let mut capped = selected.join("\n");
    if had_trailing_newline {
        capped.push('\n');
    }
    capped
}

/// Prefer the launching terminal's geometry; headless launches use a modest
/// 120x40 pane. A forced 200-column canvas makes the first ordinary attachment
/// rewrap a large amount of TUI output before the backend can redraw.
fn agent_dimensions(terminal: Option<(u16, u16)>) -> (u16, u16) {
    terminal
        .filter(|(cols, rows)| (2..=1000).contains(cols) && (2..=1000).contains(rows))
        .unwrap_or((120, 40))
}

/// Session-environment key holding the backend a session was launched with.
const SESSION_BACKEND_VAR: &str = "OMAR_BACKEND";

/// Session-environment key describing the backend's side channel, if it has one.
const SESSION_DELIVERY_VAR: &str = "OMAR_DELIVERY";

#[derive(Debug, Clone)]
pub struct TmuxClient {
    prefix: String,
}

/// An agent name as tmux will store it.
///
/// tmux reads `.` in a target as the window/pane separator, so a name carrying
/// one has to be flattened first: `new-session -s a.b` silently creates `a_b`,
/// and every later lookup for `a.b` then misses. Team instances qualify their
/// agents as `instance.agent`, which makes this the only spelling that
/// survives a round trip.
pub fn flatten_agent_name(agent: &str) -> String {
    agent.replace('.', "_")
}

#[cfg(test)]
thread_local! {
    pub(super) static TEST_TMUX: std::cell::RefCell<Option<std::path::PathBuf>> = const { std::cell::RefCell::new(None) };
}

pub fn tmux_command() -> Command {
    #[cfg(test)]
    if let Some(path) = TEST_TMUX.with(|path| path.borrow().clone()) {
        return Command::new(path);
    }
    let mut cmd = Command::new("tmux");
    if let Ok(server) = std::env::var("OMAR_TMUX_SERVER") {
        let server = server.trim();
        if !server.is_empty() {
            cmd.args(["-L", server]);
        }
    }
    cmd
}

fn exact_session_target(target: &str) -> String {
    if target.starts_with('=') || target.contains(':') || target.contains('.') {
        target.to_string()
    } else {
        format!("={target}")
    }
}

fn exact_pane_target(target: &str) -> String {
    if target.contains(':') || target.contains('.') {
        target.to_string()
    } else if target.starts_with('=') {
        format!("{target}:")
    } else {
        format!("={target}:")
    }
}

fn popup_attach_command(target: &str) -> String {
    let tmux_server = std::env::var("OMAR_TMUX_SERVER")
        .ok()
        .map(|server| server.trim().to_string())
        .filter(|server| !server.is_empty());
    popup_attach_command_with_server(target, tmux_server.as_deref())
}

fn popup_attach_command_with_server(target: &str, tmux_server: Option<&str>) -> String {
    let mut command = String::from("env -u TMUX tmux");
    if let Some(server) = tmux_server {
        command.push_str(" -L ");
        command.push_str(server);
    }
    let popup_target = target.strip_prefix('=').unwrap_or(target);
    command.push_str(" attach-session -t ");
    command.push_str(popup_target);
    command
}

impl TmuxClient {
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
        }
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The session an agent runs in.
    pub fn session_for(&self, agent: &str) -> String {
        format!("{}{}", self.prefix, flatten_agent_name(agent))
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let output = tmux_command()
            .args(args)
            .output()
            .context("Failed to execute tmux - is tmux installed?")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // "no server running" is not an error for list-sessions.
            // Other tmux commands must surface failures to callers.
            if args.first() == Some(&"list-sessions")
                && (stderr.contains("no server running")
                    || stderr.contains("no sessions")
                    || stderr.contains("error connecting to"))
            {
                return Ok(String::new());
            }
            anyhow::bail!("tmux error: {}", stderr);
        }
        Ok(String::from_utf8_lossy(&output.stdout).into())
    }

    /// List all sessions matching the prefix
    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let output = self.run(&[
            "list-sessions",
            "-F",
            "#{session_name}|#{session_activity}|#{session_attached}|#{pane_pid}",
        ])?;

        if output.is_empty() {
            return Ok(Vec::new());
        }

        let sessions = output
            .lines()
            .filter(|line| self.prefix.is_empty() || line.starts_with(&self.prefix))
            .filter_map(|line| {
                let parts: Vec<&str> = line.split('|').collect();
                if parts.len() != 4 {
                    return None;
                }
                Some(Session::new(
                    parts[0].to_string(),
                    parts[1].parse().ok()?,
                    parts[2] == "1",
                    parts[3].parse().ok()?,
                ))
            })
            .collect();

        Ok(sessions)
    }

    /// List all sessions (regardless of prefix)
    pub fn list_all_sessions(&self) -> Result<Vec<Session>> {
        let output = self.run(&[
            "list-sessions",
            "-F",
            "#{session_name}|#{session_activity}|#{session_attached}|#{pane_pid}",
        ])?;

        if output.is_empty() {
            return Ok(Vec::new());
        }

        let sessions = output
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.split('|').collect();
                if parts.len() != 4 {
                    return None;
                }
                Some(Session::new(
                    parts[0].to_string(),
                    parts[1].parse().ok()?,
                    parts[2] == "1",
                    parts[3].parse().ok()?,
                ))
            })
            .collect();

        Ok(sessions)
    }

    /// Capture the last N lines of a pane's output, including ANSI escape
    /// sequences (suitable for display in a colored dashboard).
    pub fn capture_pane(&self, target: &str, lines: i32) -> Result<String> {
        let target = exact_pane_target(target);
        self.run(&[
            "capture-pane",
            "-e",
            "-t",
            &target,
            "-p",
            "-S",
            &(-lines).to_string(),
        ])
    }

    /// Capture the last N lines of a pane's output as plain text (no ANSI
    /// escapes). Required for substring matching — e.g. Claude Code renders
    /// its banner as `Claude<ESC>[0m <ESC>[1mCode`, so a raw ANSI capture
    /// would *not* contain the contiguous string "Claude Code".
    pub fn capture_pane_plain(&self, target: &str, lines: i32) -> Result<String> {
        let target = exact_pane_target(target);
        let output = self.run(&[
            "capture-pane",
            "-t",
            &target,
            "-p",
            "-S",
            &(-lines).to_string(),
        ])?;
        Ok(tail_pane_lines(output, lines))
    }

    /// Get the name of the command currently running in a pane.
    ///
    /// Returns the executable name (e.g. "opencode", "claude", "zsh").
    pub fn get_pane_command(&self, target: &str) -> Result<String> {
        let target = exact_pane_target(target);
        let output = self.run(&[
            "display-message",
            "-t",
            &target,
            "-p",
            "#{pane_current_command}",
        ])?;
        Ok(output.trim().to_string())
    }

    /// Get the pane process id.
    pub fn get_pane_pid(&self, target: &str) -> Result<u32> {
        let target = exact_pane_target(target);
        let output = self.run(&["display-message", "-t", &target, "-p", "#{pane_pid}"])?;
        output.trim().parse().context("Failed to parse pane pid")
    }

    /// Get the full command line for the process running in a pane.
    pub fn get_pane_process_command(&self, target: &str) -> Result<String> {
        let pid = self.get_pane_pid(target)?;
        let output = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
            .context("Failed to execute ps")?;
        if !output.status.success() {
            anyhow::bail!(
                "ps failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Get the activity timestamp of a pane.
    ///
    /// Uses `#{window_activity}` — the per-pane `#{pane_activity}` format is
    /// empty on tmux 3.6a (macOS homebrew) unless `monitor-activity` is
    /// enabled, which would break readiness checks entirely. Window activity
    /// is universally populated and tracks the most recent input/output in
    /// the window. Since OMAR worker sessions always have exactly one window
    /// and one pane, window-level granularity is equivalent to pane-level.
    pub fn get_pane_activity(&self, target: &str) -> Result<i64> {
        let target = exact_pane_target(target);
        let output = self.run(&["display-message", "-t", &target, "-p", "#{window_activity}"])?;
        output
            .trim()
            .parse()
            .context("Failed to parse window activity timestamp")
    }

    /// Send keys to a pane
    pub fn send_keys(&self, target: &str, keys: &str) -> Result<()> {
        let target = exact_pane_target(target);
        self.run(&["send-keys", "-t", &target, keys])?;
        Ok(())
    }

    /// Send literal text to a pane.
    ///
    /// For small payloads uses `send-keys -l` directly. For large payloads
    /// (>= 2 KB) writes to a temporary file and uses `load-buffer` +
    /// `paste-buffer` to avoid tmux's internal message-size limit, which
    /// silently drops oversized `send-keys -l` arguments.
    pub const LARGE_PAYLOAD_THRESHOLD: usize = 2048;

    pub fn send_keys_literal(&self, target: &str, text: &str) -> Result<()> {
        let target = exact_pane_target(target);
        if text.len() < Self::LARGE_PAYLOAD_THRESHOLD {
            self.run(&["send-keys", "-t", &target, "-l", "--", text])?;
        } else {
            // Avoid passing large text as a CLI arg; owner-only temp file is
            // removed when `tmp` drops.
            let mut tmp = crate::paths::create_private_temp_file("omar-task", "txt")
                .context("Failed to create temp file for task payload")?;
            tmp.write_all(text.as_bytes())
                .context("Failed to write task to temp file")?;
            let path_str = tmp
                .path()
                .to_str()
                .context("Temp file path is not valid UTF-8")?;
            self.run(&["load-buffer", path_str])?;
            self.run(&["paste-buffer", "-t", &target])?;
        }
        Ok(())
    }

    /// Deliver through the backend channel without reading or editing its composer.
    /// Channel discovery may be retried during startup. A send is attempted only
    /// once: an ambiguous transport error must not duplicate the message.
    pub fn deliver_prompt(&self, session: &str, text: &str, opts: &DeliveryOptions) -> Result<()> {
        self.deliver_prompt_until(session, text, opts, &|| false)
    }

    pub fn deliver_prompt_until(
        &self,
        session: &str,
        text: &str,
        opts: &DeliveryOptions,
        answered: &dyn Fn() -> bool,
    ) -> Result<()> {
        let deadline = Instant::now() + opts.startup_timeout;
        loop {
            if answered() {
                return Ok(());
            }
            let backend = self
                .session_backend(session)
                .context("agent backend is not stamped; refusing terminal input")?;
            let pid = self.get_pane_pid(session)?;
            let stamp = self.session_delivery(session);
            if let Some(backend) = crate::backend::by_name(&backend) {
                let target = crate::backend::Target {
                    name: session,
                    pane_pid: pid,
                    stamp: stamp.as_deref(),
                };
                match backend.deliver(&target, text) {
                    Ok(()) => return Ok(()),
                    Err(error) if error.is::<crate::backend::NotReady>() => {
                        // Nothing was sent. Only this condition is safe to
                        // retry automatically.
                    }
                    Err(error) => return Err(error),
                }
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "no delivery channel available for {session} ({backend}); composer untouched"
            );
            thread::sleep(opts.poll_interval);
        }
    }

    /// Wait for pane activity to be quiet for `quiet` duration, or until `timeout`.
    /// Returns Ok(()) as soon as the pane becomes stable; returns Ok(()) anyway
    /// after timeout (best-effort — caller should proceed regardless).
    pub fn wait_for_stable(
        &self,
        session: &str,
        quiet: Duration,
        timeout: Duration,
        poll_interval: Duration,
        require_initial_change: bool,
    ) -> Result<()> {
        let start = Instant::now();
        let mut last_activity = self.get_pane_activity(session).unwrap_or(0);
        let mut last_content = self.capture_pane(session, 50).unwrap_or_default();
        let mut saw_change = false;
        let mut last_change = Instant::now();

        while start.elapsed() < timeout {
            thread::sleep(poll_interval);
            let current = self.get_pane_activity(session).unwrap_or(last_activity);
            let content = self
                .capture_pane(session, 50)
                .unwrap_or_else(|_| last_content.clone());
            if current != last_activity || content != last_content {
                saw_change = true;
                last_activity = current;
                last_content = content;
                last_change = Instant::now();
            } else if last_change.elapsed() >= quiet && (!require_initial_change || saw_change) {
                return Ok(());
            }
        }
        // Timed out waiting for stability — proceed anyway
        Ok(())
    }

    /// Wait until pane output contains ALL of the provided markers.
    /// Matching is case-insensitive; returns false on timeout.
    ///
    /// All-match (rather than any-match) semantics matter for backends like
    /// Claude Code v2.1.116, where the product banner paints hundreds of
    /// ms before the input widget is actually ready to accept keystrokes.
    /// Matching on only one of several markers would let the caller fire
    /// Enter into a pane that silently swallows it.
    pub fn wait_for_markers(
        &self,
        session: &str,
        markers: &[&str],
        timeout: Duration,
        poll_interval: Duration,
    ) -> bool {
        if markers.is_empty() {
            return true;
        }
        let needles: Vec<String> = markers.iter().map(|m| m.to_ascii_lowercase()).collect();
        let start = Instant::now();
        while start.elapsed() < timeout {
            // Use plain capture (no ANSI escapes) so multi-word markers like
            // "Claude Code" match even when the TUI styles each word
            // independently (Claude Code inserts a reset between them).
            if let Ok(content) = self.capture_pane_plain(session, 120) {
                let hay = content.to_ascii_lowercase();
                if needles.iter().all(|needle| hay.contains(needle)) {
                    return true;
                }
            }
            thread::sleep(poll_interval);
        }
        false
    }

    /// Create a new detached session
    pub fn new_session(&self, name: &str, command: &str, workdir: Option<&str>) -> Result<()> {
        self.new_session_with_backend(name, command, workdir, None)
    }

    /// Keep the selected backend through generated shell bootstraps. Raw
    /// commands are classified only by executable position.
    pub(crate) fn new_session_with_backend(
        &self,
        name: &str,
        command: &str,
        workdir: Option<&str>,
        backend: Option<&str>,
    ) -> Result<()> {
        let (cols, rows) = agent_dimensions(crossterm::terminal::size().ok());
        let cols = cols.to_string();
        let rows = rows.to_string();
        let mut args = vec!["new-session", "-d", "-s", name, "-x", &cols, "-y", &rows];

        if let Some(dir) = workdir {
            args.extend(["-c", dir]);
        }

        // The backend decides what its pane is launched with; this only
        // hands tmux what it was given.
        let backend = backend
            .and_then(crate::backend::by_name)
            .or_else(|| crate::backend::detect(command));
        let setup = match backend {
            Some(backend) => backend.prepare_pane(name, command)?,
            None => crate::backend::PaneSetup {
                command: command.to_string(),
                ..Default::default()
            },
        };
        let env: Vec<String> = setup
            .env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        for value in &env {
            args.extend(["-e", value]);
        }
        // Execute the provided command through a shell so the full string is
        // interpreted consistently (including quoted args and shell metacharacters)
        // instead of relying on tmux's shell-command parser heuristics.
        args.extend(["sh", "-lc", &setup.command]);
        let palette = setup.window_style.as_ref().map(|style| {
            format!(
                "set-option -w -t {} window-style '{style}'",
                crate::manager::shell_single_quote(name)
            )
        });
        if let Some(palette) = &palette {
            // One tmux command queue establishes the palette before the
            // server processes the new pane's terminal-probe output.
            args.extend([
                ";",
                "if-shell",
                "-F",
                "-t",
                name,
                "#{==:#{window-style},default}",
                palette,
            ]);
        }
        self.run(&args)?;
        self.run(&["set-option", "-t", name, "history-limit", "10000"])?;
        // Record which backend this session was launched with. Everything that
        // later needs to know reads it back instead of guessing from the pane,
        // which cannot be done reliably: `#{pane_current_command}` is `node`
        // for any npm-installed backend, and banner text scrolls away.
        if let Some(backend) = backend {
            let _ = self.set_session_backend(name, backend.kind().name());
        }
        if let Some(stamp) = &setup.stamp {
            let _ = self.set_session_delivery(name, stamp);
        }
        // Finish channel setup before the launcher can return or exec tmux.
        if let Some(backend) = backend {
            if let Some(stamp) = backend.provision(name, &setup.command)? {
                self.set_session_delivery(name, &stamp)
                    .context("record backend delivery channel")?;
            }
        }
        Ok(())
    }

    /// Stamp the backend name into the session's own environment.
    pub fn set_session_backend(&self, name: &str, backend: &str) -> Result<()> {
        let target = exact_session_target(name);
        self.run(&[
            "set-environment",
            "-t",
            &target,
            SESSION_BACKEND_VAR,
            backend,
        ])?;
        Ok(())
    }

    /// Read one variable out of a session's own environment.
    pub fn session_env(&self, name: &str, var: &str) -> Option<String> {
        let target = exact_session_target(name);
        let output = self.run(&["show-environment", "-t", &target, var]).ok()?;
        let value = output.trim().strip_prefix(&format!("{}=", var))?;
        (!value.is_empty()).then(|| value.to_string())
    }

    /// Record how events reach this session's backend without the input box.
    /// Each backend defines its own stamp; see `crate::backend`.
    pub fn set_session_delivery(&self, name: &str, stamp: &str) -> Result<()> {
        let target = exact_session_target(name);
        self.run(&[
            "set-environment",
            "-t",
            &target,
            SESSION_DELIVERY_VAR,
            stamp,
        ])?;
        Ok(())
    }

    pub fn session_delivery(&self, name: &str) -> Option<String> {
        self.session_env(name, SESSION_DELIVERY_VAR)
    }

    /// Which backend is running in this session.
    ///
    /// Prefers the stamp written at launch. Falls back to the pane command and
    /// then to the pane process's full argv, so sessions started by an older
    /// OMAR — or attached by hand — are still identified when they can be.
    pub fn session_backend(&self, name: &str) -> Option<String> {
        if let Some(value) = self.session_env(name, SESSION_BACKEND_VAR) {
            return Some(value);
        }

        self.get_pane_command(name)
            .ok()
            .and_then(|command| crate::backend::command_name(&command))
            .or_else(|| {
                self.get_pane_process_command(name)
                    .ok()
                    .and_then(|command| crate::backend::command_name(&command))
            })
            .map(|backend| backend.to_string())
    }

    /// Kill a session
    pub fn kill_session(&self, name: &str) -> Result<()> {
        let target = exact_session_target(name);
        self.run(&["kill-session", "-t", &target])?;
        Ok(())
    }

    /// Stop the assistant and its sidecars, including background children
    /// which may ignore the terminal hangup sent by `kill-session` alone.
    pub fn kill_session_tree(&self, name: &str) -> Result<()> {
        if !self.has_session(name)? {
            return Ok(());
        }
        let tree = crate::process::process_tree(self.get_pane_pid(name)?);
        crate::process::signal_tree(&tree, "-TERM");
        if self.has_session(name)? {
            self.kill_session(name)?;
        }
        thread::sleep(Duration::from_millis(500));
        crate::process::signal_tree(&tree, "-KILL");
        Ok(())
    }

    /// Check if a session exists
    pub fn has_session(&self, name: &str) -> Result<bool> {
        let target = exact_session_target(name);
        let result = tmux_command()
            .args(["has-session", "-t", &target])
            .output()
            .context("Failed to execute tmux")?;

        Ok(result.status.success())
    }

    /// Return true when a tmux session exists and has at least one live pane.
    ///
    /// A user can exit the process inside a pane while tmux keeps the session
    /// around with `remain-on-exit`. `has-session` is still true in that state,
    /// but the session cannot accept input or be attached as a running agent.
    pub fn session_has_live_pane(&self, name: &str) -> Result<bool> {
        let target = exact_session_target(name);
        let result = tmux_command()
            .args(["list-panes", "-t", &target, "-F", "#{pane_dead}"])
            .output()
            .context("Failed to execute tmux")?;

        if !result.status.success() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            if stderr.contains("can't find")
                || stderr.contains("no server running")
                || stderr.contains("no sessions")
                || stderr.contains("error connecting to")
            {
                return Ok(false);
            }
            anyhow::bail!("tmux error: {}", stderr);
        }

        Ok(String::from_utf8_lossy(&result.stdout)
            .lines()
            .any(|line| line.trim() != "1"))
    }

    /// Find a session by exact name.
    ///
    /// Unfiltered, unlike `list_sessions`: the caller has already named one
    /// session, and the prefix is for enumerating an EA's agents rather than
    /// for deciding whether a named one exists. Filtering here could not see
    /// the EA's own pane at all -- the manager is `<prefix>ea-<id>` where a
    /// worker is `<prefix><ea>-<name>` -- so `ensure_session_not_attached`
    /// called it missing while `has_session`, which asks tmux directly, found
    /// it.
    pub fn get_session(&self, name: &str) -> Result<Option<Session>> {
        let target = exact_session_target(name);
        let sessions = self.list_all_sessions()?;
        Ok(sessions
            .into_iter()
            .find(|session| exact_session_target(&session.name) == target))
    }

    /// Ensure the session exists and is not currently attached.
    pub fn ensure_session_not_attached(&self, name: &str) -> Result<Session> {
        let session = self
            .get_session(name)?
            .ok_or_else(|| anyhow!("Session '{}' not found", name))?;
        if session.attached {
            anyhow::bail!("Cannot kill attached session")
        }
        Ok(session)
    }

    /// Attach to a session (blocks until detached)
    pub fn attach_session(&self, session: &str) -> Result<()> {
        let target = exact_session_target(session);
        tmux_command()
            .args(["attach-session", "-t", &target])
            .status()
            .context("Failed to attach to tmux session")?;
        Ok(())
    }

    /// Open a popup attached to a session
    pub fn attach_popup(&self, session: &str, width: &str, height: &str) -> Result<()> {
        let target = exact_session_target(session);
        let command = popup_attach_command(&target);
        let status = tmux_command()
            .args(["display-popup", "-E", "-w", width, "-h", height, &command])
            .status()
            .context("Failed to open tmux popup")?;
        if !status.success() {
            anyhow::bail!("tmux popup exited with status {}", status);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_geometry_uses_the_terminal_or_a_bounded_headless_fallback() {
        assert_eq!(agent_dimensions(Some((91, 27))), (91, 27));
        assert_eq!(agent_dimensions(None), (120, 40));
        for invalid in [(0, 0), (1, 24), (80, 1001)] {
            assert_eq!(agent_dimensions(Some(invalid)), (120, 40));
        }
    }

    #[test]
    fn an_agent_pane_starts_at_the_launch_geometry() {
        // Detached launches use the caller's terminal when available, otherwise
        // the same moderate fallback regardless of tmux's server defaults.
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }
        let session = "omar-test-pane-size";
        let _ = tmux_command()
            .args(["kill-session", "-t", session])
            .output();
        let _guard = SessionGuard(session.to_string());

        TmuxClient::new("")
            .new_session(session, "exec sh", None)
            .expect("session");

        let reported = tmux_command()
            .args([
                "display-message",
                "-p",
                "-t",
                session,
                "#{window_width}x#{window_height}",
            ])
            .output()
            .expect("tmux answers");
        let size = String::from_utf8_lossy(&reported.stdout).trim().to_string();
        let (cols, rows) = agent_dimensions(crossterm::terminal::size().ok());
        assert_eq!(size, format!("{cols}x{rows}"));
    }

    #[test]
    fn a_claude_pane_is_launched_to_accept_peer_messages() {
        // Whatever path spawned it, the pane's launch line carries the setting
        // that keeps a peer message out of Claude Code's approval dialog.
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }
        let session = "omar-test-claude-inbound";
        let _ = tmux_command()
            .args(["kill-session", "-t", session])
            .output();
        let _guard = SessionGuard(session.to_string());

        TmuxClient::new("")
            .new_session(session, "claude --version; exec sh", None)
            .expect("session");

        let reported = tmux_command()
            .args([
                "display-message",
                "-p",
                "-t",
                session,
                "#{pane_start_command}",
            ])
            .output()
            .expect("tmux answers");
        let launched = String::from_utf8_lossy(&reported.stdout);
        // tmux escapes the quotes when it shows the line, so match loosely.
        assert!(
            launched.contains("claude --settings") && launched.contains("crossSessionInbound"),
            "launch line was not given the setting: {launched}"
        );
    }

    #[test]
    fn a_qualified_agent_name_survives_the_round_trip_to_tmux() {
        // tmux would store `n1.agent` as `n1_agent` and then fail every lookup
        // for the name it was given, so the flattening has to happen here.
        let client = TmuxClient::new("omar-agent-0-");
        assert_eq!(client.session_for("n1.agent"), "omar-agent-0-n1_agent");
        // Unqualified names, which is every agent outside a main block, are
        // untouched.
        assert_eq!(client.session_for("worker"), "omar-agent-0-worker");
    }

    #[test]
    fn test_client_creation() {
        let client = TmuxClient::new("");
        assert_eq!(client.prefix(), "");
    }

    #[test]
    fn test_plain_capture_tail_caps_real_idle_claude_fixture() {
        // Captured locally with:
        // `tmux capture-pane -t omar-fixture-claude -p -S -30`
        // from an idle Claude Code v2.1.136 pane.
        const IDLE_CLAUDE_CAPTURE: &str = r#" ▐▛███▜▌   Claude Code v2.1.136
▝▜█████▛▘  Opus 4.7 with low effort · Claude Max
  ▘▘ ▝▝    /workspace/omar

───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
❯ Try "refactor dashboard.rs"
───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
  PR #133











































"#;

        let capped = tail_pane_lines(IDLE_CLAUDE_CAPTURE.to_string(), 1);

        assert_eq!(capped.lines().count(), 1);
        assert!(!capped.contains("Claude Code"));
        assert!(!capped.contains("PR #133"));
    }

    #[test]
    fn test_client_with_different_prefix() {
        let client = TmuxClient::new("test-");
        assert_eq!(client.prefix(), "test-");
    }

    #[test]
    fn test_exact_target_prefixes_plain_session_names() {
        assert_eq!(
            exact_session_target("omar-agent-0-gx-r"),
            "=omar-agent-0-gx-r"
        );
        assert_eq!(exact_session_target("=already-exact"), "=already-exact");
        assert_eq!(exact_session_target("session:1.0"), "session:1.0");
        assert_eq!(
            exact_pane_target("omar-agent-0-gx-r"),
            "=omar-agent-0-gx-r:"
        );
        assert_eq!(exact_pane_target("=already-exact"), "=already-exact:");
        assert_eq!(exact_pane_target("session:1.0"), "session:1.0");
    }

    #[test]
    fn test_popup_attach_command_unsets_nested_tmux_and_quotes_target() {
        assert_eq!(
            popup_attach_command_with_server("=omar-agent-ea-0", None),
            "env -u TMUX tmux attach-session -t omar-agent-ea-0"
        );
    }

    #[test]
    fn test_popup_attach_command_preserves_custom_tmux_server() {
        assert_eq!(
            popup_attach_command_with_server("=omar-agent-ea-0", Some("omar-test-server")),
            "env -u TMUX tmux -L omar-test-server attach-session -t omar-agent-ea-0"
        );
    }

    fn tmux_available() -> bool {
        tmux_command()
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Cleanup guard: kill the named tmux session on drop (even on panic).
    struct SessionGuard(String);
    impl Drop for SessionGuard {
        fn drop(&mut self) {
            let _ = tmux_command()
                .args(["kill-session", "-t", &self.0])
                .output();
        }
    }

    #[test]
    fn test_has_session_uses_exact_target_not_tmux_prefix_match() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let existing = "omar-test-prefix-root";
        let prefix_only = "omar-test-prefix-r";
        let _ = tmux_command()
            .args(["kill-session", "-t", existing])
            .output();
        let _guard = SessionGuard(existing.to_string());

        let ok = tmux_command()
            .args(["new-session", "-d", "-s", existing, "sleep", "60"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("Skipping test: failed to create tmux session");
            return;
        }

        let client = TmuxClient::new("omar-test-");
        assert!(client.has_session(existing).unwrap());
        assert!(
            !client.has_session(prefix_only).unwrap(),
            "tmux prefix target matching must not make {prefix_only} resolve to {existing}"
        );
        assert!(
            client.send_keys(prefix_only, "C-l").is_err(),
            "pane targets must also avoid tmux prefix matching"
        );
    }

    #[test]
    fn test_wait_for_stable_returns_on_idle_pane() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-test-wait-stable";
        let _ = tmux_command()
            .args(["kill-session", "-t", session])
            .output();
        let _guard = SessionGuard(session.to_string());

        let ok = tmux_command()
            .args(["new-session", "-d", "-s", session])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return;
        }

        // Let the shell prompt finish drawing
        thread::sleep(Duration::from_millis(300));

        let client = TmuxClient::new("omar-test-");
        let start = Instant::now();
        client
            .wait_for_stable(
                session,
                Duration::from_millis(200),
                Duration::from_secs(3),
                Duration::from_millis(50),
                false,
            )
            .unwrap();
        // Should return within a couple hundred ms on a fully idle pane.
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "wait_for_stable took too long on idle pane: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn test_wait_for_markers_detects_backend_banner_text() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-test-wait-markers";
        let _ = tmux_command()
            .args(["kill-session", "-t", session])
            .output();
        let _guard = SessionGuard(session.to_string());

        let ok = tmux_command()
            .args(["new-session", "-d", "-s", session])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return;
        }

        let client = TmuxClient::new("omar-test-");
        let _ = client.send_keys_literal(session, "echo OpenAI Codex");
        let _ = client.send_keys(session, "Enter");
        let found = client.wait_for_markers(
            session,
            &["openai codex"],
            Duration::from_secs(3),
            Duration::from_millis(50),
        );
        assert!(found, "Expected marker not detected in tmux pane");
    }

    /// Regression: on tmux 3.6a (macOS homebrew) `#{pane_activity}` is empty
    /// unless `monitor-activity` is enabled. `get_pane_activity` used to
    /// swallow this with `unwrap_or(0)`, freezing the "activity timestamp"
    /// at 0 forever and breaking `wait_for_stable` on
    /// readiness-gated prompt delivery. This test asserts we get a usable,
    /// advancing timestamp out of the box (no monitor-activity needed).
    #[test]
    fn a_sessions_backend_is_recorded_at_launch_and_read_back() {
        // The pane cannot be asked what it is running: `pane_current_command`
        // is `node` for every npm-installed backend, and banner text scrolls
        // away. So the launcher records it and everything else reads the stamp.
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-test-backend-stamp";
        let _ = tmux_command()
            .args(["kill-session", "-t", session])
            .output();
        let _guard = SessionGuard(session.to_string());

        let ok = tmux_command()
            .args(["new-session", "-d", "-s", session, "sleep", "60"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("Skipping test: failed to create tmux session");
            return;
        }

        let client = TmuxClient::new("");
        client
            .set_session_backend(session, "codex")
            .expect("stamp the backend");

        assert_eq!(client.session_backend(session).as_deref(), Some("codex"));
    }

    #[test]
    fn a_session_without_a_stamp_falls_back_to_the_pane_command() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-test-backend-unstamped";
        let _ = tmux_command()
            .args(["kill-session", "-t", session])
            .output();
        let _guard = SessionGuard(session.to_string());

        let ok = tmux_command()
            .args(["new-session", "-d", "-s", session, "sleep", "60"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("Skipping test: failed to create tmux session");
            return;
        }

        // `sleep` is not a backend, so nothing is claimed rather than a guess.
        assert_eq!(TmuxClient::new("").session_backend(session), None);
    }

    #[test]
    fn test_get_pane_activity_returns_usable_timestamp() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-test-pane-activity";
        let _ = tmux_command()
            .args(["kill-session", "-t", session])
            .output();
        let _guard = SessionGuard(session.to_string());

        let ok = tmux_command()
            .args(["new-session", "-d", "-s", session])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return;
        }

        let client = TmuxClient::new("omar-test-");
        let t0 = client
            .get_pane_activity(session)
            .expect("get_pane_activity must parse a timestamp on a fresh session");
        assert!(
            t0 > 0,
            "activity timestamp should be a real unix time, got {}",
            t0
        );

        // Generate activity and verify the timestamp advances.
        let _ = client.send_keys(session, "Space");
        thread::sleep(Duration::from_secs(2));
        let t1 = client.get_pane_activity(session).unwrap();
        assert!(
            t1 >= t0,
            "activity timestamp must not go backwards: {} -> {}",
            t0,
            t1
        );
    }

    /// Regression: Claude Code's banner renders each word of "Claude Code"
    /// with its own bold/reset ANSI pair — `Claude<ESC>[0m <ESC>[1mCode` —
    /// so an ANSI-inclusive capture (`-e`) contains the bytes
    /// `claude\x1b[0m code`, which the literal substring "claude code" does
    /// NOT match. `wait_for_markers` must use a plain capture so multi-word
    /// markers survive styling. Without this, readiness detection silently
    /// fails for claude (works for single-word markers like "OpenAI Codex",
    /// which is why the bug escaped earlier testing).
    #[test]
    fn test_wait_for_markers_handles_ansi_styled_multiword_banner() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-test-ansi-marker";
        let _ = tmux_command()
            .args(["kill-session", "-t", session])
            .output();
        let _guard = SessionGuard(session.to_string());

        let ok = tmux_command()
            .args(["new-session", "-d", "-s", session])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return;
        }

        let client = TmuxClient::new("omar-test-");
        // Reproduce the Claude Code banner pattern: each word wrapped in
        // its own bold/reset pair, exactly like the real TUI.
        let banner = "printf '\\033[1mClaude\\033[0m \\033[1mCode\\033[0m v2.1.113\\n'";
        let _ = client.send_keys_literal(session, banner);
        let _ = client.send_keys(session, "Enter");

        // Confirm the bug's precondition: an ANSI-inclusive capture does
        // NOT contain the contiguous bytes "claude code".
        thread::sleep(Duration::from_millis(300));
        let ansi_capture = client.capture_pane(session, 50).unwrap_or_default();
        assert!(
            !ansi_capture.to_ascii_lowercase().contains("claude code"),
            "precondition: styled banner must NOT contain contiguous 'claude code' in an ANSI capture (if it does, the test is trivially passing and won't catch regressions)"
        );

        // And the fix: wait_for_markers must still find "Claude Code".
        let found = client.wait_for_markers(
            session,
            &["Claude Code"],
            Duration::from_secs(3),
            Duration::from_millis(50),
        );
        assert!(
            found,
            "wait_for_markers must detect multi-word marker under ANSI styling"
        );
    }

    /// Regression: `wait_for_markers` must require ALL markers to be
    /// present, not any-of. Claude Code v2.1.116 paints "Claude Code" in
    /// its banner several hundred ms before the input widget is actually
    /// wired up to accept Enter; any-of matching returned true immediately
    /// when only the banner was visible and let `deliver_prompt` fire
    /// Enter into a pane that would silently swallow it. With all-of
    /// semantics, we don't succeed until every marker (banner + input-
    /// widget prompt glyph "❯" for claude) has rendered.
    #[test]
    fn test_wait_for_markers_requires_all_markers() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-test-wait-markers-all";
        let _ = tmux_command()
            .args(["kill-session", "-t", session])
            .output();
        let _guard = SessionGuard(session.to_string());

        let ok = tmux_command()
            .args(["new-session", "-d", "-s", session])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return;
        }

        let client = TmuxClient::new("omar-test-");

        // Print only the first marker. With the old any-of semantics
        // wait_for_markers would return true; with all-of it must not.
        let _ = client.send_keys_literal(session, "echo FIRST_MARKER_ONLY");
        let _ = client.send_keys(session, "Enter");
        thread::sleep(Duration::from_millis(300));

        let found = client.wait_for_markers(
            session,
            &["FIRST_MARKER_ONLY", "SECOND_MARKER_MISSING"],
            Duration::from_millis(600),
            Duration::from_millis(50),
        );
        assert!(
            !found,
            "wait_for_markers must require ALL markers; returning true \
             when only one is present regresses the Claude Code v2.1.116 \
             Enter-swallow fix"
        );

        // Now print the second marker too; both present -> must return true.
        let _ = client.send_keys_literal(session, "echo SECOND_MARKER_MISSING");
        let _ = client.send_keys(session, "Enter");
        let found = client.wait_for_markers(
            session,
            &["FIRST_MARKER_ONLY", "SECOND_MARKER_MISSING"],
            Duration::from_secs(3),
            Duration::from_millis(50),
        );
        assert!(
            found,
            "wait_for_markers must return true once ALL markers are present"
        );
    }

    /// Regression: the claude readiness marker set includes "❯" (U+276F),
    /// the non-ASCII input-widget prompt glyph. `wait_for_markers` lowercases
    /// the hay with `to_ascii_lowercase`, which leaves "❯" byte-identical,
    /// so substring matching must still find it. This test locks in that
    /// UTF-8 markers work, so nobody later "fixes" readiness matching with a
    /// full-Unicode lowercase transform that would shift the bytes and break
    /// the match. Also guards against a prior bug where `capture_pane` (with
    /// `-e`, ANSI-inclusive) was used for marker matching, which inserts
    /// escape sequences mid-string and breaks multi-byte char boundaries.
    #[test]
    fn test_wait_for_markers_matches_claude_prompt_glyph() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-test-claude-glyph-marker";
        let _ = tmux_command()
            .args(["kill-session", "-t", session])
            .output();
        let _guard = SessionGuard(session.to_string());

        let ok = tmux_command()
            .args(["new-session", "-d", "-s", session])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return;
        }

        let client = TmuxClient::new("omar-test-");

        // Put "Claude Code" and "❯" directly into the pane as literal
        // bytes via `send-keys -l`. Avoids running a shell command so
        // the test is independent of the sandbox's default shell actually
        // executing inputs.
        let _ = client.send_keys_literal(session, "Claude Code ❯ ");

        let found = client.wait_for_markers(
            session,
            &["Claude Code", "❯"],
            Duration::from_secs(3),
            Duration::from_millis(50),
        );
        assert!(
            found,
            "wait_for_markers must match the non-ASCII \"❯\" glyph used as \
             Claude Code's input prompt — regresses the v2.1.116 Enter- \
             swallow fix if missing"
        );
    }

    #[test]
    fn large_payload_threshold_is_reasonable() {
        // Threshold must be large enough that typical short tasks go through send-keys
        // but small enough to catch the ~1-3 KB tasks that silently fail
        const { assert!(TmuxClient::LARGE_PAYLOAD_THRESHOLD >= 512) };
        const { assert!(TmuxClient::LARGE_PAYLOAD_THRESHOLD <= 8192) };
    }
}
