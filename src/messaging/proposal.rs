//! Review state is scoped to the requesting person and DM, never an AI claim of consent.
use super::{
    config::Selected,
    runtime::{Proposal, Runtime},
    Destination, Gateway, Incoming, Messenger,
};
use anyhow::{ensure, Result};
use std::time::{Duration, Instant};

pub(super) struct Pending {
    pub sender: String,
    pub destination: Destination,
    pub proposal: Proposal,
    pub expires: Instant,
}

pub(super) fn action(command: &str) -> bool {
    command.split_once(' ').is_some_and(|(verb, token)| {
        matches!(verb, "approve" | "revise" | "cancel") && uuid::Uuid::parse_str(token).is_ok()
    })
}

pub(super) fn summary(proposal: &Proposal) -> Result<String> {
    let d = &proposal.design;
    ensure!(
        d.files.keys().all(|path| path.len() <= 180),
        "Schema paths are too long for Discord review"
    );
    ensure!(
        d.preview.edges.len() <= 100 && d.files.len() <= 30,
        "Proposal is too large for Discord review; ask for a smaller workflow"
    );
    ensure!(
        d.program.len() + d.files.values().map(String::len).sum::<usize>() <= 256 * 1024,
        "Proposal files exceed Discord review limit"
    );
    let mut labels = std::collections::BTreeMap::new();
    for p in &d.preview.ports {
        labels.insert(p.id.clone(), p.name.clone());
    }
    for t in &d.preview.timers {
        labels.insert(t.id.clone(), t.name.clone());
    }
    for r in &d.preview.reactions {
        labels.insert(r.id.clone(), format!("{} [{}]", r.name, r.agent));
    }
    let mut graph = String::new();
    for e in &d.preview.edges {
        graph.push_str(&format!(
            "{} -> {}\n",
            labels.get(&e.source).unwrap_or(&e.source),
            labels.get(&e.target).unwrap_or(&e.target)
        ));
    }
    if graph.is_empty() {
        graph.push_str("No connections\n");
    }
    let agents = d
        .preview
        .agents
        .iter()
        .map(|a| format!("{}: {}", a.name, a.backend))
        .collect::<Vec<_>>()
        .join(", ");
    let proofs = if d.properties.is_empty() {
        "No formal properties were checked.".to_owned()
    } else {
        format!(
            "Formal properties proved: {}/{}.",
            d.properties.iter().filter(|p| p.proved).count(),
            d.properties.len()
        ) + &d
            .properties
            .iter()
            .map(|p| {
                format!(
                    "\n- {}: {}. {}",
                    p.name,
                    if p.proved { "proved" } else { "NOT proved" },
                    p.description
                )
            })
            .collect::<String>()
    };
    let ports = d
        .preview
        .ports
        .iter()
        .map(|p| format!("{}: {}", p.name, p.ty))
        .collect::<Vec<_>>()
        .join("\n");
    let text = format!("Workflow proposal: {}\n{}\n\nAgents: {}\n\nGraph:\n```text\n{}```\nPorts and constraints:\n```text\n{}\n```\nInputs:\n```json\n{}\n```\nCompile check: passed. {}\nReview the source and schema files before approving. Nothing has run yet.",d.preview.team,proposal.summary,agents,graph,ports,serde_json::to_string_pretty(&d.inputs)?,proofs);
    ensure!(
        text.len() <= 24 * 1024,
        "Proposal summary exceeds Discord review limit"
    );
    Ok(text)
}

impl<R: Runtime, M: Messenger> Gateway<R, M> {
    pub(super) async fn review_action(&self, message: &Incoming) {
        let (verb, token) = message.command.split_once(' ').unwrap();
        let d = &message.destination;
        let result = {
            let mut state = self.state.lock().unwrap();
            state.pending.retain(|_, p| p.expires > Instant::now());
            if state.busy {
                Err("OMAR is still working. Wait for it to finish, then try again.")
            } else if state.uncertain {
                Err("A previous run needs checking in OMAR. No new run started.")
            } else if !state.pending.get(token).is_some_and(|p| {
                p.sender == message.sender_id
                    && p.destination.platform == d.platform
                    && p.destination.account_id == d.account_id
                    && p.destination.conversation_id == d.conversation_id
            }) {
                Err("This proposal has expired, changed, or already been handled. Ask for a fresh proposal.")
            } else {
                let pending = state.pending.remove(token).unwrap();
                if verb == "approve" {
                    state.busy = true;
                }
                Ok(pending)
            }
        };
        let pending = match result {
            Ok(pending) => pending,
            Err(text) => {
                self.reply(d, text.into(), None).await;
                return;
            }
        };
        match verb {
            "approve"=>{
                let _busy=super::Busy(&self.state);
                let request=serde_json::json!({"conversation_id":pending.proposal.conversation_id,"proposal_sequence":pending.proposal.sequence});
                self.execute(d,Selected {request:&request,output_port:"",timeout_seconds:300}).await;
            }
            "revise"=>self.reply(d,"Tell me what you want changed. I will show a new proposal for your approval; nothing has run.".into(),None).await,
            _=>self.reply(d,"Proposal cancelled. No workflow was started.".into(),None).await,
        }
    }
    pub(super) async fn present_proposal(&self, message: &Incoming, proposal: Proposal) {
        let token = uuid::Uuid::new_v4().to_string();
        if summary(&proposal).is_err() {
            self.reply(&message.destination,"The proposal is too large to review here. Ask the assistant to simplify it; no run was started.".into(),None).await;
            return;
        }
        self.state.lock().unwrap().pending.insert(
            token.clone(),
            Pending {
                sender: message.sender_id.clone(),
                destination: message.destination.clone(),
                proposal: proposal.clone(),
                expires: Instant::now() + Duration::from_secs(1800),
            },
        );
        if self
            .messenger
            .review(&message.destination, &proposal, &token)
            .await
            .is_err()
        {
            self.state.lock().unwrap().pending.remove(&token);
            self.reply(&message.destination,"Could not deliver the complete proposal for review. Nothing was run; ask for a fresh proposal.".into(),None).await;
        }
    }
}
