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
    #[serde(default)]
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
pub trait Runtime: Send + Sync {
    fn approved(&self) -> impl std::future::Future<Output = Result<Vec<ApprovedWorkflow>>> + Send;

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
    async fn approved(&self) -> Result<Vec<ApprovedWorkflow>> {
        let root = self.base.split("/chats/").next().unwrap_or(&self.base);
        Ok(serde_json::from_value(
            Self::new(root.to_owned())?
                .call("/v1/workflows", None)
                .await?,
        )?)
    }

    async fn start(&self, request: &Value) -> Result<Run> {
        let target = if let Some(id) = request["conversation_id"].as_str() {
            uuid::Uuid::parse_str(id)?;
            format!(
                "{}/chats/{id}",
                self.base.split("/chats/").next().unwrap_or(&self.base)
            )
        } else {
            self.base.clone()
        };
        let mut run: Run =
            serde_json::from_value(Self::new(target)?.call("/v1/runs", Some(request)).await?)?;
        run.conversation_id = request["conversation_id"].as_str().map(str::to_owned);
        anyhow::ensure!(!run.run_id.is_empty(), "Missing run ID");
        Ok(run)
    }
    async fn wait(&self, mut run: Run, timeout: u64) -> Result<Run> {
        let conversation = run.conversation_id.clone();
        let target = if let Some(id) = &conversation {
            uuid::Uuid::parse_str(id)?;
            format!(
                "{}/chats/{id}",
                self.base.split("/chats/").next().unwrap_or(&self.base)
            )
        } else {
            self.base.clone()
        };
        let runtime = Self::new(target)?;
        let deadline = Instant::now() + Duration::from_secs(timeout + 15);
        while matches!(
            run.status.as_str(),
            "starting" | "running" | "stopping" | "pausing"
        ) {
            if Instant::now() >= deadline {
                bail!("Monitoring deadline reached");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            let mut url = reqwest::Url::parse(&format!("{}/v1/runs/", runtime.base))?;
            url.path_segments_mut()
                .map_err(|_| anyhow::anyhow!("Invalid runtime URL"))?
                .pop_if_empty()
                .push(&run.run_id);
            let path = url
                .path()
                .strip_prefix(reqwest::Url::parse(&runtime.base)?.path())
                .unwrap_or("")
                .to_string();
            run = serde_json::from_value(runtime.call(&path, None).await?)?;
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
