//! Claude Code: a system prompt on the line, hooks and the peer socket
//! through one `--settings`, and OMAR's tools over MCP.
use std::path::{Path, PathBuf};

use super::{detect, detect_token, Backend, Kind, Launch};
use crate::manager::{
    actor_file, backend_native_disallowed_tools_csv, materialize_mcp_context_file, mcp_ea_dir,
    omar_server_exe, shell_single_quote, write_private_file, McpLaunchContext,
};

pub struct Claude;

impl Backend for Claude {
    fn kind(&self) -> Kind {
        Kind::Claude
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["claude", "claude-code", "claude_code", "claudecode"]
    }
    fn executables(&self) -> &'static [&'static str] {
        // Homebrew ships Claude Code as a single-file executable named
        // `claude.exe`, which is what the pane reports it is running.
        &["claude", "claude.exe"]
    }
    fn default_command(&self) -> &'static str {
        "claude --dangerously-skip-permissions"
    }
    fn readiness_markers(&self) -> &'static [&'static str] {
        &["Claude Code", "❯"]
    }
    fn launch_command(&self, launch: &Launch<'_>) -> String {
        let base_command = with_coordination_hooks(launch.base_command, launch.context);
        match materialize_claude_mcp_config(launch.context) {
            Some(mcp_config) => format!(
                "{} --append-system-prompt \"{}\" --mcp-config {} --disallowedTools {}",
                base_command,
                launch.shell_expr,
                shell_single_quote(&mcp_config.display().to_string()),
                shell_single_quote(&backend_native_disallowed_tools_csv()),
            ),
            None => format!(
                "{} --append-system-prompt \"{}\"",
                base_command, launch.shell_expr
            ),
        }
    }
    /// The prompt goes in by file, so it never touches argv and is unbounded;
    /// the placeholders are resolved on disk since nothing pipes it through
    /// sed at launch.
    fn ea_launch_command(
        &self,
        base_command: &str,
        prompt_file: &Path,
        prompt: &str,
        context: &McpLaunchContext,
    ) -> Option<String> {
        std::fs::write(prompt_file, prompt).ok();
        let base_command = with_coordination_hooks(base_command, context);
        Some(match materialize_claude_mcp_config(context) {
            Some(mcp_config) => format!(
                "{} --append-system-prompt-file {} --mcp-config {} --disallowedTools {}",
                base_command,
                shell_single_quote(&prompt_file.display().to_string()),
                shell_single_quote(&mcp_config.display().to_string()),
                shell_single_quote(&backend_native_disallowed_tools_csv()),
            ),
            None => format!(
                "{} --append-system-prompt-file {}",
                base_command,
                shell_single_quote(&prompt_file.display().to_string()),
            ),
        })
    }
}

/// `--settings '{"crossSessionInbound":"accept"}'`, as it goes on a launch line.
///
/// It lets OMAR deliver events over Claude Code's cross-session peer socket.
/// Without it the session holds an inbound message behind an approval dialog —
/// OMAR does not attest a permission mode, and an agent launched with
/// `--dangerously-skip-permissions` distrusts a sender that has not. The dialog
/// covers the composer, so the held message is worse than no channel at all.
/// `channel` looks for the same setting before it uses the socket.
pub(crate) fn claude_inbound_settings() -> String {
    format!(
        "--settings {}",
        shell_single_quote(crate::channel::CLAUDE_INBOUND_ACCEPT)
    )
}

/// Put the inbound-peer setting on a claude launch line that has none.
///
/// It goes right after the `claude` token: order means nothing to claude, and
/// the end of the line may belong to a pipe or to a second command. A line
/// that already carries `--settings` is the operator's and is left alone —
/// Claude Code keeps only the last one it is given, so adding another would
/// replace theirs. Other backends' lines come back unchanged.
pub fn ensure_claude_inbound_settings(command: &str) -> String {
    if !detect(command).is_some_and(|b| b.kind() == Kind::Claude)
        || command
            .split_whitespace()
            .any(|token| token == "--settings" || token.starts_with("--settings="))
    {
        return command.to_string();
    }
    let mut end = 0;
    for token in command.split_whitespace() {
        let start = end + command[end..].find(token).expect("token was cut from here");
        end = start + token.len();
        if detect_token(token).is_some_and(|b| b.kind() == Kind::Claude) {
            break;
        }
    }
    format!(
        "{} {}{}",
        &command[..end],
        claude_inbound_settings(),
        &command[end..]
    )
}

/// Keep caller-supplied settings intact. The durable scheduler and MCP
/// projections still supervise sessions whose custom settings own their hooks.
pub(crate) fn with_coordination_hooks(command: &str, context: &McpLaunchContext) -> String {
    if context.topology.is_some()
        || command
            .split_whitespace()
            .any(|t| t == "--settings" || t.starts_with("--settings="))
    {
        return command.to_owned();
    }
    let Some(context_file) = materialize_mcp_context_file(context) else {
        return command.to_owned();
    };
    let Some(exe) = omar_server_exe() else {
        return command.to_owned();
    };
    let hook = format!(
        "{} agent-hook --context-file {}",
        shell_single_quote(&exe.display().to_string()),
        shell_single_quote(&context_file.display().to_string())
    );
    let handler = serde_json::json!([{"hooks":[{"type":"command","command":hook,"timeout":5}]}]);
    let settings = serde_json::json!({"crossSessionInbound":"accept","hooks":{
        "SessionStart":handler,"UserPromptSubmit":handler,"Stop":handler
    }});
    format!(
        "{command} --settings {}",
        shell_single_quote(&settings.to_string())
    )
}

pub(crate) fn materialize_claude_mcp_config(context: &McpLaunchContext) -> Option<PathBuf> {
    let server_exe = omar_server_exe()?;
    let context_file = materialize_mcp_context_file(context)?;
    let json = serde_json::json!({
        "mcpServers": {
            "omar": {
                "type": "stdio",
                "command": server_exe,
                "args": ["mcp-server", "--context-file", context_file],
            }
        }
    });

    let dir = mcp_ea_dir(context)?;
    let path = match &context.topology {
        Some(topology) => dir.join(format!(
            "claude-mcp-topology-{}-{}.json",
            topology.team, topology.agent
        )),
        None => dir.join(actor_file(context.agent_name.as_deref(), "claude-mcp")),
    };
    write_private_file(&path, &serde_json::to_vec(&json).ok()?).ok()?;
    Some(path)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::tests::*;
    use crate::manager::TopologyMcpContext;

    #[test]
    fn a_claude_launch_line_is_told_to_accept_peer_messages_once() {
        // Without this the session holds OMAR's messages behind an approval
        // dialog that covers the composer, and no event is ever delivered.
        let flag = r#"--settings '{"crossSessionInbound":"accept"}'"#;
        let once = ensure_claude_inbound_settings("claude --dangerously-skip-permissions");
        assert_eq!(
            once,
            format!("claude {flag} --dangerously-skip-permissions")
        );
        assert_eq!(ensure_claude_inbound_settings(&once), once);
        // By the executable: the tail of a line may belong to another program.
        assert_eq!(
            ensure_claude_inbound_settings("TERM=xterm claude -p hi 2>&1 | tee run.log"),
            format!("TERM=xterm claude {flag} -p hi 2>&1 | tee run.log")
        );
        // The delivery side looks for exactly this pair in the pane's argv.
        assert!(once.contains(&format!(
            "--settings '{}'",
            crate::channel::CLAUDE_INBOUND_ACCEPT
        )));
    }

    #[test]
    fn an_operators_own_settings_are_not_replaced() {
        // Claude Code keeps only the last `--settings`; adding ours after the
        // operator's would silently drop theirs.
        for line in [
            "claude --settings ~/team.json",
            "claude --settings=~/team.json --dangerously-skip-permissions",
        ] {
            assert_eq!(ensure_claude_inbound_settings(line), line);
        }
    }

    #[test]
    fn only_claude_is_told_about_inbound_peer_messages() {
        // The flag is Claude Code's; handing it to another backend would be an
        // unrecognised argument at launch.
        for base in [
            "codex",
            "opencode",
            "cursor agent --yolo",
            "agy --dangerously-skip-permissions",
            "bash",
        ] {
            let cmd = ensure_claude_inbound_settings(base);
            assert!(
                !cmd.contains("crossSessionInbound"),
                "{base} must not receive a Claude-only flag: {cmd}"
            );
        }
    }

    #[test]
    fn coordination_hooks_preserve_explicit_settings_and_skip_topology() {
        let dir = tempfile::tempdir().unwrap();
        let context = test_mcp_context(dir.path());
        let custom = "claude --settings '/tmp/my settings.json'";
        assert_eq!(with_coordination_hooks(custom, &context), custom);
        let hooked = with_coordination_hooks("claude", &context);
        for event in ["SessionStart", "UserPromptSubmit", "Stop"] {
            assert!(hooked.contains(event));
        }
        let mut topology = context;
        topology.topology = Some(TopologyMcpContext {
            team: "t".into(),
            agent: "a".into(),
            endpoint: "localhost:1".into(),
            token: "t".into(),
        });
        assert_eq!(with_coordination_hooks("claude", &topology), "claude");
    }
}
