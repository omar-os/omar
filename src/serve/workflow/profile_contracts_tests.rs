use super::*;

#[test]
fn saved_repair_binding_requires_registry_permission_and_a_bounded_explicit_graph() {
    let server = crate::serve::tests::test_server();
    let context = &server.context;
    let program =
        include_str!("../../../tests/topology/src/ValidatedHandoff.omar").replace("Web", "Codex");
    let (mut draft, _, inputs) = wd::recovery_tests::fixture(false);
    draft.steps[0].operation = Operation::StructuredExtraction;
    draft.preferences.overrides = BTreeMap::from([
        ("flow.writer".into(), "codex-economy".into()),
        ("flow.repairer".into(), "codex-quality".into()),
    ]);
    let state = compile_preview(context, &program).unwrap();
    let mut registry = wd::profiles::Registry::load(&context.omar_dir).unwrap();
    for profile in &mut registry.profiles {
        profile.isolation = ExecutionIsolation::PortsOnly;
        profile.tools = vec!["workflow_ports".into()];
        profile.identity.declared_provider = Some("synthetic-provider".into());
    }
    let install = |registry: &wd::profiles::Registry| {
        fs::write(
            context.omar_dir.join("execution-profiles.json"),
            serde_json::to_vec(registry).unwrap(),
        )
        .unwrap()
    };
    install(&registry);
    let first = create(context, &program, &inputs, draft, &state).unwrap();
    let request = |plan: &WorkflowPlanRevision| {
        serde_json::from_value::<StartRunRequest>(json!({
            "program":program,"inputs":inputs,"plan":{"id":plan.id,"revision":plan.revision}
        }))
        .unwrap()
    };
    let run_dir = tempfile::tempdir().unwrap();
    assert!(admit(
        context,
        &request(&first),
        &state,
        "not-allowed",
        run_dir.path()
    )
    .unwrap_err()
    .to_string()
    .contains("does not permit a different repair model"));

    registry
        .profiles
        .iter_mut()
        .find(|p| p.id == "codex-economy")
        .unwrap()
        .fallback = Some(ProfileFallbackPolicy {
        profile_id: "codex-quality".into(),
        trigger: FallbackTrigger::ValidationRejected,
        reason: "Synthetic one-repair policy".into(),
        max_attempts: 1,
        max_deadline_ms: 2000,
        max_cost_usd: None,
    });
    install(&registry);
    // Registry edits alone cannot change an already reviewed run binding.
    assert!(admit(
        context,
        &request(&first),
        &state,
        "needs-review",
        run_dir.path()
    )
    .unwrap_err()
    .to_string()
    .contains("revision conflict"));
    let payload = json!({"id":first.id,"expected_revision":first.revision,"request_id":"permit-repair",
        "program":program,"inputs":inputs,"overrides":first.draft.preferences.overrides});
    let (status, value) = save(context, &serde_json::to_vec(&payload).unwrap());
    assert_eq!(status, 200, "{value}");
    let saved: WorkflowPlanRevision = serde_json::from_value(value).unwrap();
    let route = &saved
        .selections
        .iter()
        .find(|d| d.agent == "flow.writer")
        .unwrap()
        .repair_routes[0];
    assert!(route.permitted);
    assert_eq!(route.profile_id.as_deref(), Some("codex-quality"));
    let (bindings, _, policies) =
        admit(context, &request(&saved), &state, "allowed", run_dir.path()).unwrap();
    assert_eq!(bindings["flow.writer"].id, "codex-economy");
    assert_eq!(bindings["flow.repairer"].id, "codex-quality");
    assert_eq!(policies["flow.reaction.2"].execution.max_invocations, 1);
    assert_eq!(
        policies["flow.reaction.2"].execution.deadline_ms,
        Some(2000)
    );
    let receipt: ExecutionReceipt =
        serde_json::from_slice(&fs::read(run_dir.path().join("execution-receipt.json")).unwrap())
            .unwrap();
    assert!(receipt.cost_usd.is_none());
    assert_eq!(receipt.profiles["flow.repairer"].id, "codex-quality");
}

#[test]
fn profile_facts_refresh_locally_and_export_drops_prices_quota_and_limit_claims() {
    let server = crate::serve::tests::test_server();
    let context = &server.context;
    let mut registry = wd::profiles::Registry::load(&context.omar_dir).unwrap();
    let p = &mut registry.profiles[0];
    let binding = p.configuration_digest();
    let now = wd::now();
    let source = ReviewedProfileSource {
        source: "test://fixture".into(),
        source_digest: wd::digest(b"test"),
        locator: "Fixture only".into(),
        observed_at: now,
        reviewed_at: now,
        expires_at: now + 60,
        reviewer: "Synthetic reviewer".into(),
        conditions: "Not real model limits or economics".into(),
    };
    p.advertised_limits = Some(AdvertisedModelLimits {
        profile_digest: binding.clone(),
        context_window_tokens: Some(8000),
        max_output_tokens: Some(2000),
        source: source.clone(),
    });
    p.economics = Some(ProfileEconomics {
        profile_digest: binding,
        billing_model: BillingModel::Subscription,
        subscription_plan: Some("Test plan".into()),
        prices: vec![],
        quota: None,
        connection_scope: None,
        source,
    });
    fs::write(
        context.omar_dir.join("execution-profiles.json"),
        serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();
    let program =
        include_str!("../../../tests/topology/src/ValidatedHandoff.omar").replace("Web", "Codex");
    let (draft, _, inputs) = wd::recovery_tests::fixture(false);
    let state = compile_preview(context, &program).unwrap();
    let plan = create(context, &program, &inputs, draft, &state).unwrap();
    assert!(plan
        .selections
        .iter()
        .all(|s| s.facts.advertised_limits == ProfileFactStatus::Current));
    let reference = PlanRef {
        id: plan.id.clone(),
        revision: plan.revision,
    };
    let (status, bundle) = export(context, &serde_json::to_vec(&reference).unwrap());
    assert_eq!(status, 200, "{bundle}");
    assert!(bundle["profiles"]["flow.writer"]["advertised_limits"].is_null());
    assert!(bundle["profiles"]["flow.writer"]["economics"].is_null());
    // A destination/source refresh may update descriptive facts without
    // changing executable model settings or importing origin price authority.
    registry.profiles[0].economics = None;
    registry.profiles[0].advertised_limits = None;
    fs::write(
        context.omar_dir.join("execution-profiles.json"),
        serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();
    let (status, value) = import(
        context,
        &serde_json::to_vec(&json!({"request_id":"facts-import","bundle":bundle})).unwrap(),
    );
    assert_eq!(status, 200, "{value}");
    let imported: WorkflowPlanRevision = serde_json::from_value(value).unwrap();
    assert!(imported
        .selections
        .iter()
        .all(|s| s.facts.economics == ProfileFactStatus::Unknown));
}
