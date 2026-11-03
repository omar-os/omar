//! The designer may inspect only chat-owned data through this metered adapter.
//! Native shell/web tools remain outside its coverage and are never reported as free.
use super::*;
use wd::research::{Category, Data, Store};

pub(super) fn ledger(context: &Context_) -> Result<Store> {
    Store::new(
        &crate::ea::ea_state_dir(context.ea_id, &context.omar_dir),
        &context.conversation_id,
    )
}

fn operator_messages(context: &Context_) -> Vec<ChatMessage> {
    context
        .history
        .lock()
        .expect("history poisoned")
        .conversation(&context.conversation_id)
        .messages
        .iter()
        .filter(|m| m.role == ChatRole::Operator)
        .cloned()
        .collect()
}

pub(in crate::serve) fn begin_for_message(
    context: &Context_,
    message: &ChatMessage,
) -> Result<ResearchUsage> {
    anyhow::ensure!(
        message.role == ChatRole::Operator,
        "research requires an operator message"
    );
    let store = ledger(context)?;
    // Closing earlier scopes prevents a delayed native tool call from continuing
    // research under an obsolete request. No new request can reset the old ledger.
    for old in operator_messages(context)
        .into_iter()
        .filter(|m| m.sequence < message.sequence)
    {
        let id = Store::session_id(old.sequence);
        if store.exists(&id)? && store.summary(&id)?.stop_reason.is_none() {
            store.finish(&id, ResearchStop::Superseded, wd::research::now_ms())?;
        }
    }
    store.begin(
        message.sequence,
        &wd::digest(message.text.as_bytes()),
        ResearchLimits::default(),
        wd::research::now_ms(),
    )
}

fn current(context: &Context_) -> Result<Option<ResearchUsage>> {
    operator_messages(context)
        .last()
        .map(|message| begin_for_message(context, message))
        .transpose()
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Lookup {
    OperatorMessage {
        message_sequence: u64,
        #[serde(default)]
        offset: usize,
    },
    PlanInput {
        plan: PlanRef,
        input_port: String,
        #[serde(default)]
        json_pointer: String,
        #[serde(default)]
        offset: usize,
    },
    External {
        location: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    token: String,
    session_id: String,
    request_id: String,
    lookup: Option<Lookup>,
    stop: Option<ResearchStop>,
}

pub(in crate::serve) fn lookup(context: &Context_, body: &[u8]) -> (u16, Value) {
    // Serialize scope selection with operator messages and proposal submission.
    let _operation = context
        .chat_operation
        .lock()
        .expect("chat operation poisoned");
    respond((|| {
        let request: Request = serde_json::from_slice(body)?;
        anyhow::ensure!(
            request.token == *context.agent_token.lock().expect("token poisoned"),
            "forbidden"
        );
        let usage = current(context)?.context("research requires an operator request")?;
        anyhow::ensure!(
            request.session_id == usage.session_id,
            "research scope is no longer the current operator request"
        );
        anyhow::ensure!(
            wd::store::identifier(&request.request_id),
            "invalid research request ID"
        );
        let store = ledger(context)?;
        if let Some(stop) = request.stop {
            anyhow::ensure!(
                request.lookup.is_none()
                    && matches!(
                        stop,
                        ResearchStop::SufficientEvidence
                            | ResearchStop::NeedsUserInput
                            | ResearchStop::MissingCapability
                    ),
                "invalid research finish request"
            );
            return Ok(
                json!({"research":store.finish(&usage.session_id, stop, wd::research::now_ms())?}),
            );
        }
        let lookup = request
            .lookup
            .context("provide one lookup or stop reason")?;
        let fingerprint = wd::digest(&serde_json::to_vec(&lookup)?);
        Ok(json!(store.lookup(&usage.session_id, &request.request_id, &fingerprint, Category::Source, wd::research::now_ms, || {
            match lookup {
                Lookup::OperatorMessage {message_sequence,offset} => {
                    let message = operator_messages(context).into_iter().find(|m|m.sequence==message_sequence).context("unknown operator message in this chat")?;
                    Ok(Data { value:json!(message.text), location:format!("chat:{}/message:{message_sequence}",context.conversation_id),offset })
                }
                Lookup::PlanInput {plan,input_port,json_pointer,offset} => {
                    anyhow::ensure!(json_pointer.len() <= 4000, "research JSON pointer is too long");
                    let plan = super::store(context)?.read(&plan.id,plan.revision)?;
                    let input = plan.inputs.get(&input_port).context("unknown saved workflow input")?;
                    let decoded = if !json_pointer.is_empty() {input.as_str().map(serde_json::from_str::<Value>).transpose()?} else {None};
                    let value = decoded.as_ref().unwrap_or(input).pointer(&json_pointer).context("research JSON pointer does not exist")?;
                    Ok(Data {value:json!(wd::research::content(value)?),location:format!("plan:{}@{}/input:{input_port}{json_pointer}",plan.id,plan.revision),offset})
                }
                Lookup::External {location} => {
                    anyhow::ensure!(!location.trim().is_empty() && location.len()<=4000, "invalid external research location");
                    anyhow::bail!("External retrieval is unsupported by this metered adapter. No network request or charge was made. Ask for a source in the chat or a saved workflow input.")
                }
            }
        })?))
    })())
}

pub(in crate::serve) fn catalog(context: &Context_, body: &[u8], evidence: bool) -> (u16, Value) {
    let _operation = context
        .chat_operation
        .lock()
        .expect("chat operation poisoned");
    respond((|| {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            token: String,
        }
        let request: Request = serde_json::from_slice(body)?;
        anyhow::ensure!(
            request.token == *context.agent_token.lock().expect("token poisoned"),
            "forbidden"
        );
        let usage = current(context)?.context("catalog research requires an operator request")?;
        let id = if evidence {
            "capability-evidence"
        } else {
            "execution-profiles"
        };
        let reply = ledger(context)?.lookup(
            &usage.session_id,
            id,
            id,
            Category::Catalog,
            wd::research::now_ms,
            || {
                let (status, value) = if evidence {
                    super::evidence(context)
                } else {
                    super::profiles(context)
                };
                anyhow::ensure!(status == 200, "catalog could not be loaded: {value}");
                Ok(Data {
                    value,
                    location: format!("configured:{id}"),
                    offset: 0,
                })
            },
        )?;
        Ok(json!(reply))
    })())
}

/// Called only after proposal validation. Model-supplied counters are discarded.
pub(super) fn attach(context: &Context_, draft: &mut PlanDraft) -> Result<()> {
    draft.research.usage = None;
    if let Some(usage) = current(context)? {
        anyhow::ensure!(
            draft
                .research
                .session_id
                .as_ref()
                .is_none_or(|id| *id == usage.session_id),
            "proposal research scope does not match the current operator request"
        );
        let usage = ledger(context)?.finish(
            &usage.session_id,
            ResearchStop::ProposalSubmitted,
            wd::research::now_ms(),
        )?;
        if usage.source_calls == 0 {
            draft.research.gaps.push(
                "No task source was read through the bounded research adapter for this request."
                    .into(),
            );
        }
        draft.research.session_id = Some(usage.session_id.clone());
        draft.research.usage = Some(usage);
    } else {
        anyhow::ensure!(
            draft.research.session_id.is_none(),
            "research scope has no operator request in this chat"
        );
    }
    Ok(())
}

pub(super) fn validate_saved(context: &Context_, draft: &PlanDraft) -> Result<()> {
    match (&draft.research.session_id, &draft.research.usage) {
        (Some(id), Some(usage)) => anyhow::ensure!(
            usage.session_id == *id
                && usage.stop_reason.is_some()
                && ledger(context)?.summary(id)? == *usage,
            "saved research usage differs from the host ledger"
        ),
        (None, None) => {}
        _ => anyhow::bail!("research scope and usage must be bound together"),
    }
    Ok(())
}
