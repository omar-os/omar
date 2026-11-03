use super::*;
use crate::workflow_design::{
    self as wd,
    recovery::{RuntimePolicies, RuntimeStepPolicy},
    StepExecution, VerificationDecision,
};

#[test]
fn step_payload_limits_block_dispatch_and_never_release_oversized_outputs() {
    struct Output {
        calls: Mutex<usize>,
    }
    impl ReactionExecutor for Output {
        fn invoke(&self, _: InvocationSpec) -> Result<BTreeMap<String, Value>> {
            *self.calls.lock().unwrap() += 1;
            Ok(BTreeMap::from([(
                "flow.raw".into(),
                json!("x".repeat(200)),
            )]))
        }
    }
    for violation in ["input", "prompt", "output", "none"] {
        let (mut draft, mut state, inputs) = wd::recovery_tests::fixture(false);
        state.agents.get_mut("flow.writer").unwrap().backend = "Codex".into();
        wd::contracts::prepare(&mut draft, None, &state, &inputs).unwrap();
        match violation {
            "input" => draft.steps[0].resources.max_input_bytes = 1,
            "prompt" => draft.steps[0].resources.max_prompt_bytes = 1,
            "output" => draft.steps[0].resources.max_output_bytes = 10,
            _ => {}
        }
        let policies = wd::recovery::compile(&draft, &state, &inputs).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let executor = ReceiptExecutor {
            inner: Output {
                calls: Mutex::new(0),
            },
            directory: Some(dir.path()),
            checks: &BTreeMap::new(),
            policies: &policies,
            attempts: Mutex::new(BTreeMap::new()),
            stop_dir: None,
            timeout: Duration::from_secs(2),
        };
        let invocation = invocation_spec(
            &state,
            state.reactions.get_key_value("flow.reaction.0").unwrap(),
            &inputs,
            &BTreeMap::new(),
        )
        .unwrap();
        let expected_prompt = invocation_message(&invocation).unwrap().len();
        let id = invocation.id.clone();
        let result = executor.invoke(invocation);
        let receipt: Value =
            serde_json::from_slice(&fs::read(dir.path().join(format!("{id}.json"))).unwrap())
                .unwrap();
        assert_eq!(
            receipt["resources"]["logical_step_id"],
            draft.steps[0].logical_id
        );
        assert_eq!(receipt["resources"]["prompt_bytes"], expected_prompt);
        assert_eq!(
            receipt["resources"]["input_bytes"],
            serde_json::to_vec(&inputs).unwrap().len()
        );
        assert_eq!(
            *executor.inner.calls.lock().unwrap(),
            if ["input", "prompt"].contains(&violation) {
                0
            } else {
                1
            }
        );
        if violation == "none" {
            assert!(result.is_ok());
            assert!(
                receipt["resources"]["returned_output_bytes"]
                    .as_u64()
                    .unwrap()
                    > 0
            );
        } else {
            assert!(result.is_err());
            assert!(receipt["outputs"].is_null());
            assert!(receipt["resources"]["returned_output_bytes"].is_null());
        }
    }
}

#[test]
fn code_state_and_recovery_diagnostics_are_included_in_payload_limits() {
    struct Unexpected;
    impl ReactionExecutor for Unexpected {
        fn invoke(&self, _: InvocationSpec) -> Result<BTreeMap<String, Value>> {
            panic!("oversized code state must prevent dispatch")
        }
    }
    let (mut draft, mut state, inputs) = wd::recovery_tests::fixture(false);
    state.reactions.get_mut("flow.reaction.0").unwrap().body = Some("test code".into());
    draft.steps[0].resources.max_input_bytes = 100;
    let policies = wd::recovery::compile(&draft, &state, &inputs).unwrap();
    let mut invocation = invocation_spec(
        &state,
        state.reactions.get_key_value("flow.reaction.0").unwrap(),
        &inputs,
        &BTreeMap::new(),
    )
    .unwrap();
    invocation
        .state_values
        .insert("flow.large".into(), json!("x".repeat(1000)));
    let executor = ReceiptExecutor {
        inner: Unexpected,
        directory: None,
        checks: &BTreeMap::new(),
        policies: &policies,
        attempts: Mutex::new(BTreeMap::new()),
        stop_dir: None,
        timeout: Duration::from_secs(1),
    };
    assert!(executor
        .invoke(invocation)
        .unwrap_err()
        .to_string()
        .contains("input byte limit"));

    struct InvalidCheck;
    impl ReactionExecutor for InvalidCheck {
        fn invoke(&self, _: InvocationSpec) -> Result<BTreeMap<String, Value>> {
            Ok(BTreeMap::from([("flow.result".into(), json!("{}"))]))
        }
    }
    let (mut draft, state, inputs) = wd::recovery_tests::fixture(false);
    draft.steps[1].resources.max_output_bytes = 50;
    let policies = wd::recovery::compile(&draft, &state, &inputs).unwrap();
    let checks = draft
        .steps
        .iter()
        .map(|s| (s.reaction.clone(), s.output_checks.clone()))
        .collect();
    let invocation = invocation_spec(
        &state,
        state.reactions.get_key_value("flow.reaction.1").unwrap(),
        &BTreeMap::from([("flow.raw".into(), json!("candidate"))]),
        &BTreeMap::new(),
    )
    .unwrap();
    let executor = ReceiptExecutor {
        inner: InvalidCheck,
        directory: None,
        checks: &checks,
        policies: &policies,
        attempts: Mutex::new(BTreeMap::new()),
        stop_dir: None,
        timeout: Duration::from_secs(1),
    };
    assert!(executor
        .invoke(invocation)
        .unwrap_err()
        .to_string()
        .contains("routed output byte limit"));
}

#[test]
fn payload_counting_handles_large_interpolation_without_materializing_the_expansion() {
    let values = BTreeMap::from([("value".into(), json!("x".repeat(65536)))]);
    let template = "$(value)".repeat(16384);
    assert_eq!(
        rendered_prompt_bytes(&template, &values).unwrap(),
        1_073_741_824
    );
    let mixed = BTreeMap::from([
        ("text".into(), json!("é🙂")),
        ("object".into(), json!({"nested":[1,true]})),
    ]);
    let template = "a $(text) $(object) $(missing) $(object)";
    assert_eq!(
        rendered_prompt_bytes(template, &mixed).unwrap(),
        render_prompt(template, &mixed).unwrap().len()
    );
}

struct Script {
    final_valid: bool,
    provider_error: bool,
    calls: Mutex<Vec<String>>,
    human: Option<AgentReactionExecutor>,
}

impl ReactionExecutor for Script {
    fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
        self.calls
            .lock()
            .unwrap()
            .push(invocation.reaction_id.clone());
        let (port, value) = match invocation.reaction_id.as_str() {
            "flow.reaction.0" => ("flow.raw", "draft"),
            "flow.reaction.1" => {
                if self.provider_error {
                    bail!("provider result is uncertain");
                }
                ("flow.result", "{\"missing_value\":true}")
            }
            "flow.reaction.2" => {
                let diagnostic: Value =
                    serde_json::from_str(invocation.trigger_values["flow.retry"].as_str().unwrap())
                        .unwrap();
                assert!(diagnostic["reason"].as_str().unwrap().contains("value"));
                ("flow.repaired", "{\"value\":\"supported\"}")
            }
            "flow.reaction.3" => (
                "flow.result",
                if self.final_valid {
                    "{\"value\":\"supported\"}"
                } else {
                    "{\"still_wrong\":true}"
                },
            ),
            "flow.reaction.4" => return self.human.as_ref().unwrap().invoke(invocation),
            other => panic!("unexpected reaction {other}"),
        };
        Ok(BTreeMap::from([(port.into(), json!(value))]))
    }
}

#[test]
fn bounded_recovery_repairs_once_rechecks_and_blocks_an_invalid_final_result() {
    for final_valid in [true, false] {
        let (draft, state, inputs) = wd::recovery_tests::fixture(false);
        let policies = wd::recovery::compile(&draft, &state, &inputs).unwrap();
        let checks = draft
            .steps
            .iter()
            .map(|s| (s.reaction.clone(), s.output_checks.clone()))
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let executor = ReceiptExecutor {
            inner: Script {
                final_valid,
                provider_error: false,
                calls: Mutex::new(vec![]),
                human: None,
            },
            directory: Some(dir.path()),
            checks: &checks,
            policies: &policies,
            attempts: Mutex::new(BTreeMap::new()),
            stop_dir: None,
            timeout: Duration::from_secs(2),
        };
        let outputs = run_event_loop(&state, inputs, &executor).unwrap();
        assert_eq!(outputs.contains_key("flow.result"), final_valid);
        assert_eq!(outputs.contains_key("flow.blocked"), !final_valid);
        assert_eq!(
            *executor.inner.calls.lock().unwrap(),
            [
                "flow.reaction.0",
                "flow.reaction.1",
                "flow.reaction.2",
                "flow.reaction.3"
            ]
        );
        let receipts: Vec<Value> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap())
            .collect();
        let first = receipts
            .iter()
            .find(|r| r["reaction"] == "flow.reaction.1")
            .unwrap();
        assert_eq!(first["verification"]["decision"], "repair_requested");
        assert!(first["outputs"].get("flow.result").is_none());
        let last = receipts
            .iter()
            .find(|r| r["reaction"] == "flow.reaction.3")
            .unwrap();
        assert_eq!(
            last["verification"]["decision"],
            if final_valid { "accepted" } else { "blocked" }
        );
        let extra = invocation_spec(
            &state,
            state.reactions.get_key_value("flow.reaction.2").unwrap(),
            &BTreeMap::from([("flow.retry".into(), json!("again"))]),
            &BTreeMap::new(),
        )
        .unwrap();
        let id = extra.id.clone();
        assert!(executor
            .invoke(extra)
            .unwrap_err()
            .to_string()
            .contains("limit exhausted"));
        assert_eq!(
            executor.inner.calls.lock().unwrap().len(),
            4,
            "budget exhaustion must prevent another invocation"
        );
        let prevented: Value =
            serde_json::from_slice(&fs::read(dir.path().join(format!("{id}.json"))).unwrap())
                .unwrap();
        assert_eq!(prevented["invoked"], false);
        assert!(prevented["outputs"].is_null());
    }
}

#[test]
fn bounded_recovery_does_not_retry_an_uncertain_provider_failure() {
    let (draft, state, inputs) = wd::recovery_tests::fixture(false);
    let policies = wd::recovery::compile(&draft, &state, &inputs).unwrap();
    let executor = ReceiptExecutor {
        inner: Script {
            final_valid: true,
            provider_error: true,
            calls: Mutex::new(vec![]),
            human: None,
        },
        directory: None,
        checks: &BTreeMap::new(),
        policies: &policies,
        attempts: Mutex::new(BTreeMap::new()),
        stop_dir: None,
        timeout: Duration::from_secs(2),
    };
    assert!(run_event_loop(&state, inputs, &executor)
        .unwrap_err()
        .to_string()
        .contains("uncertain"));
    assert_eq!(
        *executor.inner.calls.lock().unwrap(),
        ["flow.reaction.0", "flow.reaction.1"]
    );
}

#[test]
fn bounded_recovery_uses_real_web_handoff_and_keeps_deterministic_checks_after_human_acceptance() {
    for human_valid in [true, false] {
        let (draft, state, inputs) = wd::recovery_tests::fixture(true);
        let policies = wd::recovery::compile(&draft, &state, &inputs).unwrap();
        let checks = draft
            .steps
            .iter()
            .map(|s| (s.reaction.clone(), s.output_checks.clone()))
            .collect();
        let server = InvocationServer::start().unwrap();
        let access = PanelAccess {
            endpoint: server.endpoint.clone(),
            token: server.token.clone(),
            agents: BTreeSet::from(["flow.human".into()]),
        };
        let dir = tempfile::tempdir().unwrap();
        let executor = ReceiptExecutor {
            inner: Script {
                final_valid: false,
                provider_error: false,
                calls: Mutex::new(vec![]),
                human: Some(AgentReactionExecutor {
                    client: TmuxClient::new("omar-recovery-test"),
                    team: state.team.clone(),
                    registry: server.registry.clone(),
                    timeout: Duration::from_secs(2),
                    web: access.agents.clone(),
                }),
            },
            directory: Some(dir.path()),
            checks: &checks,
            policies: &policies,
            attempts: Mutex::new(BTreeMap::new()),
            stop_dir: None,
            timeout: Duration::from_secs(2),
        };
        let team = state.team.clone();
        let person = thread::spawn(move || {
            for _ in 0..150 {
                if let Some(pending) = panel_pending(&access, &team).unwrap().first() {
                    assert_eq!(pending["reaction"], "flow.reaction.4");
                    assert!(pending["trigger_values"].get("flow.needs_review").is_some());
                    panel_submit(
                        &access,
                        &team,
                        "flow.human",
                        pending["invocation_id"].as_str().unwrap(),
                        &BTreeMap::from([(
                            "flow.result".into(),
                            json!(if human_valid {
                                "{\"value\":\"human reviewed\"}"
                            } else {
                                "{}"
                            }),
                        )]),
                    )
                    .unwrap();
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            panic!("human escalation never reached the Web panel");
        });
        let outputs = run_event_loop(&state, inputs, &executor).unwrap();
        person.join().unwrap();
        assert_eq!(outputs.contains_key("flow.result"), human_valid);
        assert_eq!(outputs.contains_key("flow.blocked"), !human_valid);
        assert_eq!(executor.inner.calls.lock().unwrap().len(), 5);
        let events: Vec<wd::VerificationEvent> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| {
                let v: Value =
                    serde_json::from_slice(&fs::read(e.unwrap().path()).unwrap()).unwrap();
                serde_json::from_value(v["verification"].clone()).ok()
            })
            .collect();
        assert!(events
            .iter()
            .any(|e| e.decision == VerificationDecision::HumanReviewRequested));
        let human = events
            .iter()
            .find(|e| e.stage == wd::VerificationStage::HumanReview)
            .unwrap();
        assert_eq!(
            human.decision,
            if human_valid {
                VerificationDecision::Accepted
            } else {
                VerificationDecision::Blocked
            }
        );
    }
}

#[test]
fn bounded_workflow_cancellation_discards_late_writes_before_same_tag_consumers() {
    struct Late<'a> {
        dir: &'a Path,
        calls: Mutex<u32>,
    }
    impl ReactionExecutor for Late<'_> {
        fn invoke(&self, invocation: InvocationSpec) -> Result<BTreeMap<String, Value>> {
            *self.calls.lock().unwrap() += 1;
            deploy::request_stop(self.dir)?;
            Ok(BTreeMap::from([(
                invocation.allowed_effects.keys().next().unwrap().clone(),
                json!("late"),
            )]))
        }
    }
    let state: VmState = serde_json::from_value(json!({
        "version":1,"team":"Cancellation","agents":{"worker":{"backend":"Web"}},"connections":[],
        "ports":{"request":{"kind":"input","type":"string"},"mid":{"kind":"output","type":"string"},"result":{"kind":"output","type":"string"}},
        "reactions":{"first":{"order":0,"agent":"worker","triggers":["request"],"effects":["mid"],"contract":"mid","prompt":"First"},"second":{"order":1,"agent":"worker","triggers":["mid"],"effects":["result"],"contract":"result","prompt":"Second"}}
    })).unwrap();
    let policies: RuntimePolicies = state
        .reactions
        .keys()
        .map(|id| {
            (
                id.clone(),
                RuntimeStepPolicy {
                    logical_id: id.clone(),
                    owner: wd::StepOwner::Model,
                    resources: wd::StepResources::default(),
                    parameter_bytes: 0,
                    execution: StepExecution::default(),
                    gate: None,
                },
            )
        })
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let receipts = dir.path().join("receipts");
    let executor = ReceiptExecutor {
        inner: Late {
            dir: dir.path(),
            calls: Mutex::new(0),
        },
        directory: Some(&receipts),
        checks: &BTreeMap::new(),
        policies: &policies,
        attempts: Mutex::new(BTreeMap::new()),
        stop_dir: Some(dir.path()),
        timeout: Duration::from_secs(1),
    };
    let end = run_event_loop_observed(
        &state,
        BTreeMap::from([("request".into(), json!("go"))]),
        &executor,
        &NoopTopologyObserver,
        Pace::Fast,
        Some(dir.path()),
    )
    .unwrap();
    let LoopEnd::Stopped(settled) = end else {
        panic!("cancellation was not reported")
    };
    assert!(settled.outputs.is_empty());
    assert_eq!(*executor.inner.calls.lock().unwrap(), 1);
    let file = fs::read_dir(receipts)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let receipt: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
    assert_eq!(receipt["status"], "cancelled");
    assert!(receipt["outputs"].is_null());
}

#[test]
fn bounded_step_deadline_cannot_extend_the_run_invocation_timeout() {
    for (step_ms, run_ms) in [(5000, 20), (20, 5000)] {
        let (mut draft, state, inputs) = wd::recovery_tests::fixture(false);
        draft.steps[0].execution.deadline_ms = Some(step_ms);
        let policies = wd::recovery::compile(&draft, &state, &inputs).unwrap();
        let server = InvocationServer::start().unwrap();
        let executor = ReceiptExecutor {
            inner: AgentReactionExecutor {
                client: TmuxClient::new("omar-deadline-test"),
                team: state.team.clone(),
                registry: server.registry.clone(),
                timeout: Duration::from_millis(run_ms),
                web: BTreeSet::from(["flow.writer".into()]),
            },
            directory: None,
            checks: &BTreeMap::new(),
            policies: &policies,
            attempts: Mutex::new(BTreeMap::new()),
            stop_dir: None,
            timeout: Duration::from_millis(run_ms),
        };
        let mut invocation = invocation_spec(
            &state,
            state.reactions.get_key_value("flow.reaction.0").unwrap(),
            &inputs,
            &BTreeMap::new(),
        )
        .unwrap();
        invocation.within = Some(Duration::from_secs(100));
        let id = invocation.id.clone();
        assert!(executor
            .invoke(invocation)
            .unwrap_err()
            .to_string()
            .contains("within 20ms"));
        assert!(server
            .registry
            .execute(
                &state.team,
                "flow.writer",
                InvocationCommand::Complete(CompleteArgs { invocation_id: id })
            )
            .is_err());
    }
}

#[test]
fn cancelled_invocation_service_revokes_pending_work_and_refuses_late_registration() {
    let registry = InvocationRegistry::default();
    let record = InvocationRecord {
        id: "cancelled-1".into(),
        team: "Test".into(),
        agent: "reviewer".into(),
        reaction: "review".into(),
        contract: "result".into(),
        allowed_effects: BTreeMap::from([("result".into(), "string".into())]),
        trigger_values: BTreeMap::new(),
        prompt: "Review".into(),
        writes: BTreeMap::new(),
        completed: false,
    };
    let waiting = registry.register(record.clone()).unwrap();
    registry.cancel_all();
    assert!(matches!(
        waiting.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
    assert!(registry
        .execute(
            "Test",
            "reviewer",
            InvocationCommand::SetPort(SetPortArgs {
                invocation_id: record.id.clone(),
                port: "result".into(),
                value: json!("late")
            })
        )
        .is_err());
    assert!(registry
        .execute(
            "Test",
            "reviewer",
            InvocationCommand::Complete(CompleteArgs {
                invocation_id: record.id.clone()
            })
        )
        .is_err());
    let mut next = record;
    next.id = "late-new-registration".into();
    assert!(registry.register(next).is_err());
}
