//! opencode: launched bare on a port, configured through the environment,
//! and given its prompt as the first message over HTTP.

use super::{Backend, Kind, Launch};
use crate::manager::{
    actor_file, materialize_mcp_context_file, mcp_ea_dir, omar_server_exe, shell_single_quote,
    write_private_file, McpLaunchContext, BACKEND_NATIVE_AGENT_TOOLS, BACKEND_NATIVE_WAKE_TOOLS,
};

pub struct Opencode;

impl Backend for Opencode {
    fn kind(&self) -> Kind {
        Kind::Opencode
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["opencode"]
    }
    fn executables(&self) -> &'static [&'static str] {
        &["opencode"]
    }
    fn default_command(&self) -> &'static str {
        // opencode has no permission-skip flag.
        "opencode"
    }
    fn readiness_markers(&self) -> &'static [&'static str] {
        &["tab agents", "ctrl+p commands"]
    }
    /// opencode has no `--append-system-prompt`, and `--prompt` is treated
    /// as the first user message, which makes the model read the agent
    /// prompt descriptively and ask back "What is your agent name?". So the
    /// prompt rides in the first message instead.
    fn takes_prompt_in_first_message(&self) -> bool {
        true
    }
    /// Spawned bare, with a port so OMAR can reach it without the input box;
    /// the prompt arrives through that channel as synthetic context.
    fn launch_command(&self, launch: &Launch<'_>) -> String {
        let base_command = with_opencode_port(launch.base_command);
        match opencode_config_env(launch.context) {
            Some(config) => format!(
                "OPENCODE_CONFIG_CONTENT={} {}",
                shell_single_quote(&config),
                base_command
            ),
            None => base_command,
        }
    }
}

/// Give opencode a port so OMAR can reach it without the input box.
///
/// opencode only listens when it is told a port: with none it talks to an
/// in-process worker over a fake hostname, and there is nothing to connect to.
/// Nothing is broken if the port cannot be claimed — the pane simply launches
/// without a side channel and events go through the composer.
pub(crate) fn with_opencode_port(base_command: &str) -> String {
    if base_command
        .split_whitespace()
        .any(|token| token == "--port" || token.starts_with("--port=") || token == "--hostname")
    {
        return base_command.to_string();
    }
    match crate::channel::free_port() {
        Some(port) => format!("{} --port {}", base_command, port),
        None => base_command.to_string(),
    }
}

pub(crate) fn materialize_opencode_coordination_plugin(
    context: &McpLaunchContext,
) -> Option<String> {
    if context.topology.is_some() {
        return None;
    }
    let exe = omar_server_exe()?;
    let context_file = materialize_mcp_context_file(context)?;
    let path = mcp_ea_dir(context)?
        .join(actor_file(context.agent_name.as_deref(), "coordination").replace(".json", ".mjs"));
    let body = format!(
        "const exe = {};\nconst contextFile = {};\n{}",
        serde_json::to_string(&exe).ok()?,
        serde_json::to_string(&context_file).ok()?,
        include_str!("../backend_hooks/opencode.mjs")
    );
    write_private_file(&path, body.as_bytes()).ok()?;
    let encoded: String = path
        .to_str()?
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    Some(format!("file://{encoded}"))
}

pub(crate) fn opencode_config_env(context: &McpLaunchContext) -> Option<String> {
    let server_exe = omar_server_exe()?;
    let context_file = materialize_mcp_context_file(context)?;
    // Disable every backend-native tool that overlaps an OMAR MCP tool so
    // delegation/scheduling can only flow through OMAR and stays visible in
    // the dashboard. Names that opencode does not expose are no-ops.
    let mut tools = serde_json::Map::new();
    for name in BACKEND_NATIVE_WAKE_TOOLS
        .iter()
        .chain(BACKEND_NATIVE_AGENT_TOOLS.iter())
    {
        tools.insert((*name).to_string(), serde_json::Value::Bool(false));
    }
    let mut config = serde_json::json!({
        "mcp": {
            "omar": {
                "type": "local",
                "enabled": true,
                "command": [
                    server_exe.display().to_string(),
                    "mcp-server",
                    "--context-file",
                    context_file.display().to_string()
                ]
            }
        },
        "tools": tools,
        "permission": {
            "doom_loop": "deny"
        }
    });
    if let Some(plugin) = materialize_opencode_coordination_plugin(context) {
        config["plugin"] = serde_json::json!([plugin]);
    }
    Some(config.to_string())
}
