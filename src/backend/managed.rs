//! The client side of OMAR's protocol runner: a backend process OMAR owns,
//! with a durable inbox that never writes to a terminal composer.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::{Target, WRITE_TIMEOUT};

/// The stamp a session carries when its launch line names a runner socket.
pub(crate) fn stamp(command: &str) -> Option<String> {
    managed_launch_socket(command).map(|socket| format!("managed:{}", socket.display()))
}

pub(crate) fn from_stamp(stamp: &str) -> Option<PathBuf> {
    let rest = stamp.strip_prefix("managed:")?;
    (!rest.is_empty()).then(|| PathBuf::from(rest))
}

/// Where an invocation-scoped attempt's runner reports the conversation its
/// backend opened: beside its socket, in a directory new to its launch.
pub(crate) fn report_path(socket: &Path) -> PathBuf {
    socket.with_file_name("conversation.json")
}

/// The report of the runner behind `target`, and only if a process of this
/// pane wrote it.
fn report(target: &Target<'_>) -> Option<serde_json::Value> {
    let socket = from_stamp(target.stamp?)?;
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(report_path(&socket)).ok()?).ok()?;
    let pid = u32::try_from(report["pid"].as_u64()?).ok()?;
    crate::process::process_tree(target.pane_pid)
        .iter()
        .any(|(member, _)| *member == pid)
        .then_some(report)
}

/// The conversation an attempt's runner reported opening.
pub(crate) fn reported_conversation(target: &Target<'_>) -> Option<String> {
    report(target)?["conversation_id"]
        .as_str()
        .map(str::to_owned)
}

/// The runner still reports `id`, and found nothing that keeps it from
/// being new: its CLI's own store had no entry for it before the worker
/// started and has one now, and nothing that store hands a new conversation.
pub(crate) fn prove_reported(target: &Target<'_>, id: &str) -> Result<()> {
    let report = report(target).context("the runner's report is gone")?;
    anyhow::ensure!(
        report["conversation_id"] == id,
        "the runner now reports {}",
        report["conversation_id"]
    );
    match report["refused"].as_str() {
        Some(why) => anyhow::bail!("{why}"),
        None => Ok(()),
    }
}

/// Hand a message to the runner's inbox. A reply names the accepted message;
/// anything else is a rejection, and nothing was queued.
pub(crate) fn deliver(socket: &std::path::Path, text: &str) -> Result<()> {
    use std::io::BufRead;
    let mut stream = UnixStream::connect(socket)?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    stream.set_read_timeout(Some(WRITE_TIMEOUT))?;
    writeln!(stream, "{}", serde_json::json!({"text":text}))?;
    let mut reply = String::new();
    std::io::BufReader::new(stream).read_line(&mut reply)?;
    let reply: serde_json::Value = serde_json::from_str(&reply)?;
    anyhow::ensure!(
        reply["accepted"].is_string(),
        "protocol inbox rejected message: {reply}"
    );
    Ok(())
}

/// Read the per-pane endpoint without changing or interpreting CODEX_HOME.
/// Preserve support for already-running legacy launches during the transition.
pub(crate) fn managed_launch_socket(command: &str) -> Option<PathBuf> {
    let assignment = command.split_once("export OMAR_AGENT_SOCKET=")?.1;
    unquote_single(assignment).map(PathBuf::from)
}

/// Undo `shell_single_quote`: read one `'...'` word, in which a literal quote
/// appears as `'\''`.
pub(crate) fn unquote_single(text: &str) -> Option<String> {
    let mut rest = text.strip_prefix('\'')?;
    let mut out = String::new();
    loop {
        let (chunk, tail) = rest.split_once('\'')?;
        out.push_str(chunk);
        match tail.strip_prefix("\\''") {
            // An escaped quote: the word continues.
            Some(tail) => {
                out.push('\'');
                rest = tail;
            }
            // Anything else closes the word.
            None => return Some(out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A report counts only from this pane's processes, while it still
    /// names the conversation, and only if the runner found nothing against
    /// it.
    #[test]
    fn a_report_counts_only_from_the_panes_own_runner() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("agent.sock");
        let stamp = format!("managed:{}", socket.display());
        let write = |pid: u32, refused: Option<&str>| {
            let report =
                serde_json::json!({"conversation_id": "new", "pid": pid, "refused": refused});
            std::fs::write(report_path(&socket), report.to_string()).unwrap();
        };
        let mut pane = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let target = |pane_pid| Target {
            name: "worker",
            pane_pid,
            stamp: Some(&stamp),
        };
        assert_eq!(reported_conversation(&target(pane.id())), None);
        // This test's own process is no part of the pane's tree.
        write(std::process::id(), None);
        assert_eq!(reported_conversation(&target(pane.id())), None);
        assert!(prove_reported(&target(pane.id()), "new").is_err());
        write(pane.id(), None);
        assert_eq!(
            reported_conversation(&target(pane.id())).as_deref(),
            Some("new")
        );
        prove_reported(&target(pane.id()), "new").unwrap();
        assert!(prove_reported(&target(pane.id()), "other")
            .unwrap_err()
            .to_string()
            .contains("now reports"));
        write(pane.id(), Some("its store already held it"));
        assert_eq!(
            prove_reported(&target(pane.id()), "new")
                .unwrap_err()
                .to_string(),
            "its store already held it"
        );
        pane.kill().unwrap();
        pane.wait().unwrap();
    }
}
