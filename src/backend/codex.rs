//! Codex: a per-agent app-server the TUI attaches to, or a native exec
//! runner when the TUI would refuse the flags.

use anyhow::{Context, Result};
use uuid::Uuid;

use super::{detect, detect_token, Backend, Kind, Launch};
use crate::manager::{
    managed_agent_command, materialize_mcp_context_file, materialize_prompt_file, omar_server_exe,
    shell_single_quote, short_protocol_dir, write_private_file, McpLaunchContext,
};

pub struct Codex;

impl Backend for Codex {
    fn kind(&self) -> Kind {
        Kind::Codex
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["codex"]
    }
    fn executables(&self) -> &'static [&'static str] {
        &["codex"]
    }
    fn default_command(&self) -> &'static str {
        "codex --dangerously-bypass-approvals-and-sandbox"
    }
    fn readiness_markers(&self) -> &'static [&'static str] {
        &["OpenAI Codex"]
    }
    fn normalize_command(&self, command: &str) -> String {
        ensure_codex_runtime_flags(command)
    }
    /// Conversations stay in the operator's normal home. A dedicated
    /// app-server owns the per-agent overrides and a socket the TUI connects
    /// to explicitly; custom flags that the TUI refuses to attach with use a
    /// native exec runner instead, preserving Codex config layers.
    fn launch_command(&self, launch: &Launch<'_>) -> String {
        let base_command = launch.base_command;
        let mcp_context = launch.context;
        let instructions = std::fs::read_to_string(launch.prompt_file).map(|body| {
            launch
                .substitutions
                .iter()
                .fold(body, |body, (pattern, replacement)| {
                    body.replace(pattern, replacement)
                })
        });
        let (tui_command, reasoning_effort) = codex_launch_reasoning_effort(base_command);
        if !codex_refuses_to_attach(&tui_command) {
            if let Some(command) = instructions.ok().and_then(|instructions| {
                codex_server_command(
                    mcp_context,
                    &instructions,
                    reasoning_effort.as_deref(),
                    &tui_command,
                )
            }) {
                return command;
            }
        }
        let rendered = materialize_prompt_file(launch.prompt_file, launch.substitutions);
        let protocol = (|| -> Result<String> {
            let (mut command, initial_session) = codex_exec_command(base_command)?;
            let overrides =
                codex_mcp_overrides(mcp_context).context("cannot configure Codex MCP")?;
            command.push(' ');
            command.push_str(&overrides);
            managed_agent_command("codex", &command, &rendered, mcp_context, initial_session)
        })();
        protocol.unwrap_or_else(|error| {
            format!(
                "printf '%s\n' {} >&2; exit 1",
                shell_single_quote(&format!("OMAR Codex protocol launch failed: {error:#}"))
            )
        })
    }
}

pub(crate) fn ensure_codex_runtime_flags(base_command: &str) -> String {
    if !detect(base_command).is_some_and(|b| b.kind() == Kind::Codex) {
        return base_command.to_string();
    }

    // Remote resume must use the thread's persisted permissions. Codex rejects
    // permission overrides on this path, including an automatically added YOLO.
    let mut words = base_command.split_whitespace();
    if words.any(|word| detect_token(word).is_some_and(|b| b.kind() == Kind::Codex))
        && matches!(words.next(), Some("resume" | "fork"))
    {
        return base_command.to_string();
    }
    let mut command = base_command.to_string();

    if !base_command
        .split_whitespace()
        .any(|token| token == "--dangerously-bypass-approvals-and-sandbox")
    {
        command.push_str(" --dangerously-bypass-approvals-and-sandbox");
    }

    command
}

pub(crate) fn codex_mcp_overrides(context: &McpLaunchContext) -> Option<String> {
    let server_exe = omar_server_exe()?;
    let context_file = materialize_mcp_context_file(context)?;
    let command = serde_json::to_string(&server_exe.display().to_string()).ok()?;
    let args = serde_json::to_string(&vec![
        "mcp-server".to_string(),
        "--context-file".to_string(),
        context_file.display().to_string(),
    ])
    .ok()?;
    let command_arg = format!("mcp_servers.omar.command={}", command);
    let args_arg = format!("mcp_servers.omar.args={}", args);
    Some(format!(
        "-c features.scheduled_tasks=false -c {} -c {}",
        shell_single_quote(&command_arg),
        shell_single_quote(&args_arg)
    ))
}

/// The longest socket path codex will bind.
///
/// Not the operating system's limit — macOS itself accepts 103 bytes — but
/// codex's own, measured against the shipped 0.147.0 binary: 95 binds and 96
/// fails with "path must be shorter than SUN_LEN". A path in between passes
/// the OS and is refused by codex, which loses the channel silently and leaves
/// a home behind for nothing, so the stricter bound is the useful one. It may
/// move with the codex version.
pub(crate) const SUN_PATH_MAX: usize = 96;

/// Flags that make codex refuse to attach to a running app-server.
///
/// The refusal is silent, so a pane carrying one of these would start a server
/// nobody joins, sit out the socket wait, and then have provisioning poll it
/// for ninety seconds — all to arrive where it started. Better to see the flag
/// and not build the home at all.
pub(crate) const CODEX_NO_ATTACH_FLAGS: &[&str] = &[
    "-c",
    "--config",
    "--profile",
    "-p",
    "--strict-config",
    "--search",
    "--approve-for-me",
    "--enable",
    "--disable",
    "--dangerously-bypass-hook-trust",
];

/// Flags unsupported by remote TUI attachment run through native `exec` instead.
/// Keep profile/config semantics in Codex itself; never flatten its config layers.
pub(crate) fn codex_exec_command(command: &str) -> Result<(String, Option<String>)> {
    let words = shlex::split(command).context("invalid quoted Codex command")?;
    let executable = words
        .iter()
        .position(|word| detect_token(word).is_some_and(|b| b.kind() == Kind::Codex))
        .context("missing Codex executable")?;
    anyhow::ensure!(
        !words
            .iter()
            .any(|word| matches!(word.as_str(), ";" | "&&" | "||" | "|")),
        "use a wrapper executable for compound Codex commands"
    );
    let resume = words
        .iter()
        .enumerate()
        .skip(executable + 1)
        .find(|(_, word)| word.as_str() == "resume")
        .map(|(i, _)| i);
    let initial_session = resume
        .map(|i| {
            words
                .get(i + 1)
                .filter(|v| !v.starts_with('-'))
                .cloned()
                .context("managed Codex resume requires an explicit session ID")
        })
        .transpose()?;
    let mut out = Vec::new();
    for (i, word) in words.iter().enumerate() {
        if resume.is_some_and(|r| i == r || i == r + 1) {
            continue;
        }
        if i < executable {
            if let Some((name, value)) = word.split_once('=') {
                if name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                    out.push(format!("{name}={}", shell_single_quote(value)));
                    continue;
                }
            }
        }
        match word.as_str() {
            "--search" if i > executable => out.push("-c 'web_search=\"live\"'".into()),
            "--no-alt-screen" if i > executable => {}
            _ => out.push(shell_single_quote(word)),
        }
        if i == executable {
            out.push("exec --skip-git-repo-check".into());
        }
    }
    Ok((out.join(" "), initial_session))
}

pub(crate) fn codex_refuses_to_attach(base_command: &str) -> bool {
    base_command.split_whitespace().any(|token| {
        let flag = token.split_once('=').map_or(token, |(flag, _)| flag);
        CODEX_NO_ATTACH_FLAGS.contains(&flag)
            || (token.starts_with("-c") && !token.starts_with("--"))
            || (token.starts_with("-p") && !token.starts_with("--"))
    })
}

/// Move the override emitted by `spawn_agent` onto the dedicated app-server.
/// Only consume that known literal spelling, leaving arbitrary shell/config
/// expressions on the fallback path. Keep the original command for fallback
/// so an IO failure never loses the requested effort.
pub(crate) fn codex_launch_reasoning_effort(base_command: &str) -> (String, Option<String>) {
    // spawn_agent appends the override; runtime flag normalization may append
    // the bypass flag after it. Do not search inside arbitrary shell arguments.
    let runtime_flag = " --dangerously-bypass-approvals-and-sandbox";
    let (command, suffix) = base_command
        .strip_suffix(runtime_flag)
        .map_or((base_command, ""), |command| (command, runtime_flag));
    for value in ["low", "medium", "high", "xhigh"] {
        let flag = format!(" -c model_reasoning_effort='\"{value}\"'");
        if let Some(command) = command.strip_suffix(&flag) {
            // Multiple overrides or other config flags still trigger the
            // fallback; do not reorder or reinterpret their precedence.
            return (format!("{command}{suffix}"), Some(value.to_string()));
        }
    }
    (base_command.to_string(), None)
}

/// Isolate the live connection, not the user's saved conversations. Both the
/// server and TUI inherit the operator's CODEX_HOME (or Codex's normal default).
/// Only transient socket and prompt files live under OMAR's runtime directory.
pub(crate) fn codex_server_command(
    context: &McpLaunchContext,
    instructions: &str,
    reasoning_effort: Option<&str>,
    tui_command: &str,
) -> Option<String> {
    // The server must come from the same executable and environment wrapper
    // as the requested TUI, including an explicitly selected Codex install.
    let executable = tui_command
        .split_whitespace()
        .find(|word| detect_token(word).is_some_and(|b| b.kind() == Kind::Codex))?;
    let executable_end = tui_command.find(executable)? + executable.len();
    let server_command = &tui_command[..executable_end];
    let overrides = codex_mcp_overrides(context)?;
    let id = Uuid::new_v4().simple().to_string();
    let runtime = context.omar_dir.join("codex-runtime").join(&id[..12]);
    let socket = if runtime.join("app.sock").as_os_str().len() >= SUN_PATH_MAX {
        short_protocol_dir().ok()?.join("app.sock")
    } else {
        runtime.join("app.sock")
    };
    std::fs::create_dir_all(runtime.parent()?).ok()?;
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&runtime)
        .ok()?;
    let prompt_file = runtime.join("instructions.json");
    let encoded = serde_json::to_vec(instructions).ok()?;
    if write_private_file(&prompt_file, &encoded).is_err() {
        let _ = std::fs::remove_dir_all(&runtime);
        return None;
    }
    let effort = reasoning_effort
        .map(|effort| {
            format!(
                " -c {}",
                shell_single_quote(&format!("model_reasoning_effort=\"{effort}\""))
            )
        })
        .unwrap_or_default();
    let endpoint = shell_single_quote(&format!("unix://{}", socket.display()));
    Some(format!(
        "export OMAR_CODEX_SOCKET={socket}; \
         {server_command} app-server --listen {endpoint} {overrides}{effort} \
         -c \"developer_instructions=$(cat {prompt_file})\" >{log} 2>&1 & \
         omar_srv=$!; omar_waited=0; \
         while [ ! -S \"$OMAR_CODEX_SOCKET\" ] && [ \"$omar_waited\" -lt 100 ] \
         && kill -0 \"$omar_srv\" 2>/dev/null; \
         do sleep 0.2; omar_waited=$((omar_waited+1)); done; \
         if [ ! -S \"$OMAR_CODEX_SOCKET\" ]; then \
         cat {log} >&2; kill \"$omar_srv\" 2>/dev/null; exit 1; fi; \
         {tui_command} --remote {endpoint}",
        socket = shell_single_quote(&socket.display().to_string()),
        prompt_file = shell_single_quote(&prompt_file.display().to_string()),
        log = shell_single_quote(&runtime.join("server.log").display().to_string()),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::build_agent_command;
    use crate::manager::tests::*;

    #[test]
    fn codex_launch_does_not_install_untrusted_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let overrides = codex_mcp_overrides(&test_mcp_context(dir.path())).unwrap();
        assert!(overrides.contains("mcp_servers.omar.command"));
        assert!(!overrides.contains("hooks."));
        assert!(!overrides.contains("developer_instructions"));
    }

    #[test]
    fn codex_uses_its_alternate_screen_unless_explicitly_disabled() {
        let default = ensure_codex_runtime_flags("codex");
        assert_eq!(default, "codex --dangerously-bypass-approvals-and-sandbox");
        assert_eq!(ensure_codex_runtime_flags(&default), default);
        let explicit = "codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox";
        assert_eq!(ensure_codex_runtime_flags(explicit), explicit);
        assert_eq!(ensure_codex_runtime_flags("bash"), "bash");
    }

    #[test]
    fn codex_exec_preserves_config_precedence_and_native_resume() {
        let (command, session) = codex_exec_command(
            "env TEAM='a b' codex --profile work --search -c model_reasoning_effort='\"low\"' -c model_reasoning_effort='\"high\"' resume saved-session"
        ).unwrap();
        assert_eq!(session.as_deref(), Some("saved-session"));
        assert_eq!(
            shlex::split(&command).unwrap(),
            [
                "env",
                "TEAM=a b",
                "codex",
                "exec",
                "--skip-git-repo-check",
                "--profile",
                "work",
                "-c",
                "web_search=\"live\"",
                "-c",
                "model_reasoning_effort=\"low\"",
                "-c",
                "model_reasoning_effort=\"high\""
            ]
        );
        assert!(codex_exec_command("codex --search resume --last").is_err());
    }

    #[test]
    fn the_socket_bound_is_the_one_codex_enforces_not_the_kernel() {
        // Measured against codex 0.147.0: a 95-byte socket path binds, 96
        // fails with "path must be shorter than SUN_LEN". macOS itself would
        // take 103, and a path in that gap is the bad case — it passes the
        // kernel, codex refuses it, and the channel is lost silently while a
        // home is left behind for nothing.
        assert_eq!(
            SUN_PATH_MAX, 96,
            "the guard must reject the paths codex rejects, not the ones the OS does"
        );
    }

    #[test]
    fn deep_state_paths_keep_a_short_native_codex_socket() {
        let dir = short_tempdir();
        let deep = dir.path().join("d".repeat(90));
        std::fs::create_dir_all(&deep).unwrap();
        let prompt = deep.join("ea.md");
        std::fs::write(&prompt, "be helpful").unwrap();
        let cmd = build_agent_command("codex", &prompt, &[], &test_mcp_context(&deep));
        let socket = crate::channel::codex_launch_socket(&cmd).unwrap();
        assert!(socket.as_os_str().len() < SUN_PATH_MAX);
        assert!(socket.parent().unwrap().is_dir());
        assert!(!cmd.contains("CODEX_HOME="));
        assert!(cmd.contains("mcp_servers.omar.command"));
    }

    /// The dedup joined two strings; a missing separator is a launch that
    /// never reaches codex, and no `contains` assertion would notice.
    #[test]
    fn a_provisioning_launch_is_valid_shell() {
        let out = std::process::Command::new("sh")
            .args(["-n", "-c", ""])
            .output()
            .expect("sh -n");
        assert!(out.status.success(), "sh cannot syntax-check");

        let dir = short_tempdir();
        let cmd = codex_server_command(&test_mcp_context(dir.path()), "be helpful", None, "codex")
            .unwrap();
        let checked = std::process::Command::new("sh")
            .arg("-n")
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.take().unwrap().write_all(cmd.as_bytes())?;
                child.wait_with_output()
            })
            .expect("syntax-check the launch command");
        assert!(
            checked.status.success(),
            "generated launch is not valid shell: {}\n{}",
            String::from_utf8_lossy(&checked.stderr),
            cmd
        );
        // Syntax alone does not catch a lost separator: the append would just
        // redirect into `config.tomlcodex` and `sh -n` would still be happy.
        assert!(
            cmd.contains("; codex app-server"),
            "the trust fragment must end in a separator: {cmd}"
        );
    }

    #[test]
    fn launching_codex_preserves_legacy_conversation_history() {
        let dir = short_tempdir();
        let history = dir.path().join("codex/old/sessions/conversation.jsonl");
        std::fs::create_dir_all(history.parent().unwrap()).unwrap();
        std::fs::write(&history, "persistent conversation").unwrap();
        codex_server_command(&test_mcp_context(dir.path()), "be helpful", None, "codex").unwrap();
        assert_eq!(
            std::fs::read_to_string(history).unwrap(),
            "persistent conversation"
        );
    }
}
