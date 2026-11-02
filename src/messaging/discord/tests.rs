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

#[test]
fn shared_catalog_reuses_sources_relative_to_catalog_not_adapter() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("catalog")).unwrap();
    std::fs::create_dir(temp.path().join("discord")).unwrap();
    std::fs::write(temp.path().join("catalog/demo.omar"), "canonical workflow").unwrap();
    std::fs::write(
        temp.path().join("catalog/workflows.json"),
        json!({"workflows":{"demo":{"programPath":"demo.omar","outputPort":"out"}}}).to_string(),
    )
    .unwrap();
    let path = temp.path().join("discord/config.json");
    let mut config = json!({"platform":"discord","accountId":"999","allowedUsers":["123"],"runtimeUrl":"http://127.0.0.1:45073/chats/test","workflowCatalog":"../catalog/workflows.json"});
    std::fs::write(&path, config.to_string()).unwrap();
    let loaded = Config::load(&path).unwrap();
    assert_eq!(
        loaded.select("demo").unwrap().request["program"],
        "canonical workflow"
    );
    assert!(loaded.allowed_conversations.is_empty());
    config["workflows"] = json!({"copy":{"programPath":"demo.omar","outputPort":"out"}});
    std::fs::write(&path, config.to_string()).unwrap();
    assert!(Config::load(&path).is_err());
    config.as_object_mut().unwrap().remove("workflows");
    config["allowedConversations"] = json!(["123"]);
    std::fs::write(&path, config.to_string()).unwrap();
    assert!(Config::load(&path).is_err());
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
            .contains("Available workflows"));
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
