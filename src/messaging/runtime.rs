use anyhow::{bail, Result};
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
}

#[derive(Debug, thiserror::Error)]
#[error("Runtime returned HTTP {status}")]
pub struct RuntimeError {
    pub status: u16,
    pub run_id: Option<String>,
}

pub trait Runtime: Send + Sync {
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
}
impl OmarRuntime {
    pub fn new(base: String) -> Result<Self> {
        Ok(Self {
            base,
            client: client()?,
        })
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
    async fn start(&self, request: &Value) -> Result<Run> {
        let run: Run = serde_json::from_value(self.call("/v1/runs", Some(request)).await?)?;
        anyhow::ensure!(!run.run_id.is_empty(), "Missing run ID");
        Ok(run)
    }
    async fn wait(&self, mut run: Run, timeout: u64) -> Result<Run> {
        let deadline = Instant::now() + Duration::from_secs(timeout + 15);
        while matches!(
            run.status.as_str(),
            "starting" | "running" | "stopping" | "pausing"
        ) {
            if Instant::now() >= deadline {
                bail!("Monitoring deadline reached");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            let mut url = reqwest::Url::parse(&format!("{}/v1/runs/", self.base))?;
            url.path_segments_mut()
                .map_err(|_| anyhow::anyhow!("Invalid runtime URL"))?
                .pop_if_empty()
                .push(&run.run_id);
            let path = url
                .path()
                .strip_prefix(reqwest::Url::parse(&self.base)?.path())
                .unwrap_or("")
                .to_string();
            run = serde_json::from_value(self.call(&path, None).await?)?;
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
