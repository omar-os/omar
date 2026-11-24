use super::{
    config::Config,
    runtime::{client, OmarRuntime},
    Destination, Gateway, Incoming, Messenger, Reply,
};
use anyhow::{ensure, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::{
    net::TcpStream,
    sync::watch,
    task::JoinSet,
    time::{timeout, Instant},
};
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

pub fn normalize(payload: &Value, bot: &str) -> Option<Incoming> {
    let event = payload.get("event")?;
    if payload["type"] != "event_callback"
        || event.get("bot_id").is_some_and(|v| !v.is_null())
        || event.get("subtype").is_some_and(|v| !v.is_null())
        || event["user"] == bot
    {
        return None;
    }
    let dm = event["type"] == "message" && event["channel_type"] == "im";
    if !dm && event["type"] != "app_mention" {
        return None;
    }
    let text = event["text"].as_str()?.trim();
    let prefix = format!("<@{bot}> ");
    let text = if dm {
        text
    } else {
        text.strip_prefix(&prefix)?.trim()
    };
    if text.is_empty() {
        return None;
    }
    let command = if dm { None } else { super::control(text) };
    let thread = if dm {
        None
    } else {
        Some(
            match event.get("thread_ts") {
                Some(value) => value.as_str()?,
                None => event["ts"].as_str()?,
            }
            .to_owned(),
        )
    };
    event["ts"].as_str()?;
    let event_id = payload["event_id"].as_str()?;
    if event_id.is_empty() {
        return None;
    }
    Some(Incoming {
        server_context: None,
        plain_text: command.is_none(),
        platform: "slack".into(),
        account_id: payload["team_id"].as_str()?.into(),
        sender_id: event["user"].as_str()?.into(),
        event_id: event_id.into(),
        command: command.unwrap_or_else(|| text.into()),
        destination: Destination {
            platform: "slack".into(),
            account_id: payload["team_id"].as_str()?.into(),
            conversation_id: event["channel"].as_str()?.into(),
            thread_id: thread,
        },
    })
}

fn normalize_interaction(payload: &Value, event_id: &str) -> Option<Incoming> {
    let (command, channel, thread) = match payload["type"].as_str() {
        Some("block_actions") => {
            let actions = payload["actions"].as_array()?;
            if actions.len() != 1 {
                return None;
            }
            let action = &actions[0];
            if action["type"] != "button" {
                return None;
            }
            let verb = action["action_id"].as_str()?.strip_prefix("omar:")?;
            let token = action["value"].as_str()?;
            if !matches!(verb, "approve" | "revise" | "cancel")
                || uuid::Uuid::parse_str(token).ok()?.to_string() != token
            {
                return None;
            }
            let channel = payload["channel"]["id"].as_str()?;
            let thread = if channel.starts_with('D') {
                None
            } else {
                Some(payload["message"]["thread_ts"].as_str()?.to_owned())
            };
            (format!("{verb} {token}"), channel, thread)
        }
        _ if payload["command"] == "/omar" => {
            let text = payload["text"].as_str()?.trim();
            (
                super::control(if text.is_empty() { "help" } else { text })
                    .unwrap_or_else(|| "help".into()),
                payload["channel_id"].as_str()?,
                None,
            )
        }
        _ => return None,
    };
    let account = payload["team"]["id"]
        .as_str()
        .or_else(|| payload["team_id"].as_str())?;
    let sender = payload["user"]["id"]
        .as_str()
        .or_else(|| payload["user_id"].as_str())?;
    Some(Incoming {
        server_context: None,
        plain_text: false,
        platform: "slack".into(),
        account_id: account.into(),
        sender_id: sender.into(),
        event_id: format!(
            "{}:{event_id}",
            if payload.get("command").is_some() {
                "slash"
            } else {
                "action"
            }
        ),
        command,
        destination: Destination {
            platform: "slack".into(),
            account_id: account.into(),
            conversation_id: channel.into(),
            thread_id: thread,
        },
    })
}

pub fn format_reply(reply: &Reply) -> String {
    reply
        .text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn upload_url(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw).map_err(|_| anyhow::anyhow!("Invalid Slack upload URL"))?;
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.fragment().is_none()
            && url
                .host_str()
                .is_some_and(|h| h == "files.slack.com" || h.ends_with(".slack.com")),
        "Invalid Slack upload host"
    );
    Ok(url)
}
fn verified_dm<'a>(value: &'a Value, user: &str) -> Option<&'a str> {
    (value["channel"]["is_im"] == true && value["channel"]["user"] == user)
        .then(|| {
            value["channel"]["id"]
                .as_str()
                .filter(|id| id.starts_with('D'))
        })
        .flatten()
}

fn review_buttons(destination: &Destination, token: &str) -> Value {
    let mut body = json!({"channel":destination.conversation_id,"text":"Review this exact version. Nothing has run yet.","mrkdwn":false,"parse":"none",
            "blocks":[{"type":"section","text":{"type":"plain_text","text":"Approve and run this version? Only the requester can act. Buttons expire after 30 minutes or a new message in this conversation."}},
            {"type":"actions","elements":[
                {"type":"button","text":{"type":"plain_text","text":"Run workflow"},"style":"primary","action_id":"omar:approve","value":token},
                {"type":"button","text":{"type":"plain_text","text":"Revise"},"action_id":"omar:revise","value":token},
                {"type":"button","text":{"type":"plain_text","text":"Cancel"},"style":"danger","action_id":"omar:cancel","value":token}]}]});
    if let Some(thread) = &destination.thread_id {
        body["thread_ts"] = json!(thread);
    }
    body
}

pub struct SlackApi {
    client: reqwest::Client,
    base: String,
    bot_token: String,
    app_token: String,
    rate_limit: super::transport::RateLimit,
}
impl SlackApi {
    pub fn new(bot_token: String, app_token: String) -> Result<Self> {
        Ok(Self {
            client: client()?,
            base: "https://slack.com/api/".into(),
            bot_token,
            app_token,
            rate_limit: super::transport::RateLimit::new(),
        })
    }
    async fn call(&self, method: &str, body: Value, app: bool) -> Result<Value> {
        timeout(Duration::from_secs(45), async {
            let mut next = self.rate_limit.wait().await;
            for _ in 0..3 {
                tokio::time::sleep_until(*next).await;
                let request = self
                    .client
                    .post(format!("{}{method}", self.base))
                    .bearer_auth(if app {
                        &self.app_token
                    } else {
                        &self.bot_token
                    })
                    .timeout(Duration::from_secs(15));
                let request = if method == "files.getUploadURLExternal" {
                    let fields = body
                        .as_object()
                        .context("Invalid upload metadata")?
                        .iter()
                        .map(|(k, v)| {
                            (
                                k.clone(),
                                v.as_str()
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| v.to_string()),
                            )
                        })
                        .collect::<Vec<_>>();
                    request.form(&fields)
                } else {
                    request.json(&body)
                };
                let response = request
                    .send()
                    .await
                    .map_err(|_| anyhow::anyhow!("Slack API request failed"))?;
                let status = response.status();
                let retry = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<f64>().ok());
                if status.as_u16() == 429 {
                    let delay = retry.context("Slack API request failed")?;
                    super::transport::RateLimit::defer(&mut next, delay)?;
                    continue;
                }
                ensure!(status.is_success(), "Slack API request failed");
                let value: Value = response
                    .json()
                    .await
                    .map_err(|_| anyhow::anyhow!("Invalid Slack API response"))?;
                ensure!(value["ok"] == true, "Slack API request failed");
                return Ok(value);
            }
            anyhow::bail!("Slack rate limit persists")
        })
        .await
        .map_err(|_| anyhow::anyhow!("Slack API timed out"))?
    }

    async fn upload(&self, destination: &Destination, name: &str, bytes: Vec<u8>) -> Result<()> {
        let prepared = self
            .call(
                "files.getUploadURLExternal",
                json!({"filename":name,"length":bytes.len()}),
                false,
            )
            .await?;
        let url = upload_url(
            prepared["upload_url"]
                .as_str()
                .context("Missing Slack upload URL")?,
        )?;
        // Pre-authorized upload URL: never send the bot token to the upload service.
        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/octet-stream")
            .body(bytes)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Slack file upload failed"))?;
        ensure!(response.status().is_success(), "Slack file upload failed");
        let file = prepared["file_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("Missing Slack file ID")?;
        let mut completion =
            json!({"files":[{"id":file,"title":name}],"channel_id":destination.conversation_id});
        if let Some(thread) = &destination.thread_id {
            completion["thread_ts"] = json!(thread);
        }
        self.call("files.completeUploadExternal", completion, false)
            .await?;
        Ok(())
    }
}
impl Messenger for SlackApi {
    async fn prepare(&self, mut message: Incoming) -> Result<Incoming> {
        if message.event_id.starts_with("slash:")
            && !message.destination.conversation_id.starts_with('D')
        {
            let root=self.call("chat.postMessage",json!({"channel":message.destination.conversation_id,
                "text":format!("OMAR request from {}",message.sender_id),"mrkdwn":false,"parse":"none"}),false).await?;
            message.destination.thread_id = Some(
                root["ts"]
                    .as_str()
                    .context("Missing Slack thread timestamp")?
                    .into(),
            );
        }
        Ok(message)
    }
    async fn review(
        &self,
        destination: &Destination,
        proposal: &super::runtime::Proposal,
        token: &str,
    ) -> Result<()> {
        let review = super::proposal::prepare(proposal).await?;
        self.upload(destination, "workflow.png", review.graph)
            .await?;
        self.send(
            destination,
            Reply {
                text: review.summary,
                run_id: None,
            },
        )
        .await?;
        self.send(
            destination,
            Reply {
                text: format!("Review files:\n{}", review.mappings.join("\n")),
                run_id: None,
            },
        )
        .await?;
        for (name, bytes) in review.files {
            self.upload(destination, &name, bytes).await?;
        }
        let body = review_buttons(destination, token);
        self.call("chat.postMessage", body, false).await?;
        Ok(())
    }

    fn usage(&self, command: &str) -> String {
        format!("Use @Omar run {command}")
    }
    async fn send(&self, destination: &Destination, reply: Reply) -> Result<()> {
        for part in super::text_parts(&reply.text, 3500, |c| match c {
            '&' => 5,
            '<' | '>' => 4,
            _ => 1,
        }) {
            let mut body = json!({"channel":destination.conversation_id,"text":format_reply(&Reply {text:part,run_id:reply.run_id.clone()}),
            "mrkdwn":false,"parse":"none","unfurl_links":false,"unfurl_media":false});
            if let Some(thread) = &destination.thread_id {
                body["thread_ts"] = json!(thread);
            }
            self.call("chat.postMessage", body, false).await?;
        }
        Ok(())
    }
}

async fn socket_loop<R: super::runtime::Runtime + 'static, M: Messenger + 'static>(
    mut socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    bot: &str,
    gateway: Arc<Gateway<R, M>>,
    tasks: &mut JoinSet<()>,
    stop: &mut watch::Receiver<bool>,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = stop.changed() => {
                let _ = timeout(Duration::from_secs(2), socket.close(None)).await;
                return Ok(());
            }
            result = tasks.join_next(), if !tasks.is_empty() => {
                if result.is_some_and(|r| r.is_err()) { eprintln!("Message processing failed."); }
            }
            frame = socket.next() => {
                let Some(frame) = frame else { return Ok(()); };
                match frame? {
                    Message::Text(raw) => {
                        let Ok(envelope) = serde_json::from_str::<Value>(&raw) else { continue; };
                        if let Some(id) = envelope["envelope_id"].as_str() {
                            timeout(Duration::from_secs(5), socket.send(Message::Text(json!({"envelope_id":id}).to_string()))).await??;
                        }
                        if envelope["type"] == "disconnect" { let _ = socket.close(None).await; return Ok(()); }
                        let normalized = if envelope["type"] == "events_api" {
                            normalize(&envelope["payload"], bot)
                        } else if matches!(envelope["type"].as_str(), Some("slash_commands" | "interactive")) {
                            envelope["envelope_id"].as_str().and_then(|id| normalize_interaction(&envelope["payload"], id))
                        } else {None};
                        if let Some(message) = normalized {
                                let gateway = gateway.clone();
                                // Admission state is shared across reconnects; no lock spans I/O.
                                if tasks.len() < 64 { tasks.spawn(async move { gateway.handle_queued(message).await; }); }
                        }
                    }
                    Message::Ping(data) => { timeout(Duration::from_secs(5), socket.send(Message::Pong(data))).await??; }
                    Message::Close(_) => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}

pub(super) async fn account_id() -> Result<String> {
    let (bot, app) = super::credentials::load()?.into_tokens();
    let identity = SlackApi::new(bot, app)?
        .call("auth.test", json!({}), false)
        .await?;
    Ok(identity["team_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("Missing Slack workspace ID")?
        .into())
}

pub async fn run(mut config: Config) -> Result<()> {
    ensure!(
        config.platform == "slack",
        "Slack command requires a Slack configuration"
    );
    let (bot, app) = super::credentials::load()?.into_tokens();
    let api = SlackApi::new(bot, app)?;
    let identity = api.call("auth.test", json!({}), false).await?;
    ensure!(
        identity["team_id"].as_str() == Some(&config.account_id),
        "Slack workspace does not match configuration"
    );
    let bot = identity["user_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("Missing Slack bot identity")?
        .to_string();
    super::runtime::bind_chat(&mut config).await?;
    let runtime = OmarRuntime::new(config.runtime_url.clone())?
        .for_messaging("slack".into(), config.account_id.clone());
    runtime.call("/v1/chat", None).await?;
    for user in &config.allowed_users {
        match api.call("conversations.open",json!({"users":user,"return_im":true}),false).await {
            Ok(value) if verified_dm(&value,user).is_some() => {
                let channel=verified_dm(&value,user).context("Invalid Slack DM channel")?;
                if !config.allowed_conversations.iter().any(|id|id==channel) { config.allowed_conversations.push(channel.into()); }
            }
            _ => eprintln!("Could not resolve an approved Slack DM; check im:write and reinstall app scopes. Channel access is unchanged."),
        }
    }
    let gateway = Arc::new(Gateway::new(config, runtime, api));
    let (mut stop, signal) = super::shutdown_signal();
    let mut tasks = JoinSet::new();
    let mut delay = 1;
    while !*stop.borrow() {
        let started = Instant::now();
        let mut session_stop = stop.clone();
        let session = async {
            let result = gateway
                .messenger
                .call("apps.connections.open", json!({}), true)
                .await?;
            let url = result["url"].as_str().context("Missing Slack socket URL")?;
            ensure!(
                reqwest::Url::parse(url)
                    .map(|u| u.scheme() == "wss")
                    .unwrap_or(false),
                "Invalid Slack socket URL"
            );
            let (socket, _) = timeout(
                Duration::from_secs(15),
                tokio_tungstenite::connect_async(url),
            )
            .await
            .map_err(|_| anyhow::anyhow!("Slack connection timed out"))?
            .map_err(|_| anyhow::anyhow!("Slack connection failed"))?;
            eprintln!("Slack Socket Mode connected.");
            socket_loop(socket, &bot, gateway.clone(), &mut tasks, &mut session_stop).await
        };
        tokio::select! {
            _ = stop.changed() => break,
            result = session => if result.is_err() { eprintln!("Slack connection failed; reconnecting."); }
        }
        if *stop.borrow() {
            break;
        }
        if started.elapsed() > Duration::from_secs(60) {
            delay = 1;
        }
        tokio::select! { _ = stop.changed() => break, _ = tokio::time::sleep(Duration::from_secs(delay)) => {} }
        delay = (delay * 2).min(30);
    }
    tasks.abort_all();
    signal.abort();
    Ok(())
}

#[cfg(test)]
pub(super) mod tests;
