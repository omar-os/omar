pub mod config;
mod connect;
mod credential_store;
mod credentials;
pub mod discord;
mod graph_preview;
mod proposal;
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
    fn review(
        &self,
        destination: &Destination,
        proposal: &runtime::Proposal,
        token: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async move {
            self.send(
                destination,
                Reply {
                    text: format!(
                        "{}\nReply approve {token}, revise {token}, or cancel {token}.",
                        proposal::summary(proposal)?
                    ),
                    run_id: None,
                },
            )
            .await
        }
    }
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
    pending: HashMap<String, proposal::Pending>,
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
        let conversational = c.platform == "discord"
            && !message.command.is_empty()
            && message.command != "list"
            && message.command != "run"
            && !message.command.starts_with("run ");
        {
            let mut state = self.state.lock().unwrap();
            let now = Instant::now();
            state.seen.retain(|_, expires| *expires > now);
            let key = (
                message.platform.clone(),
                message.account_id.clone(),
                message.event_id.clone(),
            );
            if state.seen.contains_key(&key) {
                return;
            }
            if state.seen.len() >= self.dedup_limit {
                eprintln!("{{\"kind\":\"dedup_capacity\"}}");
                return;
            }
            state.seen.insert(key, now + self.dedup_ttl);
        }
        if c.platform == "discord" && proposal::action(&message.command) {
            self.review_action(&message).await;
            return;
        }
        let approved = if message.command == "list" || message.command.starts_with("run ") {
            match self.runtime.approved().await {
                Ok(entries) => entries,
                Err(_) => {
                    self.reply(d, "Could not load saved workflows from OMAR. Try again when the runtime is available.".into(), None).await;
                    return;
                }
            }
        } else {
            Vec::new()
        };
        let selected = approved
            .iter()
            .find(|workflow| message.command.strip_prefix("run ") == Some(workflow.name.as_str()))
            .map(|workflow| config::Selected {
                request: &workflow.request,
                output_port: &workflow.output_port,
                timeout_seconds: workflow.timeout_seconds,
            });
        let response = {
            let mut state = self.state.lock().unwrap();
            if message.command == "list" {
                let mut listing = String::from("Saved workflows:");
                for workflow in &approved {
                    listing.push_str(&format!("\n- {}", workflow.name));
                }
                if approved.is_empty() {
                    listing.push_str(
                        "\nNo approved workflows yet. Create and approve a workflow in OMAR.",
                    );
                }
                Some(listing)
            } else if selected.is_none() && !conversational {
                Some(format!(
                    "{}. Use list to see saved workflows.",
                    self.messenger.usage("<workflow>")
                ))
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
        if conversational {
            self.state.lock().unwrap().pending.retain(|_, p| {
                !(p.sender == message.sender_id
                    && p.destination.conversation_id == d.conversation_id)
            });
            match self.runtime.converse(&message.sender_id, &message.command).await {
                Ok(text)=>match self.runtime.proposal(&message.sender_id).await {
                    Ok(Some(proposal))=>self.present_proposal(&message,proposal).await,
                    Ok(None)=>self.reply(d,text,None).await,
                    Err(_)=>self.reply(d,"The assistant replied, but I could not retrieve its proposal safely. Nothing has run; ask again.".into(),None).await,
                },
                Err(_)=>self.reply(d,"Could not retrieve the assistant's answer. Check OMAR before sending again; your message was not automatically retried.".into(),None).await,
            }
            return;
        }
        let selected = selected.expect("run admitted only for a registered workflow");
        self.execute(d, selected).await;
    }
    async fn execute(&self, d: &Destination, selected: config::Selected<'_>) {
        let record = match self.runtime.start(selected.request).await {
            Ok(record) => record,
            Err(error) => {
                let error = error.downcast_ref::<RuntimeError>();
                let rejected = error.is_some_and(|e| {
                    e.status == 409 || (matches!(e.status, 400 | 413 | 503) && e.run_id.is_none())
                });
                let run_id = error.and_then(|e| e.run_id.clone());
                self.state.lock().unwrap().uncertain = !rejected;
                let text = if selected.request.get("proposal_sequence").is_some()
                    && error.is_some_and(|e| e.status == 409)
                {
                    "This proposal is stale, already handled, or its workflow is active. Wait for the current run or ask for a fresh proposal."
                } else if error.is_some_and(|e| e.status == 409) {
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
            if selected.output_port.is_empty() {
                if record.outputs.is_empty() {
                    text.push_str("\nNo output values were produced.");
                }
                for (port, value) in &record.outputs {
                    text.push_str(&format!(
                        "\n{port}: {}",
                        value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string())
                    ));
                }
            } else {
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
            action: Some(CredentialAction::Connect),
            ..
        } => connect::run("slack").await,
        Command::Discord {
            action: Some(CredentialAction::Connect),
            ..
        } => connect::run("discord").await,
        Command::Discord {
            action: Some(CredentialAction::Login),
            ..
        } => credentials::discord_login(),
        Command::Discord {
            action: Some(CredentialAction::Logout),
            ..
        } => credentials::discord_logout(),
        Command::Discord {
            config: Some(path),
            action: None,
        } => discord::run(Config::load(path)?).await,
        Command::Slack {
            action: Some(CredentialAction::Login),
            ..
        } => credentials::login(),
        Command::Slack {
            action: Some(CredentialAction::Logout),
            ..
        } => credentials::logout(),
        Command::Slack {
            config: Some(path),
            action: None,
        } => slack::run(Config::load(path)?).await,
        _ => anyhow::bail!("Provide a config file, or use `omar messaging <platform> login`"),
    }
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// Connect approved Discord DMs to local workflows; manage saved credentials
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    Discord {
        #[arg(required = true)]
        config: Option<std::path::PathBuf>,
        #[command(subcommand)]
        action: Option<CredentialAction>,
    },
    /// Run Slack with a config, or manage saved credentials with login/logout
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    Slack {
        #[arg(required = true)]
        config: Option<std::path::PathBuf>,
        #[command(subcommand)]
        action: Option<CredentialAction>,
    },
}

#[derive(clap::Subcommand)]
pub enum CredentialAction {
    /// Set up once, start/reuse local runtime :45073 and resolve a dedicated chat
    Connect,
    /// Save tokens once using hidden input and a private local file
    Login,
    /// Remove saved tokens (does not revoke tokens or unset environment variables)
    Logout,
}

pub(super) fn shutdown_signal() -> (
    tokio::sync::watch::Receiver<bool>,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let (tx, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! { result = tokio::signal::ctrl_c() => result?, _ = term.recv() => {} }
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await?;
        let _ = tx.send(true);
        Ok(())
    });
    (rx, task)
}
