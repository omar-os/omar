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
    let pane = Pane::new(Some("stub"), Some(&format!("spool:{}", spool.display())));
    TmuxClient::new("")
        .deliver_prompt("pane", "agent follow-up", &options())
        .unwrap();
    assert_eq!(
        crate::backend::spool::drain_spool(&spool),
        ["agent follow-up"]
    );
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
    let pane = Pane::new(Some("stub"), Some(&format!("spool:{}", spool.display())));
    assert!(crate::scheduler::deliver_to_tmux(
        0,
        "worker",
        "event",
        "omar-",
        &crate::scheduler::TickerBuffer::new()
    ));
    assert_eq!(crate::backend::spool::drain_spool(&spool), ["event"]);
    pane.assert_untouched();
}

// Exercise startup through the public delivery path, including discovery and
// the retry boundary. No terminal command is permitted by the Pane fixture.
fn codex_startup_case(mode: &'static str) -> (bool, usize, usize) {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("app.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let stopped = done.clone();
    let server = std::thread::spawn(move || {
        let mut connections = 0;
        let mut sends = 0;
        while !stopped.load(Ordering::SeqCst) {
            let (stream, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            connections += 1;
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            while let Ok(message) = ws.read() {
                let tungstenite::Message::Text(body) = message else {
                    continue;
                };
                let request: serde_json::Value = serde_json::from_str(&body).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "initialize" => serde_json::json!({}),
                    "initialized" => continue,
                    "thread/loaded/list" => {
                        let threads = if mode == "empty" || (mode == "ready" && connections < 3) {
                            vec![]
                        } else if mode == "ambiguous" {
                            vec!["one", "two"]
                        } else {
                            vec!["one"]
                        };
                        serde_json::json!({"data": threads, "nextCursor": null})
                    }
                    "turn/start" => {
                        sends += 1;
                        assert_eq!(request["params"]["toolOutput"]["output"], "startup event");
                        if mode == "disconnect" {
                            break;
                        }
                        if mode == "rejected" {
                            ws.send(tungstenite::Message::Text(serde_json::json!({
                                "id": request["id"], "error": {"code": -1, "message": "rejected"}
                            }).to_string())).unwrap();
                            break;
                        }
                        serde_json::json!({"turn": {"id": "turn1"}})
                    }
                    method => panic!("unexpected method {method}"),
                };
                ws.send(tungstenite::Message::Text(
                    serde_json::json!({
                        "id": request["id"], "result": result
                    })
                    .to_string(),
                ))
                .unwrap();
            }
        }
        (connections, sends)
    });
    let pane = Pane::new(Some("codex"), Some(&format!("codex:{}", socket.display())));
    let result = TmuxClient::new("").deliver_prompt(
        "pane",
        "startup event",
        &DeliveryOptions {
            startup_timeout: if mode == "empty" {
                Duration::from_millis(300)
            } else {
                Duration::from_secs(3)
            },
            poll_interval: Duration::from_millis(10),
        },
    );
    done.store(true, Ordering::SeqCst);
    let (connections, sends) = server.join().unwrap();
    pane.assert_untouched();
    (result.is_ok(), connections, sends)
}

#[test]
fn codex_waits_for_a_thread_then_sends_exactly_once() {
    assert_eq!(codex_startup_case("ready"), (true, 3, 1));
}

#[test]
fn codex_startup_timeout_never_submits_an_event() {
    let (ok, connections, sends) = codex_startup_case("empty");
    assert!(!ok);
    assert!(connections >= 2);
    assert_eq!(sends, 0);
}

#[test]
fn codex_does_not_retry_ambiguous_threads_or_attempted_sends() {
    assert_eq!(codex_startup_case("ambiguous"), (false, 1, 0));
    assert_eq!(codex_startup_case("rejected"), (false, 1, 1));
    assert_eq!(codex_startup_case("disconnect"), (false, 1, 1));
}

#[test]
fn passive_legacy_hooks_do_not_claim_to_wake_an_idle_agent() {
    for backend in ["cursor", "agy"] {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("events");
        let pane = Pane::new(Some(backend), Some(&format!("spool:{}", spool.display())));
        let error = TmuxClient::new("")
            .deliver_prompt("pane", "wake", &options())
            .unwrap_err();
        assert!(error.to_string().contains("relaunch"));
        assert!(crate::backend::spool::drain_spool(&spool).is_empty());
        pane.assert_untouched();
    }
}
