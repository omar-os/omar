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
        .handle(normalize_interaction(&slash_event("list"), "999", &channels()).unwrap())
        .await;
    assert!(gateway.messenger.replies.lock().unwrap()[0]
        .1
        .text
        .contains("demo"));
    assert_eq!(gateway.runtime.starts.load(Ordering::SeqCst), 0);
    let mut e = run_slash_event();
    e["id"] = json!("790");
    gateway
        .handle(normalize_interaction(&e, "999", &channels()).unwrap())
        .await;
    gateway
        .handle(normalize_interaction(&e, "999", &channels()).unwrap())
        .await;
    assert_eq!(gateway.runtime.starts.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.messenger.replies.lock().unwrap().len(), 3);
    e["id"] = json!("791");
    e["data"]["options"][0]["value"] = json!("missing");
    gateway
        .handle(normalize_interaction(&e, "999", &channels()).unwrap())
        .await;
    assert_eq!(gateway.runtime.starts.load(Ordering::SeqCst), 1);
    let mut unauthorized = normalize_interaction(&e, "999", &channels()).unwrap();
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
        .handle(normalize_interaction(&run_slash_event(), "999", &channels()).unwrap())
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
    let (callback_base, callback_server) = interaction_fixture(3).await;
    let identity = Identity {
        server: None,
        token: "fixture-token".into(),
        bot: "999".into(),
        channels: channels(),
        callback_base,
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
            json!({"op":0,"s":2,"t":"INTERACTION_CREATE","d":run_slash_event()}),
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
            json!({"op":0,"s":4,"t":"INTERACTION_CREATE","d":run_slash_event()}),
        )
        .await;
        let mut list = slash_event("list");
        list["id"] = json!("790");
        send_event(
            &mut server,
            json!({"op":0,"s":5,"t":"INTERACTION_CREATE","d":list}),
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
    callback_server.await.unwrap();
}

#[tokio::test]
async fn missing_heartbeat_ack_and_authentication_close_end_the_session() {
    let gateway = Arc::new(Gateway::new(
        config(),
        FakeRuntime::default(),
        FakeMessenger::default(),
    ));
    let identity = Identity {
        server: None,
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

fn slash_event(name: &str) -> Value {
    json!({"id":"1002","token":"test-token","application_id":"999","type":2,"channel_id":"456","user":{"id":"123"},
        "data":{"type":1,"name":name}})
}

#[test]
fn slash_commands_support_server_and_dm_contexts_without_machine_specific_choices() {
    let commands = slash_commands();
    assert_eq!(commands.len(), 4);
    for command in &commands {
        assert_eq!(command["contexts"], json!([0, 1]));
        assert!(command_matches(command, command));
    }
    let run = commands
        .iter()
        .find(|command| command["name"] == "run")
        .unwrap();
    assert!(run["options"][0].get("choices").is_none());
    let mut saved = run.clone();
    saved["id"] = json!("1234");
    saved["options"][0]["autocomplete"] = json!(false);
    assert!(command_matches(&saved, run));
    saved["contexts"] = json!([0]);
    assert!(!command_matches(&saved, run));
}

#[test]
fn slash_routes_only_the_verified_owner_and_valid_options() {
    for (name, expected) in [("list", "list"), ("new-chat", "new chat"), ("help", "help")] {
        let m = normalize_interaction(&slash_event(name), "999", &channels()).unwrap();
        assert_eq!(m.command, expected);
        assert!(m.destination.thread_id.is_none());
    }
    let mut e = slash_event("run");
    e["data"]["options"] = json!([{"name":"workflow","type":3,"value":"web-News-1"}]);
    assert_eq!(
        normalize_interaction(&e, "999", &channels())
            .unwrap()
            .command,
        "run web-News-1"
    );
    for (field, value) in [
        ("guild_id", json!("111")),
        ("application_id", json!("998")),
        ("channel_id", json!("457")),
        ("user", json!({"id":"124"})),
        (
            "data",
            json!({"type":1,"name":"run","options":[{"name":"workflow","type":3,"value":"../outside"}]}),
        ),
        (
            "data",
            json!({"type":1,"name":"new-chat","options":[{"name":"extra","type":3,"value":"x"}]}),
        ),
    ] {
        let mut bad = e.clone();
        bad[field] = value;
        assert!(normalize_interaction(&bad, "999", &channels()).is_none());
    }
}

#[tokio::test]
async fn slash_acknowledgement_is_a_message_and_never_uses_bot_auth() {
    let (api, server) = fixture_http("200 OK", json!({})).await;
    acknowledge_interaction(
        &api.base,
        &json!({"id":"123","token":"temporary-token","type":2}),
    )
    .await
    .unwrap();
    let wire = server.await.unwrap();
    assert!(!wire.to_lowercase().contains("authorization:"));
    let body: Value = serde_json::from_str(wire.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["type"], 4);
    assert_eq!(body["data"]["allowed_mentions"]["parse"], json!([]));
}

#[tokio::test]
async fn unchanged_slash_registration_only_reads_the_shared_bot_commands() {
    let (api, server) = fixture_http("200 OK", json!(slash_commands())).await;
    api.register_commands("999").await.unwrap();
    let wire = server.await.unwrap();
    assert!(wire.starts_with("GET /applications/999/commands "));
}

#[tokio::test]
async fn shared_bot_slash_events_execute_only_on_the_owners_gateway() {
    let first = Gateway::new(config(), FakeRuntime::default(), FakeMessenger::default());
    let mut other_config = config();
    other_config.allowed_users = vec!["124".into()];
    other_config.allowed_conversations = vec!["457".into()];
    let second = Gateway::new(
        other_config,
        FakeRuntime::default(),
        FakeMessenger::default(),
    );
    let second_channels = HashMap::from([("124".into(), "457".into())]);
    let mut e = slash_event("run");
    e["data"]["options"] = json!([{"name":"workflow","type":3,"value":"demo"}]);
    first
        .handle(normalize_interaction(&e, "999", &channels()).unwrap())
        .await;
    assert!(normalize_interaction(&e, "999", &second_channels).is_none());
    assert_eq!(first.runtime.starts.load(Ordering::SeqCst), 1);
    assert_eq!(second.runtime.starts.load(Ordering::SeqCst), 0);
    e["id"] = json!("1003");
    e["user"]["id"] = json!("124");
    e["channel_id"] = json!("457");
    assert!(normalize_interaction(&e, "999", &channels()).is_none());
    second
        .handle(normalize_interaction(&e, "999", &second_channels).unwrap())
        .await;
    assert_eq!(second.runtime.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn first_connection_registers_generic_commands_without_replacing_other_commands() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::task::spawn_blocking(move || {
        let mut registered = Vec::new();
        for index in 0..5 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            let headers = String::from_utf8(headers).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            if index == 0 {
                assert!(headers.starts_with("GET /applications/999/commands "));
            } else {
                assert!(headers.starts_with("POST /applications/999/commands "));
                registered.push(serde_json::from_slice::<Value>(&body).unwrap());
            }
            let reply = if index == 0 {
                r#"[{"name":"another-command","type":1}]"#
            } else {
                "{}"
            };
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",reply.len(),reply).unwrap();
        }
        registered
    });
    let mut api = DiscordApi::new("test-token".into()).unwrap();
    api.base = format!("http://{address}");
    api.register_commands("999").await.unwrap();
    assert_eq!(server.await.unwrap(), slash_commands());
}

fn run_slash_event() -> Value {
    let mut event = slash_event("run");
    event["id"] = json!("789");
    event["data"]["options"] = json!([{"name":"workflow","type":3,"value":"demo"}]);
    event
}

async fn interaction_fixture(count: usize) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for _ in 0..count {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).await.unwrap();
                headers.push(byte[0]);
            }
            let headers = String::from_utf8(headers).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["type"], 4);
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        }
    });
    (base, server)
}

#[tokio::test]
async fn ordinary_dm_words_go_to_the_assistant_while_slash_help_is_a_control() {
    let gateway = Gateway::new(config(), FakeRuntime::default(), FakeMessenger::default());
    for (index, text) in ["help", "list", "run demo", "new chat"].iter().enumerate() {
        let mut e = event(text);
        e["id"] = json!((800 + index).to_string());
        let message = normalize(&e, "999", &channels()).unwrap();
        assert!(message.plain_text);
        gateway.handle(message).await;
        let replies = gateway.messenger.replies.lock().unwrap();
        assert_eq!(
            replies.last().unwrap().1.text,
            format!("Answer to 123: {text}")
        );
    }
    assert_eq!(gateway.runtime.starts.load(Ordering::SeqCst), 0);
    gateway
        .handle(normalize_interaction(&slash_event("help"), "999", &channels()).unwrap())
        .await;
    let replies = gateway.messenger.replies.lock().unwrap();
    assert!(replies.last().unwrap().1.text.starts_with("Use /list"));
}

fn server_router(api: DiscordApi) -> server::ServerRouter {
    server::ServerRouter {
        config: crate::messaging::config::DiscordServer {
            guild_id: "111".into(),
            channels: vec!["456".into()],
        },
        users: vec!["123".into(), "124".into()],
        bot: "999".into(),
        api: Arc::new(api),
        seen: Default::default(),
    }
}
#[tokio::test]
async fn server_followups_use_same_thread_and_validate_members_for_slash_and_buttons() {
    for (kind, sender) in [(0, "123"), (2, "124"), (3, "123")] {
        let (api, http) = fixture_http(
            "200 OK",
            json!({"id":"789","guild_id":"111","parent_id":"456","type":11}),
        )
        .await;
        let router = server_router(api);
        let mut e = if kind == 0 {
            event("<@999> explain the result")
        } else if kind == 2 {
            slash_event("list")
        } else {
            json!({"id":"1003","type":3,"application_id":"999","data":{"component_type":2,"custom_id":format!("omar:approve:{}",uuid::Uuid::new_v4())},"message":{"id":"1001","author":{"id":"999"}}})
        };
        e["guild_id"] = json!("111");
        e["channel_id"] = json!("789");
        if kind == 0 {
            e["author"]["id"] = json!(sender);
        } else {
            e["member"] = json!({"user":{"id":sender}});
            e.as_object_mut().unwrap().remove("user");
        }
        let m = router.route(&e, kind != 0).await.unwrap().unwrap();
        assert_eq!(m.destination.conversation_id, "789");
        assert_eq!(m.sender_id, sender);
        assert_eq!(m.server_context.unwrap().parent_id, "456");
        assert!(http.await.unwrap().starts_with("GET /channels/789 "));
        if kind == 0 {
            assert_eq!(m.command, "explain the result");
        }
    }
}
#[tokio::test]
async fn server_metadata_from_other_guild_never_reaches_gateway() {
    let (api, http) = fixture_http("200 OK", json!({"id":"456","guild_id":"222","type":0})).await;
    let router = server_router(api);
    let mut e = event("<@999> hello");
    e["guild_id"] = json!("111");
    assert!(router.route(&e, false).await.unwrap().is_none());
    http.await.unwrap();
}
#[tokio::test]
async fn top_level_mention_creates_one_thread_before_processing_and_replay_creates_no_more() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut api = DiscordApi::new("fixture".into()).unwrap();
    api.base = format!("http://{}", listener.local_addr().unwrap());
    let http = tokio::spawn(async move {
        let mut wires = vec![];
        for body in [
            json!({"id":"456","guild_id":"111","type":0}),
            json!({"id":"790","guild_id":"111","parent_id":"456","type":11}),
            json!({"id":"456","guild_id":"111","type":0}),
        ] {
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
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).as_bytes()).await.unwrap();
        }
        wires
    });
    let router = server_router(api);
    let mut e = event("<@999> research this");
    e["guild_id"] = json!("111");
    let m = router.route(&e, false).await.unwrap().unwrap();
    assert_eq!(m.destination.conversation_id, "790");
    assert!(m.destination.thread_id.is_none());
    assert!(router.route(&e, false).await.unwrap().is_none());
    let wires = http.await.unwrap();
    assert!(wires[1].starts_with("POST /channels/456/messages/789/threads "));
    assert_eq!(wires.iter().filter(|w| w.starts_with("POST")).count(), 1);
}

#[tokio::test]
async fn server_socket_queues_two_users_runs_while_keeping_gateway_responsive() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut api = DiscordApi::new("fixture".into()).unwrap();
    api.base = format!("http://{}", listener.local_addr().unwrap());
    let http = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut headers = vec![];
            loop {
                let mut byte = [0];
                stream.read_exact(&mut byte).await.unwrap();
                headers.push(byte[0]);
                if headers.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            assert!(String::from_utf8(headers)
                .unwrap()
                .starts_with("GET /channels/789 "));
            let body = json!({"id":"789","guild_id":"111","type":11,"parent_id":"456"}).to_string();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).as_bytes()).await.unwrap();
        }
    });
    let router = server_router(api);
    let mut c = config();
    c.allowed_users.push("124".into());
    c.discord_server = Some(router.config.clone());
    let gate = Arc::new(tokio::sync::Notify::new());
    let gateway = Arc::new(Gateway::new(
        c,
        FakeRuntime {
            gate: Some(gate.clone()),
            ..Default::default()
        },
        FakeMessenger::default(),
    ));
    let (callback_base, callbacks) = interaction_fixture(2).await;
    let identity = Identity {
        token: "fixture".into(),
        bot: "999".into(),
        channels: channels(),
        callback_base,
        server: Some(router),
    };
    let (client, mut socket) = sockets().await;
    let mut session = Session::default();
    let mut tasks = JoinSet::new();
    let peer = async {
        send_event(
            &mut socket,
            json!({"op":10,"d":{"heartbeat_interval":1000}}),
        )
        .await;
        let identify = read_event(&mut socket).await;
        assert_eq!(identify["d"]["intents"], DIRECT_MESSAGES | 1 | (1 << 9));
        send_event(&mut socket,json!({"op":0,"s":1,"t":"READY","d":{"user":{"id":"999"},"session_id":"session","resume_gateway_url":"wss://gateway.discord.gg"}})).await;
        for (sender, event_id) in [("123", "1002"), ("124", "1003")] {
            let mut e = run_slash_event();
            e["id"] = json!(event_id);
            e["guild_id"] = json!("111");
            e["channel_id"] = json!("789");
            e["member"] = json!({"user":{"id":sender}});
            e.as_object_mut().unwrap().remove("user");
            send_event(
                &mut socket,
                json!({"op":0,"s":2,"t":"INTERACTION_CREATE","d":e}),
            )
            .await;
        }
        // The Discord heartbeat continues while the first run is waiting.
        assert_eq!(read_event(&mut socket).await["op"], 1);
        send_event(&mut socket, json!({"op":11})).await;
        assert_eq!(gateway.runtime.starts.load(Ordering::SeqCst), 1);
        gate.notify_one();
        timeout(Duration::from_secs(3), async {
            while gateway.runtime.starts.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        gate.notify_one();
        timeout(Duration::from_secs(3), async {
            while gateway.messenger.replies.lock().unwrap().len() < 4 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        send_event(&mut socket, json!({"op":7})).await;
    };
    let (result, _) = tokio::join!(
        socket_loop(client, &identity, &mut session, gateway.clone(), &mut tasks),
        peer
    );
    assert!(matches!(result.unwrap(), Reconnect::Resume));
    http.await.unwrap();
    callbacks.await.unwrap();
    assert!(gateway
        .messenger
        .replies
        .lock()
        .unwrap()
        .iter()
        .all(|(d, _)| d.conversation_id == "789"));
    assert_eq!(gateway.runtime.starts.load(Ordering::SeqCst), 2);
}
