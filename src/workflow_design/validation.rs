use super::*;

/// Deterministic checks run before effects reach downstream reactions. They
/// establish shape and referential integrity, never semantic entailment.
pub fn check_outputs(
    checks: &[OutputCheck],
    outputs: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<()> {
    for check in checks {
        let Some(raw) = outputs.get(check.port()) else {
            continue;
        }; // Alternative effect not emitted.
        let value: serde_json::Value = match raw.as_str() {
            Some(text) => {
                ensure!(text.len() <= 262_144, "checked output too large");
                serde_json::from_str(text).context("checked output is not JSON")?
            }
            None => raw.clone(),
        };
        ensure!(value.is_object(), "checked output must be a JSON object");
        match check {
            OutputCheck::JsonObject {
                required_fields, ..
            } => {
                for key in required_fields {
                    ensure!(
                        value.get(key).is_some_and(|v| !v.is_null()),
                        "missing required output field '{key}'"
                    );
                }
            }
            OutputCheck::RepositoryPatch { .. } => {
                ensure!(
                    value.get("changes").is_some_and(|v| v.is_array())
                        && value.get("explanation").is_some_and(|v| v.is_string()),
                    "repository patch needs changes and explanation"
                );
            }
            OutputCheck::GroundedBrief {
                allowed_source_ids,
                minimum_claims,
                ..
            } => {
                ensure!(
                    value["recommendation"]
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty()),
                    "brief needs a recommendation"
                );
                let claims = value["claims"]
                    .as_array()
                    .context("brief needs a claims array")?;
                ensure!(
                    claims.len() >= *minimum_claims && claims.len() <= 100,
                    "unexpected number of claims"
                );
                for claim in claims {
                    ensure!(
                        claim["text"].as_str().is_some_and(|s| !s.trim().is_empty()),
                        "claim text is missing"
                    );
                    let ids = claim["source_ids"]
                        .as_array()
                        .context("claim needs source IDs")?;
                    ensure!(
                        !ids.is_empty()
                            && ids.iter().all(|id| id
                                .as_str()
                                .is_some_and(|id| allowed_source_ids.iter().any(|s| s == id))),
                        "claim cites an unknown or empty source"
                    );
                }
                ensure!(
                    value["limitations"]
                        .as_array()
                        .is_some_and(|v| v.iter().all(|s| s.is_string())),
                    "brief needs a limitations array"
                );
            }
        }
    }
    Ok(())
}
use anyhow::{ensure, Context, Result};
use std::collections::BTreeSet;

pub fn step_contract_digests(
    draft: &PlanDraft,
    state: &crate::topology::VmState,
) -> Result<std::collections::BTreeMap<String, String>> {
    draft
        .steps
        .iter()
        .map(|step| {
            let reaction = state
                .reactions
                .get(&step.reaction)
                .context("unknown step contract reaction")?;
            let ports: std::collections::BTreeMap<_, _> = reaction
                .triggers
                .iter()
                .chain(&reaction.effects)
                .map(|id| (id, state.ports.get(id)))
                .collect();
            let contract = serde_json::json!({
                "taxonomy_revision":contracts::TAXONOMY_REVISION,
                "reaction":reaction, "ports":ports, "operation":step.operation,
                "acceptance_criteria":step.acceptance_criteria, "output_checks":step.output_checks,
                "verification_reaction":step.verification_reaction,
                "execution":step.execution, "recovery":step.recovery, "resources":step.resources,
                "required_tools":step.required_tools, "qualification_scope":step.qualification_scope,
                "connections":state.connections,
                "state_vars":if reaction.body.is_some() {serde_json::to_value(state.state_vars.iter().filter(|(_,v)|v.instance==reaction.instance).collect::<std::collections::BTreeMap<_,_>>())?} else {serde_json::Value::Null},
                "params":if reaction.body.is_some() {serde_json::to_value(state.params.iter().filter(|(_,v)|v.instance==reaction.instance).collect::<std::collections::BTreeMap<_,_>>())?} else {serde_json::Value::Null}
            });
            Ok((
                step.reaction.clone(),
                super::digest(&serde_json::to_vec(&contract)?),
            ))
        })
        .collect()
}

pub fn validate_plan(
    plan: &WorkflowPlanRevision,
    state: &crate::topology::VmState,
    for_launch: bool,
) -> Result<()> {
    validate_design(plan, state, for_launch)
}

fn validate_design(
    plan: &WorkflowPlanRevision,
    state: &crate::topology::VmState,
    for_launch: bool,
) -> Result<()> {
    ensure!(
        plan.draft.steps.len() <= 64 && plan.draft.research.sources.len() <= 100,
        "workflow design exceeds the supported size budget"
    );
    ensure!(
        plan.draft
            .research
            .sources
            .iter()
            .filter(|s| s.provenance == "retrieved")
            .count()
            <= 3,
        "a proposal may label at most three sources as retrieved; this label is not retrieval proof"
    );
    ensure!(
        !plan.draft.intent.outcome.trim().is_empty()
            && !plan.draft.intent.acceptance_criteria.is_empty(),
        "define an outcome and acceptance criteria"
    );
    ensure!(
        plan.draft.questions.len() <= 3,
        "ask at most three focused follow-up questions"
    );
    let mut ids = BTreeSet::new();
    let mut assumptions = BTreeSet::new();
    for assumption in &plan.draft.intent.assumptions {
        ensure!(
            store::identifier(&assumption.id)
                && assumptions.insert(&assumption.id)
                && !assumption.text.trim().is_empty()
                && !assumption.provenance.trim().is_empty(),
            "invalid assumption ID or provenance"
        );
    }
    for source in &plan.draft.research.sources {
        ensure!(
            ids.insert(&source.id)
                && !source.location.trim().is_empty()
                && !source.provenance.trim().is_empty(),
            "invalid or duplicate research source"
        );
    }
    let mut reactions = BTreeSet::new();
    for step in &plan.draft.steps {
        if let Some(scope) = &step.qualification_scope {
            scope.validate()?;
        }
        ensure!(reactions.insert(&step.reaction), "duplicate step mapping");
        let reaction = state
            .reactions
            .get(&step.reaction)
            .context(format!("unknown reaction '{}'", step.reaction))?;
        for check in &step.output_checks {
            ensure!(
                reaction.effects.iter().any(|p| p == check.port()),
                "output check must bind to a declared effect"
            );
            if let OutputCheck::GroundedBrief {
                allowed_source_ids,
                minimum_claims,
                ..
            } = check
            {
                ensure!(
                    *minimum_claims <= 100 && allowed_source_ids.iter().all(|id| ids.contains(id)),
                    "output check cites unknown research sources"
                );
                if for_launch {
                    ensure!(allowed_source_ids.iter().all(|id| plan.draft.research.sources.iter().any(|s|
                        &s.id == id && s.origin.as_ref().is_some_and(|o| !matches!(o, StatementOrigin::SystemProposed {..})))),
                        "grounded output checks require classified, linked sources; update the research before running");
                }
            }
        }
        ensure!(
            reaction.agent == step.agent && !step.acceptance_criteria.is_empty(),
            "step agent or acceptance criteria mismatch"
        );
        ensure!(
            step.evidence_ids.iter().all(|id| ids.contains(id)),
            "step refers to an unknown research source"
        );
        if matches!(
            step.operation,
            Operation::ExactValidation | Operation::ExactTransformation
        ) {
            ensure!(
                reaction.body.is_some(),
                "exact validation must use a code reaction"
            );
        } else if reaction.body.is_none()
            && !crate::topology::is_web_backend(
                &state
                    .agents
                    .get(&step.agent)
                    .context("step agent is missing")?
                    .backend,
            )
        {
            if let Some(profile) = plan.profiles.get(&step.agent) {
                profile.command(&state.agents[&step.agent].backend)?;
                ensure!(
                    profile.operations.contains(&step.operation),
                    "profile does not support the step operation"
                );
                ensure!(
                    step.required_tools
                        .iter()
                        .all(|t| profile.tools.contains(t)),
                    "profile lacks a required tool"
                );
                ensure!(
                    step.qualification_scope.as_ref().is_none_or(|scope| scope
                        .input_modalities
                        .iter()
                        .all(|m| profile.input_modalities.contains(m))
                        && scope
                            .output_modalities
                            .iter()
                            .all(|m| profile.output_modalities.contains(m))),
                    "profile lacks a required input or output modality"
                );
            } else {
                ensure!(
                    !for_launch,
                    "missing execution profile for '{}'",
                    step.agent
                );
            }
        }
        if matches!(
            step.operation,
            Operation::EvidenceSynthesis
                | Operation::CodeRepair
                | Operation::Planning
                | Operation::Writing
                | Operation::CodeImplementation
                | Operation::ScriptStoryboard
                | Operation::RenderCodeGeneration
                | Operation::MediaGeneration
        ) {
            ensure!(
                step.verification_reaction.is_some(),
                "synthesis and repair steps require a verification gate"
            );
        }
        if let Some(checker_id) = &step.verification_reaction {
            let checker = state
                .reactions
                .get(checker_id)
                .context("unknown verification reaction")?;
            ensure!(
                checker_id != &step.reaction && !checker.effects.is_empty(),
                "verification needs a separate reaction and accepted output"
            );
            ensure!(
                plan.draft.steps.iter().any(|s| &s.reaction == checker_id
                    && matches!(
                        s.operation,
                        Operation::ClaimVerification
                            | Operation::CodeVerification
                            | Operation::ExactValidation
                            | Operation::CodeReview
                            | Operation::AudiovisualVerification
                    )),
                "checker must declare a verification operation"
            );
            // Raw values may travel only to the declared checker. Traverse
            // connections because aliases can otherwise conceal a bypass.
            let mut reachable: BTreeSet<String> = reaction.effects.iter().cloned().collect();
            loop {
                let old = reachable.len();
                for edge in &state.connections {
                    if reachable.contains(&edge.source) {
                        reachable.insert(edge.target.clone());
                    }
                }
                if old == reachable.len() {
                    break;
                }
            }
            ensure!(
                !reachable.is_empty() && checker.triggers.iter().any(|t| reachable.contains(t)),
                "checker is not connected to the raw output"
            );
            ensure!(
                !reachable.iter().any(|id| state
                    .ports
                    .get(id)
                    .is_some_and(|port| port.kind == crate::topology::PortKind::Output)),
                "raw output bypasses verification through an exposed output port"
            );
            for (id, consumer) in &state.reactions {
                if consumer.triggers.iter().any(|t| reachable.contains(t)) {
                    ensure!(
                        id == checker_id,
                        "raw output bypasses verification through '{id}'"
                    );
                }
            }
            ensure!(
                checker.effects.iter().all(|p| !reachable.contains(p)),
                "checker must emit distinct accepted outputs"
            );
        }
    }
    ensure!(
        reactions.len() == state.reactions.len(),
        "every compiled reaction must have one step contract"
    );
    for agent in plan.profiles.keys() {
        ensure!(
            state.agents.contains_key(agent),
            "profile bound to unknown agent"
        );
    }
    let mut question_ids = BTreeSet::new();
    for q in &plan.draft.questions {
        ensure!(
            store::identifier(&q.id) && question_ids.insert(&q.id) && !q.question.trim().is_empty(),
            "invalid review question"
        );
        ensure!(
            q.target_reactions.len() <= 64
                && q.target_reactions.iter().all(|id| reactions.contains(id)),
            "review question refers to an unknown workflow step"
        );
        ensure!(
            !for_launch || !q.required || q.answer.as_ref().is_some_and(|a| !a.trim().is_empty()),
            "answer required question '{}' before launch",
            q.id
        );
    }
    if for_launch {
        ensure!(
            plan.draft
                .research
                .issues
                .iter()
                .all(|i| !i.blocking || i.resolution.is_some()),
            "resolve blocking research issues before launch"
        );
        ensure!(
            !plan.draft.questions.iter().any(|q| q.answer.is_some()),
            "incorporate the saved answers into an updated design before launch"
        );
        ensure!(
            plan.draft.intent.assumptions.iter().all(|a| a.accepted),
            "review and accept proposed assumptions before launch"
        );
    }
    recovery::compile(&plan.draft, state, &plan.inputs)?;
    Ok(())
}
