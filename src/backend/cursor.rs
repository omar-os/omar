//! Cursor: driven over its protocol by OMAR's runner.

use super::{connection, Connection, Login};
use super::{managed, spool, PaneSetup};
use super::{Backend, Kind, Launch};
use crate::manager::{managed_agent_command, materialize_prompt_file, shell_single_quote};
use anyhow::{Context, Result};

pub struct Cursor;

impl Backend for Cursor {
    fn kind(&self) -> Kind {
        Kind::Cursor
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["cursor"]
    }
    /// The editor ships `cursor`, which runs the agent as `cursor agent`; the
    /// standalone CLI installs `cursor-agent` and `agent`.
    fn executables(&self) -> &'static [&'static str] {
        &["cursor", "cursor-agent", "agent"]
    }
    fn default_command(&self) -> &'static str {
        command_for(on_path)
    }
    /// The status command ignores `CURSOR_API_KEY`, so that is checked first.
    fn connection(&self, program: &str) -> Connection {
        if let Some(name) = connection::env_credential(&["CURSOR_API_KEY"]) {
            return Connection::connected(name);
        }
        match connection::run(program, &["agent", "status", "--format", "json"]) {
            Some(output) => agent_status(&output.stdout),
            None => Connection::unknown("`cursor agent status` did not answer"),
        }
    }
    fn install_command(&self) -> Option<&'static str> {
        Some("curl -fsS https://cursor.com/install | bash")
    }
    fn login(&self, _headless: bool) -> Option<Login> {
        Some(Login {
            args: &["agent", "login"],
            note: None,
        })
    }
    fn sandbox_services(&self) -> Option<&'static [&'static str]> {
        Some(&["cursor"])
    }
    fn readiness_markers(&self) -> &'static [&'static str] {
        &["Cursor Agent"]
    }
    /// Cursor takes no message from outside its protocol, so it runs under
    /// OMAR's protocol runner, which owns a durable inbox for it.
    fn launch_command(&self, launch: &Launch<'_>) -> String {
        let rendered = materialize_prompt_file(launch.prompt_file, launch.substitutions);
        managed_agent_command(
            "cursor",
            launch.base_command,
            &rendered,
            launch.context,
            None,
        )
        .unwrap_or_else(|error| {
            format!(
                "printf '%s\n' {} >&2; exit 1",
                shell_single_quote(&format!("OMAR protocol launch failed: {error:#}"))
            )
        })
    }

    /// Cursor takes no message from outside its protocol. Under the runner
    /// nothing else is needed; a legacy launch gets its hook installed and a
    /// spool the hook drains, pointed at by the pane's environment.
    fn prepare_pane(&self, session: &str, command: &str) -> Result<PaneSetup> {
        let mut setup = PaneSetup {
            command: command.to_string(),
            ..PaneSetup::default()
        };
        if managed::managed_launch_socket(command).is_none() && install_cursor_hook() {
            spool::reset_spool(session);
            let path = spool::spool_path(session);
            // Quoted: a space anywhere in the path would otherwise split the
            // assignment and take the rest of the launch line with it.
            setup.command = format!(
                "OMAR_EVENT_SPOOL={} {}",
                shell_single_quote(&path.display().to_string()),
                command
            );
            setup.stamp = Some(spool::stamp(&path));
        }
        Ok(setup)
    }
    /// The hook only runs once the agent is already working, so a queued
    /// event cannot wake an idle one.
    fn spool_wakes_idle(&self) -> bool {
        false
    }
    fn hook_reply(&self, events: &[String]) -> Option<String> {
        Some(if events.is_empty() {
            "{}".to_string()
        } else {
            serde_json::json!({ "additional_context": events.join("\n\n") }).to_string()
        })
    }

    /// Only the hooks with an injection output contract take context, and
    /// only the ones that fire before the model reads consume the spool.
    fn hook_takes_context(&self, input: &serde_json::Value) -> bool {
        input["hook_event_name"] != "beforeSubmitPrompt"
    }
    fn hook_consumes_spool(&self, input: &serde_json::Value) -> bool {
        matches!(
            input["hook_event_name"].as_str(),
            None | Some("sessionStart" | "postToolUse" | "postToolUseFailure")
        )
    }
}

/// The standalone CLI when installed, else the editor's `cursor agent`.
fn command_for(installed: impl Fn(&str) -> bool) -> &'static str {
    [
        ("cursor-agent", "cursor-agent --yolo"),
        ("agent", "agent --yolo"),
    ]
    .into_iter()
    .find(|(executable, _)| installed(executable))
    .map_or("cursor agent --yolo", |(_, command)| command)
}

fn on_path(executable: &str) -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|dir| dir.join(executable).is_file())
}

/// The tail of the command OMAR installs, and the only thing it will replace
/// when re-installing.
const CURSOR_HOOK_ARGS: &str = "hook-drain --format cursor";

/// Install OMAR's hook so cursor-agent will collect events mid-turn.
///
/// The hook is one entry in the operator's own `~/.cursor/hooks.json`, added
/// without disturbing whatever else is in there. It runs for every pane and
/// reads `$OMAR_EVENT_SPOOL`, so one entry serves them all.
///
/// Returns false if the file cannot be written — the caller must then leave
/// delivery unavailable, because a spool nothing drains is a black hole.
pub(crate) fn install_cursor_hook() -> bool {
    let Some(exe) = std::env::current_exe().ok() else {
        return false;
    };
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let path = home.join(".cursor").join("hooks.json");

    let mut config: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok())
        .unwrap_or_else(|| serde_json::json!({ "version": 1 }));
    if !config.is_object() {
        return false;
    }

    let command = format!(
        "{} {}",
        crate::manager::shell_single_quote(&exe.display().to_string()),
        CURSOR_HOOK_ARGS
    );
    let entry = serde_json::json!({ "command": command });
    let hooks = config
        .as_object_mut()
        .expect("checked above")
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        return false;
    };

    // Migrate the old entry: beforeSubmitPrompt has no context-output field.
    if let Some(list) = hooks
        .get_mut("beforeSubmitPrompt")
        .and_then(serde_json::Value::as_array_mut)
    {
        list.retain(|hook| {
            !hook["command"]
                .as_str()
                .is_some_and(|command| command.ends_with(CURSOR_HOOK_ARGS))
        });
    }
    // Supported injection points plus a bounded stop continuation.
    for event in ["sessionStart", "postToolUse", "postToolUseFailure", "stop"] {
        let list = hooks
            .entry(event)
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        let mut kept: Vec<serde_json::Value> = list
            .into_iter()
            .filter(|hook| {
                !hook
                    .get("command")
                    .and_then(|command| command.as_str())
                    // Match our own entry precisely. A bare "hook-drain"
                    // would also drop an operator hook that merely mentions
                    // it — theirs is not ours to remove.
                    .is_some_and(|command| command.ends_with(CURSOR_HOOK_ARGS))
            })
            .collect();
        kept.push(entry.clone());
        hooks.insert(event.to_string(), serde_json::Value::Array(kept));
    }

    spool::write_json_atomically(&path, &config)
}

/// OMAR runs `cursor agent ...`, as the Cursor app's `cursor` command takes
/// it; the CLI's own installer only provides `cursor-agent`. Without the app,
/// give `~/.local/bin` a `cursor` that forwards to it, as the sandbox kit does.
pub(crate) fn link_cursor_command() -> Result<()> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    if std::env::split_paths(&path).any(|dir| dir.join("cursor").is_file()) {
        return Ok(());
    }
    let home = dirs::home_dir().context("no home directory")?;
    let shim = home.join(".local/bin/cursor");
    std::fs::create_dir_all(home.join(".local/bin"))?;
    std::fs::create_dir_all(home.join(".local/bin"))?;
    std::fs::write(
        &shim,
        "#!/bin/sh\n[ \"$1\" = agent ] && shift\nexec \"$HOME/.local/bin/cursor-agent\" \"$@\"\n",
    )?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// `cursor agent status --format json` exits 0 signed in or not.
fn agent_status(stdout: &str) -> Connection {
    let Ok(status) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return Connection::unknown("`cursor agent status` gave no JSON");
    };
    match status["isAuthenticated"].as_bool() {
        Some(true) => Connection::Connected { via: None },
        Some(false) => Connection::SignedOut,
        None => Connection::unknown("`cursor agent status` gave no answer OMAR knows"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn agent_status_reads_is_authenticated() {
        use super::{agent_status, Connection};
        assert_eq!(
            agent_status(
                r#"{"status":"unauthenticated","isAuthenticated":false,"message":"Not logged in"}"#
            ),
            Connection::SignedOut
        );
        assert_eq!(
            agent_status(r#"{"isAuthenticated":true}"#),
            Connection::Connected { via: None }
        );
        assert!(matches!(
            agent_status("Not logged in"),
            Connection::Unknown { .. }
        ));
    }

    use super::*;

    #[test]
    fn the_standalone_cli_is_preferred_over_the_editor() {
        assert_eq!(command_for(|_| true), "cursor-agent --yolo");
        assert_eq!(command_for(|name| name == "agent"), "agent --yolo");
        assert_eq!(command_for(|_| false), "cursor agent --yolo");
        for command in ["cursor-agent --yolo", "agent --yolo", "cursor agent --yolo"] {
            assert_eq!(crate::backend::command_name(command), Some("cursor"));
        }
    }

    #[test]
    fn installing_the_cursor_hook_keeps_the_operators_own_hooks() {
        let _guard = crate::test_env_lock();
        let home = tempfile::tempdir().unwrap();
        let previous = std::env::var("HOME").ok();
        std::env::set_var("HOME", home.path());

        let path = home.path().join(".cursor").join("hooks.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"version":1,"hooks":{"postToolUse":[{"command":"their-own-linter"}],
                "beforeShellExecution":[{"command":"their-audit"}]}}"#,
        )
        .unwrap();

        assert!(install_cursor_hook());
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        let commands = |event: &str| -> Vec<String> {
            config["hooks"][event]
                .as_array()
                .unwrap()
                .iter()
                .map(|hook| hook["command"].as_str().unwrap().to_string())
                .collect()
        };

        // Theirs survives, on the event they put it on and on ours.
        assert!(commands("postToolUse")
            .iter()
            .any(|c| c == "their-own-linter"));
        assert_eq!(commands("beforeShellExecution"), vec!["their-audit"]);
        for event in ["sessionStart", "postToolUse", "postToolUseFailure", "stop"] {
            assert!(
                commands(event).iter().any(|c| c.contains("hook-drain")),
                "{event} must call OMAR"
            );
        }

        // Installing again must not stack up duplicates.
        assert!(install_cursor_hook());
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let drains = config["hooks"]["postToolUse"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|hook| hook["command"].as_str().unwrap().contains("hook-drain"))
            .count();
        assert_eq!(drains, 1, "reinstalling must replace, not append");

        match previous {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn an_operator_hook_that_merely_mentions_hook_drain_is_left_alone() {
        let _guard = crate::test_env_lock();
        let home = tempfile::tempdir().unwrap();
        let previous = std::env::var("HOME").ok();
        std::env::set_var("HOME", home.path());

        let path = home.path().join(".cursor").join("hooks.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Theirs mentions the subcommand but is not ours.
        std::fs::write(
            &path,
            r#"{"hooks":{"postToolUse":[{"command":"log 'omar hook-drain ran'"}]}}"#,
        )
        .unwrap();

        assert!(install_cursor_hook());
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let commands: Vec<&str> = config["hooks"]["postToolUse"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hook| hook["command"].as_str().unwrap())
            .collect();
        assert!(
            commands.iter().any(|c| c.contains("log '")),
            "the operator's hook must survive: {commands:?}"
        );
        assert_eq!(commands.len(), 2, "ours is added, theirs is kept");

        match previous {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}
