pub mod config;
mod credentials;
pub mod runtime;
pub mod slack;
#[cfg(test)]
mod tests;

use anyhow::Result;
use config::Config;
use runtime::{Runtime, RuntimeError};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Destination {
    pub platform: String,
    pub account_id: String,
    pub conversation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Incoming {
    pub platform: String,
    pub account_id: String,
    pub sender_id: String,
    pub event_id: String,
    pub command: String,
    pub destination: Destination,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}
pub trait Messenger: Send + Sync {
    fn send(
        &self,
        destination: &Destination,
        reply: Reply,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn usage(&self, command: &str) -> String {
        format!("Use run {command}")
    }
}
#[derive(Default)]
struct State {
    seen: HashMap<(String, String, String), Instant>,
    busy: bool,
    uncertain: bool,
}
pub struct Gateway<R, M> {
    pub config: Config,
    pub runtime: R,
    pub messenger: M,
    state: Mutex<State>,
    dedup_limit: usize,
    dedup_ttl: Duration,
}
struct Busy<'a>(&'a Mutex<State>);
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.lock().unwrap().busy = false;
    }
}
impl<R: Runtime, M: Messenger> Gateway<R, M> {
    pub fn new(config: Config, runtime: R, messenger: M) -> Self {
        Self {
            config,
            runtime,
            messenger,
            state: Mutex::new(State::default()),
            dedup_limit: 2000,
            dedup_ttl: Duration::from_secs(3600),
        }
    }
    async fn reply(&self, destination: &Destination, text: String, run_id: Option<String>) {
        if self
            .messenger
            .send(
                destination,
                Reply {
                    text,
                    run_id: run_id.clone(),
                },
            )
            .await
            .is_err()
        {
            eprintln!(
                "{}",
                serde_json::json!({"kind":"delivery_failed", "destination":destination,"runId":run_id})
            );
        }
    }
    pub async fn handle(&self, message: Incoming) {
        let c = &self.config;
        let d = &message.destination;
        if message.platform != c.platform
            || message.account_id != c.account_id
            || message.event_id.is_empty()
            || !c.allowed_users.contains(&message.sender_id)
            || d.platform != c.platform
            || d.account_id != c.account_id
            || !c.allowed_conversations.contains(&d.conversation_id)
        {
            return;
        }
        let selected = message
            .command
            .strip_prefix("run ")
            .and_then(|name| c.select(name));
        let response = {
            let mut state = self.state.lock().unwrap();
            let now = Instant::now();
            state.seen.retain(|_, expires| *expires > now);
            let key = (message.platform, message.account_id, message.event_id);
            if state.seen.contains_key(&key) {
                return;
            }
            if state.seen.len() >= self.dedup_limit {
                eprintln!("{{\"kind\":\"dedup_capacity\"}}");
                return;
            }
            state.seen.insert(key, now + self.dedup_ttl);
            if message.command == "list" {
                Some(c.listing())
            } else if selected.is_none() {
                let usage = self.messenger.usage(if c.workflows.is_empty() {
                    &c.command
                } else {
                    "<workflow>"
                });
                Some(if message.command.starts_with("run ") {
                    format!("Unknown workflow. Use list to see approved workflows. {usage}")
                } else if c.workflows.is_empty() {
                    usage
                } else {
                    format!("{usage}. Use list to see approved workflows.")
                })
            } else if state.uncertain {
                Some("A previous run needs operator checking in OMAR. No new run started.".into())
            } else if state.busy {
                Some("This workflow is busy. Try again after it finishes.".into())
            } else {
                state.busy = true;
                None
            }
        };
        if let Some(text) = response {
            self.reply(d, text, None).await;
            return;
        }
        let _busy = Busy(&self.state);
        let selected = selected.expect("run admitted only for a registered workflow");
        let record = match self.runtime.start(selected.request).await {
            Ok(record) => record,
            Err(error) => {
                let error = error.downcast_ref::<RuntimeError>();
                let rejected = error.is_some_and(|e| {
                    e.status == 409 || (matches!(e.status, 400 | 413 | 503) && e.run_id.is_none())
                });
                let run_id = error.and_then(|e| e.run_id.clone());
                self.state.lock().unwrap().uncertain = !rejected;
                let text = if error.is_some_and(|e| e.status == 409) {
                    "This workflow is already active or paused in OMAR. No new run started."
                } else if rejected {
                    "OMAR rejected the request. Ask the operator to check the configuration."
                } else {
                    "Start outcome is uncertain. Check OMAR before trying again; no automatic retry."
                };
                if !rejected {
                    eprintln!(
                        "{}",
                        serde_json::json!({"kind":"start_uncertain","runId":run_id,"destination":d})
                    );
                }
                self.reply(d, text.into(), run_id).await;
                return;
            }
        };
        let id = record.run_id.clone();
        self.reply(d, format!("Started OMAR run {id}."), Some(id.clone()))
            .await;
        let record = match self.runtime.wait(record, selected.timeout_seconds).await {
            Ok(record) => record,
            Err(_) => {
                self.state.lock().unwrap().uncertain = true;
                eprintln!(
                    "{}",
                    serde_json::json!({"kind":"monitoring_failed","runId":id,"destination":d})
                );
                self.reply(
                    d,
                    format!("Could not retrieve run {id}. Check OMAR; it was not restarted."),
                    Some(id),
                )
                .await;
                return;
            }
        };
        let mut text = format!("OMAR run {}: {}.", record.run_id, record.status);
        if record.status == "completed" {
            match record.outputs.get(selected.output_port) {
                Some(value) => {
                    text.push('\n');
                    text.push_str(
                        &value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string()),
                    );
                }
                None => text.push_str(&format!(
                    "\nConfigured output {} is missing; check OMAR.",
                    selected.output_port
                )),
            }
        } else {
            text.push_str(" Check OMAR for details.");
        }
        self.reply(d, text, Some(record.run_id)).await;
    }
}

pub async fn run(command: &Command) -> Result<()> {
    match command {
        Command::Slack {
            action: Some(SlackAction::Login),
            ..
        } => credentials::login(),
        Command::Slack {
            action: Some(SlackAction::Logout),
            ..
        } => credentials::logout(),
        Command::Slack {
            config: Some(path),
            action: None,
        } => slack::run(Config::load(path)?).await,
        _ => anyhow::bail!("Provide a config file, or use `omar messaging slack login`"),
    }
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// Run Slack with a config, or manage saved credentials with login/logout
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    Slack {
        #[arg(required = true)]
        config: Option<std::path::PathBuf>,
        #[command(subcommand)]
        action: Option<SlackAction>,
    },
}

#[derive(clap::Subcommand)]
pub enum SlackAction {
    /// Save bot/app tokens once using hidden input and a private local file
    Login,
    /// Remove saved tokens (does not revoke tokens or unset environment variables)
    Logout,
}
