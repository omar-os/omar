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
fn slack_escaping_and_unicode_truncation_preserve_run_reference() {
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
    assert!(text.starts_with(&"🌍".repeat(3300)));
    assert!(text.ends_with("run id.]") && text.chars().count() < 3500);
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
                outputs: Default::default()
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
