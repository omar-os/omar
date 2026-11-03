use super::*;

fn fixture(context: &Arc<Context_>) -> (String, PlanDraft, VmState, BTreeMap<String, Value>) {
    let program =
        include_str!("../../../tests/topology/src/ValidatedHandoff.omar").replace("Web", "Codex");
    let (mut draft, _, inputs) = wd::recovery_tests::fixture(false);
    draft.steps[0].required_tools = vec!["files".into()];
    let state = compile_preview(context, &program).unwrap();
    (program, draft, state, inputs)
}

#[test]
fn unresolved_tool_access_cannot_launch_even_after_explicit_profile_override() {
    let server = crate::serve::tests::test_server();
    let context = &server.context;
    let (program, draft, state, inputs) = fixture(context);
    let first = create(context, &program, &inputs, draft, &state).unwrap();
    assert!(!first
        .selections
        .iter()
        .find(|s| s.agent == "flow.writer")
        .unwrap()
        .capability_blockers
        .is_empty());
    let payload = json!({"id":first.id,"expected_revision":first.revision,"request_id":"explicit-tool-profile",
        "program":program,"inputs":inputs,"overrides":{"flow.writer":"codex-quality"}});
    let (status, value) = save(context, &serde_json::to_vec(&payload).unwrap());
    assert_eq!(status, 200, "{value}");
    let saved: WorkflowPlanRevision = serde_json::from_value(value).unwrap();
    let request: StartRunRequest =
        serde_json::from_value(json!({"program":program,"inputs":inputs,
        "plan":{"id":saved.id,"revision":saved.revision}}))
        .unwrap();
    let run_dir = tempfile::tempdir().unwrap();
    let error = admit(context, &request, &state, "blocked-tool", run_dir.path()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("execution capability is unresolved"),
        "{error:#}"
    );
    assert!(!run_dir.path().join("plan.json").exists());
    assert!(!run_dir.path().join("execution-receipt.json").exists());

    // A supplied-input-only redesign needs no external connection attestation.
    let mut supplied = saved.draft;
    supplied.steps[0].required_tools.clear();
    supplied.revises = Some(PlanRef {
        id: saved.id,
        revision: saved.revision,
    });
    let next = create(context, &program, &inputs, supplied, &state).unwrap();
    let request: StartRunRequest =
        serde_json::from_value(json!({"program":program,"inputs":inputs,
        "plan":{"id":next.id,"revision":next.revision}}))
        .unwrap();
    admit(context, &request, &state, "supplied-inputs", run_dir.path()).unwrap();
}

#[test]
fn bundle_transfers_tool_definitions_but_never_local_connection_evidence() {
    let origin = crate::serve::tests::test_server();
    let destination = crate::serve::tests::test_server();
    let mut registry = wd::profiles::Registry::load(&origin.context.omar_dir).unwrap();
    registry.connection_scope = Some("test-environment".into());
    wd::capabilities_tests::observe(
        &mut registry.profiles[0],
        "files",
        wd::now(),
        "synthetic-cli",
    );
    wd::access::tests::observe(&mut registry.profiles[0], wd::now(), "synthetic-cli");
    registry.profiles[0].access = ProfileAccess::Verified;
    fs::write(
        origin.context.omar_dir.join("execution-profiles.json"),
        serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();
    let (program, draft, state, inputs) = fixture(&origin.context);
    let plan = create(&origin.context, &program, &inputs, draft, &state).unwrap();
    assert!(plan.profiles["flow.writer"].tool_capabilities[0]
        .connectivity
        .is_some());
    let (status, mut bundle) = export(
        &origin.context,
        &serde_json::to_vec(&PlanRef {
            id: plan.id,
            revision: plan.revision,
        })
        .unwrap(),
    );
    assert_eq!(status, 200, "{bundle}");
    assert!(bundle["profiles"]["flow.writer"]["tool_capabilities"][0]["connectivity"].is_null());
    assert!(bundle["profiles"]["flow.writer"]["access_observation"].is_null());
    assert_eq!(bundle["profiles"]["flow.writer"]["access"], "unknown");

    // Destination has the same executable contract, but no tool observation.
    registry.connection_scope = Some("destination-environment".into());
    registry.profiles[0].tool_capabilities[0].connectivity = None;
    registry.profiles[0].access_observation = None;
    fs::write(
        destination.context.omar_dir.join("execution-profiles.json"),
        serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();
    // Even an explicitly included origin attestation cannot confer destination access.
    bundle["profiles"]["flow.writer"]["tool_capabilities"][0]["connectivity"] =
        serde_json::to_value(&plan.profiles["flow.writer"].tool_capabilities[0].connectivity)
            .unwrap();
    let (status, value) = import(
        &destination.context,
        &serde_json::to_vec(&json!({"request_id":"tool-import","bundle":bundle})).unwrap(),
    );
    assert_eq!(status, 200, "{value}");
    let imported: WorkflowPlanRevision = serde_json::from_value(value).unwrap();
    assert!(imported.profiles["flow.writer"].tool_capabilities[0]
        .connectivity
        .is_none());
    let decision = imported
        .selections
        .iter()
        .find(|s| s.agent == "flow.writer")
        .unwrap();
    assert_eq!(decision.tool_access[0].status, ToolAccessStatus::Unknown);
    assert_eq!(decision.model_access.status, ModelAccessStatus::Unknown);
    assert!(!decision.capability_blockers.is_empty());

    bundle["profiles"]["flow.writer"]["tool_capabilities"][0]["permission_scope"] =
        json!(["different-permissions"]);
    let (status, value) = import(
        &destination.context,
        &serde_json::to_vec(&json!({"request_id":"changed-tool-import","bundle":bundle})).unwrap(),
    );
    assert_eq!(status, 400);
    assert!(value["error"]
        .as_str()
        .unwrap()
        .contains("different model/settings/version"));
}
