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
        || event["type"] != "app_mention"
        || event.get("bot_id").is_some_and(|v| !v.is_null())
        || event.get("subtype").is_some_and(|v| !v.is_null())
    {
        return None;
    }
    let sender = event["user"].as_str()?;
    if sender == bot {
        return None;
    }
    let text = event["text"].as_str()?.trim();
    let prefix = format!("<@{bot}> ");
    let command = text.strip_prefix(&prefix)?.trim().to_owned();
    let account_id = payload["team_id"].as_str()?.to_string();
    let event_id = payload["event_id"].as_str()?.to_string();
    if event_id.is_empty() {
        return None;
    }
    let thread = match event.get("thread_ts") {
        Some(value) => value.as_str()?,
        None => event["ts"].as_str()?,
    };
    // Validate the message timestamp even for an existing thread.
    event["ts"].as_str()?;
    Some(Incoming {
        platform: "slack".into(),
        account_id: account_id.clone(),
        sender_id: sender.into(),
        event_id,
        command,
        destination: Destination {
            platform: "slack".into(),
            account_id,
            conversation_id: event["channel"].as_str()?.into(),
            thread_id: Some(thread.into()),
        },
    })
}

pub fn format_reply(reply: &Reply) -> String {
    let escaped = reply
        .text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    if escaped.chars().count() <= 3500 {
        return escaped;
    }
    format!(
        "{}\n[Truncated; full output in OMAR{}.]",
        escaped.chars().take(3300).collect::<String>(),
        reply
            .run_id
            .as_ref()
            .map(|id| format!(" run {id}"))
            .unwrap_or_default()
    )
}

pub struct SlackApi {
    client: reqwest::Client,
    base: String,
    bot_token: String,
    app_token: String,
}
impl SlackApi {
    pub fn new(bot_token: String, app_token: String) -> Result<Self> {
        Ok(Self {
            client: client()?,
            base: "https://slack.com/api/".into(),
            bot_token,
            app_token,
        })
    }
    async fn call(&self, method: &str, body: Value, app: bool) -> Result<Value> {
        let response = self
            .client
            .post(format!("{}{method}", self.base))
            .bearer_auth(if app {
                &self.app_token
            } else {
                &self.bot_token
            })
            .json(&body)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Slack API request failed"))?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("Invalid Slack API response"))?;
        ensure!(
            status.is_success() && value["ok"] == true,
            "Slack API request failed"
        );
        Ok(value)
    }
}
impl Messenger for SlackApi {
    fn usage(&self, command: &str) -> String {
        format!("Use @Omar run {command}")
    }
    async fn send(&self, destination: &Destination, reply: Reply) -> Result<()> {
        let mut body = json!({"channel":destination.conversation_id,"text":format_reply(&reply),
            "mrkdwn":false,"parse":"none","unfurl_links":false,"unfurl_media":false});
        if let Some(thread) = &destination.thread_id {
            body["thread_ts"] = json!(thread);
        }
        self.call("chat.postMessage", body, false).await?;
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
                        if envelope["type"] == "events_api" {
                            if let Some(message) = normalize(&envelope["payload"], bot) {
                                let gateway = gateway.clone();
                                // Admission state is shared across reconnects; no lock spans I/O.
                                tasks.spawn(async move { gateway.handle(message).await; });
                            }
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

pub async fn run(config: Config) -> Result<()> {
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
    let runtime = OmarRuntime::new(config.runtime_url.clone())?;
    runtime.call("/v1/chat", None).await?;
    let gateway = Arc::new(Gateway::new(config, runtime, api));
    let (stop_tx, mut stop) = watch::channel(false);
    let signal = tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! { result = tokio::signal::ctrl_c() => result?, _ = term.recv() => {} }
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await?;
        let _ = stop_tx.send(true);
        Ok::<_, std::io::Error>(())
    });
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
mod tests;
