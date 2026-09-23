//! A file the backend's own hook drains into model context.
//!
//! Some backends take no message from outside, but run hooks that may return
//! extra context, and that context is attached to the turn rather than
//! rendered as something the user said. OMAR appends events here and
//! `omar hook-drain` hands them over when the hook fires. Unlike a socket this
//! is reactive: an event waits until the agent next runs a tool or the user
//! next submits.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

/// How long an event may sit in a spool before OMAR stops trusting the hook.
/// A hook that is not firing would otherwise swallow every event silently.
const SPOOL_STALE: Duration = Duration::from_secs(600);

/// The stamp a session carries when its events queue in a spool.
pub(crate) fn from_stamp(stamp: &str) -> Option<PathBuf> {
    let rest = stamp.strip_prefix("spool:")?;
    (!rest.is_empty()).then(|| PathBuf::from(rest))
}

pub(crate) fn stamp(path: &Path) -> String {
    format!("spool:{}", path.display())
}

/// Queue an event for the hook to hand over.
pub(crate) fn append(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create spool directory")?;
    }
    let line = serde_json::json!({ "at": now_secs(), "text": text }).to_string();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open spool {}", path.display()))?;
    writeln!(file, "{}", line).context("append to spool")?;
    Ok(())
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// Has the oldest queued event been waiting longer than a working hook would
/// ever leave it?
pub(crate) fn spool_is_stale(path: &Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    contents
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|event| event.get("at")?.as_u64())
        .min()
        .is_some_and(|oldest| now_secs().saturating_sub(oldest) > SPOOL_STALE.as_secs())
}

/// Take everything queued in a spool, leaving it empty.
///
/// Truncating as we read is what keeps an event from being delivered twice
/// when the hook fires again a moment later.
pub(crate) fn drain_spool(path: &Path) -> Vec<String> {
    // Claim the queue by renaming it: cursor registers two hook events, and
    // if both fire at once a read-then-truncate would hand the same events
    // over twice. Only one rename can win.
    let claimed = path.with_extension("draining");
    let Ok(()) = std::fs::rename(path, &claimed) else {
        return Vec::new();
    };
    let contents = std::fs::read_to_string(&claimed).unwrap_or_default();
    let _ = std::fs::remove_file(&claimed);
    contents
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|event| event.get("text")?.as_str().map(str::to_string))
        .collect()
}

/// Where a pane's pending events queue up, for backends reached by a hook.
pub(crate) fn reset_spool(session: &str) {
    // Session names are reused when a pane is killed and recreated. Left
    // alone, the new agent would be handed the old one's events, or find a
    // backlog already old enough to be treated as a dead hook.
    let _ = std::fs::remove_file(spool_path(session));
}

pub(crate) fn spool_path(session: &str) -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".omar")
        .join("events")
        .join(format!("{}.jsonl", session))
}

/// Replace a file in one step.
///
/// These hook files are shared with the operator and with other panes: a
/// half-written one reads as invalid JSON, and the next writer would treat it
/// as absent and overwrite whatever the operator had configured.
pub(crate) fn write_json_atomically(path: &Path, value: &serde_json::Value) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return false;
    }
    let temp = parent.join(format!(".omar-{}.tmp", std::process::id()));
    if std::fs::write(&temp, value.to_string()).is_err() {
        return false;
    }
    if std::fs::rename(&temp, path).is_err() {
        let _ = std::fs::remove_file(&temp);
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spool_hands_over_each_event_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pane.jsonl");

        append(&path, "first").unwrap();
        append(&path, "second").unwrap();
        assert_eq!(drain_spool(&path), vec!["first", "second"]);

        // Draining empties it, so the next hook call does not repeat them.
        assert!(drain_spool(&path).is_empty());

        append(&path, "third").unwrap();
        assert_eq!(drain_spool(&path), vec!["third"]);
    }

    #[test]
    fn an_event_with_newlines_survives_the_spool() {
        // Events are one JSON object per line; a multi-line event must not
        // become several events.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pane.jsonl");
        append(&path, "line one\nline two").unwrap();
        assert_eq!(drain_spool(&path), vec!["line one\nline two"]);
    }

    #[test]
    fn a_spool_nothing_is_draining_stops_being_used() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pane.jsonl");
        let stamp = format!("spool:{}", path.display());

        // Nothing queued: the hook has nothing to prove yet.
        append(&path, "fresh").unwrap();
        let target = super::super::Target {
            name: "pane",
            pane_pid: 0,
            stamp: Some(&stamp),
        };
        let cursor = super::super::of(super::super::Kind::Cursor);
        let fresh = cursor.deliver(&target, "next").unwrap_err();
        assert!(
            !fresh.is::<super::super::NotReady>() && fresh.to_string().contains("relaunch"),
            "a spool with recent events is a channel, and a passive one: {fresh}"
        );

        // An event that has been sitting there far too long means the hook is
        // not running, so delivery must report channel unavailability.
        let stale = serde_json::json!({ "at": 1_000, "text": "ancient" });
        std::fs::write(&path, format!("{}\n", stale)).unwrap();
        let stale = cursor.deliver(&target, "next").unwrap_err();
        assert!(
            stale.is::<super::super::NotReady>(),
            "a backed-up spool must not swallow further events: {stale}"
        );
    }

    #[test]
    fn a_reused_session_name_does_not_inherit_the_last_agents_events() {
        let _guard = crate::test_env_lock();
        let home = tempfile::tempdir().unwrap();
        let previous = std::env::var("HOME").ok();
        std::env::set_var("HOME", home.path());

        let path = spool_path("omar-agent-1-work");
        append(&path, "meant for the previous agent").unwrap();
        assert!(!drain_spool(&path).is_empty() || path.exists());

        append(&path, "still here").unwrap();
        reset_spool("omar-agent-1-work");
        assert!(drain_spool(&path).is_empty(), "a new pane starts clean");

        match previous {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn a_half_written_hook_file_never_reaches_disk() {
        // The file is shared with the operator; a truncated one reads as
        // absent and the next writer would overwrite their configuration.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("hooks.json");
        assert!(write_json_atomically(
            &path,
            &serde_json::json!({ "theirs": 1 })
        ));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&path).unwrap())
                .unwrap()["theirs"],
            1
        );
        // No temp files left behind.
        let strays: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "temp file left behind");
    }

    #[test]
    fn a_spool_stamp_round_trips() {
        assert_eq!(
            from_stamp("spool:/tmp/omar/events/pane.jsonl"),
            Some(PathBuf::from("/tmp/omar/events/pane.jsonl"))
        );
        assert_eq!(from_stamp("spool:"), None);
    }

    #[test]
    fn taking_the_spool_twice_at_once_hands_each_event_over_once() {
        // cursor registers two hook events; if both fire together a
        // read-then-truncate would deliver the same events twice.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pane.jsonl");
        for event in ["one", "two", "three"] {
            append(&path, event).unwrap();
        }

        let racers: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || drain_spool(&path))
            })
            .collect();

        let mut seen: Vec<String> = racers
            .into_iter()
            .flat_map(|racer| racer.join().unwrap())
            .collect();
        seen.sort();
        assert_eq!(seen, vec!["one", "three", "two"]);
    }
}
