//! Everything OMAR knows about an agent backend, in one place.
//!
//! Each backend is one file in this directory implementing [`Backend`]. The
//! rest of the tree reaches a backend only through the registry here: it asks
//! for one by name or detects one from a command line, and never names one
//! itself. Adding a backend is one variant of [`Kind`], one file, and one
//! entry in [`ALL`].

use std::path::Path;
use std::time::Duration;

use anyhow::Result;

use crate::manager::McpLaunchContext;

pub(crate) mod antigravity;
pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod cursor;
pub(crate) mod managed;
pub(crate) mod opencode;
pub(crate) mod pi;
pub(crate) mod spool;
pub(crate) mod stub;

/// How long to wait on a socket that has accepted the connection but is not
/// reading. A timeout reports failure; it never enables terminal-input delivery.
pub(crate) const WRITE_TIMEOUT: Duration = Duration::from_secs(3);

/// A retryable pre-send condition: nothing has been sent, and the caller may
/// try again once the backend has come up.
#[derive(Debug)]
pub struct NotReady(pub &'static str);

impl std::fmt::Display for NotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for NotReady {}

/// A launched session, as delivery needs to see it.
pub struct Target<'a> {
    /// The tmux session.
    pub name: &'a str,
    /// The pane's own process. The backend may be a child of it.
    pub pane_pid: u32,
    /// What launch recorded about how to reach the backend, if anything.
    pub stamp: Option<&'a str>,
}

/// Delivery through the transport a stamp names. The transports are shared:
/// which one a session has is decided at launch, not by its backend.
fn deliver_stamped(
    backend: &(impl Backend + ?Sized),
    target: &Target<'_>,
    stamp: &str,
    text: &str,
) -> Result<()> {
    use anyhow::Context;
    if let Some(socket) = managed::from_stamp(stamp) {
        // A pane that has exited takes its runner with it.
        if !socket.exists() {
            return Err(NotReady("the runner socket is gone").into());
        }
        return managed::deliver(&socket, text)
            .with_context(|| delivery_failed(target, "managed backend protocol"));
    }
    if let Some(path) = spool::from_stamp(stamp) {
        // A spool only works if the hook is draining it. If the oldest event
        // has been waiting too long it plainly is not.
        if spool::spool_is_stale(&path) {
            return Err(NotReady("the hook has stopped draining its spool").into());
        }
        anyhow::ensure!(
            backend.spool_wakes_idle(),
            "{} has a passive legacy hook channel; relaunch it as an OMAR protocol session to enable idle wake",
            target.name
        );
        return spool::append(&path, text).with_context(|| delivery_failed(target, "hook spool"));
    }
    if let Some(socket) = codex::from_stamp(stamp) {
        // A pane that has exited takes its app-server with it.
        if !socket.exists() {
            return Err(NotReady("the app-server socket is gone").into());
        }
        return match codex::deliver_via_app_server(&socket, text) {
            Err(error) if error.is::<NotReady>() => Err(error),
            result => result.with_context(|| delivery_failed(target, "codex app-server")),
        };
    }
    if let Some((port, session)) = opencode::from_stamp(stamp) {
        return opencode::deliver_over_http(port, &session, text)
            .with_context(|| delivery_failed(target, "opencode http api"));
    }
    backend.discover_and_deliver(target, text)
}

/// The context every final delivery error carries.
pub(crate) fn delivery_failed(target: &Target<'_>, via: &str) -> String {
    format!(
        "delivery to {} via {via} failed; composer untouched",
        target.name
    )
}

/// What proves a launched pane is ready for its first message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// A side channel the launch provisioned; delivery waits on it.
    Channel,
    /// Banner text the TUI paints once its input widget is live.
    Banner(&'static [&'static str]),
    /// Nothing is known; a caller may wait for the pane to stop changing.
    Settle,
}

/// What a pane is launched with, beyond the command an operator gave.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PaneSetup {
    /// The line the pane runs.
    pub command: String,
    /// Environment the pane's shell starts with.
    pub env: Vec<(String, String)>,
    /// A window style to apply when the operator has not set one.
    pub window_style: Option<String>,
    /// A delivery stamp to record before the pane runs.
    pub stamp: Option<String>,
}

impl PaneSetup {
    /// These backends draw a TUI in a real tmux pane, even when their launcher
    /// has no terminal. Advertise RGB and take NO_COLOR from this launch, not
    /// from the environment a long-lived tmux server happened to inherit.
    pub(crate) fn interactive(command: &str) -> Self {
        let mut env = vec![("COLORTERM".into(), "truecolor".into())];
        let no_color = std::env::var_os("NO_COLOR");
        if let Some(value) = &no_color {
            env.push(("NO_COLOR".into(), value.to_string_lossy().into_owned()));
        }
        Self {
            // An empty value is still an opt-out for some backends. Unset a
            // stale server value only when the caller did not supply one.
            command: if no_color.is_none() {
                format!("unset NO_COLOR; {command}")
            } else {
                command.into()
            },
            env,
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Antigravity,
    Claude,
    Codex,
    Cursor,
    Opencode,
    Pi,
    Stub,
}

impl Kind {
    /// The canonical name, which is what goes on the wire and in a session's
    /// backend stamp.
    pub fn name(self) -> &'static str {
        of(self).aliases()[0]
    }
}

/// What a worker launch is made of. The manager assembles it; a backend
/// turns it into its own command line.
pub struct Launch<'a> {
    /// The operator's command, already passed through `normalize_command`.
    pub base_command: &'a str,
    pub prompt_file: &'a Path,
    pub substitutions: &'a [(&'a str, &'a str)],
    /// The prompt inlined as a shell expression, for backends that take it
    /// on the command line.
    pub shell_expr: &'a str,
    pub context: &'a McpLaunchContext,
}

pub trait Backend: Send + Sync {
    fn kind(&self) -> Kind;
    /// Names an operator may type. The first is canonical.
    fn aliases(&self) -> &'static [&'static str];
    /// Executable basenames, as a pane reports the process it runs.
    fn executables(&self) -> &'static [&'static str];
    /// The command `omar -a <name>` runs.
    fn default_command(&self) -> &'static str;
    /// Startup banner text, for launch diagnostics only. Delivery waits for
    /// the backend's channel and never types into the widget these describe.
    fn readiness_markers(&self) -> &'static [&'static str] {
        &[]
    }
    /// How a caller learns that a launched pane can take work. A launch under
    /// OMAR's protocol runner has no banner: the runner's socket is the proof,
    /// and delivery waits on it. Otherwise the backend's banner, or nothing
    /// to wait for at all.
    fn readiness(&self, command: &str) -> Readiness {
        if managed::managed_launch_socket(command).is_some() {
            return Readiness::Channel;
        }
        match self.readiness_markers() {
            [] => Readiness::Settle,
            markers => Readiness::Banner(markers),
        }
    }
    /// Flags a command line must carry before this backend runs under OMAR.
    /// A line that already carries them, or that must not, comes back as is.
    fn normalize_command(&self, command: &str) -> String {
        command.to_string()
    }
    /// Whether the agent prompt must ride in the first message because the
    /// backend has no way to take it at launch.
    fn takes_prompt_in_first_message(&self) -> bool {
        false
    }
    /// A worker's launch line.
    fn launch_command(&self, launch: &Launch<'_>) -> String;
    /// Launch-time pane setup: the line as it will run, environment, and a
    /// stamp to record before the pane exists. The line is unchanged by
    /// default.
    fn prepare_pane(&self, _session: &str, command: &str) -> Result<PaneSetup> {
        Ok(PaneSetup {
            command: command.to_string(),
            ..PaneSetup::default()
        })
    }
    /// Once the pane exists: wait for the side channel and say how to reach
    /// it. By default that is the runner's socket, when the line names one.
    fn provision(&self, _session: &str, command: &str) -> Result<Option<String>> {
        Ok(managed::stamp(command))
    }
    /// Hand `text` to the agent without touching its composer. `NotReady`
    /// means nothing was sent and the caller may try again; any other error
    /// is final, and never authorizes terminal input.
    ///
    /// A stamp recorded at launch names the transport, whatever the backend;
    /// a session launch recorded nothing about is the backend's to find.
    fn deliver(&self, target: &Target<'_>, text: &str) -> Result<()> {
        match target.stamp {
            Some(stamp) => deliver_stamped(self, target, stamp, text),
            None => self.discover_and_deliver(target, text),
        }
    }
    /// Delivery to a session whose launch recorded no transport: found from
    /// the process, or not at all.
    fn discover_and_deliver(&self, _target: &Target<'_>, _text: &str) -> Result<()> {
        Err(NotReady("no delivery channel").into())
    }
    /// Whether an event queued in a hook spool reaches an idle agent. A
    /// passive hook only runs once the agent is already working.
    fn spool_wakes_idle(&self) -> bool {
        true
    }
    /// The reply the backend's hook expects, carrying queued events, or
    /// `None` when the backend runs no hook of OMAR's.
    fn hook_reply(&self, _events: &[String]) -> Option<String> {
        None
    }
    /// Whether this hook invocation takes context at all.
    fn hook_takes_context(&self, _input: &serde_json::Value) -> bool {
        true
    }
    /// Whether this hook invocation should consume the spool.
    fn hook_consumes_spool(&self, _input: &serde_json::Value) -> bool {
        true
    }
    /// Native conversation currently displayed by this pane, if unambiguous.
    fn conversation_id(&self, _target: &Target<'_>) -> Option<String> {
        None
    }

    /// An executive assistant's launch line, when it differs from a worker's.
    /// `prompt_file` holds the prompt with its placeholders intact and
    /// `prompt` is the same text with them resolved; a backend that takes the
    /// prompt by file writes the resolved text there. `None` means launch it
    /// as a worker with the prompt inlined.
    fn ea_launch_command(
        &self,
        _base_command: &str,
        _prompt_file: &Path,
        _prompt: &str,
        _context: &McpLaunchContext,
    ) -> Option<String> {
        None
    }
}

/// Native IDs are scoped to both the EA (one per chat) and backend. Never use
/// a backend's global "last session", which may belong to another window.
pub(crate) fn saved_conversation(context: &McpLaunchContext, backend: &str) -> Option<String> {
    if context.serve.is_none() || context.agent_name.is_some() || context.topology.is_some() {
        return None;
    }
    std::fs::read_to_string(conversation_path(&context.omar_dir, context.ea_id, backend))
        .ok()
        .filter(|id| !id.trim().is_empty())
}

pub(crate) fn conversation_path(
    root: &Path,
    ea_id: crate::ea::EaId,
    backend: &str,
) -> std::path::PathBuf {
    crate::ea::ea_state_dir(ea_id, root).join(format!("native-{backend}-session"))
}

pub(crate) fn remember_conversation(
    context: &McpLaunchContext,
    backend: &str,
    id: &str,
) -> Result<()> {
    if context.serve.is_some() && context.agent_name.is_none() && context.topology.is_none() {
        crate::manager::write_private_file(
            &conversation_path(&context.omar_dir, context.ea_id, backend),
            id.as_bytes(),
        )?;
    }
    Ok(())
}

pub const ALL: [&dyn Backend; 7] = [
    &claude::Claude,
    &codex::Codex,
    &cursor::Cursor,
    &opencode::Opencode,
    &pi::Pi,
    &antigravity::Antigravity,
    &stub::Stub,
];

/// Backends an operator can run an assistant on.
///
/// The stub is deliberately absent: it answers invocations without a model,
/// which is useful for exercising a run and useless for talking to.
pub const ASSISTANT: [Kind; 6] = [
    Kind::Claude,
    Kind::Codex,
    Kind::Cursor,
    Kind::Opencode,
    Kind::Pi,
    Kind::Antigravity,
];

pub fn of(kind: Kind) -> &'static dyn Backend {
    ALL.into_iter()
        .find(|backend| backend.kind() == kind)
        .expect("every kind is registered")
}

pub fn assistant_names() -> [&'static str; 6] {
    ASSISTANT.map(Kind::name)
}

/// The backend an operator named, by any of its aliases.
pub fn by_name(name: &str) -> Option<&'static dyn Backend> {
    let name = name.trim().to_ascii_lowercase();
    ALL.into_iter()
        .find(|backend| backend.aliases().contains(&name.as_str()))
}

/// Like [`by_name`], with the error an operator reads.
pub fn resolve(name: &str) -> Result<&'static dyn Backend, String> {
    // `web` never reaches here: nothing is spawned for it, so there is no
    // command to resolve. It is named so a typo is told what it meant.
    by_name(name).ok_or_else(|| {
        format!(
            "Unknown backend '{}'. Supported: claude, codex, cursor, opencode, pi, agy, stub, web",
            name
        )
    })
}

/// The backend one token of a command line runs, if it is one of ours.
pub fn detect_token(token: &str) -> Option<&'static dyn Backend> {
    let token = token.trim_matches(|c| matches!(c, '"' | '\'' | '(' | ')' | '[' | ']'));
    let executable = Path::new(token)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(token);
    ALL.into_iter()
        .find(|backend| backend.executables().contains(&executable))
}

/// Read a literal shell word without evaluating it. Retain byte offsets so
/// runtime flags can be inserted after a quoted executable path. Stop at shell
/// operators or expansions: guessing through those can modify another program.
fn command_word(command: &str, offset: &mut usize) -> Option<String> {
    let mut chars = command[*offset..].char_indices().peekable();
    while chars
        .peek()
        .is_some_and(|(_, c)| c.is_whitespace() && *c != '\n')
    {
        chars.next();
    }
    let start = chars.peek()?.0;
    if chars.peek()?.1 == '#' {
        return None;
    }
    let mut word = String::new();
    let mut quote = None;
    let mut end = start;
    while let Some((index, c)) = chars.next() {
        if quote.is_none() && (c.is_whitespace() || ";|&()<>".contains(c)) {
            break;
        }
        end = index + c.len_utf8();
        match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            (Some('\''), _) => word.push(c),
            (_, '$' | '`') => return None,
            (_, '\\') => {
                let (index, escaped) = chars.next()?;
                if quote == Some('"') && !matches!(escaped, '$' | '`' | '"' | '\\' | '\n') {
                    word.push('\\');
                }
                if escaped != '\n' {
                    word.push(escaped);
                }
                end = index + escaped.len_utf8();
            }
            _ => word.push(c),
        }
    }
    if quote.is_some() || end == start {
        return None;
    }
    *offset += end;
    Some(word)
}

fn is_environment_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .enumerate()
            .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
}

/// Only classify the executable, after literal assignments and supported
/// launch wrappers. Arguments and later pipeline/compound commands are never
/// searched for backend names.
pub(crate) fn executable(command: &str) -> Option<(Kind, usize)> {
    let mut offset = 0;
    let mut word;
    loop {
        let start = offset;
        word = command_word(command, &mut offset)?;
        if !is_environment_assignment(command[start..offset].trim_start()) {
            break;
        }
    }
    if matches!(word.as_str(), "exec" | "command") {
        word = command_word(command, &mut offset)?;
        if word == "--" {
            word = command_word(command, &mut offset)?;
        }
    }
    if Path::new(&word).file_name()?.to_str()? == "env" {
        let mut options = true;
        loop {
            word = command_word(command, &mut offset)?;
            match word.as_str() {
                "-i" | "--ignore-environment" if options => continue,
                "-u" | "--unset" | "-C" | "--chdir" if options => {
                    command_word(command, &mut offset)?;
                    continue;
                }
                "--" if options => {
                    options = false;
                    continue;
                }
                _ if options && (word.starts_with("--unset=") || word.starts_with("--chdir=")) => {
                    continue
                }
                _ if is_environment_assignment(&word) => {
                    options = false;
                    continue;
                }
                _ if word.starts_with('-') => return None,
                _ => break,
            }
        }
    }
    if Path::new(&word).file_name()?.to_str()? == "omar" {
        return (command_word(command, &mut offset)? == "stub-agent")
            .then_some((Kind::Stub, offset));
    }
    detect_token(&word).map(|backend| (backend.kind(), offset))
}

pub fn detect(command: &str) -> Option<&'static dyn Backend> {
    executable(command).map(|(kind, _)| of(kind))
}

/// The canonical name of the backend a command line runs.
pub fn command_name(command: &str) -> Option<&'static str> {
    detect(command).map(|backend| backend.kind().name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_backends_take_color_preferences_from_the_launcher_not_tmux() {
        let _lock = crate::test_env_lock();
        let original = std::env::var_os("NO_COLOR");
        // All three cases matter: no override, explicit opt-out, and an empty
        // value (which some backends also treat as an opt-out).
        for caller in [None, Some("1"), Some("")] {
            match caller {
                Some(value) => std::env::set_var("NO_COLOR", value),
                None => std::env::remove_var("NO_COLOR"),
            }
            let setups: Vec<_> = [Kind::Claude, Kind::Codex]
                .map(|kind| {
                    of(kind).prepare_pane(
                        "unused",
                        r#"printf '%s|%s' "$COLORTERM" "${NO_COLOR-unset}""#,
                    )
                })
                .into_iter()
                .collect();
            // Restore before asserting, including on a failed setup.
            match &original {
                Some(value) => std::env::set_var("NO_COLOR", value),
                None => std::env::remove_var("NO_COLOR"),
            }
            for setup in setups {
                let setup = setup.unwrap();
                let output = std::process::Command::new("sh")
                    .args(["-c", &setup.command])
                    .env("NO_COLOR", "stale-server-value")
                    .env("COLORTERM", "")
                    .envs(setup.env)
                    .output()
                    .unwrap();
                assert!(output.status.success());
                assert_eq!(
                    String::from_utf8(output.stdout).unwrap(),
                    format!("truecolor|{}", caller.unwrap_or("unset"))
                );
            }
        }
    }

    #[test]
    fn native_resume_is_scoped_to_chat_and_backend_and_preserves_worker_launches() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = crate::manager::tests::test_mcp_context(dir.path());
        context.serve = Some(crate::manager::ServeMcpContext {
            endpoint: "127.0.0.1:7340".into(),
            token: "test".into(),
        });
        std::fs::create_dir_all(crate::ea::ea_state_dir(0, dir.path())).unwrap();
        remember_conversation(&context, "claude", "11111111-1111-4111-8111-111111111111").unwrap();
        remember_conversation(&context, "codex", "saved-codex").unwrap();
        let (claude, _) =
            crate::manager::build_ea_command("claude", 0, "test", dir.path(), &context);
        assert!(claude.contains("--resume '11111111-1111-4111-8111-111111111111'"));
        assert!(claude.contains("||"));
        assert!(claude.contains("--session-id '11111111-1111-4111-8111-111111111111'"));
        let (codex, _) = crate::manager::build_ea_command("codex", 0, "test", dir.path(), &context);
        assert!(
            codex.contains("codex resume 'saved-codex' --remote"),
            "{codex}"
        );
        assert!(!codex.contains("codex resume 'saved-codex' --dangerously"));
        assert!(codex.contains("|| codex --dangerously-bypass-approvals-and-sandbox --remote"));
        let (managed, _) = crate::manager::build_ea_command(
            "codex --profile work",
            0,
            "test",
            dir.path(),
            &context,
        );
        assert!(
            managed.contains("backend-runner --backend codex"),
            "{managed}"
        );
        context.ea_id = 1;
        assert!(saved_conversation(&context, "claude").is_none());
        context.ea_id = 0;
        assert!(saved_conversation(&context, "cursor").is_none());
        context.agent_name = Some("worker".into());
        assert!(saved_conversation(&context, "claude").is_none());
        context.agent_name = None;
        context.serve = None;
        assert!(saved_conversation(&context, "claude").is_none());
    }

    #[test]
    fn managed_resume_passes_the_saved_native_id_to_the_new_runner() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = crate::manager::tests::test_mcp_context(dir.path());
        context.serve = Some(crate::manager::ServeMcpContext {
            endpoint: "127.0.0.1:7340".into(),
            token: "test".into(),
        });
        std::fs::create_dir_all(crate::ea::ea_state_dir(0, dir.path())).unwrap();
        remember_conversation(&context, "cursor", "cursor-thread").unwrap();
        crate::manager::managed_agent_command(
            "cursor",
            "cursor agent --yolo",
            &dir.path().join("prompt"),
            &context,
            None,
        )
        .unwrap();
        let config = std::fs::read_dir(dir.path().join("mcp/ea-0"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("protocol-")
            })
            .unwrap();
        let config: crate::backend_runner::Config =
            serde_json::from_slice(&std::fs::read(config).unwrap()).unwrap();
        assert_eq!(config.initial_session.as_deref(), Some("cursor-thread"));
    }

    #[test]
    fn every_kind_is_registered_once_under_its_canonical_name() {
        for kind in [
            Kind::Antigravity,
            Kind::Claude,
            Kind::Codex,
            Kind::Cursor,
            Kind::Opencode,
            Kind::Pi,
            Kind::Stub,
        ] {
            assert_eq!(of(kind).kind(), kind);
            assert_eq!(by_name(kind.name()).unwrap().kind(), kind);
            assert_eq!(
                ALL.iter().filter(|b| b.kind() == kind).count(),
                1,
                "{kind:?} registered more than once"
            );
        }
        assert_eq!(
            assistant_names(),
            ["claude", "codex", "cursor", "opencode", "pi", "agy"]
        );
    }

    #[test]
    fn names_resolve_by_alias_and_case() {
        assert_eq!(by_name("Claude-Code").unwrap().kind(), Kind::Claude);
        assert_eq!(by_name("claudecode").unwrap().kind(), Kind::Claude);
        assert_eq!(by_name(" antigravity ").unwrap().kind(), Kind::Antigravity);
        assert!(by_name("aider").is_none());
        assert!(resolve("aider --yes").is_err());
        assert!(resolve("custom-agent").is_err());
        assert_eq!(
            resolve("codex").unwrap().default_command(),
            "codex --dangerously-bypass-approvals-and-sandbox"
        );
        assert_eq!(
            resolve("claude").unwrap().default_command(),
            "claude --dangerously-skip-permissions"
        );
        assert_eq!(
            resolve("cursor").unwrap().default_command(),
            "cursor agent --yolo"
        );
        assert_eq!(resolve("opencode").unwrap().default_command(), "opencode");
        assert_eq!(
            resolve("agy").unwrap().default_command(),
            "agy --dangerously-skip-permissions"
        );
        assert_eq!(
            resolve("stub").unwrap().default_command(),
            "omar stub-agent"
        );
    }

    #[test]
    fn detection_reads_the_executable_through_wrappers_and_paths() {
        assert_eq!(
            command_name("claude --dangerously-skip-permissions"),
            Some("claude")
        );
        assert_eq!(
            command_name("env FOO=bar agy --dangerously-skip-permissions"),
            Some("agy")
        );
        assert_eq!(
            command_name("/opt/homebrew/bin/claude.exe -p hi"),
            Some("claude")
        );
        assert_eq!(
            command_name("'/usr/local/bin/codex' resume abc"),
            Some("codex")
        );
        assert_eq!(
            command_name("/usr/bin/omar stub-agent --context-file x"),
            Some("stub")
        );
        assert_eq!(
            command_name("TERM=xterm opencode --port 1"),
            Some("opencode")
        );
        assert_eq!(command_name("aider --yes"), None);
    }

    #[test]
    fn command_backend_name_detects_executable_tokens() {
        assert_eq!(
            command_name("agy --dangerously-skip-permissions"),
            Some("agy")
        );
        assert_eq!(command_name("env FOO=bar /opt/bin/codex"), Some("codex"));
        assert_eq!(command_name("bash -lc 'echo hi'"), None);
    }

    #[test]
    fn a_hook_reply_carries_every_queued_event_in_the_backends_own_shape() {
        let events = vec!["standup in 5".to_string(), "CI went red".to_string()];

        let cursor: serde_json::Value =
            serde_json::from_str(&of(Kind::Cursor).hook_reply(&events).unwrap()).unwrap();
        assert_eq!(cursor["additional_context"], "standup in 5\n\nCI went red");

        let agy: serde_json::Value =
            serde_json::from_str(&of(Kind::Antigravity).hook_reply(&events).unwrap()).unwrap();
        assert_eq!(
            agy["injectSteps"][0]["ephemeralMessage"],
            "standup in 5\n\nCI went red"
        );
    }

    #[test]
    fn an_empty_spool_still_answers_with_valid_json() {
        // A hook that prints nothing reads as a failure to the backend.
        for kind in [Kind::Cursor, Kind::Antigravity] {
            let reply = of(kind).hook_reply(&[]).unwrap();
            serde_json::from_str::<serde_json::Value>(&reply)
                .unwrap_or_else(|_| panic!("{kind:?} must render JSON, got {reply:?}"));
        }
        // A backend that runs no hook of OMAR's has no reply to give, and a
        // name that is no backend at all resolves to nothing.
        assert!(of(Kind::Claude).hook_reply(&[]).is_none());
        assert!(by_name("nonsense").is_none());
    }

    #[test]
    fn provisioning_is_gated_on_the_backend_not_on_the_command() {
        // Plenty of things are launched with a `--port` — tunnels, notebook
        // servers, tensorboard. Polling one of those and then stamping
        // whatever answered as a delivery channel would send events into it.
        // The same goes for an inherited `CODEX_HOME`.
        assert_eq!(
            opencode::opencode_port("tensorboard --port 6006"),
            Some(6006)
        );
        assert!(
            codex::codex_launch_socket("export CODEX_HOME='/somewhere'; jupyter lab").is_some()
        );
        for kind in [Kind::Claude, Kind::Cursor, Kind::Antigravity] {
            // Nothing is spawned and nothing is stamped: the guard is on the
            // backend, and the flag alone must never be enough.
            assert_eq!(
                of(kind)
                    .provision(
                        "unused-session",
                        "export CODEX_HOME='/somewhere'; tensorboard --port 6006",
                    )
                    .unwrap(),
                None
            );
        }
        // And a backend that is provisioned still needs its command to say so.
        for kind in [Kind::Opencode, Kind::Codex] {
            assert_eq!(of(kind).provision("unused-session", "bare").unwrap(), None);
        }
    }

    #[test]
    fn a_backend_without_a_side_channel_resolves_to_nothing() {
        // "No channel" must be an ordinary
        // answer rather than an error.
        let target = Target {
            name: "pane",
            pane_pid: std::process::id(),
            stamp: None,
        };
        for kind in [
            Kind::Codex,
            Kind::Opencode,
            Kind::Pi,
            Kind::Cursor,
            Kind::Antigravity,
            Kind::Stub,
        ] {
            let error = of(kind).deliver(&target, "event").unwrap_err();
            assert!(error.is::<NotReady>(), "{kind:?}: {error}");
        }
    }
}
