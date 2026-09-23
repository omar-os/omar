//! Attaching to an agent's tmux session over a pseudo-terminal.
//!
//! `tmux attach-session` refuses to run without a controlling terminal, so the
//! session is opened behind a PTY and its bytes are relayed verbatim. Nothing
//! interprets them: the agent draws a TUI, and the viewer's job is to carry the
//! escape sequences through unchanged.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{mpsc, Mutex, OnceLock};
use std::thread;

use anyhow::{bail, Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};

use crate::tmux::tmux_command;

/// A window smaller than this is not worth attaching to, and a size that large
/// is a tmux answer we did not understand.
const MIN_DIMENSION: u16 = 2;
const MAX_DIMENSION: u16 = 1000;

/// A session target that matches this session and nothing else.
///
/// tmux resolves `-t name` by prefix when nothing matches it exactly, so with
/// sessions like `…-w` and `…-w2` a lookup for one that has since gone lands
/// silently on its neighbour — reading the wrong agent's size, or turning the
/// wrong agent's mouse on.
#[cfg(test)]
fn exact_session(session: &str) -> String {
    format!("={session}")
}

/// The same, for the commands that want a target inside the session.
///
/// `display-message`, `set-option` and `show-options` reject a bare `=name`
/// ("no such session"); the trailing colon is what makes it the session's
/// current window rather than a window called `=name`.
fn exact_target(session: &str) -> String {
    format!("={session}:")
}

/// The size tmux is currently drawing a session at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSize {
    pub cols: u16,
    pub rows: u16,
    /// Rows tmux spends on its status line, which belong to the client rather
    /// than the window.
    pub status_lines: u16,
}

impl WindowSize {
    /// The terminal height a client must have for the window to keep `rows`.
    ///
    /// tmux gives the window whatever is left after its status line, so a
    /// client exactly `rows` tall silently costs the agent a row — and tmux
    /// does not give it back on detach.
    pub fn client_rows(&self) -> u16 {
        self.rows.saturating_add(self.status_lines)
    }
}

/// Ask tmux how large the session's window is (excluding status lines).
pub fn window_size(session: &str) -> Result<WindowSize> {
    let output = tmux_command()
        .args([
            "display-message",
            "-p",
            "-t",
            &exact_target(session),
            "#{window_width}x#{window_height}x#{status}",
        ])
        .output()
        .context("failed to ask tmux for the window size")?;
    if !output.status.success() {
        bail!(
            "tmux could not describe session '{session}': {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    parse_window_size(String::from_utf8_lossy(&output.stdout).trim())
}

/// All viewers served by this daemon share one baseline. Serializing open/drop
/// prevents one viewer from restoring over another (including opposite close
/// orders). Immutable tmux IDs prevent cleanup from touching a replacement
/// session with the same name.
#[derive(Debug)]
struct Viewers {
    count: usize,
    original: WindowSize,
    target: String,
    server_pid: String,
    policy: Option<String>,
}

fn viewers() -> &'static Mutex<HashMap<String, Viewers>> {
    static VIEWERS: OnceLock<Mutex<HashMap<String, Viewers>>> = OnceLock::new();
    VIEWERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn tmux_text(args: &[&str]) -> Result<String> {
    let output = tmux_command().args(args).output()?;
    if !output.status.success() {
        bail!("tmux: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn local_policy(target: &str) -> Result<Option<String>> {
    let policy = tmux_text(&["show-options", "-w", "-t", target, "-v", "window-size"])?;
    Ok((!policy.is_empty()).then_some(policy))
}

fn validate_dimensions(cols: u16, rows: u16) -> Result<()> {
    if !(MIN_DIMENSION..=MAX_DIMENSION).contains(&cols)
        || !(MIN_DIMENSION..=MAX_DIMENSION).contains(&rows)
    {
        bail!("implausible viewer size {cols}x{rows}");
    }
    Ok(())
}

fn parse_window_size(reported: &str) -> Result<WindowSize> {
    let mut fields = reported.split('x');
    let (Some(cols), Some(rows), Some(status), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        bail!("tmux reported an unreadable window size '{reported}'");
    };
    let size = WindowSize {
        cols: cols.parse().context("window width is not a number")?,
        rows: rows.parse().context("window height is not a number")?,
        // The status option is `off`, `on`, or a count of lines.
        status_lines: match status {
            "off" => 0,
            "on" => 1,
            count => count
                .parse()
                .with_context(|| format!("unreadable status option '{count}'"))?,
        },
    };
    if !(MIN_DIMENSION..=MAX_DIMENSION).contains(&size.cols)
        || !(MIN_DIMENSION..=MAX_DIMENSION).contains(&size.rows)
    {
        bail!("tmux reported an implausible window size '{reported}'");
    }
    Ok(size)
}

/// A live attachment to one agent's session.
///
/// Dropping it detaches: the `tmux attach` process is killed, which is what
/// closing the viewer means. The agent's own session is untouched.
pub struct Attachment {
    child: Box<dyn Child + Send + Sync>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    output: mpsc::Receiver<Vec<u8>>,
    /// Registry key includes the tmux socket and immutable window ID.
    viewer_key: String,
    pub size: WindowSize,
}

impl Attachment {
    /// Attach to a session by its exact name.
    ///
    /// A session, not a prefix and an agent: the assistant's is
    /// `<base>ea-<id>` and is built from no agent name at all, so the caller
    /// names the one it means rather than passing an empty prefix to say so.
    pub fn open_session(session: &str) -> Result<Self> {
        Self::open_session_sized(session, None)
    }

    /// Start the PTY at the viewer's measured geometry, before tmux emits its
    /// first redraw. The optional size is in client cells, including status.
    pub fn open_session_sized(session: &str, dimensions: Option<(u16, u16)>) -> Result<Self> {
        if let Some((cols, rows)) = dimensions {
            validate_dimensions(cols, rows)?;
        }
        let mut registry = viewers().lock().unwrap_or_else(|e| e.into_inner());
        let original = window_size(session)?;
        let identity = tmux_text(&[
            "display-message",
            "-p",
            "-t",
            &exact_target(session),
            "#{socket_path}|#{pid}|#{session_id}:#{window_id}",
        ])?;
        let (server, target) = identity.rsplit_once('|').context("missing tmux identity")?;
        let (_, server_pid) = server.rsplit_once('|').context("missing tmux server PID")?;
        let window = target.split_once(':').context("missing window ID")?.1;
        let viewer_key = format!("{server}|{window}");
        let policy = local_policy(target)?;
        let (cols, rows) = dimensions.unwrap_or((original.cols, original.client_rows()));
        let size = WindowSize {
            cols,
            rows: rows.saturating_sub(original.status_lines),
            status_lines: original.status_lines,
        };

        let pty = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to open a pseudo-terminal")?;

        // Immutable IDs avoid prefix matching and name reuse during setup.
        let mut command = CommandBuilder::new("tmux");
        if let Ok(server) = std::env::var("OMAR_TMUX_SERVER") {
            let server = server.trim().to_string();
            if !server.is_empty() {
                command.args(["-L", &server]);
            }
        }
        command.args(["attach-session", "-t", target]);
        // A nested tmux would refuse to attach.
        command.env_remove("TMUX");
        // tmux refuses to attach without a terminal it recognises, and the
        // daemon inherits whatever started it — nothing at all under launchd,
        // systemd or CI, where the attach exits at once and the viewer shows an
        // agent that never draws. The far end is xterm.js, so this is not a
        // guess about the environment: it is what the viewer actually is.
        command.env("TERM", "xterm-256color");
        // The browser supports RGB. Without this, tmux may downgrade the
        // agent's truecolor sequences to its client's indexed palette.
        command.env("COLORTERM", "truecolor");

        // Acquire handles before spawning: a later failure must not leak an
        // attached client without a Drop owner.
        let mut reader = pty
            .master
            .try_clone_reader()
            .context("failed to read from the pseudo-terminal")?;
        let writer = pty
            .master
            .take_writer()
            .context("failed to write to the pseudo-terminal")?;
        let child = pty
            .slave
            .spawn_command(command)
            .context("failed to start tmux attach")?;
        drop(pty.slave);
        registry
            .entry(viewer_key.clone())
            .or_insert_with(|| Viewers {
                count: 0,
                original,
                target: target.to_string(),
                server_pid: server_pid.to_string(),
                policy,
            })
            .count += 1;
        // Mouse is a session option and also enables tmux scrollback.
        let _ = tmux_command()
            .args(["set-option", "-t", target, "mouse", "on"])
            .output();

        // The master must outlive the reader thread but is not itself Send in
        // every implementation, so the thread owns only the reader.
        let (sender, output) = mpsc::channel();
        thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        if sender.send(buffer[..read].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Ok(Self {
            child,
            master: pty.master,
            writer,
            output,
            viewer_key,
            size,
        })
    }

    /// Bytes the agent has drawn since the last call, if any.
    pub fn read(&self, timeout: std::time::Duration) -> Option<Vec<u8>> {
        self.output.recv_timeout(timeout).ok()
    }

    /// Reflow the session to the viewer's shape.
    ///
    /// This is what a terminal does when its window changes, and it is why the
    /// viewer never has to scale: the agent redraws at the size being watched.
    /// The last viewer restores the baseline on drop when no other client
    /// owns the window. tmux's sizing policy arbitrates concurrent viewers.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        validate_dimensions(cols, rows)?;
        if (cols, rows) == (self.size.cols, self.size.client_rows()) {
            return Ok(());
        }
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        self.size = WindowSize {
            cols,
            rows: rows.saturating_sub(self.size.status_lines),
            status_lines: self.size.status_lines,
        };
        Ok(())
    }

    /// Send keystrokes to the agent.
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        let mut registry = viewers().lock().unwrap_or_else(|e| e.into_inner());
        let _ = self.child.kill();
        let _ = self.child.wait();
        let Some(viewers) = registry.get_mut(&self.viewer_key) else {
            return;
        };
        viewers.count -= 1;
        if viewers.count != 0 {
            return;
        }
        let viewers = registry.remove(&self.viewer_key).expect("last viewer");

        // A server restart can reuse both session and window IDs.
        if tmux_text(&["display-message", "-p", "-t", &viewers.target, "#{pid}"]).ok()
            != Some(viewers.server_pid.clone())
        {
            return;
        }
        // Never pin a window that another terminal (or another daemon) still
        // owns. list-clients is server-wide: linked windows in other sessions
        // count too. On any uncertainty leave tmux in control.
        let Ok(clients) = tmux_text(&["list-clients", "-F", "#{session_id}"]) else {
            return;
        };
        let window = viewers.target.split_once(':').expect("window ID").1;
        let Ok(sessions) = tmux_text(&["list-windows", "-a", "-F", "#{session_id} #{window_id}"])
        else {
            return;
        };
        let occupied = sessions
            .lines()
            .filter_map(|line| line.split_once(' '))
            .any(|(session, id)| id == window && clients.lines().any(|client| client == session));
        if occupied {
            return;
        }
        // Respect an operator changing the sizing policy while a viewer is
        // open. We do not own that new policy and must not unset it.
        if local_policy(&viewers.target).ok() != Some(viewers.policy.clone()) {
            return;
        }
        let _ = tmux_command()
            .args([
                "resize-window",
                "-t",
                &viewers.target,
                "-x",
                &viewers.original.cols.to_string(),
                "-y",
                &viewers.original.rows.to_string(),
            ])
            .output();
        // resize-window changes window-size to manual. Restore the exact local
        // policy, including inheritance, rather than always unsetting it.
        match viewers.policy {
            Some(policy) => {
                let _ = tmux_command()
                    .args([
                        "set-option",
                        "-w",
                        "-t",
                        &viewers.target,
                        "window-size",
                        &policy,
                    ])
                    .output();
            }
            None => {
                let _ = tmux_command()
                    .args([
                        "set-option",
                        "-w",
                        "-u",
                        "-t",
                        &viewers.target,
                        "window-size",
                    ])
                    .output();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Requires tmux, so it is skipped where the binary is absent.
    fn tmux_available() -> bool {
        tmux_command()
            .arg("-V")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    /// Waits for tmux to apply a size change, up to a generous deadline.
    ///
    /// A fixed sleep is a guess at how long that takes, and the guess that held
    /// on a quiet machine was short on a loaded CI runner. Returns whatever the
    /// size is when the deadline passes, so the caller's assertion is what
    /// reports the failure.
    fn settles(session: &str, want: impl Fn(&WindowSize) -> bool) -> WindowSize {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let size = window_size(session).expect("size");
            if want(&size) || std::time::Instant::now() >= deadline {
                return size;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Clients tmux believes are attached, for assertions that need one.
    ///
    /// A resize only reaches the session through an attached client, so "the
    /// window did not change" and "nothing ever attached" look identical from
    /// the size alone. This tells them apart in the failure message.
    fn clients(session: &str) -> String {
        match tmux_command()
            .args(["list-clients", "-t", &exact_target(session)])
            .output()
        {
            Ok(output) => {
                let listed = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if listed.is_empty() {
                    format!("none ({})", String::from_utf8_lossy(&output.stderr).trim())
                } else {
                    listed
                }
            }
            Err(error) => format!("list-clients failed: {error}"),
        }
    }

    /// Kills only its own session, so it cannot disturb the operator's server.
    struct SessionGuard(String);

    impl Drop for SessionGuard {
        fn drop(&mut self) {
            let _ = tmux_command()
                .args(["kill-session", "-t", &exact_session(&self.0)])
                .output();
        }
    }

    #[test]
    fn attaching_relays_both_ways_and_leaves_the_window_alone() {
        if !tmux_available() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        // Sessions are shared with whatever server the operator is running, so
        // the name is distinctive and only it gets cleaned up. Setting
        // OMAR_TMUX_SERVER instead would be process-global and would race the
        // other tmux tests.
        let session = "omar-terminal-relay-probe";
        let _ = tmux_command()
            .args(["kill-session", "-t", &exact_session(session)])
            .output();
        let _guard = SessionGuard(session.to_string());

        // A deliberately unusual size: if attaching resizes it, this changes.
        tmux_command()
            .args([
                "new-session",
                "-d",
                "-s",
                session,
                "-x",
                "173",
                "-y",
                "47",
                "sh",
            ])
            .output()
            .expect("tmux runs");

        let before = window_size(session).expect("size before");
        assert_eq!((before.cols, before.rows), (173, 47));

        {
            let mut attachment = Attachment::open_session(session).expect("attach");
            assert_eq!(
                attachment.size, before,
                "the viewer adopts the agent's size"
            );

            attachment.write(b"echo omar-relay-ok\n").expect("write");
            let mut seen = String::new();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            while !seen.contains("omar-relay-ok") && std::time::Instant::now() < deadline {
                if let Some(chunk) = attachment.read(std::time::Duration::from_millis(200)) {
                    seen.push_str(&String::from_utf8_lossy(&chunk));
                }
            }
            assert!(seen.contains("omar-relay-ok"), "got: {seen}");

            assert_eq!(
                window_size(session).expect("size during"),
                before,
                "attaching must not resize the agent's window"
            );
        }

        // Dropping detaches. The status line is why this is not simply `rows`:
        // a client exactly as tall as the window costs the agent a row.
        assert_eq!(
            settles(session, |size| *size == before),
            before,
            "detaching must leave the window as it was"
        );
    }

    #[test]
    fn the_web_attachment_preserves_truecolor_escape_sequences() {
        if !tmux_available() {
            return;
        }
        let session = "omar-terminal-rgb";
        let _guard = test_session(session, None);
        tmux_text(&[
            "respawn-pane",
            "-k",
            "-t",
            &exact_target(session),
            r"printf '\033[38;2;17;83;149mOMAR_RGB\033[0m\n'; sleep 30",
        ])
        .unwrap();
        let attachment = Attachment::open_session(session).unwrap();
        let mut seen = String::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !seen.contains("OMAR_RGB") && std::time::Instant::now() < deadline {
            if let Some(chunk) = attachment.read(std::time::Duration::from_millis(100)) {
                seen.push_str(&String::from_utf8_lossy(&chunk));
            }
        }
        assert!(seen.contains("OMAR_RGB"), "terminal did not draw: {seen:?}");
        assert!(
            seen.contains("38;2;17;83;149m"),
            "RGB was lost or reduced to indexed color: {seen:?}"
        );
    }

    #[test]
    fn a_viewer_reflows_the_session_and_puts_it_back() {
        // The viewer resizes the session to its own shape rather than scaling a
        // picture of it — that is what makes the text crisp at any panel size.
        // Leaving the agent at the viewer's shape afterwards is the cost that
        // has to be paid back on detach.
        if !tmux_available() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let session = "omar-terminal-reflow-probe";
        let _ = tmux_command()
            .args(["kill-session", "-t", &exact_session(session)])
            .output();
        let _guard = SessionGuard(session.to_string());
        tmux_command()
            .args([
                "new-session",
                "-d",
                "-s",
                session,
                "-x",
                "200",
                "-y",
                "50",
                "sh",
            ])
            .output()
            .expect("tmux runs");
        let before = window_size(session).expect("size before");
        assert_eq!((before.cols, before.rows), (200, 50));

        {
            let mut attachment = Attachment::open_session(session).expect("attach");
            // `open_session` returns as soon as tmux is spawned, and a tmux that
            // cannot attach exits instead — which looks exactly like a session
            // that ignored the resize. Wait for the client to exist first, so a
            // failure here says which of the two happened.
            let attached = {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    let listed = clients(session);
                    if !listed.starts_with("none") || std::time::Instant::now() >= deadline {
                        break listed;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            };
            assert!(
                !attached.starts_with("none"),
                "no client attached: {attached}"
            );

            attachment.resize(96, 30).expect("resize");
            // tmux follows the client, so the agent is now drawing at the
            // viewer's shape.
            let during = settles(session, |size| size.cols == 96);
            assert_eq!(
                during.cols,
                96,
                "the session did not follow the viewer (clients: {})",
                clients(session)
            );
        }

        let after = settles(session, |size| {
            (size.cols, size.rows) == (before.cols, before.rows)
        });
        assert_eq!(
            (after.cols, after.rows),
            (before.cols, before.rows),
            "the viewer left the agent at its own shape"
        );
    }

    #[test]
    fn a_failed_attach_leaves_the_session_as_it_found_it() {
        // Mouse tracking is turned on for the viewer's benefit. An attempt that
        // never attaches has no viewer, so it has no business changing anything.
        if !tmux_available() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let session = "omar-terminal-failed-attach";
        let _ = tmux_command()
            .args(["kill-session", "-t", &exact_session(session)])
            .output();
        let _guard = SessionGuard(session.to_string());
        tmux_command()
            .args(["new-session", "-d", "-s", session, "sh"])
            .output()
            .expect("tmux runs");
        tmux_command()
            .args(["set-option", "-t", &exact_target(session), "mouse", "off"])
            .output()
            .expect("tmux runs");

        // A name that does not resolve: the attach fails before any viewer.
        assert!(Attachment::open_session("omar-terminal-does-not-exist").is_err());

        let mouse = tmux_command()
            .args(["show-options", "-t", &exact_target(session), "-v", "mouse"])
            .output()
            .expect("tmux answers");
        assert_eq!(String::from_utf8_lossy(&mouse.stdout).trim(), "off");
    }

    #[test]
    fn attaching_lets_the_viewer_scroll() {
        // Without mouse tracking a tmux client never sees a wheel event, so the
        // viewer cannot scroll back through what the agent already drew.
        if !tmux_available() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let session = "omar-terminal-mouse-probe";
        let _ = tmux_command()
            .args(["kill-session", "-t", &exact_session(session)])
            .output();
        let _guard = SessionGuard(session.to_string());
        tmux_command()
            .args([
                "new-session",
                "-d",
                "-s",
                session,
                "-x",
                "80",
                "-y",
                "24",
                "sh",
            ])
            .output()
            .expect("tmux runs");
        tmux_command()
            .args(["set-option", "-t", &exact_target(session), "mouse", "off"])
            .output()
            .expect("tmux runs");

        let attachment = Attachment::open_session(session).expect("attach");

        let reported = tmux_command()
            .args(["show-options", "-t", &exact_target(session), "-v", "mouse"])
            .output()
            .expect("tmux answers");
        drop(attachment);
        assert_eq!(String::from_utf8_lossy(&reported.stdout).trim(), "on");
    }

    #[test]
    fn a_missing_session_is_not_confused_with_its_neighbour() {
        // tmux resolves `-t name` by prefix when nothing matches exactly, so a
        // lookup for an agent that has gone would land on one whose name it is
        // a prefix of — reporting that agent's size, or attaching to it.
        if !tmux_available() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let gone = "omar-terminal-prefix";
        let neighbour = "omar-terminal-prefix2";
        for name in [gone, neighbour] {
            let _ = tmux_command()
                .args(["kill-session", "-t", &exact_session(name)])
                .output();
        }
        let _guard = SessionGuard(neighbour.to_string());
        tmux_command()
            .args([
                "new-session",
                "-d",
                "-s",
                neighbour,
                "-x",
                "111",
                "-y",
                "31",
                "sh",
            ])
            .output()
            .expect("tmux runs");

        // The neighbour is fine to read; the one that does not exist must not
        // resolve to it.
        assert_eq!(window_size(neighbour).expect("neighbour").cols, 111);
        assert!(
            window_size(gone).is_err(),
            "a session that does not exist resolved to '{neighbour}'"
        );
        assert!(
            Attachment::open_session(gone).is_err(),
            "attached to the wrong session"
        );
    }

    fn test_session(name: &str, policy: Option<&str>) -> SessionGuard {
        let _ = tmux_command()
            .args(["kill-session", "-t", &exact_session(name)])
            .output();
        tmux_text(&[
            "new-session",
            "-d",
            "-s",
            name,
            "-x",
            "120",
            "-y",
            "40",
            "sh",
        ])
        .unwrap();
        tmux_text(&["set-option", "-t", &exact_target(name), "status", "off"]).unwrap();
        if let Some(policy) = policy {
            tmux_text(&[
                "set-option",
                "-w",
                "-t",
                &exact_target(name),
                "window-size",
                policy,
            ])
            .unwrap();
        }
        SessionGuard(name.to_string())
    }

    #[test]
    fn concurrent_viewers_restore_the_first_baseline_in_either_close_order() {
        if !tmux_available() {
            return;
        }
        for first_closes_first in [true, false] {
            let session = "omar-terminal-concurrent";
            let _guard = test_session(session, Some("smallest"));
            let original = window_size(session).unwrap();
            let first = Attachment::open_session_sized(session, Some((96, 30))).unwrap();
            assert_eq!(settles(session, |s| s.cols == 96).cols, 96);
            let second = Attachment::open_session_sized(session, Some((80, 24))).unwrap();
            assert_eq!(settles(session, |s| s.cols == 80).cols, 80);
            if first_closes_first {
                drop(first);
                assert_eq!(window_size(session).unwrap().cols, 80);
                assert_eq!(
                    local_policy(&exact_target(session)).unwrap().as_deref(),
                    Some("smallest")
                );
                drop(second);
            } else {
                drop(second);
                assert_eq!(settles(session, |s| s.cols == 96).cols, 96);
                drop(first);
            }
            assert_eq!(settles(session, |s| *s == original), original);
            assert_eq!(
                local_policy(&exact_target(session)).unwrap().as_deref(),
                Some("smallest")
            );
        }
    }

    #[test]
    fn an_external_client_keeps_ownership_when_the_web_viewer_closes() {
        if !tmux_available() {
            return;
        }
        let session = "omar-terminal-external";
        let _guard = test_session(session, Some("smallest"));
        let attachment = Attachment::open_session_sized(session, Some((96, 30))).unwrap();
        assert_eq!(settles(session, |s| s.cols == 96).cols, 96);
        // A real PTY client outside the daemon's viewer registry.
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: 25,
                cols: 70,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new("tmux");
        if let Ok(server) = std::env::var("OMAR_TMUX_SERVER") {
            if !server.trim().is_empty() {
                command.args(["-L", server.trim()]);
            }
        }
        command.args(["attach-session", "-t", &exact_session(session)]);
        command.env_remove("TMUX");
        command.env("TERM", "xterm-256color");
        let mut child = pty.slave.spawn_command(command).unwrap();
        drop(pty.slave);
        let during = settles(session, |s| s.cols == 70);
        drop(attachment);
        let after = window_size(session).unwrap();
        let policy = local_policy(&exact_target(session)).unwrap();
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!((during.cols, during.rows), (70, 25));
        assert_eq!(
            after, during,
            "detach must not pin over the remaining client"
        );
        assert_eq!(policy.as_deref(), Some("smallest"));
    }

    #[test]
    fn real_alternate_screen_reflows_without_duplicate_or_cutoff_rows_and_receives_escape() {
        if !tmux_available() {
            return;
        }
        let session = "omar-terminal-alt-reflow";
        let _guard = test_session(session, Some("smallest"));
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/ci/terminal_reflow_fixture.py");
        let command = format!(
            "python3 {}",
            crate::manager::shell_single_quote(&fixture.display().to_string())
        );
        tmux_text(&["respawn-pane", "-k", "-t", &exact_target(session), &command]).unwrap();
        let mut attachment = Attachment::open_session_sized(session, Some((96, 30))).unwrap();
        for (cols, rows) in [(96, 30), (72, 22), (140, 38), (90, 27)] {
            attachment.resize(cols, rows).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let screen = loop {
                let screen =
                    tmux_text(&["capture-pane", "-p", "-t", &exact_target(session)]).unwrap();
                let lines: Vec<_> = screen.lines().collect();
                if (lines.len() == usize::from(rows)
                    && lines[0] == format!("FRAME {cols}x{rows}")
                    && lines[1] == format!("{}R", "x".repeat(usize::from(cols) - 1))
                    && lines.last() == Some(&"BOTTOM"))
                    || std::time::Instant::now() >= deadline
                {
                    break screen;
                }
                let _ = attachment.read(std::time::Duration::from_millis(50));
            };
            let lines: Vec<_> = screen.lines().collect();
            assert_eq!(lines.len(), usize::from(rows), "{screen}");
            assert_eq!(lines[0], format!("FRAME {cols}x{rows}"));
            assert_eq!(lines[1], format!("{}R", "x".repeat(usize::from(cols) - 1)));
            assert_eq!(lines.last(), Some(&"BOTTOM"));
            assert_eq!(screen.matches("FRAME").count(), 1);
        }
        attachment.write(b"\x1b").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let screen = loop {
            let screen = tmux_text(&["capture-pane", "-p", "-t", &exact_target(session)]).unwrap();
            if screen.contains("ESCAPE-RECEIVED") || std::time::Instant::now() >= deadline {
                break screen;
            }
            let _ = attachment.read(std::time::Duration::from_millis(50));
        };
        assert!(screen.contains("ESCAPE-RECEIVED"), "{screen}");
    }

    #[test]
    fn detach_preserves_manual_policy_and_operator_policy_changes() {
        if !tmux_available() {
            return;
        }
        let session = "omar-terminal-policy";
        let _guard = test_session(session, Some("manual"));
        let attachment = Attachment::open_session_sized(session, Some((90, 28))).unwrap();
        drop(attachment);
        assert_eq!(
            local_policy(&exact_target(session)).unwrap().as_deref(),
            Some("manual")
        );
        let attachment = Attachment::open_session(session).unwrap();
        tmux_text(&[
            "set-option",
            "-w",
            "-t",
            &exact_target(session),
            "window-size",
            "largest",
        ])
        .unwrap();
        drop(attachment);
        assert_eq!(
            local_policy(&exact_target(session)).unwrap().as_deref(),
            Some("largest")
        );
    }

    #[test]
    fn detach_does_not_touch_a_replacement_session() {
        if !tmux_available() {
            return;
        }
        let session = "omar-terminal-replaced";
        let _guard = test_session(session, Some("smallest"));
        let attachment = Attachment::open_session(session).unwrap();
        tmux_text(&["kill-session", "-t", &exact_session(session)]).unwrap();
        tmux_text(&[
            "new-session",
            "-d",
            "-s",
            session,
            "-x",
            "77",
            "-y",
            "23",
            "sh",
        ])
        .unwrap();
        let replacement = window_size(session).unwrap();
        drop(attachment);
        assert_eq!(window_size(session).unwrap(), replacement);
    }

    #[test]
    fn window_sizes_are_read_and_bounded() {
        let size = parse_window_size("200x50xon").unwrap();
        assert_eq!((size.cols, size.rows, size.status_lines), (200, 50, 1));
        // A client must be a row taller than the window to leave it intact.
        assert_eq!(size.client_rows(), 51);
        assert_eq!(parse_window_size("200x50xoff").unwrap().client_rows(), 50);
        assert_eq!(parse_window_size("200x50x2").unwrap().client_rows(), 52);
        // tmux answers on stdout, so anything unparseable is a protocol change
        // rather than something to guess at.
        for bad in [
            "",
            "200",
            "200x50",
            "200x50xmaybe",
            "0x50xon",
            "200x9999xon",
        ] {
            assert!(parse_window_size(bad).is_err(), "{bad} should be rejected");
        }
    }
}
