use super::*;
const PROGRAM: &str = include_str!("../../../tests/topology/src/ValidatedHandoff.omar");

#[test]
fn saved_source_reorders_rebind_checks_and_keep_logical_identity_across_restart() {
    let server = crate::serve::tests::test_server();
    let (draft, state, inputs) = wd::recovery_tests::fixture(false);
    let original = create(&server.context, PROGRAM, &inputs, draft, &state).unwrap();
    let first = PROGRAM.find("    prompt writer").unwrap();
    let second = PROGRAM.find("    prompt checker(raw)").unwrap();
    let third = PROGRAM.find("    prompt repairer").unwrap();
    let reordered = format!(
        "{}{}{}{}",
        &PROGRAM[..first],
        &PROGRAM[second..third],
        &PROGRAM[first..second],
        &PROGRAM[third..]
    );
    let payload = json!({"id":original.id,"expected_revision":1,"request_id":"reorder","program":reordered,"inputs":inputs});
    let (status, value) = save(&server.context, &serde_json::to_vec(&payload).unwrap());
    assert_eq!(status, 200, "{value}");
    let saved: WorkflowPlanRevision = serde_json::from_value(value).unwrap();
    assert_eq!(
        saved.draft.steps[0].logical_id,
        original.draft.steps[0].logical_id
    );
    assert_eq!(saved.draft.steps[0].reaction, "flow.reaction.1");
    assert_eq!(
        saved.draft.steps[0].verification_reaction.as_deref(),
        Some("flow.reaction.0")
    );
    let reopened = store(&server.context).unwrap().read(&saved.id, 2).unwrap();
    let compiled = compile_preview(&server.context, &reordered).unwrap();
    wd::contracts::validate_saved(&reopened, &compiled).unwrap();
    let run: StartRunRequest = serde_json::from_value(
        json!({"program":reordered,"inputs":inputs,"plan":{"id":saved.id,"revision":2}}),
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (_, _, policies) =
        admit(&server.context, &run, &compiled, "reordered", dir.path()).unwrap();
    assert_eq!(
        policies["flow.reaction.1"].logical_id,
        original.draft.steps[0].logical_id
    );
    let mut regenerated = saved.draft.clone();
    regenerated.revises = Some(PlanRef {
        id: saved.id.clone(),
        revision: 2,
    });
    for step in &mut regenerated.steps {
        step.logical_id.clear();
    }
    let next = create(&server.context, &reordered, &inputs, regenerated, &compiled).unwrap();
    assert_eq!(
        next.draft.steps[0].logical_id,
        saved.draft.steps[0].logical_id
    );
}

#[test]
fn compiler_owns_interfaces_and_unexplained_handoffs_cannot_launch() {
    let server = crate::serve::tests::test_server();
    let (draft, state, inputs) = wd::recovery_tests::fixture(false);
    let first = create(&server.context, PROGRAM, &inputs, draft, &state).unwrap();
    let mut forged = first.draft.clone();
    forged.steps[0].interface.as_mut().unwrap().outputs.clear();
    let saved = create(&server.context, PROGRAM, &inputs, forged, &state).unwrap();
    assert!(!saved.draft.steps[0]
        .interface
        .as_ref()
        .unwrap()
        .outputs
        .is_empty());
    let mut tampered = saved.clone();
    tampered.draft.steps[0]
        .interface
        .as_mut()
        .unwrap()
        .dependencies
        .push("fiction".into());
    assert!(wd::contracts::validate_saved(&tampered, &state).is_err());
    let mut missing = saved.draft.clone();
    missing.steps[0].decomposition_reason = None;
    let review = create(&server.context, PROGRAM, &inputs, missing, &state).unwrap();
    let run:StartRunRequest=serde_json::from_value(json!({"program":PROGRAM,"inputs":inputs,"plan":{"id":review.id,"revision":review.revision}})).unwrap();
    let dir = tempfile::tempdir().unwrap();
    assert!(admit(
        &server.context,
        &run,
        &state,
        "missing-rationale",
        dir.path()
    )
    .unwrap_err()
    .to_string()
    .contains("benefit"));
    let mut unsupported = review.draft;
    unsupported.steps[0].resources.max_cost_usd = Some(0.01);
    assert!(
        create(&server.context, PROGRAM, &inputs, unsupported, &state)
            .unwrap_err()
            .to_string()
            .contains("unsupported")
    );
}
