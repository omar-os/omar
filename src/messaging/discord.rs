//! Discord DMs only. Workflow policy stays in the shared Gateway.
use super::{
    config::Config,
    runtime::{client, OmarRuntime},
    Destination, Gateway, Incoming, Messenger, Reply,
};
use anyhow::{ensure, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    sync::Mutex,
    task::JoinSet,
    time::{timeout, Instant},
};
use tokio_tungstenite::tungstenite::Message;

const DIRECT_MESSAGES: u64 = 1 << 12;

fn id(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()))
}

pub(super) fn normalize(
    event: &Value,
    bot: &str,
    channels: &HashMap<String, String>,
) -> Option<Incoming> {
    if event.get("guild_id").is_some()
        || event["type"] != 0
        || event["author"]["bot"] == true
        || event.get("webhook_id").is_some()
    {
        return None;
    }
    let sender = id(&event["author"]["id"])?;
    let channel = id(&event["channel_id"])?;
    if sender == bot || channels.get(sender).map(String::as_str) != Some(channel) {
        return None;
    }
    let message = id(&event["id"])?;
    Some(Incoming {
        platform: "discord".into(),
        account_id: bot.into(),
        sender_id: sender.into(),
        event_id: message.into(),
        command: event["content"].as_str()?.trim().into(),
        destination: Destination {
            platform: "discord".into(),
            account_id: bot.into(),
            conversation_id: channel.into(),
            thread_id: Some(message.into()),
        },
    })
}

fn normalize_interaction(
    event: &Value,
    bot: &str,
    channels: &HashMap<String, String>,
) -> Option<Incoming> {
    if event.get("guild_id").is_some()
        || event["type"] != 3
        || event["data"]["component_type"] != 2
        || event["application_id"] != bot
        || event["user"]["bot"] == true
        || event["message"]["author"]["id"] != bot
    {
        return None;
    }
    let sender = id(&event["user"]["id"])?;
    let channel = id(&event["channel_id"])?;
    if channels.get(sender).map(String::as_str) != Some(channel) {
        return None;
    }
    let custom = event["data"]["custom_id"].as_str()?;
    let mut parts = custom.split(':');
    if parts.next()? != "omar" {
        return None;
    }
    let verb = parts.next()?;
    let token = parts.next()?;
    if !matches!(verb, "approve" | "revise" | "cancel")
        || parts.next().is_some()
        || uuid::Uuid::parse_str(token).ok()?.to_string() != token
    {
        return None;
    }
    Some(Incoming {
        platform: "discord".into(),
        account_id: bot.into(),
        sender_id: sender.into(),
        event_id: id(&event["id"])?.into(),
        command: format!("{verb} {token}"),
        destination: Destination {
            platform: "discord".into(),
            account_id: bot.into(),
            conversation_id: channel.into(),
            thread_id: Some(id(&event["message"]["id"])?.into()),
        },
    })
}

async fn acknowledge_interaction(base: &str, event: &Value) -> Result<()> {
    let interaction = id(&event["id"]).context("Invalid interaction ID")?;
    let token = event["token"]
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 512
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
        .context("Invalid interaction token")?;
    // Acknowledge immediately, before compilation/model work. Never log the
    // callback URL: it contains a short-lived interaction credential.
    let response = client()?
        .post(format!(
            "{base}/interactions/{interaction}/{token}/callback"
        ))
        .json(&json!({"type":6}))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("Discord interaction acknowledgement failed"))?;
    ensure!(
        response.status().is_success(),
        "Discord interaction acknowledgement rejected"
    );
    Ok(())
}

// Text files are UTF-8, with generated header-safe attachment names. No new
// HTTP/multipart dependency is needed for this small fixed upload format.
fn multipart(payload: &Value, files: &[(String, Vec<u8>)]) -> (String, Vec<u8>) {
    let boundary = format!("omar-{}", uuid::Uuid::new_v4());
    let mut body=format!("--{boundary}\r\nContent-Disposition: form-data; name=\"payload_json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n",payload).into_bytes();
    for (i, (name, bytes)) in files.iter().enumerate() {
        let mime = if name.ends_with(".png") {
            "image/png"
        } else {
            "text/plain; charset=utf-8"
        };
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"files[{i}]\"; filename=\"{name}\"\r\nContent-Type: {mime}\r\n\r\n").as_bytes());
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (boundary, body)
}

fn message_parts(text: &str) -> Vec<String> {
    // Count UTF-16 units conservatively, including emoji, against Discord's limit.
    let mut parts = Vec::new();
    let mut part = String::new();
    let mut units = 0;
    for c in text.chars() {
        if units + c.len_utf16() > 2000 {
            parts.push(std::mem::take(&mut part));
            units = 0;
        }
        part.push(c);
        units += c.len_utf16();
    }
    if !part.is_empty() {
        parts.push(part);
    }
    parts
}

struct DiscordApi {
    client: reqwest::Client,
    token: String,
    base: String,
    // Serial delivery also respects a global 429 and the last route's reset window.
    next_request: Mutex<Instant>,
}
impl DiscordApi {
    fn new(token: String) -> Result<Self> {
        Ok(Self {
            client: client()?,
            token,
            base: "https://discord.com/api/v10".into(),
            next_request: Mutex::new(Instant::now()),
        })
    }
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        timeout(
            Duration::from_secs(45),
            self.request(method, path, body, &[]),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Discord API timed out"))?
    }
    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        files: &[(String, Vec<u8>)],
    ) -> Result<Value> {
        let mut next = self.next_request.lock().await;
        for _ in 0..3 {
            tokio::time::sleep_until(*next).await;
            let mut request = self
                .client
                .request(method.clone(), format!("{}{path}", self.base))
                .header("Authorization", format!("Bot {}", self.token))
                .header(
                    "User-Agent",
                    "DiscordBot (https://github.com/omar-os/omar, 0.4)",
                )
                .timeout(Duration::from_secs(15));
            if let Some(body) = &body {
                if files.is_empty() {
                    request = request.json(body);
                } else {
                    let (boundary, bytes) = multipart(body, files);
                    request = request
                        .header(
                            "Content-Type",
                            format!("multipart/form-data; boundary={boundary}"),
                        )
                        .body(bytes);
                }
            }
            let mut response = request
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("Discord API request failed"))?;
            let status = response.status();
            let remaining = response
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let reset = response
                .headers()
                .get("x-ratelimit-reset-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<f64>().ok());
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| anyhow::anyhow!("Discord API response failed"))?
            {
                ensure!(
                    bytes.len() + chunk.len() <= 128 * 1024,
                    "Discord API response too large"
                );
                bytes.extend_from_slice(&chunk);
            }
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("Invalid Discord API response"))?;
            let delay = if status.as_u16() == 429 {
                value["retry_after"].as_f64()
            } else if remaining.as_deref() == Some("0") {
                reset
            } else {
                None
            };
            if let Some(seconds) = delay {
                ensure!(
                    seconds.is_finite() && (0.0..=86400.0).contains(&seconds),
                    "Invalid Discord rate limit"
                );
                *next = Instant::now() + Duration::from_secs_f64(seconds + 0.1);
            }
            if status.as_u16() == 429 {
                ensure!(delay.is_some(), "Missing Discord rate limit");
                continue;
            }
            // Never retry a message after an ambiguous network error or server failure.
            ensure!(
                status.is_success(),
                "Discord API returned HTTP {}",
                status.as_u16()
            );
            return Ok(value);
        }
        anyhow::bail!("Discord rate limit persists")
    }
}
impl Messenger for DiscordApi {
    async fn review(
        &self,
        destination: &Destination,
        proposal: &super::runtime::Proposal,
        token: &str,
    ) -> Result<()> {
        let snapshot = proposal.design.preview.clone();
        let image =
            tokio::task::spawn_blocking(move || super::graph_preview::png(&snapshot)).await??;
        let graph_files = vec![("workflow.png".to_owned(), image)];
        let payload = json!({"content":format!("**{}** — workflow preview",proposal.design.preview.team),"allowed_mentions":{"parse":[]},
            "attachments":[{"id":0,"filename":"workflow.png","description":"Checked workflow graph: teams, agents, ports and connections"}]});
        timeout(
            Duration::from_secs(45),
            self.request(
                reqwest::Method::POST,
                &format!("/channels/{}/messages", destination.conversation_id),
                Some(payload),
                &graph_files,
            ),
        )
        .await??;
        self.send(
            destination,
            Reply {
                text: super::proposal::visual_summary(proposal)?,
                run_id: None,
            },
        )
        .await?;
        let mut files = vec![(
            "workflow.omar".to_owned(),
            proposal.design.program.as_bytes().to_vec(),
        )];
        let mut names = vec!["workflow.omar = program.omar".to_owned()];
        for (i, (path, text)) in proposal.design.files.iter().enumerate() {
            let name = format!("schema-{i}.json");
            names.push(format!("{name} = {path}"));
            files.push((name, text.as_bytes().to_vec()));
        }
        for (batch, mapping) in files.chunks(8).zip(names.chunks(8)) {
            let payload = json!({"content":format!("Review files:\n{}",mapping.join("\n")),"allowed_mentions":{"parse":[]},"flags":4,
                "attachments":batch.iter().enumerate().map(|(i,(name,_))|json!({"id":i,"filename":name})).collect::<Vec<_>>()});
            timeout(
                Duration::from_secs(45),
                self.request(
                    reqwest::Method::POST,
                    &format!("/channels/{}/messages", destination.conversation_id),
                    Some(payload),
                    batch,
                ),
            )
            .await??;
        }
        self.call(reqwest::Method::POST,&format!("/channels/{}/messages",destination.conversation_id),Some(json!({
            "content":"Approve and run this exact version? Run starts its agents locally. Revise asks for changes; Cancel starts nothing. Buttons expire in 30 minutes or when you send a new message.",
            "allowed_mentions":{"parse":[]},"flags":4,
            "components":[{"type":1,"components":[
                {"type":2,"style":3,"label":"Run workflow","custom_id":format!("omar:approve:{token}")},
                {"type":2,"style":2,"label":"Revise","custom_id":format!("omar:revise:{token}")},
                {"type":2,"style":4,"label":"Cancel","custom_id":format!("omar:cancel:{token}")}
            ]}]
        }))).await?;
        Ok(())
    }

    async fn send(&self, destination: &Destination, reply: Reply) -> Result<()> {
        ensure!(
            id(&json!(destination.conversation_id)).is_some(),
            "Invalid Discord destination"
        );
        for content in message_parts(&reply.text) {
            let mut body = json!({"content":content, "allowed_mentions":{"parse":[],"replied_user":false}, "flags":4});
            if let Some(message) = &destination.thread_id {
                body["message_reference"] =
                    json!({"message_id":message,"fail_if_not_exists":false});
            }
            self.call(
                reqwest::Method::POST,
                &format!("/channels/{}/messages", destination.conversation_id),
                Some(body),
            )
            .await?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct Session {
    sequence: Option<u64>,
    session_id: Option<String>,
    resume_url: Option<String>,
}
impl Session {
    fn identify(&self, token: &str) -> Value {
        match (&self.session_id, self.sequence) {
            (Some(session), Some(sequence)) => {
                json!({"op":6,"d":{"token":token,"session_id":session,"seq":sequence}})
            }
            _ => json!({"op":2,"d":{"token":token,"intents":DIRECT_MESSAGES,
                "properties":{"os":std::env::consts::OS,"browser":"omar","device":"omar"}}}),
        }
    }
    fn ready(&mut self, value: &Value, bot: &str) -> Result<()> {
        ensure!(value["user"]["id"] == bot, "Discord identity changed");
        let url = value["resume_gateway_url"]
            .as_str()
            .context("Missing Discord resume URL")?;
        gateway_url(url)?;
        self.session_id = Some(
            value["session_id"]
                .as_str()
                .filter(|v| !v.is_empty())
                .context("Missing Discord session")?
                .into(),
        );
        self.resume_url = Some(url.into());
        Ok(())
    }
}
fn gateway_url(raw: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(raw).context("Invalid Discord Gateway URL")?;
    ensure!(
        url.scheme() == "wss"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.fragment().is_none()
            && url
                .host_str()
                .is_some_and(|h| h == "discord.gg" || h.ends_with(".discord.gg")),
        "Invalid Discord Gateway host"
    );
    url.set_query(Some("v=10&encoding=json"));
    Ok(url.into())
}
#[derive(Debug)]
enum Reconnect {
    Resume,
    Fresh,
    Fatal,
}
fn close_action(code: u16) -> Reconnect {
    match code {
        4004 | 4010..=4014 => Reconnect::Fatal,
        4007 | 4009 => Reconnect::Fresh,
        _ => Reconnect::Resume,
    }
}

struct Identity {
    token: String,
    bot: String,
    channels: HashMap<String, String>,
    callback_base: String,
}

async fn connection<R: super::runtime::Runtime + 'static, M: Messenger + 'static>(
    url: &str,
    identity: &Identity,
    session: &mut Session,
    gateway: Arc<Gateway<R, M>>,
    tasks: &mut JoinSet<()>,
) -> Result<Reconnect> {
    let (socket, _) = timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async(gateway_url(url)?),
    )
    .await??;
    socket_loop(socket, identity, session, gateway, tasks).await
}

async fn socket_loop<R, M, S>(
    mut socket: tokio_tungstenite::WebSocketStream<S>,
    identity: &Identity,
    session: &mut Session,
    gateway: Arc<Gateway<R, M>>,
    tasks: &mut JoinSet<()>,
) -> Result<Reconnect>
where
    R: super::runtime::Runtime + 'static,
    M: Messenger + 'static,
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let hello = timeout(Duration::from_secs(15), socket.next())
        .await?
        .context("Discord disconnected before Hello")??;
    if let Message::Close(frame) = &hello {
        return Ok(frame
            .as_ref()
            .map(|f| close_action(u16::from(f.code)))
            .unwrap_or(Reconnect::Resume));
    }
    let Message::Text(raw) = hello else {
        anyhow::bail!("Missing Discord Hello");
    };
    let hello: Value = serde_json::from_str(&raw)?;
    ensure!(hello["op"] == 10, "Missing Discord Hello");
    let ms = hello["d"]["heartbeat_interval"]
        .as_u64()
        .context("Missing heartbeat interval")?;
    ensure!((1000..=120000).contains(&ms), "Invalid heartbeat interval");
    let interval = Duration::from_millis(ms);
    let jitter = u64::from_le_bytes(uuid::Uuid::new_v4().as_bytes()[..8].try_into().unwrap()) % ms;
    let mut heartbeat =
        tokio::time::interval_at(Instant::now() + Duration::from_millis(jitter), interval);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    timeout(
        Duration::from_secs(5),
        socket.send(Message::Text(session.identify(&identity.token).to_string())),
    )
    .await??;
    let mut pending_ack = false;
    let ready_deadline = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(ready_deadline);
    let mut ready = false;
    loop {
        tokio::select! {
            _ = &mut ready_deadline, if !ready => anyhow::bail!("Discord readiness timed out"),
            _ = heartbeat.tick() => {
                ensure!(!pending_ack, "Discord heartbeat not acknowledged");
                timeout(Duration::from_secs(5), socket.send(Message::Text(json!({"op":1,"d":session.sequence}).to_string()))).await??;
                pending_ack = true;
            }
            done = tasks.join_next(), if !tasks.is_empty() => {
                if done.is_some_and(|r| r.is_err()) { eprintln!("Discord message processing failed."); }
            }
            frame = socket.next() => {
                let Some(frame) = frame else { return Ok(Reconnect::Resume); };
                match frame? {
                    Message::Text(raw) => {
                        ensure!(raw.len() <= 256 * 1024, "Discord event too large");
                        let event: Value = serde_json::from_str(&raw)?;
                        if let Some(seq) = event["s"].as_u64() { session.sequence = Some(seq); }
                        match event["op"].as_u64() {
                            Some(0) => match event["t"].as_str() {
                                Some("READY") => { session.ready(&event["d"], &identity.bot)?; ready = true; eprintln!("Discord DMs connected."); }
                                Some("RESUMED") => { ready = true; eprintln!("Discord DMs resumed."); }
                                Some("INTERACTION_CREATE") => if let Some(message) = normalize_interaction(&event["d"], &identity.bot, &identity.channels) {
                                    if tasks.len()<64 && acknowledge_interaction(&identity.callback_base,&event["d"]).await.is_ok() {
                                        let gateway=gateway.clone(); tasks.spawn(async move { gateway.handle(message).await; });
                                    }
                                },
                                Some("MESSAGE_CREATE") => if let Some(message) = normalize(&event["d"], &identity.bot, &identity.channels) {
                                    // Bound task creation; retained gateway state survives reconnects.
                                    if tasks.len() < 64 { let gateway = gateway.clone(); tasks.spawn(async move { gateway.handle(message).await; }); }
                                },
                                _ => {}
                            },
                            Some(1) => { timeout(Duration::from_secs(5), socket.send(Message::Text(json!({"op":1,"d":session.sequence}).to_string()))).await??; pending_ack = true; }
                            Some(7) => return Ok(Reconnect::Resume),
                            Some(9) => return Ok(if event["d"] == true { Reconnect::Resume } else { Reconnect::Fresh }),
                            Some(11) => pending_ack = false,
                            _ => {}
                        }
                    }
                    Message::Ping(data) => { timeout(Duration::from_secs(5), socket.send(Message::Pong(data))).await??; }
                    Message::Close(frame) => return Ok(frame.map(|f| close_action(u16::from(f.code))).unwrap_or(Reconnect::Resume)),
                    _ => {}
                }
            }
        }
    }
}

pub(super) async fn account_id() -> Result<String> {
    let api = DiscordApi::new(super::credentials::discord_load()?)?;
    let identity = api.call(reqwest::Method::GET, "/users/@me", None).await?;
    ensure!(identity["bot"] == true, "Expected a Discord bot token");
    Ok(id(&identity["id"])
        .context("Missing Discord bot ID")?
        .into())
}

pub async fn run(mut config: Config) -> Result<()> {
    ensure!(
        config.platform == "discord",
        "Discord command requires a Discord configuration"
    );
    ensure!(
        id(&json!(config.account_id)).is_some()
            && config.allowed_users.iter().all(|u| id(&json!(u)).is_some()),
        "Discord IDs must be numeric"
    );
    let api = DiscordApi::new(super::credentials::discord_load()?)?;
    let identity = api.call(reqwest::Method::GET, "/users/@me", None).await?;
    ensure!(
        identity["bot"] == true && identity["id"].as_str() == Some(&config.account_id),
        "Discord bot does not match accountId"
    );
    let discovery = api.call(reqwest::Method::GET, "/gateway/bot", None).await?;
    ensure!(
        discovery["session_start_limit"]["remaining"]
            .as_u64()
            .is_some_and(|n| n > 0),
        "Discord session limit exhausted; try later"
    );
    let url = gateway_url(
        discovery["url"]
            .as_str()
            .context("Missing Discord Gateway URL")?,
    )?;
    super::runtime::bind_chat(&mut config).await?;
    let runtime = OmarRuntime::new(config.runtime_url.clone())?;
    runtime.call("/v1/chat", None).await?;
    let mut channels = HashMap::new();
    // Resolve only one-to-one channels with approved recipients, never group DMs.
    for user in &config.allowed_users {
        let dm = api
            .call(
                reqwest::Method::POST,
                "/users/@me/channels",
                Some(json!({"recipient_id":user})),
            )
            .await
            .context("Cannot open approved Discord DM; check mutual server and privacy settings")?;
        ensure!(
            dm["type"] == 1
                && dm["recipients"]
                    .as_array()
                    .is_some_and(|r| r.len() == 1 && r[0]["id"] == *user),
            "Expected a one-to-one Discord DM"
        );
        channels.insert(
            user.clone(),
            id(&dm["id"]).context("Missing Discord DM ID")?.to_string(),
        );
    }
    config.allowed_conversations = channels.values().cloned().collect();
    let identity = Identity {
        bot: config.account_id.clone(),
        token: api.token.clone(),
        channels,
        callback_base: api.base.clone(),
    };
    let gateway = Arc::new(Gateway::new(config, runtime, api));
    let mut session = Session::default();
    let mut tasks = JoinSet::new();
    let (mut stop, signal) = super::shutdown_signal();
    let mut delay = 5;
    let result = async {
        while !*stop.borrow() {
            let started = Instant::now();
            let resume_url = session.resume_url.clone().unwrap_or_else(|| url.clone());
            tokio::select! {
                _ = stop.changed() => break,
                result = connection(&resume_url, &identity, &mut session, gateway.clone(), &mut tasks) => match result {
                    Ok(Reconnect::Fatal) => anyhow::bail!("Discord rejected the connection; check bot token and configuration"),
                    Ok(Reconnect::Fresh) => session = Session::default(),
                    _ => eprintln!("Discord disconnected; reconnecting."),
                }
            }
            if started.elapsed() > Duration::from_secs(60) { delay = 5; }
            tokio::select! { _ = stop.changed() => break, _ = tokio::time::sleep(Duration::from_secs(delay)) => {} }
            delay = (delay * 2).min(30);
        }
        Ok(())
    }.await;
    tasks.abort_all();
    signal.abort();
    result
}

#[cfg(test)]
mod tests;
