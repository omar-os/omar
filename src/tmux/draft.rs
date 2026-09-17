//! Preserve composer drafts across web, MCP and scheduler prompt delivery.
use super::TmuxClient;
use crate::scheduler::pane_input;
use anyhow::{Context, Result};
use std::io::Write;
use std::path::PathBuf;

/// Capture the user's in-progress draft from an agent pane.
///
/// The backend is read from the session stamp written at launch rather than
/// sniffed from the pane, and the capture is the visible pane only, so
/// scrollback cannot contribute stale prompt rows.
fn get_pane_input(client: &TmuxClient, target: &str) -> pane_input::PaneInput {
    let Some(backend) = client.session_backend(target) else {
        return pane_input::PaneInput::Unknown("backend not identified");
    };
    let Some(shape) = pane_input::Shape::for_backend(&backend) else {
        return pane_input::PaneInput::Unknown("backend has no known input shape");
    };
    let Ok(capture) = client.capture_pane_visible(target) else {
        return pane_input::PaneInput::Unknown("pane capture failed");
    };

    let caret = client
        .caret_position(target)
        .map(|(row, col)| pane_input::Caret { row, col });
    let width = client.pane_width(target);
    pane_input::extract(shape, &capture, caret, width)
}

/// Empty the input box, confirming it by re-reading the pane.
///
/// `C-u` kills one line at a time — a visual line on some backends, a logical
/// one on others — so the count needed varies with the draft. Rather than
/// guess, press and re-check until the box reads empty.
#[derive(Debug, PartialEq, Eq)]
enum Cleared {
    /// The box is empty.
    Empty,
    /// Nothing was sent, so whatever was there is untouched.
    Untouched,
    /// Keys were sent and the box still holds text — it is damaged now.
    Partial,
}

/// A key that empties the whole composer at once, for backends that have one.
///
/// Only opencode is listed, and only because its binding was checked against a
/// live pane: `ctrl+c` is registered as "clear input" while the composer holds
/// text and as "quit" while it does not, so it is safe exactly when we use it —
/// straight after a read that said there was a draft — and catastrophic
/// otherwise. Everything else empties the box a line at a time, which is slower
/// but has no way to kill the agent.
pub(super) fn whole_buffer_clear_key(backend: &str) -> Option<&'static str> {
    match backend {
        "opencode" => Some("C-c"),
        _ => None,
    }
}

fn clear_pane_input(client: &TmuxClient, target: &str) -> Cleared {
    const MAX_CLEAR_ROUNDS: usize = 40;
    let clear_key = client
        .session_backend(target)
        .and_then(|backend| whole_buffer_clear_key(&backend));
    let mut last: Option<String> = None;
    let mut removed = false;
    let mut keys_sent = false;
    let mut stalls = 0;

    for _ in 0..MAX_CLEAR_ROUNDS {
        let current = match get_pane_input(client, target) {
            pane_input::PaneInput::Empty => return Cleared::Empty,
            // Never keep hammering a pane we cannot read. Whether the draft is
            // still whole depends on how far we got, and the caller needs to
            // know: putting it back on top of an intact draft duplicates it.
            pane_input::PaneInput::Unknown(_) => {
                return if keys_sent {
                    Cleared::Partial
                } else {
                    Cleared::Untouched
                };
            }
            pane_input::PaneInput::Draft(draft) => draft,
        };

        // Stop when the keys stop achieving anything, rather than pressing
        // forty times and then reporting a box we damaged as merely stubborn.
        if last.as_deref() == Some(current.as_str()) {
            stalls += 1;
            if stalls >= 2 {
                return if removed {
                    Cleared::Partial
                } else {
                    Cleared::Untouched
                };
            }
        } else {
            if last.is_some() {
                removed = true;
            }
            stalls = 0;
        }
        last = Some(current);

        // The read above said there is a draft, which is the condition that
        // makes this key mean "clear" rather than "quit".
        if let Some(key) = clear_key {
            keys_sent |= client.send_keys(target, key).is_ok();
            std::thread::sleep(std::time::Duration::from_millis(60));
            continue;
        }

        // `C-u` kills back to the start of the line. At the start of the
        // buffer it does nothing, forever, which leaves a draft the user is
        // editing from the top permanently unclearable — so when a press
        // achieves nothing, add `C-k`, which kills forward and takes the
        // newline with it.
        //
        // `C-k` is held back until then on purpose: antigravity binds it to
        // approving a waiting subagent, and approving one on the user's behalf
        // to tidy an input box is not a trade worth making routinely.
        keys_sent |= client.send_keys(target, "C-u").is_ok();
        if stalls > 0 {
            keys_sent |= client.send_keys(target, "C-k").is_ok();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    match get_pane_input(client, target) {
        pane_input::PaneInput::Empty => Cleared::Empty,
        pane_input::PaneInput::Unknown(_) if keys_sent => Cleared::Partial,
        _ if removed => Cleared::Partial,
        _ => Cleared::Untouched,
    }
}

/// Own a recoverable copy before sending any clearing keys. If restoration
/// cannot be verified safely, keep that copy and report its path to the caller.
pub(super) struct SavedDraft<'a> {
    client: &'a TmuxClient,
    target: &'a str,
    saved: Option<(String, PathBuf)>,
}

impl<'a> SavedDraft<'a> {
    pub(super) fn capture(client: &'a TmuxClient, target: &'a str) -> Result<Self> {
        let mut guard = Self {
            client,
            target,
            saved: None,
        };
        // Raw shell/demo sessions and the line-oriented stub have no TUI
        // composer to protect. Unknown stamped backends still fail closed.
        if matches!(
            client.session_backend(target).as_deref(),
            None | Some("stub")
        ) {
            return Ok(guard);
        }
        let draft = match get_pane_input(client, target) {
            pane_input::PaneInput::Empty => return Ok(guard),
            pane_input::PaneInput::Draft(draft) => draft,
            pane_input::PaneInput::Unknown(reason) => {
                anyhow::bail!("cannot protect draft: {}", reason)
            }
        };
        let path =
            crate::paths::private_temp_dir()?.join(format!("draft-{}.txt", uuid::Uuid::new_v4()));
        let mut file = crate::paths::create_private_file(&path)?;
        file.write_all(draft.as_bytes())?;
        file.sync_all()?;
        match clear_pane_input(client, target) {
            Cleared::Empty => {
                guard.saved = Some((draft, path));
                Ok(guard)
            }
            Cleared::Untouched => {
                let _ = std::fs::remove_file(path);
                anyhow::bail!("cannot clear draft; input left untouched")
            }
            Cleared::Partial => {
                // Never append a full draft to a surviving fragment.
                anyhow::bail!(
                    "could not clear draft completely; original saved at {}",
                    path.display()
                )
            }
        }
    }

    pub(super) fn restore(&mut self) -> Result<()> {
        let Some((draft, path)) = self.saved.take() else {
            return Ok(());
        };
        let result = (|| {
            anyhow::ensure!(
                clear_pane_input(self.client, self.target) == Cleared::Empty,
                "input box could not be cleared safely"
            );
            self.client.paste_text(self.target, &draft)
        })();
        match result {
            Ok(()) => {
                let _ = std::fs::remove_file(path);
                Ok(())
            }
            Err(error) => Err(error).with_context(|| {
                format!("draft restore failed; original saved at {}", path.display())
            }),
        }
    }
}

impl Drop for SavedDraft<'_> {
    fn drop(&mut self) {
        // Covers unwinding as well as ordinary delivery errors.
        if let Err(error) = self.restore() {
            tracing::error!("{error:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Pane {
        dir: tempfile::TempDir,
    }

    impl Pane {
        fn new(mode: &str, draft: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let command = dir.path().join("tmux");
            std::fs::write(
                &command,
                include_str!("../../tests/fixtures/delivery/tmux.py"),
            )
            .unwrap();
            std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::write(
                dir.path().join("state.json"),
                serde_json::json!({
                    "mode": mode, "input": draft,
                })
                .to_string(),
            )
            .unwrap();
            super::super::client::TEST_TMUX.with(|path| *path.borrow_mut() = Some(command));
            Self { dir }
        }

        fn state(&self) -> serde_json::Value {
            serde_json::from_str(
                &std::fs::read_to_string(self.dir.path().join("state.json")).unwrap(),
            )
            .unwrap()
        }

        fn deliver(&self) -> Result<()> {
            let opts = super::super::DeliveryOptions {
                max_retries: 1,
                verify_timeout: std::time::Duration::from_millis(200),
                poll_interval: std::time::Duration::from_millis(1),
                ..Default::default()
            };
            TmuxClient::new("").deliver_prompt("pane", "scheduled event", &opts)
        }
    }

    impl Drop for Pane {
        fn drop(&mut self) {
            super::super::client::TEST_TMUX.with(|path| *path.borrow_mut() = None);
        }
    }

    #[test]
    fn line_oriented_stub_delivery_does_not_require_a_composer() {
        let pane = Pane::new("success", "");
        let mut state = pane.state();
        state["backend"] = serde_json::json!("stub");
        std::fs::write(pane.dir.path().join("state.json"), state.to_string()).unwrap();
        pane.deliver().unwrap();
        assert_eq!(pane.state()["submitted"], 1);
    }

    #[test]
    fn shared_delivery_restores_multiline_drafts_after_each_failure_stage() {
        // This is the entry point used by both web chat and MCP spawn delivery.
        for mode in ["paste_before", "paste_after", "enter"] {
            let original = "fix the parser\nthen add coverage";
            let pane = Pane::new(mode, original);
            let error = pane.deliver().expect_err(mode);
            assert!(format!("{error:#}").contains("simulated tmux failure"));
            assert_eq!(pane.state()["input"], original, "{mode}");
            assert_eq!(
                pane.state().get("submitted"),
                None,
                "draft must never be submitted"
            );
        }
    }

    #[test]
    fn shared_delivery_restores_draft_after_success_without_resubmission() {
        let pane = Pane::new("success", "my draft");
        pane.deliver().unwrap();
        assert_eq!(pane.state()["input"], "my draft");
        assert_eq!(pane.state()["submitted"], 1);
    }

    #[test]
    fn unreadable_composer_retains_the_private_original_instead_of_appending() {
        for mode in ["unreadable_clear", "unreadable_after"] {
            let pane = Pane::new(mode, "my draft");
            let error = format!("{:#}", pane.deliver().unwrap_err());
            let path = error
                .split("original saved at ")
                .nth(1)
                .unwrap()
                .split(':')
                .next()
                .unwrap();
            assert_eq!(std::fs::read_to_string(path).unwrap(), "my draft");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            assert_ne!(
                pane.state()["input"],
                "my draft",
                "must not blindly append into unreadable input"
            );
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn failed_restore_keeps_original_and_reports_recovery_path() {
        let pane = Pane::new("restore_fail", "my draft");
        let client = TmuxClient::new("");
        let mut draft = SavedDraft::capture(&client, "pane").unwrap();
        let path = draft.saved.as_ref().unwrap().1.clone();
        let error = format!("{:#}", draft.restore().unwrap_err());
        assert!(error.contains(path.to_str().unwrap()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "my draft");
        drop(draft);
        assert_eq!(
            pane.state()["commands"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|command| command[0] == "paste-buffer")
                .count(),
            1,
            "drop must not repeat an uncertain restore"
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn unreadable_draft_blocks_delivery_before_any_clearing_keys() {
        let pane = Pane::new("success", "my draft");
        let mut state = pane.state();
        state["unreadable"] = serde_json::json!(true);
        std::fs::write(pane.dir.path().join("state.json"), state.to_string()).unwrap();
        assert!(pane
            .deliver()
            .unwrap_err()
            .to_string()
            .contains("cannot protect draft"));
        assert_eq!(pane.state()["input"], "my draft");
        assert!(!pane.state()["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|command| command[0] == "send-keys" || command[0] == "paste-buffer"));
    }

    #[test]
    fn scheduler_side_channel_success_never_clears_a_draft() {
        let pane = Pane::new("success", "my scheduler draft");
        let spool = pane.dir.path().join("events.jsonl");
        let mut state = pane.state();
        state["channel"] = serde_json::json!(format!("spool:{}", spool.display()));
        std::fs::write(pane.dir.path().join("state.json"), state.to_string()).unwrap();
        assert!(crate::scheduler::deliver_to_tmux(
            0,
            "worker",
            "event",
            "omar-",
            &crate::scheduler::TickerBuffer::new(),
            true
        ));
        assert_eq!(pane.state()["input"], "my scheduler draft");
        assert!(!pane.state()["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|command| command[0] == "send-keys" || command[0] == "paste-buffer"));
        assert_eq!(crate::channel::drain_spool(&spool), vec!["event"]);
    }

    #[test]
    fn draft_guard_restores_on_unwind() {
        let pane = Pane::new("success", "my draft");
        let client = TmuxClient::new("");
        assert!(std::panic::catch_unwind(|| {
            let _draft = SavedDraft::capture(&client, "pane").unwrap();
            panic!("delivery worker panicked");
        })
        .is_err());
        assert_eq!(pane.state()["input"], "my draft");
    }

    #[test]
    fn scheduler_restores_draft_when_side_channel_and_fallback_both_fail() {
        let pane = Pane::new("paste_before", "my scheduler draft");
        let socket = pane.dir.path().join("codex.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            drop(listener.accept().unwrap());
        });
        let mut state = pane.state();
        state["channel"] = serde_json::json!(format!("codex:{}", socket.display()));
        std::fs::write(pane.dir.path().join("state.json"), state.to_string()).unwrap();
        assert!(!crate::scheduler::deliver_to_tmux(
            0,
            "worker",
            "event",
            "omar-",
            &crate::scheduler::TickerBuffer::new(),
            true
        ));
        server.join().unwrap();
        assert_eq!(pane.state()["input"], "my scheduler draft");
    }

    #[test]
    fn only_a_verified_clear_key_is_ever_sent() {
        // `ctrl+c` means "clear the input" while opencode's composer holds
        // text and "quit" while it does not, so it is only ever sent straight
        // after a read that found a draft. No other backend gets one until its
        // binding has been checked the same way against a live pane — a wrong
        // guess here kills the user's agent.
        assert_eq!(whole_buffer_clear_key("opencode"), Some("C-c"));
        for backend in ["claude", "codex", "cursor", "agy", "stub", ""] {
            assert_eq!(
                whole_buffer_clear_key(backend),
                None,
                "{backend} must fall back to clearing a line at a time"
            );
        }
    }
}
