//! The client side of OMAR's protocol runner: a backend process OMAR owns,
//! with a durable inbox that never writes to a terminal composer.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use anyhow::Result;

use super::WRITE_TIMEOUT;

/// The stamp a session carries when its launch line names a runner socket.
pub(crate) fn stamp(command: &str) -> Option<String> {
    managed_launch_socket(command).map(|socket| format!("managed:{}", socket.display()))
}

pub(crate) fn from_stamp(stamp: &str) -> Option<PathBuf> {
    let rest = stamp.strip_prefix("managed:")?;
    (!rest.is_empty()).then(|| PathBuf::from(rest))
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

/// The runner stores its native conversation beside its configuration.
pub(crate) fn conversation_id(target: &super::Target<'_>) -> Option<String> {
    let socket = from_stamp(target.stamp?)?;
    let path = std::fs::read_to_string(socket.parent()?.join("inbox-path")).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    value["session"].as_str().map(str::to_owned)
}

pub(crate) fn ended_turn(target: &super::Target<'_>) -> Option<String> {
    let socket = from_stamp(target.stamp?)?;
    let path = std::fs::read_to_string(socket.parent()?.join("inbox-path")).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if !value["pending"].as_array()?.is_empty() {
        return None;
    }
    value["completed"].as_str().map(str::to_owned)
}

pub(crate) fn blocked_input(target: &super::Target<'_>) -> Option<String> {
    let socket = from_stamp(target.stamp?)?;
    let path = std::fs::read_to_string(socket.parent()?.join("inbox-path")).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    value["input"].as_str().map(str::to_owned)
}

pub(crate) fn answer_input(target: &super::Target<'_>, id: &str, text: &str) -> Result<bool> {
    use std::io::BufRead;
    let socket = from_stamp(target.stamp.unwrap_or_default())
        .ok_or(super::NotReady("runner socket missing"))?;
    let mut stream = UnixStream::connect(socket)?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    stream.set_read_timeout(Some(WRITE_TIMEOUT))?;
    writeln!(
        stream,
        "{}",
        serde_json::json!({"answer_input":id,"text":text})
    )?;
    let mut reply = String::new();
    std::io::BufReader::new(stream).read_line(&mut reply)?;
    let reply: serde_json::Value = serde_json::from_str(&reply)?;
    Ok(reply["answered"] == true)
}
