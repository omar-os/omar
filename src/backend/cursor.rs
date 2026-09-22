//! Cursor: driven over its protocol by OMAR's runner.

use super::{Backend, Kind, Launch};
use crate::manager::{managed_agent_command, materialize_prompt_file, shell_single_quote};

pub struct Cursor;

impl Backend for Cursor {
    fn kind(&self) -> Kind {
        Kind::Cursor
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["cursor"]
    }
    fn executables(&self) -> &'static [&'static str] {
        &["cursor"]
    }
    fn default_command(&self) -> &'static str {
        "cursor agent --yolo"
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
}
