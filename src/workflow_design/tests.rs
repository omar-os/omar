use super::*;
use serde_json::json;
use std::collections::BTreeMap;

fn registry() -> profiles::Registry {
    profiles::Registry::load(tempfile::tempdir().unwrap().path()).unwrap()
}

fn scope() -> QualificationScope {
    QualificationScope {
        task_family: "grounded-documents".into(),
        task_slice: "supplied-fact-extraction".into(),
        contract_version: "v1".into(),
        complexity: TaskComplexity::Routine,
        consequence: TaskConsequence::Low,
        input_modalities: vec![Modality::Text],
        output_modalities: vec![Modality::StructuredData],
    }
}

fn contracts() -> BTreeMap<String, String> {
    BTreeMap::from([("flow.work".into(), digest(b"test contract"))])
}

/// Synthetic record for policy tests only; never installed in the shipped
/// catalog and not a measurement of any real model.
fn qualify(registry: &mut profiles::Registry) {
    registry.connection_scope = Some("test-environment".into());
    let observed_at = registry.evidence[0].reviewed_at;
    let profile = registry
        .profiles
        .iter_mut()
        .find(|p| p.id == "claude-economy")
        .unwrap();
    profile.access = ProfileAccess::Verified;
    capabilities_tests::observe(profile, "files", observed_at, "test-cli-1");
    access::tests::observe(profile, observed_at, "test-cli-1");
    let e = &mut registry.evidence[0];
    e.id = "test-local-extraction".into();
    e.source_kind = EvidenceSourceKind::LocalEvaluation;
    e.author = Some("Test evaluator".into());
    e.source = "test://report".into();
    e.locally_qualified = true;
    e.qualification = Some(LocalQualification {
        scope: scope(),
        step_contract_digest: digest(b"test contract"),
        profile_digest: profile.configuration_digest(),
        runtime_contract: profiles::RUNTIME_CONTRACT_REVISION.into(),
        backend_version: "test-cli-1".into(),
        benchmark: "Policy fixture".into(),
        benchmark_version: "v1".into(),
        dataset_digest: digest(b"test dataset"),
        harness_digest: digest(b"test harness"),
        grader_digest: digest(b"test grader"),
        protocol_digest: digest(b"frozen test protocol"),
        report: "test://report".into(),
        report_digest: digest(b"test report"),
        held_out: true,
        labels_reviewed: true,
        semantic_reviewed: true,
        reviewer: "Test reviewer".into(),
        reviewed_at: e.reviewed_at,
        total_cases: 100,
        accepted_cases: 95,
        independent_groups: 30,
        minimum_cases: 100,
        minimum_independent_groups: 30,
        required_acceptance_rate: 0.8,
        acceptance_rate_lower_bound: 0.9,
        uncertainty_method: "Synthetic test bound; not a measured estimate".into(),
    });
}

fn step() -> StepSpec {
    StepSpec {
        logical_id: String::new(),
        decomposition_reason: Some(
            "Extract relevant evidence before independent verification".into(),
        ),
        interface: None,
        resources: StepResources::default(),
        reaction: "flow.work".into(),
        agent: "flow.worker".into(),
        operation: Operation::StructuredExtraction,
        purpose: "Extract supplied evidence".into(),
        acceptance_criteria: vec!["Every claim cites a source".into()],
        required_tools: vec!["files".into()],
        evidence_ids: vec![],
        verification_reaction: None,
        output_checks: vec![],
        qualification_scope: None,
        execution: StepExecution::default(),
        recovery: None,
    }
}

fn plan() -> WorkflowPlanRevision {
    WorkflowPlanRevision {
        schema_version: 1,
        id: "plan-1".into(),
        revision: 0,
        conversation_id: "chat-1".into(),
        created_at: 0,
        request_id: "save-1".into(),
        program: "source-v1".into(),
        source_digest: digest(b"source-v1"),
        step_contracts: BTreeMap::new(),
        inputs: BTreeMap::new(),
        draft: PlanDraft {
            revises: None,
            intent: IntentRevision {
                outcome: "Grounded brief".into(),
                message_sequences: vec![],
                constraints: vec![],
                requirements: vec![],
                assumptions: vec![],
                acceptance_criteria: vec!["Supported claims".into()],
            },
            research: ResearchBrief {
                summary: "Supplied documents".into(),
                sources: vec![],
                gaps: vec![],
                capability_evidence_ids: vec![],
                issues: vec![],
                session_id: None,
                usage: None,
            },
            steps: vec![step()],
            questions: vec![],
            preferences: RoutingPreferences::default(),
        },
        selections: vec![],
        profiles: BTreeMap::new(),
        evidence_revision: "test".into(),
        routing_policy_revision: profiles::ROUTING_POLICY_REVISION.into(),
    }
}

#[test]
fn immutable_revisions_survive_reopen_and_reject_stale_cross_chat_and_path_access() {
    let temp = tempfile::tempdir().unwrap();
    let store = store::Store::new(temp.path(), "chat-1").unwrap();
    let first = store.save(plan(), 0).unwrap();
    assert_eq!(first.revision, 1);
    assert_eq!(store.save(plan(), 0).unwrap(), first);
    let mut next = first.clone();
    next.request_id = "save-2".into();
    next.program = "source-v2".into();
    assert!(store
        .save(next.clone(), 0)
        .unwrap_err()
        .to_string()
        .contains("conflict"));
    let second = store.save(next, 1).unwrap();
    assert_eq!(store.save(plan(), 0).unwrap(), first);
    assert_eq!(second.revision, 2);
    let reopened = store::Store::new(temp.path(), "chat-1").unwrap();
    assert_eq!(reopened.read("plan-1", 1).unwrap().program, "source-v1");
    assert_eq!(reopened.latest("plan-1").unwrap().unwrap(), second);
    assert!(store::Store::new(temp.path(), "chat-2")
        .unwrap()
        .read("plan-1", 1)
        .is_err());
    assert!(reopened.latest("../chat-2").is_err());
    let mut reused = first;
    reused.program = "changed".into();
    reused.request_id = "save-2".into();
    assert!(store.save(reused, 2).is_err());
}

#[test]
fn router_respects_tools_backend_overrides_and_local_evidence_expiry() {
    let mut r = registry();
    let backends = BTreeMap::from([("flow.worker".into(), "claude".into())]);
    let pref = RoutingPreferences::default();
    let (decisions, _) = r
        .select(
            &[step()],
            &pref,
            &backends,
            &contracts(),
            &BTreeMap::new(),
            now(),
        )
        .unwrap();
    assert_eq!(decisions[0].profile_id.as_deref(), Some("claude-quality"));
    assert!(decisions[0].provisional);
    let mut explicit = pref.clone();
    explicit
        .overrides
        .insert("flow.worker".into(), "claude-economy".into());
    assert_eq!(
        r.select(
            &[step()],
            &explicit,
            &backends,
            &contracts(),
            &BTreeMap::new(),
            now()
        )
        .unwrap()
        .0[0]
            .profile_id
            .as_deref(),
        Some("claude-economy")
    );
    let mut impossible = step();
    impossible.required_tools.push("video-renderer".into());
    assert!(r
        .select(
            &[impossible],
            &pref,
            &backends,
            &contracts(),
            &BTreeMap::new(),
            now()
        )
        .unwrap()
        .0[0]
        .profile_id
        .is_none());
    let at = r.evidence[0].reviewed_at + 1;
    let versions = BTreeMap::from([("claude".into(), "test-cli-1".into())]);
    qualify(&mut r);
    let mut scoped = step();
    scoped.qualification_scope = Some(scope());
    assert_eq!(
        r.select(
            &[scoped.clone()],
            &pref,
            &backends,
            &contracts(),
            &versions,
            at
        )
        .unwrap()
        .0[0]
            .profile_id
            .as_deref(),
        Some("claude-economy")
    );
    assert_eq!(
        r.select(
            &[scoped],
            &pref,
            &backends,
            &contracts(),
            &versions,
            r.evidence[0].expires_at
        )
        .unwrap()
        .0[0]
            .profile_id
            .as_deref(),
        Some("claude-quality")
    );
    let mut restricted = pref;
    restricted.allowed_backends = vec!["codex".into()];
    assert!(r
        .select(
            &[step()],
            &restricted,
            &backends,
            &contracts(),
            &versions,
            at
        )
        .unwrap()
        .0[0]
        .profile_id
        .is_none());
}

#[test]
fn a_verified_flag_cannot_qualify_economy_without_current_native_access_evidence() {
    let mut r = registry();
    qualify(&mut r);
    let at = r.evidence[0].reviewed_at + 1;
    let mut s = step();
    s.qualification_scope = Some(scope());
    let backends = BTreeMap::from([("flow.worker".into(), "claude".into())]);
    let versions = BTreeMap::from([("claude".into(), "test-cli-1".into())]);
    let selected = |r: &profiles::Registry, prefs: &RoutingPreferences| {
        r.select(&[s.clone()], prefs, &backends, &contracts(), &versions, at)
            .unwrap()
            .0
    };
    assert_eq!(
        selected(&r, &RoutingPreferences::default())[0]
            .profile_id
            .as_deref(),
        Some("claude-economy")
    );
    let profile = r
        .profiles
        .iter_mut()
        .find(|p| p.id == "claude-economy")
        .unwrap();
    let observation = profile.access_observation.take().unwrap();
    assert_eq!(profile.access, ProfileAccess::Verified);
    assert_eq!(
        selected(&r, &RoutingPreferences::default())[0]
            .profile_id
            .as_deref(),
        Some("claude-quality")
    );
    let profile = r
        .profiles
        .iter_mut()
        .find(|p| p.id == "claude-economy")
        .unwrap();
    profile.access_observation = Some(observation);
    profile.access_observation.as_mut().unwrap().outcome = ModelAccessOutcome::Unavailable;
    let explicit = RoutingPreferences {
        allowed_backends: vec![],
        overrides: BTreeMap::from([("flow.worker".into(), "claude-economy".into())]),
    };
    assert!(selected(&r, &explicit)[0].profile_id.is_none());
}

#[test]
fn qualification_rejects_provider_flags_unreviewed_labels_and_incomplete_results() {
    let mut r = registry();
    r.evidence[0].locally_qualified = true;
    assert!(r
        .validate()
        .unwrap_err()
        .to_string()
        .contains("reviewed local evaluation"));
    qualify(&mut r);
    r.validate().unwrap();
    for case in 0..9 {
        let mut invalid = r.clone();
        let q = invalid.evidence[0].qualification.as_mut().unwrap();
        match case {
            0 => q.held_out = false,
            1 => q.labels_reviewed = false,
            2 => q.semantic_reviewed = false,
            3 => q.independent_groups = 1,
            4 => q.total_cases = 99,
            5 => q.acceptance_rate_lower_bound = 0.79,
            6 => q.acceptance_rate_lower_bound = 1.0,
            7 => q.report_digest = "unhashed".into(),
            _ => q.reviewer.clear(),
        }
        assert!(
            invalid.validate().is_err(),
            "accepted invalid qualification case {case}"
        );
    }
    // Failed studies may be retained as evidence, but never as qualification.
    r.evidence[0].locally_qualified = false;
    r.evidence[0]
        .qualification
        .as_mut()
        .unwrap()
        .acceptance_rate_lower_bound = 0.5;
    r.validate().unwrap();
}

#[test]
fn qualification_is_bound_to_scope_configuration_runtime_and_every_shared_step() {
    let mut r = registry();
    qualify(&mut r);
    let at = r.evidence[0].reviewed_at + 1;
    let backends = BTreeMap::from([("flow.worker".into(), "claude".into())]);
    let versions = BTreeMap::from([("claude".into(), "test-cli-1".into())]);
    let preferences = RoutingPreferences::default();
    let mut scoped = step();
    scoped.qualification_scope = Some(scope());
    let selected =
        |r: &profiles::Registry, steps: &[StepSpec], versions: &BTreeMap<String, String>| {
            r.select(steps, &preferences, &backends, &contracts(), versions, at)
                .unwrap()
                .0[0]
                .clone()
        };
    assert_eq!(
        selected(&r, &[scoped.clone()], &versions)
            .profile_id
            .as_deref(),
        Some("claude-economy")
    );
    let changed_contracts = BTreeMap::from([("flow.work".into(), digest(b"changed prompt"))]);
    assert_eq!(
        r.select(
            &[scoped.clone()],
            &preferences,
            &backends,
            &changed_contracts,
            &versions,
            at
        )
        .unwrap()
        .0[0]
            .profile_id
            .as_deref(),
        Some("claude-quality")
    );
    assert_eq!(
        selected(&r, &[step()], &versions).profile_id.as_deref(),
        Some("claude-quality")
    );
    assert_eq!(
        selected(&r, &[scoped.clone()], &BTreeMap::new())
            .profile_id
            .as_deref(),
        Some("claude-quality")
    );
    let changed_cli = BTreeMap::from([("claude".into(), "test-cli-2".into())]);
    assert_eq!(
        selected(&r, &[scoped.clone()], &changed_cli)
            .profile_id
            .as_deref(),
        Some("claude-quality")
    );
    for case in 0..4 {
        let mut other = scoped.clone();
        let scope = other.qualification_scope.as_mut().unwrap();
        match case {
            0 => scope.task_family = "other-family".into(),
            1 => scope.task_slice = "other-slice".into(),
            2 => scope.contract_version = "v2".into(),
            _ => scope.complexity = TaskComplexity::Ambiguous,
        }
        assert_eq!(
            selected(&r, &[other], &versions).profile_id.as_deref(),
            Some("claude-quality")
        );
    }
    let mut shared = scoped.clone();
    shared.reaction = "flow.other".into();
    shared.qualification_scope.as_mut().unwrap().task_slice = "unqualified-slice".into();
    assert_eq!(
        selected(&r, &[scoped.clone(), shared], &versions)
            .profile_id
            .as_deref(),
        Some("claude-quality")
    );
    let mut video = scoped.clone();
    video
        .qualification_scope
        .as_mut()
        .unwrap()
        .output_modalities = vec![Modality::Video];
    assert!(selected(&r, &[video], &versions).profile_id.is_none());
    let mut changed = r.clone();
    changed
        .profiles
        .iter_mut()
        .find(|p| p.id == "claude-economy")
        .unwrap()
        .effort = Some("high".into());
    assert_eq!(
        selected(&changed, &[scoped.clone()], &versions)
            .profile_id
            .as_deref(),
        Some("claude-quality")
    );
    // Even qualified results for consequential work do not silently enable
    // economy routing under the initial policy. A deliberate override can.
    scoped.qualification_scope.as_mut().unwrap().consequence = TaskConsequence::High;
    r.evidence[0].qualification.as_mut().unwrap().scope =
        scoped.qualification_scope.clone().unwrap();
    assert_eq!(
        selected(&r, &[scoped.clone()], &versions)
            .profile_id
            .as_deref(),
        Some("claude-quality")
    );
    let mut explicit = preferences;
    explicit
        .overrides
        .insert("flow.worker".into(), "claude-economy".into());
    assert_eq!(
        r.select(&[scoped], &explicit, &backends, &contracts(), &versions, at)
            .unwrap()
            .0[0]
            .profile_id
            .as_deref(),
        Some("claude-economy")
    );
}

#[test]
fn qualification_fingerprint_changes_with_compiled_prompt_schema_and_acceptance_checks() {
    let mut plan = plan();
    let mut state: crate::topology::VmState = serde_json::from_value(json!({
        "version":1,"team":"Flow","agents":{"flow.worker":{"backend":"Codex"}},
        "ports":{"flow.input":{"kind":"input","type":"string"},"flow.raw":{"kind":"output","type":"string"}},"connections":[],
        "reactions":{"flow.work":{"order":0,"agent":"flow.worker","triggers":["flow.input"],"effects":["flow.raw"],"contract":"flow.raw","prompt":"Extract $(flow.input)"}}
    })).unwrap();
    let original = validation::step_contract_digests(&plan.draft, &state).unwrap();
    state.reactions.get_mut("flow.work").unwrap().prompt = "Invent a result".into();
    assert_ne!(
        original,
        validation::step_contract_digests(&plan.draft, &state).unwrap()
    );
    state.reactions.get_mut("flow.work").unwrap().prompt = "Extract $(flow.input)".into();
    state.ports.get_mut("flow.raw").unwrap().ty = "int".into();
    assert_ne!(
        original,
        validation::step_contract_digests(&plan.draft, &state).unwrap()
    );
    state.ports.get_mut("flow.raw").unwrap().ty = "string".into();
    assert_eq!(
        original,
        validation::step_contract_digests(&plan.draft, &state).unwrap()
    );
    plan.draft.steps[0].acceptance_criteria = vec!["A different criterion".into()];
    assert_ne!(
        original,
        validation::step_contract_digests(&plan.draft, &state).unwrap()
    );
}

#[test]
fn native_settings_survive_codex_conversion_and_reject_injection_and_backend_mismatch() {
    let r = registry();
    let p = r.profiles.iter().find(|p| p.id == "codex-quality").unwrap();
    let command = p.command("Codex").unwrap();
    assert!(crate::backend::codex::codex_refuses_to_attach(&command));
    let (native, _) = crate::backend::codex::codex_exec_command(&command).unwrap();
    let words = shlex::split(&native).unwrap();
    assert!(words.windows(2).any(|w| w == ["--model", p.model.as_str()]));
    assert!(words.contains(&"model_reasoning_effort=\"high\"".to_string()));
    let resumed = format!("{command} resume abc");
    let (native, session) = crate::backend::codex::codex_exec_command(&resumed).unwrap();
    assert_eq!(session.as_deref(), Some("abc"));
    assert!(native.contains(&p.model));
    assert!(p.command("ClaudeCode").is_err());
    let mut bad = p.clone();
    bad.model = "model; touch /tmp/injected".into();
    assert!(bad.command("Codex").is_err());
    bad = p.clone();
    bad.effort = Some("invented".into());
    assert!(bad.validate().is_err());
    let claude = r
        .profiles
        .iter()
        .find(|p| p.id == "claude-quality")
        .unwrap();
    assert!(claude
        .command("ClaudeCode")
        .unwrap()
        .contains("--effort 'high'"));
    let mut restricted = p.clone();
    restricted.isolation = ExecutionIsolation::PortsOnly;
    restricted.tools = vec!["workflow_ports".into()];
    let command =
        crate::backend::codex::ensure_codex_runtime_flags(&restricted.command("Codex").unwrap());
    assert!(command.contains("--sandbox read-only"));
    assert!(command.contains("--ignore-user-config"));
    assert!(!command.contains("--dangerously-bypass"));
}

#[test]
fn gate_rejects_raw_bypass_and_unreviewed_assumptions() {
    let mut p = plan();
    p.draft.steps[0].operation = Operation::EvidenceSynthesis;
    p.draft.steps[0].verification_reaction = Some("flow.check".into());
    let mut checker = step();
    checker.reaction = "flow.check".into();
    checker.agent = "flow.checker".into();
    checker.operation = Operation::ClaimVerification;
    p.draft.steps.push(checker);
    let r = registry();
    let profile = r.profiles.iter().find(|p| p.id == "codex-quality").unwrap();
    p.profiles.insert("flow.worker".into(), profile.clone());
    p.profiles.insert("flow.checker".into(), profile.clone());
    let mut state:crate::topology::VmState=serde_json::from_value(json!({
        "version":1,"team":"Flow","agents":{"flow.worker":{"backend":"Codex"},"flow.checker":{"backend":"Codex"}},"ports":{},"connections":[],
        "reactions":{"flow.work":{"order":0,"agent":"flow.worker","triggers":["flow.input"],"effects":["flow.raw"],"contract":"flow.raw","prompt":"synthesize"},
        "flow.check":{"order":1,"agent":"flow.checker","triggers":["flow.raw"],"effects":["flow.accepted"],"contract":"flow.accepted","prompt":"check"}}
    })).unwrap();
    validation::validate_plan(&p, &state, true).unwrap();
    state
        .reactions
        .get_mut("flow.work")
        .unwrap()
        .triggers
        .push("flow.raw".into());
    assert!(validation::validate_plan(&p, &state, true)
        .unwrap_err()
        .to_string()
        .contains("bypasses"));
    state.reactions.get_mut("flow.work").unwrap().triggers.pop();
    state.ports.insert(
        "flow.raw".into(),
        serde_json::from_value(json!({"kind":"output","type":"string"})).unwrap(),
    );
    assert!(validation::validate_plan(&p, &state, true)
        .unwrap_err()
        .to_string()
        .contains("exposed output"));
    state.ports.clear();
    p.draft.intent.assumptions.push(Assumption {
        id: "a1".into(),
        text: "Audience is internal".into(),
        provenance: "designer inference".into(),
        origin: None,
        confirmation: None,
        accepted: false,
    });
    assert!(validation::validate_plan(&p, &state, true).is_err());
    validation::validate_plan(&p, &state, false).unwrap();
    p.draft.questions.push(
        serde_json::from_value(json!({
            "id":"audience","question":"Who reads the brief?","reason":"Changes the synthesis.",
            "target_reactions":["flow.work"]
        }))
        .unwrap(),
    );
    validation::validate_plan(&p, &state, false).unwrap();
    p.draft.questions[0].target_reactions = vec!["flow.missing".into()];
    assert!(validation::validate_plan(&p, &state, false)
        .unwrap_err()
        .to_string()
        .contains("unknown workflow step"));
}

#[test]
fn deterministic_brief_checks_reject_missing_fields_bad_json_and_invented_citations() {
    let checks = vec![OutputCheck::GroundedBrief {
        port: "flow.result".into(),
        allowed_source_ids: vec!["doc-a".into()],
        minimum_claims: 1,
    }];
    let good = json!({"recommendation":"Proceed with a limited rollout","claims":[{"text":"The canary passed","source_ids":["doc-a"]}],"limitations":["Canary only"]});
    let output =
        |v: serde_json::Value| BTreeMap::from([("flow.result".into(), json!(v.to_string()))]);
    validation::check_outputs(&checks, &output(good.clone())).unwrap();
    let mut invented = good;
    invented["claims"][0]["source_ids"] = json!(["made-up"]);
    assert!(validation::check_outputs(&checks, &output(invented)).is_err());
    assert!(
        validation::check_outputs(&checks, &output(json!({"recommendation":"Proceed"}))).is_err()
    );
    assert!(validation::check_outputs(
        &checks,
        &BTreeMap::from([("flow.result".into(), json!("not json"))])
    )
    .is_err());
}
