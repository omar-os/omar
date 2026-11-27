//! The model-free backend: answers invocations from the topology context.

use std::path::{Path, PathBuf};

use super::{spool, PaneSetup};
use super::{Backend, Kind, Launch, Target};
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
        // A session name is reused by every attempt; an earlier worker's
        // conversation must never be read as this one's.
        let _ = std::fs::remove_file(conversation_file(&path));
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

    /// What the stub wrote beside its spool, and only if the process that
    /// wrote it is this pane's.
    fn conversation_id(&self, target: &Target<'_>) -> Option<String> {
        let path = conversation_file(&spool::from_stamp(target.stamp?)?);
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        let pid = u32::try_from(value["pid"].as_u64()?).ok()?;
        (pid == target.pane_pid || super::claude::child_pids(target.pane_pid).contains(&pid))
            .then(|| value["session_id"].as_str().map(str::to_owned))
            .flatten()
    }

    /// Each worker is a new `omar stub-agent` process holding nothing but its
    /// launch context, and it reports the attempt's `native_session` from
    /// there as its conversation.
    fn fresh_conversation(&self) -> bool {
        true
    }
}

/// Where a stub worker records the conversation it was started with.
pub(crate) fn conversation_file(spool: &Path) -> PathBuf {
    spool.with_extension("conversation")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_panes_stub_reports_a_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("agent.jsonl");
        let stamp = spool::stamp(&spool);
        let target = |pane_pid| Target {
            name: "agent",
            pane_pid,
            stamp: Some(&stamp),
        };
        let me = std::process::id();
        assert_eq!(Stub.conversation_id(&target(me)), None);
        let write = |pid: u32| {
            std::fs::write(
                conversation_file(&spool),
                serde_json::json!({"pid": pid, "session_id": "new"}).to_string(),
            )
            .unwrap()
        };
        write(me);
        assert_eq!(Stub.conversation_id(&target(me)), Some("new".into()));
        // Another process's file is not this pane's conversation: the runtime
        // keeps waiting rather than trusting it.
        write(1);
        assert_eq!(Stub.conversation_id(&target(me)), None);
        assert_eq!(
            Stub.conversation_id(&Target {
                stamp: None,
                ..target(me)
            }),
            None
        );
    }

    #[test]
    fn a_new_stub_pane_forgets_the_last_conversation() {
        let _lock = crate::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _state = crate::manager::tests::EnvVarGuard::set("OMAR_STATE_DIR", dir.path());
        let stale = conversation_file(&spool::spool_path("agent"));
        std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
        std::fs::write(&stale, r#"{"pid":1,"session_id":"old"}"#).unwrap();
        let setup = Stub.prepare_pane("agent", "omar stub-agent").unwrap();
        assert!(!stale.exists());
        assert_eq!(
            setup.stamp.as_deref().and_then(spool::from_stamp),
            Some(stale.with_extension("jsonl"))
        );
    }
}
