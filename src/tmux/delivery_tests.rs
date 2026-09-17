//! Every agent delivery path must leave terminal input and history untouched.
use super::{DeliveryOptions, TmuxClient};
use std::time::Duration;

struct Pane(tempfile::TempDir);
impl Pane {
    fn new(backend: Option<&str>, stamp: Option<&str>) -> Self {
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
                "backend": backend, "channel": stamp, "input": "my unfinished draft",
                "forbid_input": true
            })
            .to_string(),
        )
        .unwrap();
        super::client::TEST_TMUX.with(|path| *path.borrow_mut() = Some(command));
        Self(dir)
    }
    fn assert_untouched(&self) {
        let state: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(self.0.path().join("state.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(state["input"], "my unfinished draft");
        assert!(!state["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|command| matches!(
                command[0].as_str(),
                Some("send-keys" | "load-buffer" | "paste-buffer" | "capture-pane")
            )));
    }
}
impl Drop for Pane {
    fn drop(&mut self) {
        super::client::TEST_TMUX.with(|path| *path.borrow_mut() = None);
    }
}
fn options() -> DeliveryOptions {
    DeliveryOptions {
        startup_timeout: Duration::ZERO,
        poll_interval: Duration::ZERO,
    }
}
#[test]
fn agent_delivery_uses_channel_without_reading_or_editing_composer() {
    let dir = tempfile::tempdir().unwrap();
    let spool = dir.path().join("events");
    let pane = Pane::new(Some("cursor"), Some(&format!("spool:{}", spool.display())));
    TmuxClient::new("")
        .deliver_prompt("pane", "agent follow-up", &options())
        .unwrap();
    assert_eq!(crate::channel::drain_spool(&spool), ["agent follow-up"]);
    pane.assert_untouched();
}
#[test]
fn missing_channels_never_fall_back_to_terminal_input() {
    for backend in [
        None,
        Some("codex"),
        Some("cursor"),
        Some("unknown"),
        Some("raw"),
    ] {
        let pane = Pane::new(backend, None);
        assert!(TmuxClient::new("")
            .deliver_prompt("pane", "event", &options())
            .is_err());
        pane.assert_untouched();
    }
}
#[test]
fn failed_channel_never_falls_back_to_terminal_input() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("app.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        drop(listener.accept().unwrap());
    });
    let pane = Pane::new(Some("codex"), Some(&format!("codex:{}", socket.display())));
    assert!(TmuxClient::new("")
        .deliver_prompt("pane", "event", &options())
        .is_err());
    server.join().unwrap();
    pane.assert_untouched();
}
#[test]
fn scheduler_defers_missing_channel_without_editing_input() {
    let pane = Pane::new(Some("codex"), None);
    assert!(!crate::scheduler::deliver_to_tmux(
        0,
        "worker",
        "event",
        "omar-",
        &crate::scheduler::TickerBuffer::new()
    ));
    pane.assert_untouched();
}
#[test]
fn scheduler_delivers_through_channel_without_editing_input() {
    let dir = tempfile::tempdir().unwrap();
    let spool = dir.path().join("events");
    let pane = Pane::new(Some("cursor"), Some(&format!("spool:{}", spool.display())));
    assert!(crate::scheduler::deliver_to_tmux(
        0,
        "worker",
        "event",
        "omar-",
        &crate::scheduler::TickerBuffer::new()
    ));
    assert_eq!(crate::channel::drain_spool(&spool), ["event"]);
    pane.assert_untouched();
}
