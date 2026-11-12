use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, time::Duration};
use tokio::time::Instant;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub run_id: String,
    pub status: String,
    #[serde(default)]
    pub outputs: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("Runtime returned HTTP {status}")]
pub struct RuntimeError {
    pub status: u16,
    pub run_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovedWorkflow {
    pub name: String,
    pub request: Value,
    pub output_port: String,
    pub timeout_seconds: u64,
}

#[derive(Clone, Debug)]
pub struct Proposal {
    pub conversation_id: String,
    pub sequence: u64,
    pub summary: String,
    pub design: crate::serve::ProposedDesign,
}

pub trait Runtime: Send + Sync {
    fn proposal(
        &self,
        _sender: &str,
    ) -> impl std::future::Future<Output = Result<Option<Proposal>>> + Send {
        async { Ok(None) }
    }
    fn approved(&self) -> impl std::future::Future<Output = Result<Vec<ApprovedWorkflow>>> + Send {
        async { Ok(Vec::new()) }
    }
    fn converse(
        &self,
        _sender: &str,
        _text: &str,
    ) -> impl std::future::Future<Output = Result<String>> + Send {
        async { bail!("Conversation is unavailable") }
    }
    fn start(&self, request: &Value) -> impl std::future::Future<Output = Result<Run>> + Send;
    fn wait(&self, run: Run, timeout: u64)
        -> impl std::future::Future<Output = Result<Run>> + Send;
}

pub fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .build()?)
}

pub struct OmarRuntime {
    pub base: String,
    client: reqwest::Client,
    conversations: tokio::sync::Mutex<BTreeMap<String, String>>,
    discord_account: Option<String>,
}
impl OmarRuntime {
    pub fn new(base: String) -> Result<Self> {
        Ok(Self {
            base,
            client: client()?,
            conversations: tokio::sync::Mutex::new(BTreeMap::new()),
            discord_account: None,
        })
    }

    pub fn for_discord(mut self, account: String) -> Self {
        self.discord_account = Some(account);
        self
    }

    pub async fn call(&self, path: &str, body: Option<&Value>) -> Result<Value> {
        let url = format!("{}{path}", self.base);
        let request = match body {
            Some(body) => self
                .client
                .post(url)
                .json(body)
                .timeout(Duration::from_secs(60)),
            None => self.client.get(url).timeout(Duration::from_secs(10)),
        };
        // Never retry admission: a lost response can follow a successful start.
        let response = request
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Runtime request failed"))?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("Invalid runtime response"))?;
        if !status.is_success() {
            return Err(RuntimeError {
                status: status.as_u16(),
                run_id: value["run_id"].as_str().map(str::to_owned),
            }
            .into());
        }
        Ok(value)
    }
}
impl Runtime for OmarRuntime {
    async fn proposal(&self, sender: &str) -> Result<Option<Proposal>> {
        let base = self.conversations.lock().await.get(sender).cloned();
        let Some(base) = base else {
            return Ok(None);
        };
        let chat = OmarRuntime::new(base)?;
        let history = chat.call("/v1/chat", None).await?;
        let messages = history["messages"]
            .as_array()
            .context("Invalid chat history")?;
        let after = messages
            .iter()
            .rev()
            .find(|m| m["role"] == "operator")
            .and_then(|m| m["sequence"].as_u64())
            .unwrap_or(0);
        let Some(message) = messages.iter().find(|m| {
            m["sequence"].as_u64().is_some_and(|s| s > after)
                && m["role"] == "assistant"
                && m["progress"] == false
        }) else {
            return Ok(None);
        };
        if message["design"].is_null() {
            return Ok(None);
        }
        let conversation_id = history["id"]
            .as_str()
            .context("Missing chat ID")?
            .to_owned();
        uuid::Uuid::parse_str(&conversation_id)?;
        Ok(Some(Proposal {
            conversation_id,
            sequence: message["sequence"].as_u64().context("Missing sequence")?,
            summary: message["text"].as_str().unwrap_or("").to_owned(),
            design: serde_json::from_value(message["design"].clone())?,
        }))
    }

    async fn approved(&self) -> Result<Vec<ApprovedWorkflow>> {
        let root = self.base.split("/chats/").next().unwrap_or(&self.base);
        let runtime = OmarRuntime::new(root.to_owned())?;
        Ok(serde_json::from_value(
            runtime.call("/v1/workflows", None).await?,
        )?)
    }

    async fn converse(&self, sender: &str, text: &str) -> Result<String> {
        let account = self
            .discord_account
            .as_deref()
            .context("Missing Discord account")?;
        let root = OmarRuntime::new(
            self.base
                .split("/chats/")
                .next()
                .unwrap_or(&self.base)
                .to_owned(),
        )?;
        let fresh = text == "new chat";
        let record = root.call("/v1/chats/messaging", Some(&serde_json::json!({
            "platform": "discord", "accountId": format!("assistant-{account}-{sender}"), "fresh": fresh
        }))).await?;
        let id = record["id"].as_str().context("Missing conversation ID")?;
        uuid::Uuid::parse_str(id)?;
        let base = format!("{}/chats/{id}", root.base);
        let mut conversations = self.conversations.lock().await;
        if fresh {
            conversations.remove(sender);
            return Ok("Started a new chat. Your previous conversation is saved in OMAR.".into());
        }
        conversations.insert(sender.to_owned(), base.clone());
        drop(conversations);
        let chat = OmarRuntime::new(base)?;
        // Do not retry a POST: a lost response can follow successful delivery.
        let accepted = chat
            .call("/v1/chat", Some(&serde_json::json!({"text":text})))
            .await?;
        let sequence = accepted["sequence"]
            .as_u64()
            .context("Missing message sequence")?;
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            let history = chat.call("/v1/chat", None).await?;
            if let Some(reply) = conversation_reply(&history, sequence) {
                return Ok(reply);
            }
            anyhow::ensure!(Instant::now() < deadline, "Assistant reply timed out");
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn start(&self, request: &Value) -> Result<Run> {
        let conversation = request["conversation_id"].as_str();
        let target = match conversation {
            Some(id) => {
                uuid::Uuid::parse_str(id)?;
                let root = self.base.split("/chats/").next().unwrap_or(&self.base);
                OmarRuntime::new(format!("{root}/chats/{id}"))?
            }
            None => OmarRuntime::new(self.base.clone())?,
        };
        let response = if let Some(sequence) = request.get("proposal_sequence") {
            target
                .call(
                    "/v1/chat/approve",
                    Some(&serde_json::json!({"sequence":sequence})),
                )
                .await?
        } else {
            target.call("/v1/runs", Some(request)).await?
        };
        let mut run: Run = serde_json::from_value(response)?;
        run.conversation_id = conversation.map(str::to_owned);
        anyhow::ensure!(!run.run_id.is_empty(), "Missing run ID");
        Ok(run)
    }
    async fn wait(&self, mut run: Run, timeout: u64) -> Result<Run> {
        let conversation = run.conversation_id.clone();
        let target = match &conversation {
            Some(id) => {
                uuid::Uuid::parse_str(id)?;
                let root = self.base.split("/chats/").next().unwrap_or(&self.base);
                OmarRuntime::new(format!("{root}/chats/{id}"))?
            }
            None => OmarRuntime::new(self.base.clone())?,
        };
        let deadline = Instant::now() + Duration::from_secs(timeout + 15);
        while matches!(
            run.status.as_str(),
            "starting" | "running" | "stopping" | "pausing"
        ) {
            if Instant::now() >= deadline {
                bail!("Monitoring deadline reached");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            let mut url = reqwest::Url::parse(&format!("{}/v1/runs/", target.base))?;
            url.path_segments_mut()
                .map_err(|_| anyhow::anyhow!("Invalid runtime URL"))?
                .pop_if_empty()
                .push(&run.run_id);
            let path = url
                .path()
                .strip_prefix(reqwest::Url::parse(&target.base)?.path())
                .unwrap_or("")
                .to_string();
            run = serde_json::from_value(target.call(&path, None).await?)?;
            run.conversation_id = conversation.clone();
        }
        anyhow::ensure!(
            matches!(
                run.status.as_str(),
                "completed" | "failed" | "stopped" | "paused"
            ),
            "Unknown run status"
        );
        Ok(run)
    }
}

/// Resolve only at connector startup; never rebind or retry an ambiguous run admission.
pub(super) async fn bind_chat(config: &mut super::config::Config) -> Result<()> {
    if !config.auto_chat {
        return Ok(());
    }
    let mut url = reqwest::Url::parse(&config.runtime_url)?;
    url.set_path("");
    let base = url.as_str().trim_end_matches('/').to_string();
    let runtime = OmarRuntime::new(base.clone())?;
    let chat = runtime.call("/v1/chats/messaging", Some(&serde_json::json!({"platform":config.platform,"accountId":config.account_id}))).await
        .map_err(|error| {
            if error.downcast_ref::<RuntimeError>().is_some_and(|e|e.status==404) {
                anyhow::anyhow!("Runtime needs the automatic-chat API; restart the same named runtime using this OMAR build, then reconnect")
            } else { error }
        })?;
    let id = chat["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing dedicated chat ID"))?;
    uuid::Uuid::parse_str(id).map_err(|_| anyhow::anyhow!("Invalid dedicated chat ID"))?;
    config.runtime_url = format!("{base}/chats/{id}");
    config.auto_chat = false;
    eprintln!("Using dedicated {} chat {id} at {base}.", config.platform);
    Ok(())
}

// Ignore old replies and interim commentary, including after an HTTP reconnect.
fn conversation_reply(history: &Value, after: u64) -> Option<String> {
    let reply = history["messages"].as_array()?.iter().find(|m| {
        m["sequence"].as_u64().is_some_and(|s| s > after)
            && m["role"] == "assistant"
            && m["progress"] == false
    })?;
    let mut text = reply["text"].as_str()?.to_owned();
    if reply.get("design").is_some_and(|d| !d.is_null()) {
        text.push_str("\nWorkflow proposal saved; it has not been run.");
    }
    Some(text)
}

#[cfg(test)]
mod conversation_tests {
    use super::*;
    #[test]
    fn only_returns_new_final_assistant_replies_and_labels_proposals() {
        let mut history = serde_json::json!({"messages":[
            {"sequence":1,"role":"assistant","text":"old","progress":false},
            {"sequence":3,"role":"operator","text":"question","progress":false},
            {"sequence":4,"role":"assistant","text":"working","progress":true}
        ]});
        assert_eq!(conversation_reply(&history, 2), None);
        history["messages"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "sequence":5,"role":"assistant","text":"answer","progress":false,"design":{}
            }));
        assert!(conversation_reply(&history, 2)
            .unwrap()
            .starts_with("answer\nWorkflow proposal"));
        assert_eq!(conversation_reply(&history, 5), None);
    }
}

#[cfg(test)]
mod conversation_http_tests {
    use super::*;
    use std::io::{Read, Write};
    #[tokio::test]
    async fn conversation_context_survives_connector_restart_and_can_be_reset() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let root = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut creations = Vec::new();
            let mut routes = Vec::new();
            let mut chats = BTreeMap::new();
            for _ in 0..16 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut raw = Vec::new();
                while !raw.ends_with(b"\r\n\r\n") {
                    let mut b = [0];
                    stream.read_exact(&mut b).unwrap();
                    raw.push(b[0]);
                }
                let headers = String::from_utf8(raw).unwrap();
                let route = headers.lines().next().unwrap().to_owned();
                let len = headers
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; len];
                stream.read_exact(&mut body).unwrap();
                let value = if route.starts_with("POST /v1/chats/messaging ") {
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    let key = request["accountId"].as_str().unwrap().to_owned();
                    creations.push(key.clone());
                    if request["fresh"] == true {
                        chats.remove(&key);
                    }
                    serde_json::json!({"id":chats.entry(key).or_insert_with(|| uuid::Uuid::new_v4().to_string())})
                } else if route.starts_with("POST ") {
                    routes.push(route);
                    serde_json::json!({"sequence":10})
                } else {
                    serde_json::json!({"messages":[{"sequence":11,"role":"assistant","progress":false,"text":"hello"}]})
                };
                let body = value.to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
            }
            assert_eq!(creations.len(), 6);
            assert_ne!(creations[0], creations[2]);
            assert_eq!(routes[0], routes[1]);
            assert_ne!(routes[0], routes[2]);
            assert_eq!(routes[0], routes[3]);
            assert_ne!(routes[0], routes[4]);
        });
        let runtime = OmarRuntime::new(root.clone())
            .unwrap()
            .for_discord("999".into());
        assert_eq!(runtime.converse("111", "hello").await.unwrap(), "hello");
        runtime.converse("111", "follow up").await.unwrap();
        runtime.converse("222", "different user").await.unwrap();
        let restarted = OmarRuntime::new(root).unwrap().for_discord("999".into());
        restarted.converse("111", "after restart").await.unwrap();
        restarted.converse("111", "new chat").await.unwrap();
        restarted.converse("111", "fresh context").await.unwrap();
        server.join().unwrap();
    }
}
