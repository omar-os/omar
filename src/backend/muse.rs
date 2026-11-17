//! Muse Code (Meta): driven over the Agent Client Protocol by OMAR's runner.
//! `muse acp` takes prompts on stdin, so nothing ever types into its TUI.

use std::path::PathBuf;

use super::{connection, Connection, Login};
use super::{Backend, Kind, Launch};
use crate::manager::{managed_agent_command, materialize_prompt_file, shell_single_quote};

pub struct Muse;

impl Backend for Muse {
    fn kind(&self) -> Kind {
        Kind::Muse
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["muse", "muse-code", "musecode"]
    }
    fn executables(&self) -> &'static [&'static str] {
        &["muse"]
    }
    /// `--yolo` as on Muse's own TUI; the runner turns it into what `muse
    /// acp` takes (see `acp_command`).
    fn default_command(&self) -> &'static str {
        "muse --yolo"
    }
    /// `META_API_KEY` wins over the account login, as in Muse itself. The
    /// stored login is only looked for, never opened.
    fn connection(&self, _program: &str) -> Connection {
        if let Some(name) = connection::env_credential(&["META_API_KEY"]) {
            return Connection::connected(name);
        }
        match auth_path() {
            Some(path) if path.is_file() => Connection::connected("Meta account"),
            _ => Connection::SignedOut,
        }
    }
    fn install_command(&self) -> Option<&'static str> {
        Some("curl -fsSL https://dev.meta.ai/install.sh | bash")
    }
    fn login(&self, _headless: bool) -> Option<Login> {
        Some(Login {
            args: &["login"],
            note: None,
        })
    }
    fn credential_vars(&self) -> &'static [&'static str] {
        &["META_API_KEY"]
    }
    fn logout(&self) -> Option<Login> {
        Some(Login {
            args: &["logout"],
            note: None,
        })
    }
    fn sign_in_check(&self, _program: &str) -> Option<Vec<&'static str>> {
        Some(vec!["exec", connection::CHECK_PROMPT])
    }
    /// Muse takes no message from outside its protocol, so it runs under
    /// OMAR's protocol runner, which owns a durable inbox for it.
    fn launch_command(&self, launch: &Launch<'_>) -> String {
        let rendered = materialize_prompt_file(launch.prompt_file, launch.substitutions);
        managed_agent_command("muse", launch.base_command, &rendered, launch.context, None)
            .unwrap_or_else(|error| {
                format!(
                    "printf '%s\n' {} >&2; exit 1",
                    shell_single_quote(&format!("OMAR protocol launch failed: {error:#}"))
                )
            })
    }
}

/// Where Muse keeps its account login, by the launcher's own rules.
fn auth_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MUSE_AUTH_PATH") {
        return Some(PathBuf::from(path));
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .map(|config| config.join("muse").join("auth.json"))
}

/// The operator's `muse …` line as `muse acp …`. `acp` must come right after
/// the executable, and refuses `--yolo`: there approval is chosen per session
/// (the runner selects `allowAll`), so only the rest of what `--yolo` means
/// on the TUI is passed on.
pub(crate) fn acp_command(command: &str) -> String {
    let mut words: Vec<&str> = Vec::new();
    let mut placed = false;
    for word in command.split_whitespace() {
        if word == "--yolo" {
            continue;
        }
        words.push(word);
        if !placed && super::detect_token(word).is_some_and(|b| b.kind() == Kind::Muse) {
            words.push("acp");
            placed = true;
        }
    }
    if command.split_whitespace().any(|word| word == "--yolo") {
        words.extend(["--trust-workspace", "--disable-sandbox"]);
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_operators_line_becomes_an_acp_host_with_yolo_spelled_out() {
        assert_eq!(
            acp_command("muse --yolo"),
            "muse acp --trust-workspace --disable-sandbox"
        );
        assert_eq!(
            acp_command("TERM=xterm /opt/bin/muse --model spark --yolo --provider echo"),
            "TERM=xterm /opt/bin/muse acp --model spark --provider echo --trust-workspace --disable-sandbox"
        );
        // Without --yolo the sandbox stays on and approval stays Muse's.
        assert_eq!(acp_command("muse"), "muse acp");
    }
}
