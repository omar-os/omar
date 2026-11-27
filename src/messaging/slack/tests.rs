use super::*;
use crate::messaging::tests::{fixture, FakeMessenger, FakeRuntime};
use std::sync::atomic::Ordering;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
};

fn payload() -> Value {
    json!({"type":"event_callback","team_id":"account","event_id":"event","event":{"type":"app_mention","user":"user","channel":"room","ts":"100","text":"<@BOT> run demo"}})
}
#[test]
fn mention_requires_actual_leading_bot_and_ignores_bot_events() {
    assert_eq!(
        normalize(&payload(), "BOT")
            .unwrap()
            .destination
            .thread_id
            .as_deref(),
        Some("100")
    );
    for text in [
        "hello <@BOT> run demo",
        "<@OTHER> run demo",
        "<@BOT>",
        "<@BOT>run demo",
    ] {
        let mut p = payload();
        p["event"]["text"] = json!(text);
        assert!(normalize(&p, "BOT").is_none());
    }
    for (field, value) in [
        ("bot_id", json!("B")),
        ("subtype", json!("edited")),
        ("user", json!("BOT")),
        ("ts", json!(0)),
        ("thread_ts", json!({})),
    ] {
        let mut p = payload();
        p["event"][field] = value;
        assert!(normalize(&p, "BOT").is_none());
    }
    let mut p = payload();
    p["event"]["thread_ts"] = json!("parent");
    assert_eq!(
        normalize(&p, "BOT")
            .unwrap()
            .destination
            .thread_id
            .as_deref(),
        Some("parent")
    );
}
#[test]
fn malformed_envelope_fields_are_rejected() {
    for field in ["type", "team_id", "event_id", "event"] {
        let mut p = payload();
        p[field] = Value::Null;
        assert!(normalize(&p, "BOT").is_none());
    }
    let mut p = payload();
    p["event_id"] = json!("");
    assert!(normalize(&p, "BOT").is_none());
}
#[test]
fn slack_escaping_preserves_full_unicode_output() {
    assert_eq!(
        format_reply(&Reply {
            text: "<@U>&".into(),
            run_id: None
        }),
        "&lt;@U&gt;&amp;"
    );
    let text = format_reply(&Reply {
        text: "🌍".repeat(4000),
        run_id: Some("id".into()),
    });
    assert_eq!(text, "🌍".repeat(4000));
}

pub(crate) async fn http_fixture(
    status: &'static str,
    response: String,
) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut data = Vec::new();
        let mut buf = [0; 4096];
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
            if let Some(pos) = data.windows(4).position(|s| s == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&data[..pos]);
                let len = header
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|n| n.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if data.len() >= pos + 4 + len {
                    break;
                }
            }
        }
        let wire=format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len());
        socket.write_all(wire.as_bytes()).await.unwrap();
        String::from_utf8(data).unwrap()
    });
    (format!("http://{addr}/"), task)
}
#[tokio::test]
async fn slack_api_uses_thread_and_disables_mentions_and_unfurls() {
    let (base, server) = http_fixture("200 OK", json!({"ok":true}).to_string()).await;
    let mut api = SlackApi::new("test-bot".into(), "test-app".into()).unwrap();
    api.base = base;
    let d = Destination {
        platform: "slack".into(),
        account_id: "account".into(),
        conversation_id: "room".into(),
        thread_id: Some("parent".into()),
    };
    api.send(
        &d,
        Reply {
            text: "<@U>".into(),
            run_id: None,
        },
    )
    .await
    .unwrap();
    let request = server.await.unwrap();
    assert!(request
        .to_lowercase()
        .contains("authorization: bearer test-bot"));
    let body: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["thread_ts"], "parent");
    assert_eq!(body["text"], "&lt;@U&gt;");
    assert_eq!(body["mrkdwn"], false);
    assert_eq!(body["unfurl_links"], false);
}
#[tokio::test]
async fn slack_errors_never_expose_provider_body_or_tokens() {
    let (base, server) = http_fixture(
        "429 Too Many Requests",
        json!({"ok":false,"error":"secret-response"}).to_string(),
    )
    .await;
    let mut api = SlackApi::new("secret-token".into(), "app-token".into()).unwrap();
    api.base = base;
    let error = api
        .call("apps.connections.open", json!({}), true)
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "Slack API request failed");
    assert!(server
        .await
        .unwrap()
        .to_lowercase()
        .contains("authorization: bearer app-token"));
}
#[tokio::test]
async fn runtime_keeps_start_body_and_reports_admitted_error_id() {
    let (base, server) = http_fixture(
        "503 Service Unavailable",
        json!({"run_id":"already-admitted"}).to_string(),
    )
    .await;
    let runtime = OmarRuntime::new(base.trim_end_matches('/').into()).unwrap();
    let request = json!({"program":"fixed","raw_inputs":["in=hello"]});
    let error = crate::messaging::runtime::Runtime::start(&runtime, &request)
        .await
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<crate::messaging::runtime::RuntimeError>()
            .unwrap()
            .run_id
            .as_deref(),
        Some("already-admitted")
    );
    let wire = server.await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(wire.split_once("\r\n\r\n").unwrap().1).unwrap(),
        request
    );
}
#[tokio::test]
async fn runtime_and_slack_do_not_follow_redirects() {
    // With redirects disabled, neither client makes a second admission/API call.
    let (base, server) = http_fixture("302 Found", json!({"ok":true}).to_string()).await;
    let runtime = OmarRuntime::new(base.trim_end_matches('/').into()).unwrap();
    assert!(runtime.call("/v1/runs", Some(&json!({}))).await.is_err());
    server.await.unwrap();
    let (base, server) = http_fixture("302 Found", json!({"ok":true}).to_string()).await;
    let mut api = SlackApi::new("bot".into(), "app".into()).unwrap();
    api.base = base;
    assert!(api.call("auth.test", json!({}), false).await.is_err());
    server.await.unwrap();
}
#[tokio::test]
async fn runtime_unknown_status_and_missing_run_id_are_errors() {
    let (base, server) = http_fixture(
        "200 OK",
        json!({"run_id":"","status":"running"}).to_string(),
    )
    .await;
    let runtime = OmarRuntime::new(base.trim_end_matches('/').into()).unwrap();
    use crate::messaging::runtime::Runtime;
    assert!(runtime.start(&json!({})).await.is_err());
    server.await.unwrap();
    assert!(runtime
        .wait(
            crate::messaging::runtime::Run {
                run_id: "id".into(),
                status: "mystery".into(),
                outputs: Default::default(),
                conversation_id: None,
            },
            1
        )
        .await
        .is_err());
}

#[tokio::test]
async fn socket_acks_while_work_pending_handles_ping_and_retains_dedup_on_reconnect() {
    let notify = Arc::new(Notify::new());
    let mut config = fixture();
    config.platform = "slack".into();
    let gateway = Arc::new(Gateway::new(
        config,
        FakeRuntime {
            gate: Some(notify.clone()),
            ..Default::default()
        },
        FakeMessenger::default(),
    ));
    let mut tasks = JoinSet::new();
    let (_tx, mut stop) = watch::channel(false);
    for attempt in 0..2 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let envelope =
                json!({"type":"events_api","envelope_id":"envelope","payload":payload()});
            ws.send(Message::Text(envelope.to_string())).await.unwrap();
            let ack = timeout(Duration::from_secs(2), ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(ack.to_text().unwrap()).unwrap()["envelope_id"],
                "envelope"
            );
            ws.send(Message::Ping(vec![1, 2])).await.unwrap();
            let pong = timeout(Duration::from_secs(2), ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(pong, Message::Pong(vec![1, 2]));
            ws.close(None).await.unwrap();
        });
        let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        socket_loop(socket, "BOT", gateway.clone(), &mut tasks, &mut stop)
            .await
            .unwrap();
        server.await.unwrap();
        while gateway.runtime.starts.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            gateway.runtime.starts.load(Ordering::SeqCst),
            1,
            "reconnect {attempt}"
        );
    }
    notify.notify_one();
    while tasks.join_next().await.is_some() {}
    assert_eq!(gateway.messenger.replies.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn socket_shutdown_is_prompt() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        while let Some(Ok(frame)) = ws.next().await {
            if frame.is_close() {
                break;
            }
        }
    });
    let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .unwrap();
    let (tx, mut rx) = watch::channel(false);
    let mut tasks = JoinSet::new();
    let gateway = Arc::new(Gateway::new(
        fixture(),
        FakeRuntime::default(),
        FakeMessenger::default(),
    ));
    tx.send(true).unwrap();
    timeout(
        Duration::from_secs(3),
        socket_loop(socket, "BOT", gateway, &mut tasks, &mut rx),
    )
    .await
    .unwrap()
    .unwrap();
    server.await.unwrap();
}

#[test]
fn slack_dm_messages_are_plain_assistant_input_and_ignore_edits_and_bots() {
    let mut p = payload();
    p["event"] = json!({"type":"message","channel_type":"im","channel":"D123","user":"user","ts":"100.1","text":"help"});
    let m = normalize(&p, "BOT").unwrap();
    assert!(m.plain_text);
    assert_eq!(m.command, "help");
    assert!(m.destination.thread_id.is_none());
    for (field, value) in [
        ("subtype", json!("message_changed")),
        ("bot_id", json!("B123")),
        ("channel_type", json!("mpim")),
    ] {
        let mut bad = p.clone();
        bad["event"][field] = value;
        assert!(normalize(&bad, "BOT").is_none());
    }
    let mut p = payload();
    p["event"]["text"] = json!("<@BOT> help me design a workflow");
    assert!(normalize(&p, "BOT").unwrap().plain_text);
    p["event"]["text"] = json!("<@BOT> list");
    assert!(!normalize(&p, "BOT").unwrap().plain_text);
}
#[test]
fn slash_commands_map_to_shared_controls_without_parsing_plain_dm_words() {
    for (text, expected) in [
        ("list", "list"),
        ("run web-Demo-1", "run web-Demo-1"),
        ("new-chat", "new chat"),
        ("help", "help"),
        ("", "help"),
    ] {
        let p = json!({"command":"/omar","text":text,"team_id":"account","user_id":"user","channel_id":"D123"});
        let m = normalize_interaction(&p, "envelope").unwrap();
        assert_eq!(m.command, expected);
        assert!(!m.plain_text);
        assert_eq!(m.event_id, "slash:envelope");
    }
    let p = json!({"command":"/other","text":"list","team_id":"account","user_id":"user","channel_id":"D123"});
    assert!(normalize_interaction(&p, "envelope").is_none());
}
#[test]
fn slack_review_buttons_roundtrip_and_keep_the_parent_thread() {
    let destination = Destination {
        platform: "slack".into(),
        account_id: "account".into(),
        conversation_id: "room".into(),
        thread_id: Some("100.1".into()),
    };
    let token = uuid::Uuid::new_v4().to_string();
    let body = review_buttons(&destination, &token);
    for (button, verb) in body["blocks"][1]["elements"]
        .as_array()
        .unwrap()
        .iter()
        .zip(["approve", "revise", "cancel"])
    {
        let payload = json!({"type":"block_actions","team":{"id":"account"},"user":{"id":"user"},"channel":{"id":"room"},"message":{"thread_ts":"100.1"},"actions":[button]});
        let m = normalize_interaction(&payload, "envelope").unwrap();
        assert_eq!(m.command, format!("{verb} {token}"));
        assert_eq!(m.destination.thread_id.as_deref(), Some("100.1"));
        let mut bad = payload.clone();
        bad["actions"][0]["value"] = json!("not-a-token");
        assert!(normalize_interaction(&bad, "envelope").is_none());
        bad = payload;
        bad["message"]["thread_ts"] = Value::Null;
        assert!(normalize_interaction(&bad, "envelope").is_none());
    }
}
#[test]
fn dm_resolution_and_upload_targets_reject_wrong_identity_and_external_hosts() {
    assert_eq!(
        verified_dm(
            &json!({"channel":{"id":"D123","is_im":true,"user":"U1"}}),
            "U1"
        ),
        Some("D123")
    );
    for value in [
        json!({"channel":{"id":"G123","is_im":false,"user":"U1"}}),
        json!({"channel":{"id":"D123","is_im":true,"user":"U2"}}),
    ] {
        assert!(verified_dm(&value, "U1").is_none());
    }
    assert!(upload_url("https://files.slack.com/upload/v1/ABC").is_ok());
    for url in [
        "http://files.slack.com/upload",
        "https://files.slack.com.attacker.test/upload",
        "https://secret@files.slack.com/upload",
        "https://127.0.0.1/upload",
    ] {
        assert!(upload_url(url).is_err());
    }
}

async fn responses(
    replies: Vec<(&'static str, &'static str, Value)>,
) -> (SlackApi, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut api = SlackApi::new("test-bot".into(), "test-app".into()).unwrap();
    api.base = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut wires = vec![];
        for (status, extra, body) in replies {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut raw = vec![];
            loop {
                let mut byte = [0];
                stream.read_exact(&mut byte).await.unwrap();
                raw.push(byte[0]);
                if raw.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let headers = String::from_utf8(raw.clone()).unwrap();
            let len = headers
                .lines()
                .find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            let mut data = vec![0; len];
            stream.read_exact(&mut data).await.unwrap();
            raw.extend(data);
            wires.push(String::from_utf8(raw).unwrap());
            let body = body.to_string();
            stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{}",body.len(),body).as_bytes()).await.unwrap();
        }
        wires
    });
    (api, task)
}
#[tokio::test]
async fn long_replies_split_without_losing_text_or_breaking_escaped_entities() {
    let text = "<@U>&🌍".repeat(1000);
    let count = super::super::text_parts(&text, 3500, |c| match c {
        '&' => 5,
        '<' | '>' => 4,
        _ => 1,
    })
    .len();
    let (api, http) = responses(
        (0..count)
            .map(|_| ("200 OK", "", json!({"ok":true})))
            .collect(),
    )
    .await;
    let destination = normalize(&payload(), "BOT").unwrap().destination;
    api.send(
        &destination,
        Reply {
            text: text.clone(),
            run_id: None,
        },
    )
    .await
    .unwrap();
    let bodies = http
        .await
        .unwrap()
        .iter()
        .map(|w| serde_json::from_str::<Value>(w.split_once("\r\n\r\n").unwrap().1).unwrap())
        .collect::<Vec<_>>();
    let joined = bodies
        .iter()
        .map(|b| b["text"].as_str().unwrap())
        .collect::<String>();
    assert_eq!(joined, format_reply(&Reply { text, run_id: None }));
    assert!(bodies
        .iter()
        .all(|b| b["text"].as_str().unwrap().chars().count() <= 3500 && b["thread_ts"] == "100"));
}
#[tokio::test]
async fn slack_retries_only_explicit_rate_limits_and_preserves_app_token_choice() {
    let (api, http) = responses(vec![
        (
            "429 Too Many Requests",
            "Retry-After: 0\r\n",
            json!({"secret":"never log"}),
        ),
        ("200 OK", "", json!({"ok":true})),
    ])
    .await;
    api.call("apps.connections.open", json!({}), true)
        .await
        .unwrap();
    let wires = http.await.unwrap();
    assert_eq!(wires.len(), 2);
    assert!(wires
        .iter()
        .all(|w| w.to_lowercase().contains("authorization: bearer test-app")));
    let (base, http) = http_fixture(
        "500 Internal Server Error",
        json!({"ok":false,"error":"private"}).to_string(),
    )
    .await;
    let mut api = SlackApi::new("bot".into(), "app".into()).unwrap();
    api.base = base;
    assert_eq!(
        api.call("chat.postMessage", json!({}), false)
            .await
            .unwrap_err()
            .to_string(),
        "Slack API request failed"
    );
    http.await.unwrap();
}
#[tokio::test]
async fn upload_rejects_off_platform_urls_before_sending_graph_or_bot_credentials() {
    let (api, http) = responses(vec![(
        "200 OK",
        "",
        json!({"ok":true,"upload_url":"https://attacker.test/upload","file_id":"F123"}),
    )])
    .await;
    let d = normalize(&payload(), "BOT").unwrap().destination;
    assert_eq!(
        api.upload(&d, "workflow.png", vec![1, 2, 3])
            .await
            .unwrap_err()
            .to_string(),
        "Invalid Slack upload host"
    );
    let wire = http.await.unwrap().pop().unwrap();
    assert!(wire.starts_with("POST /files.getUploadURLExternal "));
    assert!(wire.contains("filename=workflow.png"));
    assert!(wire.contains("length=3"));
}

#[tokio::test]
async fn socket_acknowledges_commands_buttons_and_dms_before_shared_dispatch() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let token = uuid::Uuid::new_v4().to_string();
    let action = json!({"type":"block_actions","team":{"id":"account"},"user":{"id":"user"},"channel":{"id":"room"},"message":{"thread_ts":"100"},
        "actions":[{"type":"button","action_id":"omar:approve","value":token}]});
    let mut dm = payload();
    dm["event_id"] = json!("dm-event");
    dm["event"] = json!({"type":"message","channel_type":"im","channel":"D123","user":"user","ts":"200","text":"Hello there"});
    let peer = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        for (id, kind, payload) in [
            (
                "slash",
                "slash_commands",
                json!({"command":"/omar","text":"list","team_id":"account","user_id":"user","channel_id":"room"}),
            ),
            ("button", "interactive", action),
            ("dm", "events_api", dm),
        ] {
            ws.send(Message::Text(
                json!({"type":kind,"envelope_id":id,"payload":payload}).to_string(),
            ))
            .await
            .unwrap();
            let ack = timeout(Duration::from_secs(2), ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(ack.to_text().unwrap()).unwrap(),
                json!({"envelope_id":id})
            );
        }
        ws.close(None).await.unwrap();
    });
    let mut c = fixture();
    c.platform = "slack".into();
    c.allowed_conversations.push("D123".into());
    let gateway = Arc::new(Gateway::new(
        c,
        FakeRuntime::default(),
        FakeMessenger::default(),
    ));
    let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}"))
        .await
        .unwrap();
    let mut tasks = JoinSet::new();
    let (_tx, mut stop) = watch::channel(false);
    socket_loop(socket, "BOT", gateway.clone(), &mut tasks, &mut stop)
        .await
        .unwrap();
    peer.await.unwrap();
    while tasks.join_next().await.is_some() {}
    let replies = gateway.messenger.replies.lock().unwrap();
    assert_eq!(replies.len(), 3);
    assert!(replies
        .iter()
        .any(|(_, r)| r.text.contains("Saved workflows")));
    assert!(replies
        .iter()
        .any(|(_, r)| r.text.contains("expired, changed")));
    assert!(replies
        .iter()
        .any(|(d, r)| d.conversation_id == "D123" && r.text.contains("Hello there")));
    assert_eq!(gateway.runtime.starts.load(Ordering::SeqCst), 0);
}
