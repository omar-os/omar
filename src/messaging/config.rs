use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub runtime_url: String,
    #[serde(default)]
    pub auto_chat: bool,
    #[serde(default = "slack")]
    pub platform: String,
    #[serde(alias = "workspaceId")]
    pub account_id: String,
    pub allowed_users: Vec<String>,
    #[serde(alias = "allowedChannels")]
    #[serde(default)]
    pub allowed_conversations: Vec<String>,
}
fn slack() -> String {
    "slack".into()
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let mut config: Self =
            serde_json::from_slice(&std::fs::read(path).context("Read messaging configuration")?)
                .context("Invalid messaging configuration")?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&mut self) -> Result<()> {
        let url = reqwest::Url::parse(&self.runtime_url).context("Invalid runtime URL")?;
        let chat = url
            .path()
            .strip_prefix("/chats/")
            .unwrap_or("")
            .trim_end_matches('/');
        ensure!(
            url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && ((self.auto_chat && matches!(url.path(), "" | "/"))
                    || (!chat.is_empty()
                        && chat.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                        && (url.path() == format!("/chats/{chat}") || url.path() == format!("/chats/{chat}/")))),
            "runtimeUrl must be loopback HTTP scoped to /chats/<id>, or a runtime root with autoChat enabled"
        );
        ensure!(
            !self.platform.is_empty() && !self.account_id.is_empty(),
            "Missing messaging identity"
        );
        ensure!(
            [&self.allowed_users]
                .iter()
                .all(|ids| !ids.is_empty() && ids.iter().all(|id| !id.is_empty())),
            "Allowlists must contain IDs"
        );
        ensure!(
            if self.platform == "discord" { self.allowed_conversations.is_empty() }
            else { !self.allowed_conversations.is_empty() && self.allowed_conversations.iter().all(|id| !id.is_empty()) },
            "Discord DM channels are resolved from allowedUsers; other platforms require allowedConversations"
        );
        self.runtime_url = url.as_str().trim_end_matches('/').to_string();
        Ok(())
    }
}

pub struct Selected<'a> {
    pub request: &'a Value,
    pub output_port: &'a str,
    pub timeout_seconds: u64,
}
