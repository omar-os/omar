//! Compiler-derived handoffs and stable logical identities. Source edits never
//! reassign acceptance criteria solely by reaction list position.
use super::*;
use crate::topology::{PortKind, ReactionState, VmState};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const CONTEXT_COVERAGE: &str = "Invocation payload only; native session history, system prompts, tools, provider tokens and cost are not bounded or measured here.";
pub const TAXONOMY_REVISION: &str = "workflow-operations-v1";

pub fn validate_resources(resources: &StepResources) -> Result<()> {
    ensure!(
        [
            resources.max_input_bytes,
            resources.max_prompt_bytes,
            resources.max_output_bytes
        ]
        .iter()
        .all(|n| *n > 0 && *n <= 4_194_304),
        "step byte limits must be between 1 and 4194304"
    );
    ensure!(resources.max_model_input_tokens.is_none() && resources.max_model_output_tokens.is_none() && resources.max_cost_usd.is_none(),
        "native token/context-window and spend caps are unsupported; use enforced invocation byte/attempt/time limits or an explicitly supported adapter");
    Ok(())
}

fn reach(state: &VmState, ports: &[String]) -> BTreeSet<String> {
    let mut found: BTreeSet<_> = ports.iter().cloned().collect();
    loop {
        let before = found.len();
        for edge in &state.connections {
            if found.contains(&edge.source) {
                found.insert(edge.target.clone());
            }
        }
        if before == found.len() {
            return found;
        }
    }
}

fn schema(ty: &str) -> Result<Value> {
    for (outer, array) in [("list", true), ("option", false)] {
        if let Some(inner) = ty
            .strip_prefix(&format!("{outer}<"))
            .and_then(|s| s.strip_suffix('>'))
        {
            let nested = schema(inner)?;
            return Ok(if array {
                json!({"type":"array","items":nested})
            } else {
                json!({"anyOf":[nested,{"type":"null"}]})
            });
        }
    }
    Ok(match ty {
        "signal" => json!({"type":"null"}),
        "int" => json!({"type":"integer","minimum":i64::MIN,"maximum":i64::MAX}),
        "float" => json!({"type":"number"}),
        "bool" => json!({"type":"boolean"}),
        "string" | "path" | "bytes" => json!({"type":"string"}),
        _ => anyhow::bail!("unsupported compiled wire type '{ty}'"),
    })
}

fn fingerprints(state: &VmState, reaction: &ReactionState) -> Result<(String, String)> {
    let ports: BTreeMap<_, _> = reaction
        .triggers
        .iter()
        .chain(&reaction.effects)
        .map(|id| (id, state.ports.get(id)))
        .collect();
    let timers: BTreeMap<_, _> = reaction
        .triggers
        .iter()
        .filter_map(|id| state.timers.get(id).map(|timer| (id, timer)))
        .collect();
    let anchor = json!({"agent":reaction.agent,"instance":reaction.instance,"triggers":reaction.triggers,"effects":reaction.effects,"contract":reaction.contract,"ports":ports,"timers":timers,"code":reaction.body.is_some()});
    let anchor = digest(&serde_json::to_vec(&anchor)?);
    let implementation = digest(&serde_json::to_vec(
        &json!({"anchor":anchor,"prompt":reaction.prompt,"body":reaction.body}),
    )?);
    Ok((anchor, implementation))
}

/// Missing IDs are initialized once. Exact/unique structural matches retain
/// earlier identity; ambiguous redesigns must supply explicit logical IDs.
pub fn prepare(
    draft: &mut PlanDraft,
    previous: Option<&WorkflowPlanRevision>,
    state: &VmState,
    inputs: &BTreeMap<String, Value>,
) -> Result<()> {
    let mut used = BTreeSet::new();
    for step in &mut draft.steps {
        let reaction = state
            .reactions
            .get(&step.reaction)
            .context("unknown step reaction")?;
        let (anchor, implementation) = fingerprints(state, reaction)?;
        if step.logical_id.is_empty() {
            let candidates: Vec<_> = previous
                .into_iter()
                .flat_map(|p| &p.draft.steps)
                .filter(|old| {
                    old.interface
                        .as_ref()
                        .is_some_and(|i| i.anchor_digest == anchor)
                })
                .collect();
            let exact: Vec<_> = candidates
                .iter()
                .filter(|old| {
                    old.interface.as_ref().unwrap().implementation_digest == implementation
                })
                .collect();
            let old = if exact.len() == 1 {
                Some(*exact[0])
            } else if candidates.len() == 1 {
                Some(candidates[0])
            } else {
                None
            };
            ensure!(old.is_some() || candidates.is_empty(),"ambiguous step identity; provide the saved logical_id explicitly in the redesigned mapping");
            step.logical_id = old
                .map(|s| s.logical_id.clone())
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| {
                    format!(
                        "step-{}",
                        &digest(format!("{anchor}:{}", step.reaction).as_bytes())[..24]
                    )
                });
        }
        ensure!(
            store::identifier(&step.logical_id) && used.insert(step.logical_id.clone()),
            "invalid or duplicate logical step ID"
        );
        validate_resources(&step.resources)?;
        ensure!(
            step.decomposition_reason
                .as_ref()
                .is_none_or(|r| !r.trim().is_empty() && r.len() <= 4000),
            "invalid step decomposition reason"
        );
    }
    let interfaces = derive(draft, state, inputs)?;
    for step in &mut draft.steps {
        step.interface = Some(interfaces[&step.reaction].clone());
    }
    Ok(())
}

/// Rebind a source-only editor update using unique compiled structure, never
/// a title or reaction index. Changes to ports/ownership require redesign.
pub fn rebind(draft: &mut PlanDraft, state: &VmState) -> Result<()> {
    ensure!(
        draft.steps.len() == state.reactions.len(),
        "step set changed; request a redesigned plan with explicit logical step mappings"
    );
    let fingerprints: BTreeMap<_, _> = state
        .reactions
        .iter()
        .map(|(id, r)| Ok((id.clone(), fingerprints(state, r)?)))
        .collect::<Result<_>>()?;
    let mut remap = BTreeMap::new();
    let mut used = BTreeSet::new();
    for step in &draft.steps {
        let old = step
            .interface
            .as_ref()
            .context("legacy step binding needs a saved review before source edits")?;
        let matches: Vec<_> = fingerprints
            .iter()
            .filter(|(_, v)| v.0 == old.anchor_digest)
            .collect();
        let exact: Vec<_> = matches
            .iter()
            .filter(|(_, v)| v.1 == old.implementation_digest)
            .collect();
        let id = if exact.len() == 1 {
            exact[0].0
        } else if matches.len() == 1 {
            matches[0].0
        } else {
            anyhow::bail!("step '{}' changed wiring or has an ambiguous binding; ask OMAR to redesign with explicit logical IDs",step.logical_id)
        };
        ensure!(
            used.insert(id.clone()),
            "multiple logical steps map to the same compiled reaction; request a redesign"
        );
        remap.insert(step.reaction.clone(), id.clone());
    }
    let mapped = |id: &String| {
        remap
            .get(id)
            .cloned()
            .context("unknown dependent step while rebinding")
    };
    for step in &mut draft.steps {
        step.reaction = mapped(&step.reaction)?;
        step.agent = state.reactions[&step.reaction].agent.clone();
        step.verification_reaction = step
            .verification_reaction
            .as_ref()
            .map(mapped)
            .transpose()?;
        if let Some(recovery) = &mut step.recovery {
            recovery.repair_reaction = mapped(&recovery.repair_reaction)?;
            recovery.recheck_reaction = mapped(&recovery.recheck_reaction)?;
            if let Some(human) = &mut recovery.human_review {
                human.reaction = mapped(&human.reaction)?;
            }
        }
    }
    for question in &mut draft.questions {
        question.target_reactions = question
            .target_reactions
            .iter()
            .map(mapped)
            .collect::<Result<_>>()?;
    }
    Ok(())
}

pub fn derive(
    draft: &PlanDraft,
    state: &VmState,
    inputs: &BTreeMap<String, Value>,
) -> Result<BTreeMap<String, StepInterface>> {
    let ids: BTreeMap<_, _> = draft
        .steps
        .iter()
        .map(|s| (&s.reaction, &s.logical_id))
        .collect();
    ensure!(
        ids.len() == state.reactions.len()
            && ids.keys().all(|id| state.reactions.contains_key(*id)),
        "each compiled reaction needs one logical step"
    );
    let produced: BTreeMap<_, _> = state
        .reactions
        .iter()
        .map(|(id, r)| (id, reach(state, &r.effects)))
        .collect();
    let supplied = reach(state, &inputs.keys().cloned().collect::<Vec<_>>());
    state
        .reactions
        .iter()
        .map(|(id, r)| {
            let owner = if r.body.is_some() {
                StepOwner::Code
            } else if state
                .agents
                .get(&r.agent)
                .is_some_and(|a| crate::topology::is_web_backend(&a.backend))
            {
                StepOwner::Human
            } else {
                StepOwner::Model
            };
            let (anchor_digest, implementation_digest) = fingerprints(state, r)?;
            let input_contracts = r
                .triggers
                .iter()
                .map(|port| {
                    let timer = state.timers.contains_key(port);
                    let ty = if timer {
                        "timer_timestamp_ns"
                    } else {
                        &state
                            .ports
                            .get(port)
                            .context("missing compiled trigger")?
                            .ty
                    };
                    Ok(StepInput {
                        port: port.clone(),
                        wire_type: ty.into(),
                        schema: if timer {
                            json!({"type":"integer","minimum":0,"maximum":u64::MAX})
                        } else {
                            schema(ty)?
                        },
                        producer_steps: produced
                            .iter()
                            .filter(|(_, ports)| ports.contains(port))
                            .map(|(id, _)| ids[id].clone())
                            .collect(),
                        supplied_input: supplied.contains(port),
                        timer,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let outputs = r
                .effects
                .iter()
                .map(|port| {
                    let ty = &state.ports.get(port).context("missing compiled output")?.ty;
                    let reachable = reach(state, &[port.clone()]);
                    Ok(StepOutput {
                        port: port.clone(),
                        wire_type: ty.clone(),
                        schema: schema(ty)?,
                        consumer_steps: state
                            .reactions
                            .iter()
                            .filter(|(_, consumer)| {
                                consumer.triggers.iter().any(|p| reachable.contains(p))
                            })
                            .map(|(id, _)| ids[id].clone())
                            .collect(),
                        exposed_outputs: reachable
                            .into_iter()
                            .filter(|p| {
                                state
                                    .ports
                                    .get(p)
                                    .is_some_and(|p| p.kind == PortKind::Output)
                            })
                            .collect(),
                        checks: draft
                            .steps
                            .iter()
                            .find(|s| &s.reaction == id)
                            .unwrap()
                            .output_checks
                            .iter()
                            .filter(|check| check.port() == port)
                            .cloned()
                            .collect(),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let dependencies = input_contracts
                .iter()
                .flat_map(|i| i.producer_steps.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let state_variables = if owner == StepOwner::Code {
                state
                    .state_vars
                    .iter()
                    .filter(|(_, v)| v.instance == r.instance)
                    .map(|(name, v)| {
                        Ok(StepValueBinding {
                            name: name.clone(),
                            wire_type: v.ty.clone(),
                            schema: schema(&v.ty)?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?
            } else {
                vec![]
            };
            let parameters = if owner == StepOwner::Code {
                state
                    .params
                    .iter()
                    .filter(|(_, v)| v.instance == r.instance)
                    .map(|(name, v)| {
                        Ok(StepValueBinding {
                            name: name.clone(),
                            wire_type: v.ty.clone(),
                            schema: schema(&v.ty)?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?
            } else {
                vec![]
            };
            let shared_state_steps = if !state_variables.is_empty() {
                state
                    .reactions
                    .iter()
                    .filter(|(other, peer)| {
                        *other != id && peer.body.is_some() && peer.instance == r.instance
                    })
                    .map(|(id, _)| ids[id].clone())
                    .collect()
            } else {
                vec![]
            };
            let known_initial_trigger_bytes = if r.triggers.iter().all(|p| inputs.contains_key(p)) {
                Some(
                    serde_json::to_vec(
                        &r.triggers
                            .iter()
                            .map(|p| (p, &inputs[p]))
                            .collect::<BTreeMap<_, _>>(),
                    )?
                    .len(),
                )
            } else {
                None
            };
            Ok((
                id.clone(),
                StepInterface {
                    version: 1,
                    taxonomy_revision: TAXONOMY_REVISION.into(),
                    owner,
                    anchor_digest,
                    implementation_digest,
                    inputs: input_contracts,
                    outputs,
                    dependencies,
                    state_variables,
                    parameters,
                    shared_state_steps,
                    activation: "any_present_trigger".into(),
                    effect_contract: r.contract.clone(),
                    prompt_template_bytes: r.prompt.len(),
                    known_initial_trigger_bytes,
                    context_coverage: CONTEXT_COVERAGE.into(),
                },
            ))
        })
        .collect()
}

pub fn validate_saved(plan: &WorkflowPlanRevision, state: &VmState) -> Result<()> {
    let derived = derive(&plan.draft, state, &plan.inputs)?;
    let mut ids = BTreeSet::new();
    for step in &plan.draft.steps {
        ensure!(
            store::identifier(&step.logical_id) && ids.insert(&step.logical_id),
            "save a review with valid unique logical step IDs"
        );
        validate_resources(&step.resources)?;
        ensure!(
            step.interface.as_ref() == derived.get(&step.reaction),
            "step interface changed; save and review the compiled contracts"
        );
        ensure!(
            plan.draft.steps.len() == 1
                || step
                    .decomposition_reason
                    .as_ref()
                    .is_some_and(|r| !r.trim().is_empty()),
            "explain the benefit of each added step/handoff before launch"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_handoffs_include_schemas_aliases_consumers_and_exposed_outputs() {
        let (mut draft, mut state, inputs) = crate::workflow_design::recovery_tests::fixture(false);
        prepare(&mut draft, None, &state, &inputs).unwrap();
        let first = draft.steps[0].interface.as_ref().unwrap();
        assert_eq!(first.owner, StepOwner::Human);
        assert_eq!(first.inputs[0].schema, json!({"type":"string"}));
        assert!(first.inputs[0].supplied_input);
        assert_eq!(
            first.outputs[0].consumer_steps,
            vec![draft.steps[1].logical_id.clone()]
        );
        assert_eq!(
            draft.steps[1].interface.as_ref().unwrap().dependencies,
            vec![draft.steps[0].logical_id.clone()]
        );
        assert!(draft.steps[1]
            .interface
            .as_ref()
            .unwrap()
            .outputs
            .iter()
            .any(|p| p.exposed_outputs == ["flow.result"]));
        assert!(first.known_initial_trigger_bytes.unwrap() > 0);
        state
            .ports
            .insert("flow.alias".into(), state.ports["flow.raw"].clone());
        state.connections.push(crate::topology::ConnectionState {
            source: "flow.raw".into(),
            target: "flow.alias".into(),
            delay: None,
        });
        state.reactions.get_mut("flow.reaction.1").unwrap().triggers = vec!["flow.alias".into()];
        prepare(&mut draft, None, &state, &inputs).unwrap();
        assert_eq!(
            draft.steps[0].interface.as_ref().unwrap().outputs[0].consumer_steps,
            vec![draft.steps[1].logical_id.clone()]
        );
    }

    #[test]
    fn identities_survive_reordering_and_prompt_edits_but_ambiguous_wiring_does_not() {
        let (mut draft, mut state, inputs) = crate::workflow_design::recovery_tests::fixture(false);
        prepare(&mut draft, None, &state, &inputs).unwrap();
        let ids: Vec<_> = draft.steps.iter().map(|s| s.logical_id.clone()).collect();
        let first = state.reactions.remove("flow.reaction.0").unwrap();
        let second = state.reactions.remove("flow.reaction.1").unwrap();
        state.reactions.insert("flow.reaction.0".into(), second);
        state.reactions.insert("flow.reaction.1".into(), first);
        state.reactions.get_mut("flow.reaction.1").unwrap().prompt =
            "Updated draft instructions $(flow.request)".into();
        rebind(&mut draft, &state).unwrap();
        prepare(&mut draft, None, &state, &inputs).unwrap();
        assert_eq!(draft.steps[0].logical_id, ids[0]);
        assert_eq!(draft.steps[0].reaction, "flow.reaction.1");
        assert_eq!(
            draft.steps[0].verification_reaction.as_deref(),
            Some("flow.reaction.0")
        );
        state.reactions.get_mut("flow.reaction.1").unwrap().triggers = vec!["flow.raw".into()];
        assert!(rebind(&mut draft, &state)
            .unwrap_err()
            .to_string()
            .contains("redesign"));
        let (mut draft, mut state, inputs) = crate::workflow_design::recovery_tests::fixture(false);
        let same = state.reactions["flow.reaction.0"].clone();
        state.reactions.insert("flow.reaction.2".into(), same);
        prepare(&mut draft, None, &state, &inputs).unwrap();
        assert!(rebind(&mut draft, &state)
            .unwrap_err()
            .to_string()
            .contains("ambiguous"));
    }

    #[test]
    fn nested_types_timers_and_code_state_are_explicit_without_false_independence() {
        let (mut draft, mut state, inputs) = crate::workflow_design::recovery_tests::fixture(false);
        state.ports.get_mut("flow.request").unwrap().ty = "list<option<int>>".into();
        state.timers.insert(
            "flow.tick".into(),
            crate::topology::TimerState {
                offset: 0,
                period: 1,
                instance: "flow".into(),
            },
        );
        for key in ["flow.reaction.0", "flow.reaction.2"] {
            let r = state.reactions.get_mut(key).unwrap();
            r.body = Some("example".into());
            r.triggers.push("flow.tick".into());
        }
        state.state_vars.insert(
            "flow.count".into(),
            crate::topology::StateVarState {
                ty: "int".into(),
                initial: json!(0),
                instance: "flow".into(),
            },
        );
        state.params.insert(
            "flow.size".into(),
            crate::topology::ParamState {
                ty: "int".into(),
                value: json!(2),
                instance: "flow".into(),
            },
        );
        prepare(&mut draft, None, &state, &inputs).unwrap();
        let contract = draft.steps[0].interface.as_ref().unwrap();
        assert_eq!(contract.owner, StepOwner::Code);
        assert_eq!(
            contract.inputs[0].schema["items"]["anyOf"][0]["type"],
            "integer"
        );
        assert_eq!(contract.inputs[1].schema["type"], "integer");
        assert_eq!(contract.inputs[1].wire_type, "timer_timestamp_ns");
        assert_eq!(contract.state_variables[0].name, "flow.count");
        assert_eq!(contract.parameters[0].name, "flow.size");
        assert_eq!(
            contract.shared_state_steps,
            vec![draft.steps[2].logical_id.clone()]
        );
        assert!(contract.known_initial_trigger_bytes.is_none());
    }

    #[test]
    fn duplicate_ids_and_unsupported_resource_caps_cannot_be_saved() {
        let (mut draft, state, inputs) = crate::workflow_design::recovery_tests::fixture(false);
        draft.steps[0].logical_id = "same".into();
        draft.steps[1].logical_id = "same".into();
        assert!(prepare(&mut draft, None, &state, &inputs).is_err());
        for resources in [
            StepResources {
                max_input_bytes: 0,
                ..StepResources::default()
            },
            StepResources {
                max_model_input_tokens: Some(100),
                ..StepResources::default()
            },
            StepResources {
                max_model_output_tokens: Some(100),
                ..StepResources::default()
            },
            StepResources {
                max_cost_usd: Some(0.0),
                ..StepResources::default()
            },
        ] {
            assert!(validate_resources(&resources).is_err());
        }
    }
}
