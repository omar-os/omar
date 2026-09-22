//! Everything OMAR knows about an agent backend, in one place.
//!
//! Each backend is one file in this directory implementing [`Backend`]. The
//! rest of the tree reaches a backend only through the registry here: it asks
//! for one by name or detects one from a command line, and never names one
//! itself. Adding a backend is one variant of [`Kind`], one file, and one
//! entry in [`ALL`].

use std::path::Path;

use crate::manager::McpLaunchContext;

pub(crate) mod antigravity;
pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod cursor;
pub(crate) mod opencode;
pub(crate) mod stub;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Antigravity,
    Claude,
    Codex,
    Cursor,
    Opencode,
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

pub const ALL: [&dyn Backend; 6] = [
    &claude::Claude,
    &codex::Codex,
    &cursor::Cursor,
    &opencode::Opencode,
    &antigravity::Antigravity,
    &stub::Stub,
];

/// Backends an operator can run an assistant on.
///
/// The stub is deliberately absent: it answers invocations without a model,
/// which is useful for exercising a run and useless for talking to.
pub const ASSISTANT: [Kind; 5] = [
    Kind::Claude,
    Kind::Codex,
    Kind::Cursor,
    Kind::Opencode,
    Kind::Antigravity,
];

pub fn of(kind: Kind) -> &'static dyn Backend {
    ALL.into_iter()
        .find(|backend| backend.kind() == kind)
        .expect("every kind is registered")
}

pub fn assistant_names() -> [&'static str; 5] {
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
            "Unknown backend '{}'. Supported: claude, codex, cursor, opencode, agy, stub, web",
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

/// The backend a command line runs: the first token that is one of ours, so
/// an `env` wrapper or a variable assignment before it does not hide it.
pub fn detect(command: &str) -> Option<&'static dyn Backend> {
    command.split_whitespace().find_map(detect_token)
}

/// The canonical name of the backend a command line runs.
pub fn command_name(command: &str) -> Option<&'static str> {
    detect(command).map(|backend| backend.kind().name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_is_registered_once_under_its_canonical_name() {
        for kind in [
            Kind::Antigravity,
            Kind::Claude,
            Kind::Codex,
            Kind::Cursor,
            Kind::Opencode,
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
            ["claude", "codex", "cursor", "opencode", "agy"]
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
}
