use super::*;
use std::collections::BTreeMap;

/// Deliberately synthetic policy fixture. This is never installed in a runtime
/// registry and is not evidence that any actual model or tool was contacted.
pub(crate) fn observe(profile: &mut ExecutionProfile, tool: &str, at: u64, backend: &str) {
    profile.tool_capabilities.push(ToolCapability {
        id: tool.into(),
        version: 1,
        native_name: format!("test-{tool}"),
        transport: ToolTransport::Native,
        authentication: ToolAuthentication::NotRequired,
        effect: ToolEffect::ReadOnly,
        permission_scope: vec!["test-fixture-only".into()],
        input_modalities: vec![Modality::Text],
        output_modalities: vec![Modality::Text],
        input_schema: None,
        output_schema: None,
        connectivity: None,
    });
    let profile_digest = profile.configuration_digest();
    // Every definition change invalidates previous observations. Tests adding
    // a second tool explicitly re-attest both against the complete new setup.
    for tool in &mut profile.tool_capabilities {
        tool.connectivity = Some(ToolConnectivityObservation {
            outcome: ToolConnectionOutcome::Available,
            connection_scope: "test-environment".into(),
            profile_digest: profile_digest.clone(),
            backend_version: backend.into(),
            runtime_contract: profiles::RUNTIME_CONTRACT_REVISION.into(),
            observed_at: at,
            reviewed_at: at,
            expires_at: at + 3_600,
            reviewer: "Synthetic fixture reviewer".into(),
            report: "test://tool-attempt".into(),
            report_digest: digest(b"synthetic tool attempt"),
            attempt_id: "synthetic-attempt".into(),
            limitations: "Policy fixture only, no live connectivity evidence".into(),
        });
    }
}

fn registry() -> profiles::Registry {
    profiles::Registry::load(tempfile::tempdir().unwrap().path()).unwrap()
}

#[test]
fn tool_observations_require_current_configuration_scope_and_runtime() {
    let mut r = registry();
    let p = &mut r.profiles[0];
    let versions = BTreeMap::from([(p.backend.clone(), "test-cli".into())]);
    let assess = |p: &ExecutionProfile, scope, versions: &BTreeMap<String, String>, at| {
        capabilities::assess_tool(p, "files", scope, versions, at).status
    };
    assert_eq!(assess(p, None, &versions, 100), ToolAccessStatus::Unknown);
    observe(p, "files", 100, "test-cli");
    p.validate().unwrap();
    let scope = Some("test-environment");
    assert_eq!(
        assess(p, scope, &versions, 101),
        ToolAccessStatus::ReviewedAvailable
    );
    assert_eq!(
        assess(p, None, &versions, 101),
        ToolAccessStatus::ConnectionScopeChanged
    );
    assert_eq!(
        assess(p, Some("another-account"), &versions, 101),
        ToolAccessStatus::ConnectionScopeChanged
    );
    assert_eq!(
        assess(p, scope, &BTreeMap::new(), 101),
        ToolAccessStatus::RuntimeChanged
    );
    assert_eq!(
        assess(p, scope, &versions, 99),
        ToolAccessStatus::NotYetValid
    );
    assert_eq!(
        assess(p, scope, &versions, 3_700),
        ToolAccessStatus::Expired
    );
    let original = p.clone();
    for case in 0..4 {
        let mut changed = original.clone();
        match case {
            0 => changed.model = "other-model".into(),
            1 => changed.tool_capabilities[0]
                .permission_scope
                .push("more-access".into()),
            2 => changed.tool_capabilities[0].version += 1,
            _ => changed.effort = None,
        }
        assert_eq!(
            assess(&changed, scope, &versions, 101),
            ToolAccessStatus::ConfigurationChanged
        );
    }
    p.label = "Updated display label".into();
    p.access = ProfileAccess::Verified;
    assert_eq!(
        assess(p, scope, &versions, 101),
        ToolAccessStatus::ReviewedAvailable
    );
    p.tool_capabilities[0]
        .connectivity
        .as_mut()
        .unwrap()
        .outcome = ToolConnectionOutcome::Unavailable;
    assert_eq!(
        assess(p, scope, &versions, 101),
        ToolAccessStatus::ReviewedUnavailable
    );
}

#[test]
fn invalid_tool_attestations_and_isolation_claims_are_rejected() {
    let mut p = registry().profiles.remove(0);
    observe(&mut p, "files", 100, "test-cli");
    for case in 0..10 {
        let mut bad = p.clone();
        let tool = &mut bad.tool_capabilities[0];
        let o = tool.connectivity.as_mut().unwrap();
        match case {
            0 => o.report_digest = "not-a-digest".into(),
            1 => o.reviewer.clear(),
            2 => o.attempt_id.clear(),
            3 => o.reviewed_at = 99,
            4 => o.expires_at = 100 + capabilities::MAX_CONNECTIVITY_AGE_SECONDS + 1,
            5 => tool.permission_scope.clear(),
            6 => tool.authentication = ToolAuthentication::Unknown,
            7 => tool.input_schema = Some(serde_json::json!([])),
            8 => bad.isolation = ExecutionIsolation::PortsOnly,
            _ => {
                let duplicate = tool.clone();
                bad.tool_capabilities.push(duplicate);
            }
        }
        assert!(bad.validate().is_err(), "invalid case {case}");
    }
}

#[test]
fn router_requires_all_shared_agent_tools_and_override_cannot_bypass_admission() {
    let mut r = registry();
    r.connection_scope = Some("test-environment".into());
    let mut step: StepSpec = serde_json::from_value(serde_json::json!({
        "reaction":"flow.work", "agent":"flow.worker", "operation":"structured_extraction",
        "purpose":"Extract supplied evidence", "acceptance_criteria":["Supported"], "required_tools":["files"]
    })).unwrap();
    let mut prefs = RoutingPreferences::default();
    prefs
        .overrides
        .insert(step.agent.clone(), "codex-quality".into());
    let backends = BTreeMap::from([(step.agent.clone(), "codex".into())]);
    let versions = BTreeMap::from([("codex".into(), "test-cli".into())]);
    let select = |r: &profiles::Registry, steps: &[StepSpec], at| {
        r.select(steps, &prefs, &backends, &BTreeMap::new(), &versions, at)
            .unwrap()
            .0
    };
    let unknown = select(&r, &[step.clone()], 101);
    assert_eq!(unknown[0].profile_id.as_deref(), Some("codex-quality"));
    assert!(capabilities::require_launchable(&unknown).is_err());
    observe(&mut r.profiles[0], "files", 100, "test-cli");
    capabilities::require_launchable(&select(&r, &[step.clone()], 101)).unwrap();
    let mut other = step.clone();
    other.reaction = "flow.other".into();
    other.required_tools = vec!["web".into()];
    let shared = select(&r, &[step.clone(), other.clone()], 101);
    assert_eq!(shared[0].tool_access.len(), 2);
    assert!(capabilities::require_launchable(&shared).is_err());
    observe(&mut r.profiles[0], "web", 100, "test-cli");
    capabilities::require_launchable(&select(&r, &[step.clone(), other], 101)).unwrap();
    assert!(capabilities::require_launchable(&select(&r, &[step.clone()], 3_700)).is_err());
    step.required_tools.clear();
    capabilities::require_launchable(&select(&r, &[step], 3_700)).unwrap();
}
