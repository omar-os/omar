//! The model-free backend: answers invocations from the topology context.

use super::{spool, PaneSetup};
use super::{Backend, Kind, Launch};
use crate::manager::{materialize_mcp_context_file, omar_server_exe, shell_single_quote};
use anyhow::Result;

pub struct Stub;

impl Backend for Stub {
    fn kind(&self) -> Kind {
        Kind::Stub
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["stub"]
    }
    fn executables(&self) -> &'static [&'static str] {
        // Matched on the subcommand: the executable is `omar` itself.
        &["stub-agent"]
    }
    fn default_command(&self) -> &'static str {
        // Answers invocations without a model, so a run can be exercised end
        // to end in a test. Resolved to this binary in `launch_command`.
        "omar stub-agent"
    }
    /// The stub reads the endpoint and token straight from the topology
    /// context, so it needs no MCP server of its own.
    fn launch_command(&self, launch: &Launch<'_>) -> String {
        let exe = omar_server_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "omar".to_string());
        match materialize_mcp_context_file(launch.context) {
            Some(context_file) => format!(
                "{} stub-agent --context-file {}",
                shell_single_quote(&exe),
                shell_single_quote(&context_file.display().to_string())
            ),
            None => format!("{} stub-agent", shell_single_quote(&exe)),
        }
    }

    /// The stub reads its invocations from a spool, so every pane gets one.
    fn prepare_pane(&self, session: &str, command: &str) -> Result<PaneSetup> {
        spool::reset_spool(session);
        let path = spool::spool_path(session);
        Ok(PaneSetup {
            command: format!(
                "OMAR_EVENT_SPOOL={} {}",
                shell_single_quote(&path.display().to_string()),
                command
            ),
            stamp: Some(spool::stamp(&path)),
            ..PaneSetup::default()
        })
    }
}
