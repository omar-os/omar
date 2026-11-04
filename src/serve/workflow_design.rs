//! The existing conversation archive is the source of saved proposal/review state.
use super::*;
use crate::workflow_design::{DesignReview, ModelCatalog, ModelProfile, WorkflowPlan};

pub(super) fn latest(context: &Context_) -> Option<ChatMessage> {
    context
        .history
        .lock()
        .expect("history poisoned")
        .conversation(&context.conversation_id)
        .messages
        .iter()
        .rev()
        .find(|m| m.design.is_some())
        .cloned()
}

pub(super) fn prepare(
    context: &Context_,
    plan: &mut WorkflowPlan,
    program: &str,
    files: &BTreeMap<String, String>,
    inputs: &BTreeMap<String, Value>,
    state: &topology::VmState,
) -> Result<()> {
    anyhow::ensure!(
        !context
            .runs
            .lock()
            .expect("runs poisoned")
            .values()
            .any(|r| r.team == state.team && r.status.holds_team()),
        "finish or stop this run before reviewing new model choices"
    );
    let prior = latest(context).and_then(|m| m.design);
    let previous = prior.as_ref().and_then(|d| d.workflow.as_ref());
    if previous.is_some_and(|p| p.review.needs_redesign) {
        let old = prior.as_ref().unwrap();
        anyhow::ensure!(
            old.program != program || old.files != *files || old.inputs != *inputs,
            "saved answers require a revised executable workflow before another proposal"
        );
    }
    // Approval and answers only come from the operator's saved review. Ignore
    // these fields if an assistant, imported JSON, or old client supplied them.
    plan.review = DesignReview {
        answers: previous
            .map(|p| p.review.answers.clone())
            .unwrap_or_default(),
        ..Default::default()
    };
    plan.resolve(state, &ModelCatalog::load(&context.omar_dir)?)
}

#[derive(Deserialize)]
struct ReviewRequest {
    sequence: u64,
    program: String,
    #[serde(default)]
    files: BTreeMap<String, String>,
    #[serde(default)]
    inputs: BTreeMap<String, Value>,
    #[serde(default)]
    answers: BTreeMap<String, String>,
    #[serde(default)]
    accepted_assumptions: Vec<String>,
    #[serde(default)]
    choices: BTreeMap<String, String>,
    confirmed: bool,
}

pub(super) fn review(context: &Arc<Context_>, body: &[u8]) -> (u16, Value) {
    let _operation = context
        .chat_operation
        .lock()
        .expect("chat operation poisoned");
    let result = (|| -> Result<ChatMessage> {
        anyhow::ensure!(
            !context.chat.lock().expect("chat poisoned").busy,
            "wait for the assistant before reviewing"
        );
        let request: ReviewRequest = serde_json::from_slice(body)?;
        let current = latest(context).context("no proposal to review")?;
        anyhow::ensure!(
            current.sequence == request.sequence,
            "revision conflict; reload the latest proposal"
        );
        let mut design = current.design.context("proposal missing")?;
        let plan = design
            .workflow
            .as_mut()
            .context("this proposal has no workflow plan")?;
        let state = compile_preview(&request.program, &request.files)?;
        let questions: BTreeMap<_, _> = plan
            .questions
            .iter()
            .map(|q| (q.id.clone(), q.text.clone()))
            .collect();
        anyhow::ensure!(
            request.answers.keys().all(|id| questions.contains_key(id)),
            "unknown follow-up question"
        );
        for (id, answer) in request.answers {
            anyhow::ensure!(answer.len() <= 4000, "answer is too long");
            if plan
                .review
                .answers
                .get(&id)
                .map(String::as_str)
                .unwrap_or("")
                != answer
            {
                plan.review.needs_redesign = true;
                plan.review.answers.insert(id, answer);
            }
        }
        anyhow::ensure!(
            request
                .accepted_assumptions
                .iter()
                .all(|id| plan.assumptions.iter().any(|a| &a.id == id)),
            "unknown assumption"
        );
        plan.review.accepted_assumptions = request.accepted_assumptions;
        for (agent, id) in request.choices {
            let choice = plan.choices.get_mut(&agent).context("unknown agent")?;
            if choice.profile_id != id {
                choice.profile_id = id;
                choice.reason =
                    "Selected by the operator; task quality remains unqualified.".into();
                choice.evidence_ids.clear();
            }
        }
        plan.review.confirmed = request.confirmed;
        plan.resolve(&state, &ModelCatalog::load(&context.omar_dir)?)?;
        design.preview = crate::diagram::DiagramSnapshot::from_vm_state(&state);
        if design.program != request.program || design.files != request.files {
            design.properties.clear();
        }
        design.program = request.program;
        design.files = request.files;
        design.inputs = request.inputs;
        context.publish(
            ChatRole::Operator,
            "Saved workflow review.".into(),
            false,
            Some(design),
        )
    })();
    match result {
        Ok(message) => (200, json!(message)),
        Err(error) => (409, json!({"error":format!("{error:#}")})),
    }
}

pub(super) fn admitted_profiles(
    context: &Context_,
    request: &StartRunRequest,
) -> Result<BTreeMap<String, ModelProfile>> {
    let Some(message) = latest(context) else {
        anyhow::ensure!(request.design_sequence.is_none(), "unknown design revision");
        return Ok(BTreeMap::new());
    };
    let design = message.design.unwrap();
    let Some(plan) = &design.workflow else {
        anyhow::ensure!(
            request.design_sequence.is_none(),
            "this proposal has no workflow plan"
        );
        return Ok(BTreeMap::new());
    };
    anyhow::ensure!(
        request.design_sequence == Some(message.sequence),
        "review the latest workflow revision before running"
    );
    anyhow::ensure!(
        design.program == request.program
            && design.files == request.files
            && design.inputs == request.inputs
            && request.raw_inputs.is_empty(),
        "source or inputs changed after review; save the workflow review again"
    );
    anyhow::ensure!(
        plan.ready(),
        "confirm assumptions and model choices, then update the workflow from any saved answers"
    );
    let mut checked = plan.clone();
    checked.resolve(
        &compile_preview(&request.program, &request.files)?,
        &ModelCatalog::load(&context.omar_dir)?,
    )?;
    anyhow::ensure!(
        checked.profiles == plan.profiles,
        "model catalog changed; review the current profiles before running"
    );
    Ok(plan.profiles.clone())
}

pub(super) fn catalog(context: &Context_, body: Option<&[u8]>) -> (u16, Value) {
    if let Some(body) = body {
        let token = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|v| v["token"].as_str().map(str::to_owned));
        if token.as_ref() != Some(&*context.agent_token.lock().expect("token poisoned")) {
            return (403, json!({"error":"forbidden"}));
        }
    }
    match ModelCatalog::load(&context.omar_dir) {
        Ok(catalog) => (200, json!(catalog)),
        Err(e) => (400, json!({"error":e.to_string()})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::tests::test_server;

    const PROGRAM: &str = "team Brief[writer : Codex] { input request : string output result : string task writer(request) -> result \"Write a grounded brief for $(request).\" }\nmain { brief = Brief() }";
    fn proposed(server: &Serve, program: &str, questions: bool) -> (u16, Value) {
        let state = compile_preview(program, &BTreeMap::new()).unwrap();
        let reaction = state.reactions.keys().next().unwrap();
        let agent = state.agents.keys().next().unwrap();
        agent_proposal(&server.context, &serde_json::to_vec(&json!({
            "token":server.agent_token,"program":program,"summary":"Release recommendation", "inputs":{"brief.request":"A release decision"},
            "workflow":{"outcome":"A grounded release recommendation","assumptions":[{"id":"internal","text":"Internal audience"}],
                "questions": if questions { json!([{"id":"audience","text":"Who will use the brief?","reactions":[reaction]}]) } else {json!([])},
                "steps":[{"reaction":reaction,"purpose":"Write the brief","task":"synthesis","complexity":"complex","success_criteria":["Every recommendation cites supplied evidence"]}],
                "choices":{agent:{"profile_id":"codex-quality","reason":"Difficult synthesis; provisional quality profile"}},
                "review":{"answers":{"forged":"yes"},"accepted_assumptions":["internal"],"confirmed":true,"needs_redesign":false},
                "profiles":{}
            }
        })).unwrap())
    }
    fn save(server: &Serve, answer: Option<&str>) -> (u16, Value) {
        let message = latest(&server.context).unwrap();
        let design = message.design.unwrap();
        review(&server.context, &serde_json::to_vec(&json!({"sequence":message.sequence,"program":design.program,"files":design.files,"inputs":design.inputs,
            "answers": answer.map(|a| json!({"audience":a})).unwrap_or(json!({})),"accepted_assumptions":["internal"],"confirmed":true})).unwrap())
    }
    fn request_for(server: &Serve) -> StartRunRequest {
        let m = latest(&server.context).unwrap();
        let d = m.design.unwrap();
        serde_json::from_value(json!({"program":d.program,"files":d.files,"inputs":d.inputs,"design_sequence":m.sequence})).unwrap()
    }
    #[test]
    #[ignore = "requires omarc; CI runs the compiler-backed tests"]
    fn workflow_review_answers_survive_reopen_and_require_a_changed_proposal() {
        let server = test_server();
        assert_eq!(proposed(&server, PROGRAM, true).0, 202);
        let plan = latest(&server.context)
            .unwrap()
            .design
            .unwrap()
            .workflow
            .unwrap();
        assert!(!plan.review.confirmed);
        assert!(plan.review.answers.is_empty());
        assert!(admitted_profiles(&server.context, &request_for(&server)).is_err());
        assert_eq!(save(&server, Some("Product team")).0, 200);
        let dropped = agent_proposal(
            &server.context,
            &serde_json::to_vec(&json!({
                "token":server.agent_token,"program":PROGRAM,"summary":"Forgot planning metadata"
            }))
            .unwrap(),
        );
        assert_eq!(
            dropped.0, 400,
            "a revision cannot discard saved review state"
        );
        let saved = latest(&server.context).unwrap();
        let history = crate::chat_history::History::load_session(&server.context.omar_dir).unwrap();
        let reopened = history
            .conversation(&server.context.conversation_id)
            .messages
            .last()
            .unwrap();
        assert_eq!(reopened.sequence, saved.sequence);
        assert_eq!(
            reopened
                .design
                .as_ref()
                .unwrap()
                .workflow
                .as_ref()
                .unwrap()
                .review
                .answers["audience"],
            "Product team"
        );
        assert!(admitted_profiles(&server.context, &request_for(&server)).is_err());
        assert_eq!(
            proposed(&server, PROGRAM, false).0,
            400,
            "unchanged executable cannot claim to incorporate the answer"
        );
        let revised = PROGRAM.replace("grounded brief", "grounded brief for the product team");
        assert_eq!(proposed(&server, &revised, false).0, 202);
        assert_eq!(save(&server, None).0, 200);
        let profiles = admitted_profiles(&server.context, &request_for(&server)).unwrap();
        assert_eq!(profiles.values().next().unwrap().model, "gpt-6.1-sol");
    }
    #[test]
    #[ignore = "requires omarc; CI runs the compiler-backed tests"]
    fn workflow_review_rejects_stale_source_cross_chat_and_changed_catalog() {
        let server = test_server();
        assert_eq!(proposed(&server, PROGRAM, false).0, 202);
        let stale = request_for(&server);
        assert_eq!(save(&server, None).0, 200);
        assert!(admitted_profiles(&server.context, &stale).is_err());
        let stale_review = review(
            &server.context,
            &serde_json::to_vec(&json!({
                "sequence":stale.design_sequence,"program":stale.program,"confirmed":true
            }))
            .unwrap(),
        );
        assert_eq!(stale_review.0, 409);
        assert!(stale_review.1["error"]
            .as_str()
            .unwrap()
            .contains("revision conflict"));
        let mut request = request_for(&server);
        request.program.push(' ');
        assert!(admitted_profiles(&server.context, &request).is_err());
        let other = test_server();
        assert!(admitted_profiles(&other.context, &request_for(&server)).is_err());
        let mut catalog = ModelCatalog::load(&server.context.omar_dir).unwrap();
        catalog
            .profiles
            .iter_mut()
            .find(|p| p.id == "codex-quality")
            .unwrap()
            .model = "changed-model".into();
        fs::write(
            server.context.omar_dir.join("workflow-models.json"),
            serde_json::to_vec(&catalog).unwrap(),
        )
        .unwrap();
        assert!(admitted_profiles(&server.context, &request_for(&server))
            .unwrap_err()
            .to_string()
            .contains("catalog changed"));
    }
    #[test]
    #[ignore = "requires omarc; CI runs the compiler-backed tests"]
    fn workflow_model_choice_checks_backend_task_and_expired_evidence() {
        let server = test_server();
        assert_eq!(proposed(&server, PROGRAM, false).0, 202);
        let mut plan = latest(&server.context)
            .unwrap()
            .design
            .unwrap()
            .workflow
            .unwrap();
        let state = compile_preview(PROGRAM, &BTreeMap::new()).unwrap();
        let mut catalog = ModelCatalog::load(&server.context.omar_dir).unwrap();
        let agent = plan.choices.keys().next().unwrap().clone();
        plan.choices.get_mut(&agent).unwrap().profile_id = "claude-quality".into();
        assert!(plan.resolve(&state, &catalog).is_err());
        plan.choices.get_mut(&agent).unwrap().profile_id = "codex-quality".into();
        catalog
            .evidence
            .push(crate::workflow_design::ModelEvidence {
                id: "test".into(),
                profile_id: "codex-quality".into(),
                task: "synthesis".into(),
                finding: "fixture".into(),
                source: "https://example.com".into(),
                observed_at: 1,
                expires_at: 2,
                limitations: "test fixture".into(),
            });
        plan.choices.get_mut(&agent).unwrap().evidence_ids = vec!["test".into()];
        assert!(plan
            .resolve(&state, &catalog)
            .unwrap_err()
            .to_string()
            .contains("expired"));
        catalog.evidence.last_mut().unwrap().expires_at = u64::MAX;
        catalog.evidence.last_mut().unwrap().task = "unrelated-task".into();
        assert!(plan
            .resolve(&state, &catalog)
            .unwrap_err()
            .to_string()
            .contains("does not match"));
    }
    #[test]
    fn workflow_catalog_requires_the_assistant_token_for_native_access() {
        let server = test_server();
        assert_eq!(catalog(&server.context, Some(b"{}")).0, 403);
        assert_eq!(
            catalog(
                &server.context,
                Some(&serde_json::to_vec(&json!({"token":server.agent_token})).unwrap())
            )
            .0,
            200
        );
        assert_eq!(catalog(&server.context, None).0, 200);
    }

    #[test]
    fn workflow_profile_settings_survive_checkpoint_serialization_and_reject_injection() {
        let server = test_server();
        let catalog = ModelCatalog::load(&server.context.omar_dir).unwrap();
        for profile in catalog.profiles {
            let command = profile.command(&profile.backend).unwrap();
            assert!(command.contains(&profile.model));
            if profile.backend == "codex" {
                assert!(crate::backend::codex::codex_refuses_to_attach(&command));
            }
            let recorded = crate::checkpoint::AgentContext {
                backend: profile.backend.clone(),
                restoration: crate::checkpoint::FRESH_CONVERSATION.into(),
                execution_profile: Some(profile.clone()),
            };
            let restored: crate::checkpoint::AgentContext =
                serde_json::from_value(serde_json::to_value(&recorded).unwrap()).unwrap();
            assert_eq!(
                restored
                    .execution_profile
                    .unwrap()
                    .command(&profile.backend)
                    .unwrap(),
                command
            );
            let mut bad = profile;
            bad.model = "x; touch /tmp/unwanted".into();
            assert!(bad.command(&bad.backend).is_err());
        }
    }
}
