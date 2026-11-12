use super::*;
use crate::messaging::tests::{fixture, FakeMessenger, FakeRuntime};
use std::sync::atomic::Ordering;

fn channels() -> HashMap<String, String> {
    HashMap::from([("123".into(), "456".into())])
}
fn event(command: &str) -> Value {
    json!({"id":"789","type":0,"channel_id":"456","author":{"id":"123"},"content":command})
}
fn config() -> Config {
    let mut config = fixture();
    config.platform = "discord".into();
    config.account_id = "999".into();
    config.allowed_users = vec!["123".into()];
    config.allowed_conversations = vec!["456".into()];
    config
}

#[test]
fn only_verified_one_to_one_dm_messages_are_normalized() {
    let message = normalize(&event("  list \n"), "999", &channels()).unwrap();
    assert_eq!(message.command, "list");
    assert_eq!(message.destination.thread_id.as_deref(), Some("789"));
    for (field, value) in [
        ("guild_id", json!("111")),
        ("guild_id", Value::Null),
        ("type", json!(3)),
        ("channel_id", json!("000")),
        ("id", json!("../x")),
        ("content", Value::Null),
        ("webhook_id", json!("111")),
    ] {
        let mut e = event("run demo");
        e[field] = value;
        assert!(normalize(&e, "999", &channels()).is_none());
    }
    for author in [
        json!({"id":"999"}),
        json!({"id":"123","bot":true}),
        json!({"id":"000"}),
    ] {
        let mut e = event("run demo");
        e["author"] = author;
        assert!(normalize(&e, "999", &channels()).is_none());
    }
}

#[tokio::test]
async fn discord_uses_existing_listing_admission_dedup_and_delivery_policy() {
    let gateway = Gateway::new(config(), FakeRuntime::default(), FakeMessenger::default());
    gateway
        .handle(normalize(&event("list"), "999", &channels()).unwrap())
        .await;
    assert!(gateway.messenger.replies.lock().unwrap()[0]
        .1
        .text
        .contains("demo"));
    assert_eq!(gateway.runtime.starts.load(Ordering::SeqCst), 0);
    let mut e = event("run demo");
    e["id"] = json!("790");
    gateway
        .handle(normalize(&e, "999", &channels()).unwrap())
        .await;
    gateway
        .handle(normalize(&e, "999", &channels()).unwrap())
        .await;
    assert_eq!(gateway.runtime.starts.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.messenger.replies.lock().unwrap().len(), 3);
    e["id"] = json!("791");
    e["content"] = json!("run ../../secret");
    gateway
        .handle(normalize(&e, "999", &channels()).unwrap())
        .await;
    assert_eq!(gateway.runtime.starts.load(Ordering::SeqCst), 1);
    let mut unauthorized = normalize(&e, "999", &channels()).unwrap();
    unauthorized.sender_id = "111".into();
    gateway.handle(unauthorized).await;
    assert_eq!(gateway.messenger.replies.lock().unwrap().len(), 4);
    let gateway = Gateway::new(
        config(),
        FakeRuntime::default(),
        FakeMessenger {
            fail: true,
            ..Default::default()
        },
    );
    gateway
        .handle(normalize(&event("run demo"), "999", &channels()).unwrap())
        .await;
    assert_eq!(gateway.runtime.starts.load(Ordering::SeqCst), 1);
}

#[test]
fn long_unicode_reports_split_losslessly_within_discord_limits() {
    for text in ["🌍".repeat(2200), "a".repeat(4001), "hello".into()] {
        let parts = message_parts(&text);
        assert_eq!(parts.concat(), text);
        assert!(parts
            .iter()
            .all(|p| !p.is_empty() && p.encode_utf16().count() <= 2000));
    }
}

#[test]
fn gateway_resume_and_close_codes_follow_discord_protocol() {
    let mut session = Session::default();
    assert_eq!(session.identify("token")["d"]["intents"], DIRECT_MESSAGES);
    assert_eq!(session.identify("token")["op"], 2);
    session.sequence = Some(42);
    session.ready(&json!({"user":{"id":"999"},"session_id":"session","resume_gateway_url":"wss://gateway.discord.gg"}),"999").unwrap();
    assert_eq!(session.identify("token")["op"], 6);
    assert_eq!(session.identify("token")["d"]["seq"], 42);
    assert!(matches!(close_action(4004), Reconnect::Fatal));
    assert!(matches!(close_action(4014), Reconnect::Fatal));
    assert!(matches!(close_action(4007), Reconnect::Fresh));
    assert!(matches!(close_action(4009), Reconnect::Fresh));
    assert!(matches!(close_action(1006), Reconnect::Resume));
    for url in [
        "ws://gateway.discord.gg",
        "wss://discord.gg.attacker.com",
        "wss://secret@discord.gg",
        "wss://localhost",
    ] {
        assert!(gateway_url(url).is_err());
    }
}

async fn fixture_http(
    status: &'static str,
    body: Value,
) -> (DiscordApi, tokio::task::JoinHandle<String>) {
    // Reuse the existing Slack transport's provider-neutral HTTP fixture.
    let (base, server) =
        crate::messaging::slack::tests::http_fixture(status, body.to_string()).await;
    let mut api = DiscordApi::new("test-token".into()).unwrap();
    api.base = base.trim_end_matches('/').into();
    (api, server)
}

#[tokio::test]
async fn replies_use_bot_auth_original_message_and_disable_mentions() {
    let (api, server) = fixture_http("200 OK", json!({"id":"100"})).await;
    let d = normalize(&event("list"), "999", &channels())
        .unwrap()
        .destination;
    api.send(
        &d,
        Reply {
            text: "<@123> @everyone".into(),
            run_id: None,
        },
    )
    .await
    .unwrap();
    let wire = server.await.unwrap();
    assert!(wire
        .to_lowercase()
        .contains("authorization: bot test-token"));
    assert!(wire.starts_with("POST /channels/456/messages "));
    let body: Value = serde_json::from_str(wire.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["allowed_mentions"]["parse"], json!([]));
    assert_eq!(body["allowed_mentions"]["replied_user"], false);
    assert_eq!(body["message_reference"]["message_id"], "789");
    assert_eq!(body["flags"], 4);
}

#[tokio::test]
async fn provider_errors_and_redirects_do_not_leak_bodies_or_retry() {
    for status in ["401 Unauthorized", "500 Internal Server Error", "302 Found"] {
        let (api, server) = fixture_http(status, json!({"secret":"private-response"})).await;
        let error = api
            .call(reqwest::Method::GET, "/users/@me", None)
            .await
            .unwrap_err();
        assert!(!format!("{error:#}").contains("private-response"));
        assert!(!format!("{error:#}").contains("test-token"));
        server.await.unwrap();
    }
}

async fn sockets() -> (
    tokio_tungstenite::WebSocketStream<tokio::io::DuplexStream>,
    tokio_tungstenite::WebSocketStream<tokio::io::DuplexStream>,
) {
    let (client, server) = tokio::io::duplex(65536);
    (
        tokio_tungstenite::WebSocketStream::from_raw_socket(
            client,
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await,
        tokio_tungstenite::WebSocketStream::from_raw_socket(
            server,
            tokio_tungstenite::tungstenite::protocol::Role::Server,
            None,
        )
        .await,
    )
}
async fn send_event<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    value: Value,
) {
    socket.send(Message::Text(value.to_string())).await.unwrap();
}
async fn read_event<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
) -> Value {
    let frame = timeout(Duration::from_secs(3), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(frame.to_text().unwrap()).unwrap()
}

#[tokio::test]
async fn gateway_heartbeats_during_work_and_resumes_without_duplicate_execution() {
    let notify = Arc::new(tokio::sync::Notify::new());
    let gateway = Arc::new(Gateway::new(
        config(),
        FakeRuntime {
            gate: Some(notify.clone()),
            ..Default::default()
        },
        FakeMessenger::default(),
    ));
    let identity = Identity {
        token: "fixture-token".into(),
        bot: "999".into(),
        channels: channels(),
        callback_base: "https://discord.com/api/v10".into(),
    };
    let mut session = Session::default();
    let mut tasks = JoinSet::new();
    let (client, mut server) = sockets().await;
    let peer = async {
        send_event(
            &mut server,
            json!({"op":10,"d":{"heartbeat_interval":1000}}),
        )
        .await;
        let identify = read_event(&mut server).await;
        assert_eq!(identify["op"], 2);
        assert_eq!(identify["d"]["intents"], DIRECT_MESSAGES);
        send_event(&mut server,json!({"op":0,"s":1,"t":"READY","d":{"user":{"id":"999"},"session_id":"session","resume_gateway_url":"wss://gateway.discord.gg"}})).await;
        send_event(
            &mut server,
            json!({"op":0,"s":2,"t":"MESSAGE_CREATE","d":event("run demo")}),
        )
        .await;
        let heartbeat = read_event(&mut server).await;
        assert_eq!(heartbeat["op"], 1);
        send_event(&mut server, json!({"op":11})).await;
        send_event(&mut server, json!({"op":7})).await;
    };
    let (result, _) = tokio::join!(
        socket_loop(client, &identity, &mut session, gateway.clone(), &mut tasks),
        peer
    );
    assert!(matches!(result.unwrap(), Reconnect::Resume));
    assert_eq!(session.sequence, Some(2));
    assert_eq!(gateway.runtime.starts.load(Ordering::SeqCst), 1);
    let (client, mut server) = sockets().await;
    let peer = async {
        send_event(
            &mut server,
            json!({"op":10,"d":{"heartbeat_interval":1000}}),
        )
        .await;
        let resume = read_event(&mut server).await;
        assert_eq!(resume["op"], 6);
        assert_eq!(resume["d"]["seq"], 2);
        send_event(&mut server, json!({"op":0,"s":3,"t":"RESUMED","d":{}})).await;
        send_event(
            &mut server,
            json!({"op":0,"s":4,"t":"MESSAGE_CREATE","d":event("run demo")}),
        )
        .await;
        let mut list = event("list");
        list["id"] = json!("790");
        send_event(
            &mut server,
            json!({"op":0,"s":5,"t":"MESSAGE_CREATE","d":list}),
        )
        .await;
        timeout(Duration::from_secs(3), async {
            while gateway.messenger.replies.lock().unwrap().len() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(gateway
            .messenger
            .replies
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .1
            .text
            .contains("Saved workflows"));
        notify.notify_one();
        timeout(Duration::from_secs(3), async {
            while gateway.messenger.replies.lock().unwrap().len() < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        send_event(&mut server, json!({"op":9,"d":false})).await;
    };
    let (result, _) = tokio::join!(
        socket_loop(client, &identity, &mut session, gateway.clone(), &mut tasks),
        peer
    );
    assert!(matches!(result.unwrap(), Reconnect::Fresh));
    assert_eq!(gateway.runtime.starts.load(Ordering::SeqCst), 1);
    assert!(gateway
        .messenger
        .replies
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .1
        .text
        .contains("completed"));
    tasks.abort_all();
}

#[tokio::test]
async fn missing_heartbeat_ack_and_authentication_close_end_the_session() {
    let gateway = Arc::new(Gateway::new(
        config(),
        FakeRuntime::default(),
        FakeMessenger::default(),
    ));
    let identity = Identity {
        token: "fixture-token".into(),
        bot: "999".into(),
        channels: channels(),
        callback_base: "https://discord.com/api/v10".into(),
    };
    let mut session = Session::default();
    let mut tasks = JoinSet::new();
    let (client, mut server) = sockets().await;
    let peer = async {
        send_event(
            &mut server,
            json!({"op":10,"d":{"heartbeat_interval":1000}}),
        )
        .await;
        assert_eq!(read_event(&mut server).await["op"], 2);
        send_event(&mut server,json!({"op":0,"s":1,"t":"READY","d":{"user":{"id":"999"},"session_id":"session","resume_gateway_url":"wss://gateway.discord.gg"}})).await;
        assert_eq!(read_event(&mut server).await["op"], 1);
        tokio::time::sleep(Duration::from_secs(3)).await;
    };
    let (result, _) = tokio::join!(
        socket_loop(client, &identity, &mut session, gateway.clone(), &mut tasks),
        peer
    );
    assert!(result.unwrap_err().to_string().contains("not acknowledged"));
    let (client, mut server) = sockets().await;
    server
        .send(Message::Close(Some(
            tokio_tungstenite::tungstenite::protocol::CloseFrame {
                code: 4004.into(),
                reason: "".into(),
            },
        )))
        .await
        .unwrap();
    assert!(matches!(
        socket_loop(client, &identity, &mut session, gateway, &mut tasks)
            .await
            .unwrap(),
        Reconnect::Fatal
    ));
}

#[tokio::test]
async fn discord_rate_limit_waits_before_retry_and_never_leaks_the_body() {
    let (api, server) = fixture_http(
        "429 Too Many Requests",
        json!({"retry_after":0.02,"secret":"private-response"}),
    )
    .await;
    let started = Instant::now();
    let error = api
        .call(reqwest::Method::GET, "/users/@me", None)
        .await
        .unwrap_err();
    assert!(started.elapsed() >= Duration::from_millis(100));
    assert!(!format!("{error:#}").contains("private-response"));
    server.await.unwrap();
}

#[test]
fn buttons_only_normalize_verified_dm_actions_and_never_accept_client_programs() {
    let token = uuid::Uuid::new_v4().to_string();
    let e = json!({"id":"1001","application_id":"999","type":3,"channel_id":"456","user":{"id":"123"},
        "message":{"id":"1000","author":{"id":"999"}},"data":{"component_type":2,"custom_id":format!("omar:approve:{token}")}});
    let m = normalize_interaction(&e, "999", &channels()).unwrap();
    assert_eq!(m.command, format!("approve {token}"));
    for (field, value) in [
        ("guild_id", json!("1")),
        ("application_id", json!("111")),
        ("channel_id", json!("777")),
        ("type", json!(2)),
        ("user", json!({"id":"111"})),
    ] {
        let mut bad = e.clone();
        bad[field] = value;
        assert!(normalize_interaction(&bad, "999", &channels()).is_none());
    }
    let mut bad = e.clone();
    bad["data"]["custom_id"] = json!("omar:approve:../../program");
    assert!(normalize_interaction(&bad, "999", &channels()).is_none());
}
#[test]
fn review_upload_preserves_source_schema_and_json_without_header_injection() {
    let proposal = crate::messaging::tests::review_fixture();
    let summary = super::super::proposal::summary(&proposal).unwrap();
    assert!(summary.contains("Graph:"));
    assert!(summary.contains("No formal properties"));
    let files = vec![
        (
            "workflow.omar".into(),
            proposal.design.program.as_bytes().to_vec(),
        ),
        (
            "schema-0.json".into(),
            proposal
                .design
                .files
                .values()
                .next()
                .unwrap()
                .as_bytes()
                .to_vec(),
        ),
    ];
    let payload = json!({"attachments":[{"id":0,"filename":"workflow.omar"},{"id":1,"filename":"schema-0.json"}]});
    let (boundary, bytes) = multipart(&payload, &files);
    let body = String::from_utf8(bytes).unwrap();
    assert!(body.contains(&proposal.design.program));
    assert!(body.contains("files[1]"));
    assert!(body.ends_with(&format!("--{boundary}--\r\n")));
}

#[tokio::test]
async fn interaction_ack_is_deferred_before_work_and_never_uses_the_bot_token() {
    let (api, server) = fixture_http("200 OK", json!({})).await;
    acknowledge_interaction(&api.base, &json!({"id":"123","token":"temporary-token"}))
        .await
        .unwrap();
    let wire = server.await.unwrap();
    assert!(wire.starts_with("POST /interactions/123/temporary-token/callback "));
    assert!(!wire.to_lowercase().contains("authorization:"));
    let body: Value = serde_json::from_str(wire.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["type"], 6);
}

#[tokio::test]
async fn discord_review_sends_graph_and_source_before_enabling_version_bound_buttons() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut wires = Vec::new();
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut data = Vec::new();
            let mut buf = [0; 4096];
            loop {
                let n = stream.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n]);
                if let Some(pos) = data.windows(4).position(|p| p == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&data[..pos]);
                    let len = header
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if data.len() >= pos + 4 + len {
                        break;
                    }
                }
            }
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await.unwrap();
            wires.push(data);
        }
        wires
    });
    let mut api = DiscordApi::new("fixture-token".into()).unwrap();
    api.base = format!("http://{address}");
    let proposal = crate::messaging::tests::review_fixture();
    let token = uuid::Uuid::new_v4().to_string();
    let destination = normalize(&event("create"), "999", &channels())
        .unwrap()
        .destination;
    api.review(&destination, &proposal, &token).await.unwrap();
    let wires = server.await.unwrap();
    assert!(wires[0].windows(8).any(|w| w == b"\x89PNG\r\n\x1a\n"));
    assert!(String::from_utf8_lossy(&wires[0]).contains("image/png"));
    let wires = wires[1..]
        .iter()
        .map(|w| String::from_utf8(w.clone()).unwrap())
        .collect::<Vec<_>>();
    assert!(wires[0].contains("Graph:"));
    assert!(!wires[0].contains("custom_id"));
    assert!(wires[1].contains("multipart/form-data"));
    assert!(wires[1].contains(&proposal.design.program));
    assert!(wires[1].contains("schema-0.json"));
    let buttons: Value = serde_json::from_str(wires[2].split_once("\r\n\r\n").unwrap().1).unwrap();
    let components = buttons["components"][0]["components"].as_array().unwrap();
    assert_eq!(components.len(), 3);
    assert_eq!(components[0]["custom_id"], format!("omar:approve:{token}"));
    assert_eq!(buttons["allowed_mentions"]["parse"], json!([]));
}
