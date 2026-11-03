use super::*;
use crate::workflow_design::{self as wd, *};
pub(super) mod access_evidence;
#[cfg(test)]
mod benchmark_tests;
#[cfg(test)]
mod capabilities_tests;
#[cfg(test)]
mod contracts_tests;
#[cfg(test)]
mod profile_contracts_tests;
mod provenance;
#[cfg(test)]
mod provenance_tests;
#[cfg(test)]
mod repository_tests;
pub(super) mod research;
#[cfg(test)]
mod research_tests;

fn store(context: &Context_) -> Result<wd::store::Store> {
    wd::store::Store::new(
        &crate::ea::ea_state_dir(context.ea_id, &context.omar_dir),
        &context.conversation_id,
    )
}

pub(super) fn review_context(context: &Context_, reference: &PlanRef) -> Result<String> {
    let store = store(context)?;
    store.read(&reference.id, reference.revision)?;
    anyhow::ensure!(
        store
            .latest(&reference.id)?
            .is_some_and(|p| p.revision == reference.revision),
        "revision conflict: reload the saved review"
    );
    Ok(format!("\n\nWorkflow review context: read plan {}, revision {} with omar_get_workflow_plan. Incorporate its saved answers and propose updated source. Include revises={{id:{:?},revision:{}}} in the new plan.",reference.id,reference.revision,reference.id,reference.revision))
}

fn respond(result: Result<Value>) -> (u16, Value) {
    match result {
        Ok(value) => (200, value),
        Err(error) => {
            let message = format!("{error:#}");
            (
                if message.contains("conflict") {
                    409
                } else {
                    400
                },
                json!({"error":message}),
            )
        }
    }
}

pub(super) fn profiles(context: &Context_) -> (u16, Value) {
    respond(
        wd::profiles::Registry::load(&context.omar_dir)
            .map(|r| json!({"profiles":r.profiles,"revision":r.revision()})),
    )
}

pub(super) fn evidence(context: &Context_) -> (u16, Value) {
    respond(wd::profiles::Registry::load(&context.omar_dir).and_then(|r| Ok(json!({"repository_verifiers":wd::repository::catalog(&context.omar_dir)?,"evidence":r.evidence,"assessments":r.evidence.iter().filter_map(|e| r.profiles.iter().find(|p| p.id==e.profile_id).map(|p| (e.id.clone(),wd::benchmark::assess(e,p,wd::now())))).collect::<BTreeMap<_,_>>(),"revision":r.revision(),"policy_revision":wd::profiles::ROUTING_POLICY_REVISION,"runtime_contract":wd::profiles::RUNTIME_CONTRACT_REVISION,"profile_digests":r.profiles.iter().map(|p| (p.id.clone(), p.configuration_digest())).collect::<BTreeMap<_,_>>(),"policy":"Automatic economy routing requires a routine, low-consequence task and reviewed held-out qualification matching its scope, modalities, exact profile settings and installed CLI version. Provider claims only shortlist candidates."}))))
}

fn qualification_runtime_versions(registry: &wd::profiles::Registry) -> BTreeMap<String, String> {
    // Installation/version only. This never probes credentials or tool access.
    // Connectivity observations also require the exact observed runtime version.
    registry
        .profiles
        .iter()
        .filter(|p| {
            p.access_observation.is_some()
                || p.tool_capabilities.iter().any(|t| t.connectivity.is_some())
                || registry
                    .evidence
                    .iter()
                    .any(|e| e.profile_id == p.id && e.locally_qualified)
        })
        .map(|p| p.backend.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter_map(|backend| {
            crate::backend_probe::backend_version(&backend).map(|version| (backend, version))
        })
        .collect()
}

fn resolve(context: &Context_, plan: &mut WorkflowPlanRevision, state: &VmState) -> Result<()> {
    wd::contracts::prepare(&mut plan.draft, None, state, &plan.inputs)?;
    provenance::validate(context, &mut plan.draft, &plan.inputs)?;
    let registry = wd::profiles::Registry::load(&context.omar_dir)?;
    for id in &plan.draft.research.capability_evidence_ids {
        anyhow::ensure!(
            registry.evidence.iter().any(|e| &e.id == id),
            "unknown capability evidence ID: {id}"
        );
    }
    {
        let history = context.history.lock().expect("history poisoned");
        let conversation = history.conversation(&context.conversation_id);
        for sequence in &plan.draft.intent.message_sequences {
            anyhow::ensure!(
                conversation
                    .messages
                    .iter()
                    .any(|m| m.sequence == *sequence && m.role == ChatRole::Operator),
                "intent refers to an unknown operator message"
            );
        }
    }
    let backends = state
        .agents
        .iter()
        .map(|(id, a)| {
            (
                id.clone(),
                topology::canonical_backend(&a.backend).to_string(),
            )
        })
        .collect();
    plan.step_contracts = wd::validation::step_contract_digests(&plan.draft, state)?;
    (plan.selections, plan.profiles) = registry.select(
        &plan.draft.steps,
        &plan.draft.preferences,
        &backends,
        &plan.step_contracts,
        &qualification_runtime_versions(&registry),
        wd::now(),
    )?;
    plan.evidence_revision = registry.revision();
    plan.routing_policy_revision = wd::profiles::ROUTING_POLICY_REVISION.into();
    wd::repository::resolve(&context.omar_dir, plan, state)?;
    wd::validation::validate_plan(plan, state, false)
}

pub(super) fn create(
    context: &Context_,
    program: &str,
    inputs: &BTreeMap<String, Value>,
    mut draft: PlanDraft,
    state: &VmState,
) -> Result<WorkflowPlanRevision> {
    let previous = draft
        .revises
        .as_ref()
        .map(|reference| store(context)?.read(&reference.id, reference.revision))
        .transpose()?;
    provenance::prepare(&mut draft, previous.as_ref())?;
    if let Some(previous) = &previous {
        // A regenerated proposal cannot silently discard saved operator
        // choices. A renamed/incompatible agent is reported by the router.
        for (agent, profile) in &previous.draft.preferences.overrides {
            draft
                .preferences
                .overrides
                .insert(agent.clone(), profile.clone());
        }
        if !previous.draft.preferences.allowed_backends.is_empty() {
            draft.preferences.allowed_backends =
                previous.draft.preferences.allowed_backends.clone();
        }
    }
    wd::contracts::prepare(&mut draft, previous.as_ref(), state, inputs)?;
    let mut plan = WorkflowPlanRevision {
        schema_version: 1,
        id: previous
            .as_ref()
            .map_or_else(|| Uuid::new_v4().to_string(), |p| p.id.clone()),
        revision: 0,
        conversation_id: context.conversation_id.clone(),
        created_at: 0,
        request_id: Uuid::new_v4().to_string(),
        program: program.into(),
        source_digest: wd::digest(program.as_bytes()),
        step_contracts: BTreeMap::new(),
        inputs: inputs.clone(),
        draft,
        selections: vec![],
        profiles: BTreeMap::new(),
        evidence_revision: String::new(),
        routing_policy_revision: String::new(),
    };
    resolve(context, &mut plan, state)?;
    research::attach(context, &mut plan.draft)?;
    store(context)?.save(plan, previous.as_ref().map_or(0, |p| p.revision))
}

pub(super) fn read(context: &Context_, body: &[u8]) -> (u16, Value) {
    respond((|| {
        #[derive(Deserialize)]
        struct Request {
            id: String,
            revision: Option<u32>,
        }
        let request: Request = serde_json::from_slice(body)?;
        let store = store(context)?;
        let plan = match request.revision {
            Some(rev) => store.read(&request.id, rev)?,
            None => store
                .latest(&request.id)?
                .context("plan not found in this chat")?,
        };
        Ok(json!(plan))
    })())
}

pub(super) fn export(context: &Context_, body: &[u8]) -> (u16, Value) {
    respond((|| {
        let reference: PlanRef = serde_json::from_slice(body)?;
        let plan = store(context)?.read(&reference.id, reference.revision)?;
        // Do not include chat ownership or message IDs in a portable file.
        let mut draft = plan.draft;
        draft.revises = None;
        draft.intent.message_sequences.clear();
        provenance::detach_chat(&mut draft);
        let mut profiles = plan.profiles;
        for profile in profiles.values_mut() {
            profile.access = ProfileAccess::Unknown;
            profile.access_observation = None;
            profile.advertised_limits = None;
            profile.economics = None;
            for tool in &mut profile.tool_capabilities {
                tool.connectivity = None;
            }
        }
        Ok(json!(WorkflowBundle {
            schema_version: 1,
            program: plan.program,
            source_digest: plan.source_digest,
            inputs: plan.inputs,
            draft,
            profiles,
            evidence_revision: plan.evidence_revision,
            routing_policy_revision: plan.routing_policy_revision,
        }))
    })())
}

pub(super) fn import(context: &Arc<Context_>, body: &[u8]) -> (u16, Value) {
    let _operation = context
        .chat_operation
        .lock()
        .expect("chat operation poisoned");
    respond((|| {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            request_id: String,
            bundle: WorkflowBundle,
        }
        let request: Request = serde_json::from_slice(body)?;
        anyhow::ensure!(
            wd::store::identifier(&request.request_id),
            "invalid import request ID"
        );
        let bundle = request.bundle;
        anyhow::ensure!(
            bundle.schema_version == 1,
            "unsupported workflow bundle version"
        );
        anyhow::ensure!(
            wd::digest(bundle.program.as_bytes()) == bundle.source_digest,
            "workflow bundle source digest mismatch"
        );
        let state = compile_preview(context, &bundle.program)?;
        validate_task_metadata(&state).map_err(anyhow::Error::msg)?;
        encode_inputs(&state, &bundle.inputs)?;
        let registry = wd::profiles::Registry::load(&context.omar_dir)?;
        let mut draft = bundle.draft;
        draft.revises = None;
        draft.intent.message_sequences.clear();
        provenance::detach_chat(&mut draft);
        for assumption in &mut draft.intent.assumptions {
            assumption.accepted = false;
        }
        for (agent, requested) in &bundle.profiles {
            let current = registry
                .profiles
                .iter()
                .find(|p| p.id == requested.id)
                .with_context(|| {
                    format!(
                        "missing imported profile '{}'; configure it before importing",
                        requested.id
                    )
                })?;
            // Access evidence and labels belong to the destination account.
            // Exact model/settings/tool bindings must remain unchanged.
            anyhow::ensure!(requested.configuration_digest() == current.configuration_digest(), "imported profile '{}' has different model/settings/version; review the destination registry", requested.id);
            anyhow::ensure!(
                current.enabled && current.access != ProfileAccess::Unavailable,
                "imported profile '{}' is unavailable",
                requested.id
            );
            draft
                .preferences
                .overrides
                .insert(agent.clone(), requested.id.clone());
        }
        let unavailable: Vec<_> = draft
            .research
            .capability_evidence_ids
            .iter()
            .filter(|id| !registry.evidence.iter().any(|e| &e.id == *id))
            .cloned()
            .collect();
        draft
            .research
            .capability_evidence_ids
            .retain(|id| !unavailable.contains(id));
        if !unavailable.is_empty() {
            draft.research.gaps.push(format!("Imported capability evidence is unavailable in this registry: {}. Routing was re-evaluated locally.", unavailable.join(", ")));
        }
        let mut plan = WorkflowPlanRevision {
            schema_version: 1,
            id: format!("import-{}", wd::digest(request.request_id.as_bytes())),
            revision: 0,
            conversation_id: context.conversation_id.clone(),
            created_at: 0,
            request_id: request.request_id,
            program: bundle.program,
            source_digest: bundle.source_digest,
            step_contracts: BTreeMap::new(),
            inputs: bundle.inputs,
            draft,
            selections: vec![],
            profiles: BTreeMap::new(),
            evidence_revision: String::new(),
            routing_policy_revision: String::new(),
        };
        resolve(context, &mut plan, &state)?;
        for (agent, requested) in bundle.profiles {
            anyhow::ensure!(
                plan.profiles
                    .get(&agent)
                    .is_some_and(|p| p.id == requested.id && p.version == requested.version),
                "imported binding for '{agent}' is incompatible with the destination workflow"
            );
        }
        let saved = store(context)?.save(plan, 0)?;
        let reference = PlanRef {
            id: saved.id.clone(),
            revision: saved.revision,
        };
        let published = context
            .history
            .lock()
            .expect("history poisoned")
            .conversation(&context.conversation_id)
            .messages
            .iter()
            .any(|m| m.design.as_ref().and_then(|d| d.plan.as_ref()) == Some(&reference));
        if !published {
            context.publish(
                ChatRole::Assistant,
                "Imported workflow. Review its assumptions and model choices before running."
                    .into(),
                false,
                Some(ProposedDesign {
                    plan: Some(reference),
                    program: saved.program.clone(),
                    inputs: saved.inputs.clone(),
                    preview: crate::diagram::DiagramSnapshot::from_vm_state(&state),
                }),
            )?;
        }
        Ok(json!(saved))
    })())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveRequest {
    id: String,
    expected_revision: u32,
    request_id: String,
    program: String,
    inputs: BTreeMap<String, Value>,
    #[serde(default)]
    answers: BTreeMap<String, String>,
    #[serde(default)]
    accepted_assumptions: Vec<String>,
    #[serde(default)]
    overrides: BTreeMap<String, String>,
}

pub(super) fn save(context: &Arc<Context_>, body: &[u8]) -> (u16, Value) {
    let _operation = context
        .chat_operation
        .lock()
        .expect("chat operation poisoned");
    respond((|| {
        let request: SaveRequest = serde_json::from_slice(body)?;
        let store = store(context)?;
        let mut plan = store.read(&request.id, request.expected_revision)?;
        let previous_source = plan.program.clone();
        plan.program = request.program;
        plan.inputs = request.inputs;
        plan.request_id = request.request_id;
        plan.source_digest = wd::digest(plan.program.as_bytes());
        for (id, answer) in request.answers {
            let question = plan
                .draft
                .questions
                .iter_mut()
                .find(|q| q.id == id)
                .context("unknown question")?;
            anyhow::ensure!(
                !answer.trim().is_empty() && answer.len() <= 4000,
                "invalid answer"
            );
            if question.answer.as_ref() != Some(&answer) || question.answer_record.is_none() {
                question.answer_record = Some(PlanRef {
                    id: plan.id.clone(),
                    revision: request
                        .expected_revision
                        .checked_add(1)
                        .context("revision overflow")?,
                });
            }
            question.answer = Some(answer);
        }
        for id in &request.accepted_assumptions {
            anyhow::ensure!(
                plan.draft.intent.assumptions.iter().any(|a| &a.id == id),
                "unknown assumption"
            );
        }
        let accepted_assumptions = request.accepted_assumptions;
        plan.draft.preferences.overrides = request.overrides;
        let state = compile_preview(context, &plan.program)?;
        if previous_source != plan.program {
            wd::contracts::rebind(&mut plan.draft, &state)?;
        }
        resolve(context, &mut plan, &state)?;
        provenance::confirm_assumptions(
            &mut plan,
            &accepted_assumptions,
            request.expected_revision,
        )?;
        if previous_source != plan.program {
            validate_task_metadata(&state).map_err(anyhow::Error::msg)?;
        }
        encode_inputs(&state, &plan.inputs)?;
        let saved = store.save(plan, request.expected_revision)?;
        Ok(json!(saved))
    })())
}

pub(super) fn admit(
    context: &Context_,
    request: &StartRunRequest,
    state: &VmState,
    run_id: &str,
    run_dir: &Path,
) -> Result<(
    BTreeMap<String, ExecutionProfile>,
    BTreeMap<String, Vec<OutputCheck>>,
    wd::recovery::RuntimePolicies,
)> {
    let mut profiles = BTreeMap::new();
    let mut checks = BTreeMap::new();
    let mut step_policies = BTreeMap::new();
    let mut connection_scope = None;
    if let Some(reference) = &request.plan {
        let store = store(context)?;
        let plan = store.read(&reference.id, reference.revision)?;
        anyhow::ensure!(
            store
                .latest(&reference.id)?
                .is_some_and(|p| p.revision == reference.revision),
            "revision conflict: review the latest workflow revision"
        );
        anyhow::ensure!(
            plan.program == request.program && plan.inputs == request.inputs,
            "revision conflict: save the edited source and inputs before deployment"
        );
        wd::validation::validate_plan(&plan, state, true)?;
        wd::contracts::validate_saved(&plan, state)?;
        provenance::validate_saved(context, &plan)?;
        step_policies = wd::recovery::compile(&plan.draft, state, &plan.inputs)?;
        anyhow::ensure!(
            plan.routing_policy_revision == wd::profiles::ROUTING_POLICY_REVISION,
            "revision conflict: routing policy changed; save and review a new plan revision"
        );
        let current = wd::profiles::Registry::load(&context.omar_dir)?;
        connection_scope = current.connection_scope.clone();
        anyhow::ensure!(
            plan.step_contracts == wd::validation::step_contract_digests(&plan.draft, state)?,
            "revision conflict: compiled task contracts changed; save and review the workflow"
        );
        let backends = state
            .agents
            .iter()
            .map(|(id, a)| {
                (
                    id.clone(),
                    topology::canonical_backend(&a.backend).to_owned(),
                )
            })
            .collect();
        let (decisions, bindings) = current.select(
            &plan.draft.steps,
            &plan.draft.preferences,
            &backends,
            &plan.step_contracts,
            &qualification_runtime_versions(&current),
            wd::now(),
        )?;
        anyhow::ensure!(
            current.revision() == plan.evidence_revision && decisions == plan.selections && bindings == plan.profiles,
            "revision conflict: routing evidence changed or expired; save and review the current selection"
        );
        wd::capabilities::require_launchable(&decisions)?;
        for p in plan.profiles.values() {
            anyhow::ensure!(
                current.profiles.contains(p),
                "execution profile changed; review a new plan revision"
            );
        }
        profiles = plan.profiles.clone();
        checks = plan
            .draft
            .steps
            .iter()
            .map(|s| (s.reaction.clone(), s.output_checks.clone()))
            .collect();
        let suites = wd::repository::resolve(&context.omar_dir, &plan, state)?;
        wd::repository::stage(run_dir, &suites)?;
        topology::write_json_atomic(&run_dir.join("plan.json"), &plan)?;
    }
    let backend_versions = profiles
        .values()
        .map(|p| p.backend.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|backend| {
            let version = crate::backend_probe::backend_version(&backend);
            (backend, version)
        })
        .collect();
    let receipt = ExecutionReceipt {
        schema_version: 1,
        run_id: run_id.into(),
        plan: request.plan.clone(),
        source_digest: wd::digest(request.program.as_bytes()),
        profiles: profiles.clone(),
        connection_scope,
        runtime_contract: wd::profiles::RUNTIME_CONTRACT_REVISION.into(),
        started_at: wd::now(),
        finished_at: None,
        status: "starting".into(),
        error: None,
        verification: vec![],
        resources: vec![],
        repository_checks: vec![],
        observed_models: profiles.keys().map(|id| (id.clone(), None)).collect(),
        observed_efforts: profiles.keys().map(|id| (id.clone(), None)).collect(),
        backend_versions,
        input_tokens: None,
        output_tokens: None,
        cost_usd: None,
    };
    topology::write_json_atomic(&run_dir.join("execution-receipt.json"), &receipt)?;
    Ok((profiles, checks, step_policies))
}

pub(super) fn finish(context: &Context_, record: &RunRecord) -> Result<()> {
    let path = crate::ea::ea_state_dir(context.ea_id, &context.omar_dir)
        .join("serve")
        .join(&record.run_id)
        .join("execution-receipt.json");
    let mut receipt: ExecutionReceipt = serde_json::from_slice(&fs::read(&path)?)?;
    receipt.finished_at = record.finished_at;
    receipt.status = serde_json::to_value(record.status)?
        .as_str()
        .unwrap_or("unknown")
        .into();
    receipt.error = record.error.clone();
    // Providers without native usage leave totals unavailable. A partial
    // total must never masquerade as the cost of the complete workflow.
    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    let invocations = path
        .parent()
        .context("missing run directory")?
        .join("invocations");
    let mut expected: BTreeMap<String, usize> = BTreeMap::new();
    receipt.verification.clear();
    receipt.resources.clear();
    receipt.repository_checks.clear();
    if invocations.exists() {
        for entry in fs::read_dir(&invocations)? {
            let file = entry?.path();
            if file.extension().is_some_and(|e| e == "json") {
                let record: Value = serde_json::from_slice(&fs::read(file)?)?;
                if let Some(checks) = record.get("repository_checks") {
                    receipt.repository_checks.extend(serde_json::from_value::<
                        Vec<RepositoryCheckReceipt>,
                    >(checks.clone())?);
                }
                if let Some(resources) = record.get("resources").filter(|r| !r.is_null()) {
                    receipt
                        .resources
                        .push(serde_json::from_value(resources.clone())?);
                }
                if let Some(event) = record.get("verification").filter(|e| !e.is_null()) {
                    receipt
                        .verification
                        .push(serde_json::from_value(event.clone())?);
                }
                if let Some(agent) = record["agent"]
                    .as_str()
                    .filter(|_| record["invoked"].as_bool() != Some(false))
                {
                    *expected.entry(agent.into()).or_default() += 1;
                }
            }
        }
    }
    receipt
        .verification
        .sort_by(|a, b| (&a.step, a.stage).cmp(&(&b.step, b.stage)));
    receipt.resources.sort_by(|a, b| {
        (&a.logical_step_id, &a.invocation_id).cmp(&(&b.logical_step_id, &b.invocation_id))
    });
    let mut complete = !receipt.profiles.is_empty() && !expected.is_empty();
    for agent in receipt.profiles.keys() {
        let usage = path
            .parent()
            .context("missing run directory")?
            .join("invocations")
            .join("usage")
            .join(format!("{agent}.json"));
        let turns: Vec<Value> = match fs::read(usage)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            Some(turns) => turns,
            None => {
                if expected.get(agent).copied().unwrap_or(0) > 0 {
                    complete = false;
                }
                continue;
            }
        };
        if turns.len() < expected.get(agent).copied().unwrap_or(0) {
            complete = false;
        }
        for turn in turns {
            match (
                turn["usage"]["input_tokens"].as_u64(),
                turn["usage"]["output_tokens"].as_u64(),
            ) {
                (Some(i), Some(o)) => {
                    input_tokens = input_tokens.saturating_add(i);
                    output_tokens = output_tokens.saturating_add(o);
                }
                _ => complete = false,
            }
            if let Some(model) = turn["model"].as_str() {
                receipt
                    .observed_models
                    .insert(agent.clone(), Some(model.into()));
            }
            if let Some(effort) = turn["effort"].as_str() {
                receipt
                    .observed_efforts
                    .insert(agent.clone(), Some(effort.into()));
            }
        }
    }
    if complete {
        receipt.input_tokens = Some(input_tokens);
        receipt.output_tokens = Some(output_tokens);
    } else {
        receipt.input_tokens = None;
        receipt.output_tokens = None;
    }
    topology::write_json_atomic(&path, &receipt)
}

pub(super) fn receipt(context: &Context_, body: &[u8]) -> (u16, Value) {
    respond((|| {
        #[derive(Deserialize)]
        struct Request {
            run_id: String,
        }
        let request: Request = serde_json::from_slice(body)?;
        anyhow::ensure!(wd::store::identifier(&request.run_id), "invalid run ID");
        let root = crate::ea::ea_state_dir(context.ea_id, &context.omar_dir)
            .join("serve")
            .join(&request.run_id);
        let receipt: ExecutionReceipt =
            serde_json::from_slice(&fs::read(root.join("execution-receipt.json"))?)?;
        if let Some(plan) = &receipt.plan {
            store(context)?.read(&plan.id, plan.revision)?;
        } else {
            anyhow::ensure!(
                context
                    .runs
                    .lock()
                    .expect("runs poisoned")
                    .contains_key(&request.run_id),
                "run not found in this chat"
            );
        }
        Ok(json!(receipt))
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    const PROGRAM: &str = r#"team Review[writer: Web, checker: Web] {
      input request : string
      action raw : string
      output result : string
      prompt writer(request) -> raw task("Draft a grounded brief", "Draft using supplied evidence.") "Draft $(request)."
      prompt checker(raw) -> result task("Verify the brief", "Check support before accepting the result.") "Verify $(raw)."
    }
    main Review { flow = Review() }"#;

    fn draft() -> PlanDraft {
        serde_json::from_value(json!({
            "intent":{"outcome":"A grounded brief","acceptance_criteria":["Supported claims"],"assumptions":[{"id":"internal","text":"Internal audience","provenance":"designer inference","accepted":false}]},
            "research":{"summary":"Supplied context","sources":[]},
            "steps":[
                {"reaction":"flow.reaction.0","agent":"flow.writer","operation":"evidence_synthesis","purpose":"Draft","decomposition_reason":"Draft the brief before independent verification.","acceptance_criteria":["Supported claims"],"verification_reaction":"flow.reaction.1"},
                {"reaction":"flow.reaction.1","agent":"flow.checker","operation":"claim_verification","purpose":"Verify","decomposition_reason":"Check factual support before delivering the brief.","acceptance_criteria":["Check support"]}
            ],
            "questions":[{"id":"audience","question":"Who reads it?","reason":"Changes detail","required":true}]
        })).unwrap()
    }

    #[test]
    fn recovery_admission_binds_finite_policies_to_the_compiled_plan() {
        let server = super::super::tests::test_server();
        let program = include_str!("../../tests/topology/src/ValidatedHandoff.omar");
        let state = compile_preview(&server.context, program).unwrap();
        let (draft, _, inputs) = wd::recovery_tests::fixture(false);
        let plan = create(&server.context, program, &inputs, draft, &state).unwrap();
        let request: StartRunRequest = serde_json::from_value(json!({
            "program":program,"inputs":inputs,"plan":{"id":plan.id,"revision":plan.revision}
        }))
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let (_, checks, policies) = admit(
            &server.context,
            &request,
            &state,
            "recovery-admission",
            temp.path(),
        )
        .unwrap();
        assert_eq!(policies.len(), 4);
        assert_eq!(policies["flow.reaction.2"].execution.max_invocations, 1);
        assert_eq!(
            policies["flow.reaction.1"].gate.as_ref().unwrap().rejection,
            VerificationDecision::RepairRequested
        );
        assert_eq!(
            policies["flow.reaction.3"].gate.as_ref().unwrap().rejection,
            VerificationDecision::Blocked
        );
        assert_eq!(checks["flow.reaction.1"], checks["flow.reaction.3"]);
    }

    #[test]
    fn recovery_http_run_records_one_repair_and_terminal_block_without_models() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.agent.default_workdir = source.path().display().to_string();
        config.dashboard.session_prefix = format!("omar-recovery-{}", Uuid::new_v4());
        let server = Serve::start("127.0.0.1:0".parse().unwrap(), &config, root.path(), 0).unwrap();
        let program = include_str!("../../tests/topology/src/ValidatedHandoff.omar");
        let state = compile_preview(&server.context, program).unwrap();
        let (mut draft, _, inputs) = wd::recovery_tests::fixture(false);
        for step in &mut draft.steps {
            step.execution.deadline_ms = Some(10_000);
        }
        let plan = create(&server.context, program, &inputs, draft, &state).unwrap();
        let http = |method: &str, path: &str, body: Option<Value>| {
            let body = body.map(|value| value.to_string());
            let response =
                super::super::tests::request(server.address(), method, path, body.as_deref());
            let (headers, body) = response.split_once("\r\n\r\n").unwrap();
            let status = headers
                .split_whitespace()
                .nth(1)
                .unwrap()
                .parse::<u16>()
                .unwrap();
            (status, serde_json::from_str::<Value>(body).unwrap())
        };
        let (status, started) = http(
            "POST",
            "/v1/runs",
            Some(json!({
                "program":program,"inputs":inputs,"plan":{"id":plan.id,"revision":plan.revision},
                "fast":true,"timeout_seconds":10
            })),
        );
        assert_eq!(status, 201, "{started}");
        let run_id = started["run_id"].as_str().unwrap();
        let panel_path = format!("/v1/runs/{run_id}/panel");
        let mut answered = BTreeSet::new();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(
                Instant::now() < deadline,
                "Web-only recovery run did not finish"
            );
            let (_, record) = http("GET", &format!("/v1/runs/{run_id}"), None);
            if matches!(
                record["status"].as_str(),
                Some("completed" | "failed" | "stopped")
            ) {
                assert_eq!(record["status"], "completed", "{record}");
                break;
            }
            let (status, panel) = http("GET", &panel_path, None);
            if status == 200 {
                for pending in panel["pending"].as_array().unwrap() {
                    let id = pending["invocation_id"].as_str().unwrap();
                    if !answered.insert(id.to_owned()) {
                        continue;
                    }
                    let (port, value) = match pending["reaction"].as_str().unwrap() {
                        "flow.reaction.0" => ("flow.raw", "draft"),
                        "flow.reaction.1" => ("flow.result", "{\"wrong\":true}"),
                        "flow.reaction.2" => {
                            assert!(pending["trigger_values"]["flow.retry"]
                                .as_str()
                                .unwrap()
                                .contains("reason"));
                            ("flow.repaired", "{\"value\":\"repaired\"}")
                        }
                        "flow.reaction.3" => ("flow.result", "{\"still_wrong\":true}"),
                        other => panic!("unexpected recovery reaction {other}"),
                    };
                    let (status, response) = http(
                        "POST",
                        &panel_path,
                        Some(json!({
                            "invocation_id":id,"agent":pending["agent"],"values":{port:value}
                        })),
                    );
                    assert_eq!(status, 202, "{response}");
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(answered.len(), 4, "exactly one draft/check/repair/recheck");
        let (status, receipt) = http(
            "POST",
            "/v1/workflow/receipts",
            Some(json!({"run_id":run_id})),
        );
        assert_eq!(status, 200, "{receipt}");
        assert_eq!(receipt["plan"]["id"], plan.id);
        assert_eq!(receipt["verification"][0]["decision"], "repair_requested");
        assert_eq!(receipt["verification"][1]["decision"], "blocked");
        assert_eq!(receipt["verification"].as_array().unwrap().len(), 2);
        assert_eq!(receipt["resources"].as_array().unwrap().len(), 4);
        assert!(receipt["resources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|usage| usage["logical_step_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("step-"))
                && usage["input_bytes"].as_u64().unwrap() > 0));
        assert!(receipt["input_tokens"].is_null());
        assert!(receipt["cost_usd"].is_null());
        let runtime_dir = crate::deploy::dir_for(root.path(), 0, &state.team);
        let outputs: Value =
            serde_json::from_slice(&fs::read(crate::deploy::outputs_path(&runtime_dir)).unwrap())
                .unwrap();
        assert!(
            outputs.get("flow.result").is_none(),
            "rejected output was delivered"
        );
        assert!(outputs["flow.blocked"].as_str().unwrap().contains("reason"));
        let (status, _) = http("GET", &panel_path, None);
        assert_eq!(status, 404, "terminal runs must not offer more work");
    }

    #[test]
    fn older_policy_revisions_remain_readable_but_require_a_new_review_before_running() {
        let server = super::super::tests::test_server();
        let context = &server.context;
        let state = compile_preview(context, PROGRAM).unwrap();
        let mut spec = draft();
        spec.questions.clear();
        spec.intent.assumptions.clear();
        let first = create(context, PROGRAM, &BTreeMap::new(), spec, &state).unwrap();
        let mut old = first.clone();
        old.routing_policy_revision.clear();
        old.request_id = "old-policy".into();
        let old = store(context).unwrap().save(old, first.revision).unwrap();
        assert_eq!(
            store(context).unwrap().read(&old.id, old.revision).unwrap(),
            old
        );
        let request: StartRunRequest = serde_json::from_value(json!({
            "program": PROGRAM, "plan": {"id":old.id,"revision":old.revision}
        }))
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        assert!(
            admit(context, &request, &state, "test-old-policy", temp.path())
                .unwrap_err()
                .to_string()
                .contains("routing policy changed")
        );
        let payload = json!({"id":old.id,"expected_revision":old.revision,"request_id":"current-policy","program":PROGRAM,"inputs":{},"answers":{},"accepted_assumptions":[],"overrides":{}});
        let (status, value) = save(context, &serde_json::to_vec(&payload).unwrap());
        assert_eq!(status, 200, "{value}");
        let saved: WorkflowPlanRevision = serde_json::from_value(value).unwrap();
        assert_eq!(
            saved.routing_policy_revision,
            wd::profiles::ROUTING_POLICY_REVISION
        );
        let request: StartRunRequest = serde_json::from_value(json!({
            "program": PROGRAM, "plan": {"id":saved.id,"revision":saved.revision}
        }))
        .unwrap();
        admit(
            context,
            &request,
            &state,
            "test-current-policy",
            temp.path(),
        )
        .unwrap();
    }

    #[test]
    fn bundle_round_trip_preserves_bindings_and_revalidates_destination_ownership() {
        let origin = super::super::tests::test_server();
        let destination = super::super::tests::test_server();
        let program = PROGRAM.replace("Web", "Codex");
        let state = compile_preview(&origin.context, &program).unwrap();
        let mut spec = draft();
        spec.questions.clear();
        spec.intent.assumptions[0].accepted = true;
        spec.preferences
            .overrides
            .insert("flow.checker".into(), "codex-economy".into());
        let inputs = BTreeMap::from([("flow.request".into(), json!("Owned supplied evidence"))]);
        let original = create(&origin.context, &program, &inputs, spec, &state).unwrap();
        let reference = serde_json::to_vec(&PlanRef {
            id: original.id.clone(),
            revision: original.revision,
        })
        .unwrap();
        let (status, bundle) = export(&origin.context, &reference);
        assert_eq!(status, 200, "{bundle}");
        assert_eq!(export(&destination.context, &reference).0, 400);
        assert!(bundle["draft"]["intent"]["message_sequences"]
            .as_array()
            .unwrap()
            .is_empty());
        let payload = json!({"request_id":"round-trip-1", "bundle":bundle});
        let (status, imported) =
            import(&destination.context, &serde_json::to_vec(&payload).unwrap());
        assert_eq!(status, 200, "{imported}");
        let saved: WorkflowPlanRevision = serde_json::from_value(imported.clone()).unwrap();
        assert_ne!(saved.id, original.id);
        assert_eq!(saved.conversation_id, destination.context.conversation_id);
        assert_eq!(saved.program, program);
        assert_eq!(saved.inputs, inputs);
        assert_eq!(saved.profiles, original.profiles);
        assert_eq!(saved.profiles["flow.checker"].id, "codex-economy");
        assert!(!saved.draft.intent.assumptions[0].accepted);
        assert_eq!(
            import(&destination.context, &serde_json::to_vec(&payload).unwrap()).1,
            imported
        );
        assert_eq!(
            destination
                .context
                .history
                .lock()
                .unwrap()
                .conversation(&saved.conversation_id)
                .messages
                .len(),
            1
        );
        let mut corrupted = payload.clone();
        corrupted["bundle"]["program"] = json!("changed source");
        assert!(import(
            &destination.context,
            &serde_json::to_vec(&corrupted).unwrap()
        )
        .1["error"]
            .as_str()
            .unwrap()
            .contains("digest"));
        let mut changed = payload;
        changed["bundle"]["profiles"]["flow.writer"]["model"] = json!("different-model");
        assert!(
            import(&destination.context, &serde_json::to_vec(&changed).unwrap()).1["error"]
                .as_str()
                .unwrap()
                .contains("different model")
        );
    }

    #[test]
    fn receipt_totals_require_every_invoked_turn_but_not_unused_repair_agents() {
        let server = super::super::tests::test_server();
        let context = &server.context;
        let root =
            crate::ea::ea_state_dir(context.ea_id, &context.omar_dir).join("serve/usage-test");
        let registry = wd::profiles::Registry::load(&context.omar_dir).unwrap();
        let profile = registry.profiles[0].clone();
        let original = ExecutionReceipt {
            schema_version: 1,
            run_id: "usage-test".into(),
            plan: None,
            source_digest: "source".into(),
            profiles: BTreeMap::from([
                ("writer".into(), profile.clone()),
                ("repairer".into(), profile),
            ]),
            connection_scope: None,
            runtime_contract: String::new(),
            started_at: 1,
            finished_at: None,
            status: "running".into(),
            error: None,
            verification: vec![],
            resources: vec![],
            repository_checks: vec![],
            observed_models: BTreeMap::new(),
            observed_efforts: BTreeMap::new(),
            backend_versions: BTreeMap::new(),
            input_tokens: Some(999),
            output_tokens: Some(999),
            cost_usd: None,
        };
        let path = root.join("execution-receipt.json");
        topology::write_json_atomic(&path, &original).unwrap();
        for id in ["first", "second"] {
            topology::write_json_atomic(
                &root.join(format!("invocations/{id}.json")),
                &json!({"agent":"writer"}),
            )
            .unwrap();
        }
        topology::write_json_atomic(
            &root.join("invocations/prevented.json"),
            &json!({"agent":"writer","invoked":false,"verification":null}),
        )
        .unwrap();
        let usage_path = root.join("invocations/usage/writer.json");
        let turn =
            json!({"usage":{"input_tokens":100,"output_tokens":10},"model":"reported-model"});
        topology::write_json_atomic(&usage_path, &json!([turn])).unwrap();
        let run = RunRecord {
            run_id: "usage-test".into(),
            team: "test".into(),
            status: RunStatus::Completed,
            diagram_address: None,
            started_at: 1,
            finished_at: Some(2),
            error: None,
        };
        finish(context, &run).unwrap();
        let read =
            || serde_json::from_slice::<ExecutionReceipt>(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            read().input_tokens,
            None,
            "one turn cannot account for two invocations"
        );
        assert_eq!(read().output_tokens, None);
        assert_eq!(
            read().observed_models["writer"].as_deref(),
            Some("reported-model")
        );
        topology::write_json_atomic(&usage_path, &json!([turn, turn])).unwrap();
        finish(context, &run).unwrap();
        assert_eq!(read().input_tokens, Some(200));
        assert_eq!(read().output_tokens, Some(20));
        assert_eq!(read().cost_usd, None);
    }

    #[test]
    fn plan_review_is_durable_scoped_and_revision_bound_before_launch() {
        let server = super::super::tests::test_server();
        let context = &server.context;
        let state = compile_preview(context, PROGRAM).unwrap();
        let inputs = BTreeMap::from([("flow.request".into(), json!("Supplied evidence"))]);
        let mut initial = draft();
        initial.intent.constraints = vec!["Use supplied evidence only".into()];
        initial.preferences.allowed_backends = vec!["codex".into()];
        let first = create(context, PROGRAM, &inputs, initial, &state).unwrap();
        let reference = PlanRef {
            id: first.id.clone(),
            revision: first.revision,
        };
        let temp = tempfile::tempdir().unwrap();
        let request = |reference: PlanRef, program: &str| {
            serde_json::from_value::<StartRunRequest>(
                json!({"program":program,"inputs":inputs,"plan":reference}),
            )
            .unwrap()
        };
        assert!(admit(
            context,
            &request(reference.clone(), PROGRAM),
            &state,
            "test-run",
            temp.path()
        )
        .is_err());
        let payload = json!({"id":first.id,"expected_revision":1,"request_id":"review-1","program":PROGRAM,"inputs":inputs,"answers":{"audience":"Product team"},"accepted_assumptions":["internal"],"overrides":{}});
        let (status, value) = save(context, &serde_json::to_vec(&payload).unwrap());
        assert_eq!(status, 200, "{value}");
        let saved: WorkflowPlanRevision = serde_json::from_value(value).unwrap();
        assert_eq!(saved.revision, 2);
        assert_eq!(
            store(context)
                .unwrap()
                .read(&saved.id, 2)
                .unwrap()
                .draft
                .questions[0]
                .answer
                .as_deref(),
            Some("Product team")
        );
        assert!(admit(
            context,
            &request(reference, PROGRAM),
            &state,
            "test-run",
            temp.path()
        )
        .unwrap_err()
        .to_string()
        .contains("conflict"));
        let mut changed = payload;
        changed["request_id"] = json!("review-2");
        assert_eq!(save(context, &serde_json::to_vec(&changed).unwrap()).0, 409);
        let other = super::super::tests::test_server();
        assert!(store(&other.context).unwrap().read(&saved.id, 2).is_err());
        let mut revised = draft();
        revised.revises = Some(PlanRef {
            id: saved.id.clone(),
            revision: 2,
        });
        revised.questions.clear();
        revised.intent.assumptions.clear();
        let third = create(context, PROGRAM, &inputs, revised, &state).unwrap();
        assert_eq!(third.id, saved.id);
        assert_eq!(third.revision, 3);
        assert_eq!(
            third.draft.intent.constraints,
            ["Use supplied evidence only"]
        );
        assert_eq!(third.draft.preferences.allowed_backends, ["codex"]);
        let reference = PlanRef {
            id: third.id.clone(),
            revision: 3,
        };
        assert!(admit(
            context,
            &request(reference.clone(), "changed source"),
            &state,
            "test-run",
            temp.path()
        )
        .is_err());
        admit(
            context,
            &request(reference, PROGRAM),
            &state,
            "test-run",
            temp.path(),
        )
        .unwrap();
        let receipt: ExecutionReceipt =
            serde_json::from_slice(&fs::read(temp.path().join("execution-receipt.json")).unwrap())
                .unwrap();
        assert_eq!(receipt.plan.unwrap().revision, 3);
        assert!(receipt.cost_usd.is_none());
    }
}
