//! Bounded, explicit recovery over the existing compiled port graph.
use super::*;
use crate::topology::{PortKind, VmState};
use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct VerificationGate {
    pub step: String,
    pub stage: VerificationStage,
    pub accepted_port: String,
    pub rejected_port: String,
    pub rejection: VerificationDecision,
}

#[derive(Debug, Clone)]
pub struct RuntimeStepPolicy {
    pub logical_id: String,
    pub owner: StepOwner,
    pub resources: StepResources,
    pub parameter_bytes: usize,
    pub execution: StepExecution,
    pub gate: Option<VerificationGate>,
}

pub type RuntimePolicies = BTreeMap<String, RuntimeStepPolicy>;

fn forward(state: &VmState, ports: &[String]) -> BTreeSet<String> {
    let mut seen: BTreeSet<_> = ports.iter().cloned().collect();
    loop {
        let before = seen.len();
        for edge in &state.connections {
            if seen.contains(&edge.source) {
                seen.insert(edge.target.clone());
            }
        }
        if before == seen.len() {
            return seen;
        }
    }
}

fn consumers(state: &VmState, ports: &[String]) -> BTreeSet<String> {
    let reachable = forward(state, ports);
    state
        .reactions
        .iter()
        .filter(|(_, r)| r.triggers.iter().any(|p| reachable.contains(p)))
        .map(|(id, _)| id.clone())
        .collect()
}

fn exclusive_producers(state: &VmState, port: &str, allowed: &[&str]) -> Result<()> {
    let downstream = forward(state, &[port.into()]);
    for (id, reaction) in &state.reactions {
        if !forward(state, &reaction.effects).is_disjoint(&downstream) {
            ensure!(
                allowed.contains(&id.as_str()),
                "recovery port '{port}' has an undeclared producer '{id}'"
            );
        }
    }
    Ok(())
}

fn exclusive_consumer(state: &VmState, port: &str, consumer: &str) -> Result<()> {
    ensure!(
        consumers(state, &[port.into()]) == BTreeSet::from([consumer.into()]),
        "recovery port '{port}' must feed only '{consumer}'"
    );
    let reachable = forward(state, &[port.into()]);
    ensure!(
        !reachable.iter().any(|id| state
            .ports
            .get(id)
            .is_some_and(|p| p.kind == PortKind::Output)),
        "recovery request '{port}' cannot be an exposed output"
    );
    ensure!(
        state.reactions[consumer]
            .triggers
            .iter()
            .all(|p| reachable.contains(p)),
        "recovery reaction '{consumer}' has another trigger that bypasses the failed check"
    );
    Ok(())
}

fn terminal(state: &VmState, port: &str) -> Result<()> {
    ensure!(
        state
            .ports
            .get(port)
            .is_some_and(|p| p.kind == PortKind::Output && p.ty == "string"),
        "blocked recovery must expose a string output explaining the failure"
    );
    ensure!(
        consumers(state, &[port.into()]).is_empty(),
        "blocked recovery must not trigger more work"
    );
    Ok(())
}

pub fn compile(
    draft: &PlanDraft,
    state: &VmState,
    inputs: &BTreeMap<String, Value>,
) -> Result<RuntimePolicies> {
    let steps: BTreeMap<_, _> = draft
        .steps
        .iter()
        .map(|s| (s.reaction.as_str(), s))
        .collect();
    let mut policies = RuntimePolicies::new();
    for step in &draft.steps {
        contracts::validate_resources(&step.resources)?;
        let reaction = state
            .reactions
            .get(&step.reaction)
            .context("missing compiled step")?;
        let execution = &step.execution;
        ensure!(
            execution.max_invocations > 0 && execution.max_invocations <= 10_000,
            "step invocation limit must be between 1 and 10000"
        );
        ensure!(
            execution
                .deadline_ms
                .is_none_or(|ms| ms > 0 && ms <= 3_600_000),
            "step deadline must be between 1 ms and one hour"
        );
        ensure!(
            execution.max_invocations == 1
                || matches!(
                    execution.effect,
                    StepEffect::Repeatable | StepEffect::HumanDecision
                ),
            "unknown or external-action steps cannot be invoked repeatedly"
        );
        if execution.effect == StepEffect::HumanDecision {
            ensure!(
                state
                    .agents
                    .get(&step.agent)
                    .is_some_and(|a| crate::topology::is_web_backend(&a.backend)),
                "human decisions require an actual Web handoff"
            );
        }
        policies.insert(
            step.reaction.clone(),
            RuntimeStepPolicy {
                logical_id: step.logical_id.clone(),
                owner: if reaction.body.is_some() {
                    StepOwner::Code
                } else if state
                    .agents
                    .get(&reaction.agent)
                    .is_some_and(|a| crate::topology::is_web_backend(&a.backend))
                {
                    StepOwner::Human
                } else {
                    StepOwner::Model
                },
                resources: step.resources.clone(),
                parameter_bytes: if reaction.body.is_some() {
                    serde_json::to_vec(
                        &state
                            .params
                            .iter()
                            .filter(|(_, p)| p.instance == reaction.instance)
                            .map(|(id, p)| (id, &p.value))
                            .collect::<BTreeMap<_, _>>(),
                    )?
                    .len()
                } else {
                    0
                },
                execution: execution.clone(),
                gate: None,
            },
        );
    }
    let mut recovery_members = BTreeSet::new();
    for step in &draft.steps {
        let Some(recovery) = &step.recovery else {
            continue;
        };
        ensure!(
            recovery.max_repair_attempts == 1,
            "the supported recovery contract permits exactly one repair attempt"
        );
        ensure!(step.execution.effect == StepEffect::Repeatable,
            "automatic recovery is only permitted for declared repeatable drafts, not unknown or external actions");
        let checker = step
            .verification_reaction
            .as_deref()
            .context("recovery requires an initial checker")?;
        let repair = steps
            .get(recovery.repair_reaction.as_str())
            .context("unknown repair step")?;
        let recheck = steps
            .get(recovery.recheck_reaction.as_str())
            .context("unknown repair checker")?;
        ensure!(
            repair.execution.effect == StepEffect::Repeatable && repair.recovery.is_none(),
            "repair must be repeatable and cannot start another recovery chain"
        );
        ensure!(
            repair.verification_reaction.as_deref() == Some(recovery.recheck_reaction.as_str()),
            "repaired output must pass its declared second checker"
        );
        let first = steps
            .get(checker)
            .context("initial checker has no step contract")?;
        for check in [*first, *recheck] {
            ensure!(
                matches!(
                    check.execution.effect,
                    StepEffect::Repeatable | StepEffect::HumanDecision
                ),
                "recovery checkers cannot perform external actions"
            );
        }
        let mut members = vec![
            step.reaction.as_str(),
            checker,
            recovery.repair_reaction.as_str(),
            recovery.recheck_reaction.as_str(),
        ];
        if let Some(human) = &recovery.human_review {
            members.push(&human.reaction);
        }
        for id in &members {
            ensure!(
                recovery_members.insert(*id),
                "recovery reactions must be distinct and belong to one recovery contract"
            );
            ensure!(
                steps
                    .get(id)
                    .is_some_and(|s| s.execution.max_invocations == 1),
                "each recovery reaction must have a one-invocation limit"
            );
        }
        for port in [&recovery.retry_port, &recovery.failure_port] {
            ensure!(
                state.ports.get(port).is_some_and(|p| p.ty == "string"),
                "recovery diagnostics require a declared string port"
            );
        }
        let mut decisions = vec![
            recovery.retry_port.clone(),
            recovery.failure_port.clone(),
            recovery.accepted_port.clone(),
        ];
        if let Some(human) = &recovery.human_review {
            decisions.push(human.blocked_port.clone());
        }
        let protected = forward(state, &decisions);
        for input in inputs.keys() {
            ensure!(
                forward(state, &[input.clone()]).is_disjoint(&protected),
                "input '{input}' bypasses a recovery decision"
            );
        }
        crate::topology::require_exclusive_effects(
            &state.reactions[checker].contract,
            &[&recovery.accepted_port, &recovery.retry_port],
        )?;
        crate::topology::require_exclusive_effects(
            &state.reactions[&recovery.recheck_reaction].contract,
            &[&recovery.accepted_port, &recovery.failure_port],
        )?;
        exclusive_producers(state, &recovery.retry_port, &[checker])?;
        exclusive_consumer(state, &recovery.retry_port, &recovery.repair_reaction)?;
        exclusive_producers(state, &recovery.failure_port, &[&recovery.recheck_reaction])?;
        let mut acceptors = vec![checker, recovery.recheck_reaction.as_str()];
        let checks_for = |s: &StepSpec| {
            s.output_checks
                .iter()
                .filter(|c| c.port() == recovery.accepted_port)
                .cloned()
                .collect::<Vec<_>>()
        };
        ensure!(
            checks_for(first) == checks_for(recheck),
            "recheck must retain the original acceptance checks"
        );
        if let Some(human) = &recovery.human_review {
            let reviewer = steps
                .get(human.reaction.as_str())
                .context("unknown human recovery step")?;
            ensure!(
                reviewer.execution.effect == StepEffect::HumanDecision,
                "recovery escalation must be a declared Web human decision"
            );
            exclusive_consumer(state, &recovery.failure_port, &human.reaction)?;
            terminal(state, &human.blocked_port)?;
            exclusive_producers(state, &human.blocked_port, &[&human.reaction])?;
            crate::topology::require_exclusive_effects(
                &state.reactions[&human.reaction].contract,
                &[&recovery.accepted_port, &human.blocked_port],
            )?;
            ensure!(
                checks_for(reviewer) == checks_for(first),
                "human acceptance cannot override deterministic acceptance checks"
            );
            acceptors.push(&human.reaction);
            policies.get_mut(&human.reaction).unwrap().gate = Some(VerificationGate {
                step: step.reaction.clone(),
                stage: VerificationStage::HumanReview,
                accepted_port: recovery.accepted_port.clone(),
                rejected_port: human.blocked_port.clone(),
                rejection: VerificationDecision::Blocked,
            });
        } else {
            terminal(state, &recovery.failure_port)?;
        }
        exclusive_producers(state, &recovery.accepted_port, &acceptors)?;
        // Delayed port edges are included: an action/microstep is not proof
        // that a repair loop is bounded. Ordinary source-only loops are unchanged.
        for start in &members {
            let mut stack = vec![start.to_string()];
            let mut visited = BTreeSet::new();
            while let Some(id) = stack.pop() {
                if !visited.insert(id.clone()) {
                    continue;
                }
                for next in consumers(state, &state.reactions[&id].effects) {
                    ensure!(next != *start, "recovery cycle returns to '{start}'");
                    stack.push(next);
                }
            }
        }
        policies.get_mut(checker).unwrap().gate = Some(VerificationGate {
            step: step.reaction.clone(),
            stage: VerificationStage::InitialCheck,
            accepted_port: recovery.accepted_port.clone(),
            rejected_port: recovery.retry_port.clone(),
            rejection: VerificationDecision::RepairRequested,
        });
        policies.get_mut(&recovery.recheck_reaction).unwrap().gate = Some(VerificationGate {
            step: step.reaction.clone(),
            stage: VerificationStage::RepairCheck,
            accepted_port: recovery.accepted_port.clone(),
            rejected_port: recovery.failure_port.clone(),
            rejection: if recovery.human_review.is_some() {
                VerificationDecision::HumanReviewRequested
            } else {
                VerificationDecision::Blocked
            },
        });
    }
    Ok(policies)
}

/// Called after provider completion and before scheduler delivery. Only a
/// deterministic output rejection follows this repair path. Provider/tool
/// errors never arrive here and therefore never trigger automatic repetition.
#[cfg(test)]
pub fn check_and_route(
    reaction: &str,
    checks: &[OutputCheck],
    gate: Option<&VerificationGate>,
    writes: BTreeMap<String, Value>,
) -> Result<(BTreeMap<String, Value>, Option<VerificationEvent>)> {
    ensure!(
        !checks
            .iter()
            .any(|c| matches!(c, OutputCheck::RepositoryPatch { .. })
                && writes.contains_key(c.port())),
        "repository checks require the trusted runtime verifier"
    );
    let validation = validation::check_outputs(checks, &writes);
    route_checked(reaction, checks, gate, writes, validation)
}

/// Only the host runtime supplies this result after structural and executable
/// checks. Infrastructure errors must return before calling this function.
pub(super) fn route_checked(
    reaction: &str,
    checks: &[OutputCheck],
    gate: Option<&VerificationGate>,
    mut writes: BTreeMap<String, Value>,
    validation: Result<()>,
) -> Result<(BTreeMap<String, Value>, Option<VerificationEvent>)> {
    let Some(gate) = gate else {
        validation?;
        return Ok((writes, None));
    };
    let error = validation.err().map(|e| format!("{e:#}"));
    if let Some(error) = &error {
        let candidate = serde_json::to_string(&writes)?;
        let diagnostic = serde_json::json!({
            "reason":error, "candidate_digest":super::digest(candidate.as_bytes()),
            "candidate_excerpt":candidate.chars().take(16_384).collect::<String>(),
            "candidate_truncated":candidate.chars().count() > 16_384,
            "next":gate.rejection,
        });
        writes = BTreeMap::from([(
            gate.rejected_port.clone(),
            Value::String(diagnostic.to_string()),
        )]);
        // Generated diagnostics must not bypass a declared failure schema.
        // A conflicting schema stops the run; there is no recursive retry.
        validation::check_outputs(checks, &writes)?;
    }
    let decision = if writes.contains_key(&gate.accepted_port)
        && !writes.contains_key(&gate.rejected_port)
    {
        VerificationDecision::Accepted
    } else if writes.contains_key(&gate.rejected_port) && !writes.contains_key(&gate.accepted_port)
    {
        ensure!(
            writes[&gate.rejected_port]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty()),
            "a rejected check needs a failure explanation"
        );
        gate.rejection
    } else {
        bail!("checker must emit exactly one accepted or rejected outcome");
    };
    ensure!(
        writes.len() == 1,
        "checker emitted effects outside its recovery decision"
    );
    let event = VerificationEvent {
        step: gate.step.clone(),
        reaction: reaction.into(),
        stage: gate.stage,
        decision,
        validation_error: error,
    };
    Ok((writes, Some(event)))
}
