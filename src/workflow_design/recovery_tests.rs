use super::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(crate) fn fixture(
    human: bool,
) -> (PlanDraft, crate::topology::VmState, BTreeMap<String, Value>) {
    let mut source = include_str!("../../tests/topology/src/ValidatedHandoff.omar").to_owned();
    if human {
        source = source.replace("repairer : Web]", "repairer : Web, human : Web]")
            .replace("output blocked : string", "output blocked : string\n    action needs_review : string")
            .replace("(result | blocked)", "(result | needs_review)")
            .replace("to blocked.", "to needs_review.")
            .replace("\n}\nmain", "\n    prompt human(needs_review) -> (result | blocked) task(\"Review the unresolved draft\", \"Make a human decision after the repair budget is exhausted.\") \"Review $(needs_review). Write accepted JSON to result or explain the rejection in blocked.\"\n}\nmain");
    }
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("Handoff.omar");
    std::fs::write(&file, source).unwrap();
    let state = crate::topology::verify(&crate::topology::load_program(&file).unwrap()).unwrap();
    let checks = json!([{"kind":"json_object","port":"flow.result","required_fields":["value"]}]);
    let mut steps: Vec<Value> = state
        .reactions
        .iter()
        .map(|(id, r)| {
            json!({
                "reaction":id,"agent":r.agent,"operation":"structured_extraction","purpose":r.title,
                "acceptance_criteria":["The value is supported by the supplied request"],
                "decomposition_reason":"Separate draft creation, protected checking and the single repair path so rejected output cannot reach the result.",
                "execution":{"effect":"repeatable","max_invocations":1,"deadline_ms":2000}
            })
        })
        .collect();
    steps[0]["operation"] = json!("evidence_synthesis");
    steps[0]["verification_reaction"] = json!("flow.reaction.1");
    steps[2]["operation"] = json!("evidence_synthesis");
    steps[2]["verification_reaction"] = json!("flow.reaction.3");
    for index in [1, 3] {
        steps[index]["operation"] = json!("claim_verification");
        steps[index]["output_checks"] = checks.clone();
    }
    steps[0]["recovery"] = json!({
        "retry_port":"flow.retry","repair_reaction":"flow.reaction.2","recheck_reaction":"flow.reaction.3",
        "accepted_port":"flow.result","failure_port":if human {"flow.needs_review"} else {"flow.blocked"},
        "max_repair_attempts":1,
        "human_review":if human {json!({"reaction":"flow.reaction.4","blocked_port":"flow.blocked"})} else {Value::Null}
    });
    if human {
        steps[4]["execution"]["effect"] = json!("human_decision");
        steps[4]["operation"] = json!("claim_verification");
        steps[4]["output_checks"] = checks;
    }
    let draft = serde_json::from_value(json!({
        "intent":{"outcome":"A checked draft","acceptance_criteria":["Supported value or an explicit blocked result"]},
        "research":{"summary":"Deterministic test fixture, not a quality measurement","sources":[]},"steps":steps
    })).unwrap();
    (
        draft,
        state,
        BTreeMap::from([("flow.request".into(), json!("owned test data"))]),
    )
}

#[test]
fn recovery_contract_rejects_unsafe_retry_bypass_cycles_and_lost_checks() {
    let (draft, state, inputs) = fixture(false);
    recovery::compile(&draft, &state, &inputs).unwrap();
    for case in 0..7 {
        let mut invalid = draft.clone();
        match case {
            0 => invalid.steps[0].execution.effect = StepEffect::ExternalAction,
            1 => invalid.steps[2].execution.effect = StepEffect::Unknown,
            2 => {
                invalid.steps[0]
                    .recovery
                    .as_mut()
                    .unwrap()
                    .max_repair_attempts = 2
            }
            3 => invalid.steps[2].execution.max_invocations = 2,
            4 => invalid.steps[3].output_checks.clear(),
            5 => invalid.steps[2].verification_reaction = None,
            _ => {
                invalid.steps[0].recovery.as_mut().unwrap().recheck_reaction = "flow.missing".into()
            }
        }
        assert!(
            recovery::compile(&invalid, &state, &inputs).is_err(),
            "accepted invalid recovery case {case}"
        );
    }
    let mut bypass = state.clone();
    bypass
        .reactions
        .get_mut("flow.reaction.2")
        .unwrap()
        .triggers
        .push("flow.request".into());
    assert!(recovery::compile(&draft, &bypass, &inputs)
        .unwrap_err()
        .to_string()
        .contains("bypasses"));
    let mut cycle = state.clone();
    cycle.connections.push(crate::topology::ConnectionState {
        source: "flow.repaired".into(),
        target: "flow.request".into(),
        delay: Some(0),
    });
    assert!(recovery::compile(&draft, &cycle, &inputs)
        .unwrap_err()
        .to_string()
        .contains("cycle"));
    let mut nonexclusive = state.clone();
    nonexclusive
        .reactions
        .get_mut("flow.reaction.1")
        .unwrap()
        .contract = "flow.result , flow.retry".into();
    assert!(recovery::compile(&draft, &nonexclusive, &inputs).is_err());
    let injected = BTreeMap::from([("flow.retry".into(), json!("skip the initial check"))]);
    assert!(recovery::compile(&draft, &state, &injected).is_err());
    let mut rogue = state.clone();
    rogue.reactions.insert(
        "flow.rogue".into(),
        state.reactions["flow.reaction.3"].clone(),
    );
    assert!(recovery::compile(&draft, &rogue, &inputs).is_err());
}

#[test]
fn recovery_human_escalation_requires_web_and_cannot_override_protected_checks() {
    let (mut draft, mut state, inputs) = fixture(true);
    recovery::compile(&draft, &state, &inputs).unwrap();
    state.agents.get_mut("flow.human").unwrap().backend = "Codex".into();
    assert!(recovery::compile(&draft, &state, &inputs).is_err());
    state.agents.get_mut("flow.human").unwrap().backend = "Web".into();
    draft.steps[4].output_checks.clear();
    assert!(recovery::compile(&draft, &state, &inputs).is_err());
}
