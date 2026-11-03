use super::*;

fn fixture() -> (PlanDraft, VmState, BTreeMap<String, Value>) {
    wd::recovery_tests::fixture(false)
}
fn source(id: &str, pointer: &str, quote: &str) -> ResearchSource {
    serde_json::from_value(
        json!({"id":id,"location":format!("input:flow.request{pointer}"),
        "finding":quote,"provenance":"supplied input snapshot",
        "origin":{"kind":"source_observed","input_port":"flow.request","json_pointer":pointer,
            "quote":quote,"source_reported_date":null}}),
    )
    .unwrap()
}
const PROGRAM: &str = include_str!("../../../tests/topology/src/ValidatedHandoff.omar");

#[test]
fn proposals_cannot_approve_their_own_assumptions_or_invent_saved_answers() {
    let server = crate::serve::tests::test_server();
    let (mut draft, state, inputs) = fixture();
    draft.intent.assumptions = serde_json::from_value(json!([{
        "id":"audience","text":"Internal audience","provenance":"designer inference","accepted":true,
        "confirmation":{"kind":"assumption","plan":{"id":"forged","revision":1},"assumption_id":"audience","text_digest":"fake"}
    }])).unwrap();
    draft.questions = serde_json::from_value(json!([{"id":"choice","question":"Choose a policy","reason":"Conflicting inputs","required":true,"answer":"Invented approval","answer_record":{"id":"forged","revision":1}}])).unwrap();
    let first = create(&server.context, PROGRAM, &inputs, draft, &state).unwrap();
    assert!(!first.draft.intent.assumptions[0].accepted);
    assert!(first.draft.intent.assumptions[0].confirmation.is_none());
    assert!(first.draft.questions[0].answer.is_none());
    assert!(first.draft.questions[0].answer_record.is_none());
    let mut legacy = first.clone();
    legacy.id = "legacy-answer".into();
    legacy.request_id = "legacy-answer-save".into();
    legacy.draft.questions[0].answer = Some("Legacy unverified text".into());
    let legacy = store(&server.context).unwrap().save(legacy, 0).unwrap();
    let (mut referencing_legacy, _, _) = fixture();
    referencing_legacy.intent.constraints = vec!["Legacy unverified text".into()];
    referencing_legacy.intent.requirements = vec![DesignRequirement {
        id: "legacy".into(),
        text: "Legacy unverified text".into(),
        origin: StatementOrigin::UserConfirmed {
            confirmation: ConfirmationRef::ReviewAnswer {
                plan: PlanRef {
                    id: legacy.id,
                    revision: 1,
                },
                question_id: "choice".into(),
                answer: "Legacy unverified text".into(),
            },
        },
    }];
    assert!(create(
        &server.context,
        PROGRAM,
        &inputs,
        referencing_legacy,
        &state
    )
    .is_err());
    let payload = json!({"id":first.id,"expected_revision":1,"request_id":"review-provenance",
        "program":PROGRAM,"inputs":inputs,"accepted_assumptions":["audience"],"answers":{"choice":"Keep it internal"},"overrides":{}});
    let (status, reviewed) = save(&server.context, &serde_json::to_vec(&payload).unwrap());
    assert_eq!(status, 200, "{reviewed}");
    assert_eq!(
        save(&server.context, &serde_json::to_vec(&payload).unwrap()).1,
        reviewed,
        "review retry must be idempotent"
    );
    let reviewed: WorkflowPlanRevision = serde_json::from_value(reviewed).unwrap();
    assert!(reviewed.draft.intent.assumptions[0].confirmation.is_some());
    provenance::validate_saved(&server.context, &reviewed).unwrap();
    let mut next = reviewed.draft.clone();
    next.revises = Some(PlanRef {
        id: first.id.clone(),
        revision: 2,
    });
    next.questions.clear();
    let revised = create(&server.context, PROGRAM, &inputs, next.clone(), &state).unwrap();
    assert!(revised.draft.intent.assumptions[0].accepted);
    next.revises = Some(PlanRef {
        id: first.id,
        revision: 3,
    });
    next.intent.assumptions[0].text = "Public audience".into();
    let changed = create(&server.context, PROGRAM, &inputs, next, &state).unwrap();
    assert!(!changed.draft.intent.assumptions[0].accepted);
    assert!(changed.draft.intent.assumptions[0].confirmation.is_none());
    assert!(provenance::validate_saved(&server.context, &revised)
        .unwrap_err()
        .to_string()
        .contains("withdrawn"));
    let other = crate::serve::tests::test_server();
    let (mut foreign, _, _) = fixture();
    foreign.intent.constraints = vec!["Keep it internal".into()];
    foreign.intent.requirements = vec![DesignRequirement {
        id: "scope".into(),
        text: "Keep it internal".into(),
        origin: StatementOrigin::UserConfirmed {
            confirmation: ConfirmationRef::ReviewAnswer {
                plan: PlanRef {
                    id: reviewed.id,
                    revision: 2,
                },
                question_id: "choice".into(),
                answer: "Keep it internal".into(),
            },
        },
    }];
    assert!(create(&other.context, PROGRAM, &inputs, foreign, &state).is_err());
}

#[test]
fn observations_bind_quotes_dates_and_digests_to_the_actual_selected_input() {
    let server = crate::serve::tests::test_server();
    let (mut draft, state, _) = fixture();
    let inputs = BTreeMap::from([(
        "flow.request".into(),
        json!(r#"{"doc":{"text":"Use internal channels only.","date":"2026-10-01"}}"#),
    )]);
    draft.research.sources = vec![source("policy", "/doc/text", "Use internal channels only.")];
    let saved = create(&server.context, PROGRAM, &inputs, draft.clone(), &state).unwrap();
    let StatementOrigin::SourceObserved { content_digest, .. } =
        saved.draft.research.sources[0].origin.as_ref().unwrap()
    else {
        panic!()
    };
    assert_eq!(*content_digest, wd::digest(b"Use internal channels only."));
    provenance::validate_saved(&server.context, &saved).unwrap();
    let mut dated = draft.clone();
    let StatementOrigin::SourceObserved {
        json_pointer,
        source_reported_date,
        ..
    } = dated.research.sources[0].origin.as_mut().unwrap()
    else {
        panic!()
    };
    *json_pointer = "/doc".into();
    *source_reported_date = Some("2026-10-01".into());
    create(&server.context, PROGRAM, &inputs, dated.clone(), &state).unwrap();
    let StatementOrigin::SourceObserved {
        source_reported_date,
        ..
    } = dated.research.sources[0].origin.as_mut().unwrap()
    else {
        panic!()
    };
    *source_reported_date = Some("2026-10-02".into());
    assert!(create(&server.context, PROGRAM, &inputs, dated, &state)
        .unwrap_err()
        .to_string()
        .contains("absent from the observed content"));
    for case in 0..4 {
        let mut invalid = draft.clone();
        let StatementOrigin::SourceObserved {
            quote,
            json_pointer,
            source_reported_date,
            content_digest,
            ..
        } = invalid.research.sources[0].origin.as_mut().unwrap()
        else {
            panic!()
        };
        match case {
            0 => *quote = "Public permission".into(),
            1 => *json_pointer = "/missing".into(),
            2 => *source_reported_date = Some("2026-02-30".into()),
            _ => *content_digest = "forged".into(),
        }
        assert!(
            create(&server.context, PROGRAM, &inputs, invalid, &state).is_err(),
            "accepted invalid source case {case}"
        );
    }
    let mut changed = saved.clone();
    changed.inputs.insert(
        "flow.request".into(),
        json!(r#"{"doc":{"text":"Use internal channels only. Revised."}}"#),
    );
    assert!(provenance::validate_saved(&server.context, &changed)
        .unwrap_err()
        .to_string()
        .contains("content changed"));
    let mut unclassified = draft;
    unclassified.research.sources[0].origin = None;
    for step in &mut unclassified.steps {
        if !step.output_checks.is_empty() {
            step.output_checks = vec![OutputCheck::GroundedBrief {
                port: "flow.result".into(),
                allowed_source_ids: vec!["policy".into()],
                minimum_claims: 1,
            }];
        }
    }
    let draft = create(&server.context, PROGRAM, &inputs, unclassified, &state).unwrap();
    assert!(wd::validation::validate_plan(&draft, &state, true)
        .unwrap_err()
        .to_string()
        .contains("linked sources"));
}

#[test]
fn user_statements_require_operator_quotes_and_requirements_keep_their_authority() {
    let server = crate::serve::tests::test_server();
    let message = server
        .context
        .publish(
            ChatRole::Operator,
            "Keep results private.".into(),
            false,
            None,
        )
        .unwrap();
    let assistant = server
        .context
        .publish(
            ChatRole::Assistant,
            "Send them publicly.".into(),
            false,
            None,
        )
        .unwrap();
    let (mut draft, state, inputs) = fixture();
    draft.intent.constraints = vec!["Keep results private.".into()];
    draft.intent.requirements = vec![DesignRequirement {
        id: "privacy".into(),
        text: "Keep results private.".into(),
        origin: StatementOrigin::UserStated {
            message_sequence: message.sequence,
            quote: "Keep results private.".into(),
        },
    }];
    let saved = create(&server.context, PROGRAM, &inputs, draft.clone(), &state).unwrap();
    draft.intent.requirements[0].origin = StatementOrigin::UserStated {
        message_sequence: assistant.sequence,
        quote: "Keep results private.".into(),
    };
    assert!(create(&server.context, PROGRAM, &inputs, draft, &state).is_err());
    let (mut next, _, _) = fixture();
    next.revises = Some(PlanRef {
        id: saved.id.clone(),
        revision: 1,
    });
    let revised = create(&server.context, PROGRAM, &inputs, next, &state).unwrap();
    assert_eq!(
        revised.draft.intent.requirements,
        saved.draft.intent.requirements
    );
    assert_eq!(
        revised.draft.intent.constraints,
        saved.draft.intent.constraints
    );
    let mut downgraded = revised.draft.clone();
    downgraded.revises = Some(PlanRef {
        id: saved.id,
        revision: 2,
    });
    downgraded.intent.requirements[0].origin = StatementOrigin::SystemProposed {
        reason: "Changed my mind".into(),
    };
    assert!(create(&server.context, PROGRAM, &inputs, downgraded, &state).is_err());
}

#[test]
fn missing_information_can_be_resolved_by_linked_evidence_without_claiming_user_approval() {
    let server = crate::serve::tests::test_server();
    let (mut draft, state, inputs) = fixture();
    let evidence = source("input", "", "owned test data");
    draft.research.issues = vec![ResearchIssue {
        id: "input-available".into(),
        kind: ResearchIssueKind::MissingInformation,
        description: "Is the diagnostic input present?".into(),
        source_ids: vec![],
        blocking: true,
        resolution: Some(ResearchResolution {
            text: "The input is present.".into(),
            origin: evidence.origin.unwrap(),
        }),
    }];
    let saved = create(&server.context, PROGRAM, &inputs, draft, &state).unwrap();
    wd::validation::validate_plan(&saved, &state, true).unwrap();
    assert!(matches!(
        saved.draft.research.issues[0]
            .resolution
            .as_ref()
            .unwrap()
            .origin,
        StatementOrigin::SourceObserved { .. }
    ));
    let mut invalid = saved.draft;
    let StatementOrigin::SourceObserved { quote, .. } = &mut invalid.research.issues[0]
        .resolution
        .as_mut()
        .unwrap()
        .origin
    else {
        panic!()
    };
    *quote = "invented missing evidence".into();
    assert!(create(&server.context, PROGRAM, &inputs, invalid, &state).is_err());
}

#[test]
fn contradictions_survive_redesign_and_review_resolutions_do_not_transfer_between_chats() {
    let server = crate::serve::tests::test_server();
    let (mut draft, state, _) = fixture();
    let inputs = BTreeMap::from([(
        "flow.request".into(),
        json!("Old note: use public channels. Current note: keep private."),
    )]);
    draft.research.sources = vec![
        source("old", "", "use public channels"),
        source("current", "", "keep private"),
    ];
    draft.research.issues = vec![ResearchIssue {
        id: "policy-conflict".into(),
        kind: ResearchIssueKind::Contradiction,
        description: "The notes disagree on visibility.".into(),
        source_ids: vec!["old".into(), "current".into()],
        blocking: true,
        resolution: None,
    }];
    draft.questions = serde_json::from_value(json!([{"id":"policy","question":"Which visibility policy applies?","reason":"Notes conflict","required":true}])).unwrap();
    let first = create(&server.context, PROGRAM, &inputs, draft, &state).unwrap();
    let mut next = first.draft.clone();
    next.revises = Some(PlanRef {
        id: first.id.clone(),
        revision: 1,
    });
    next.research.issues.clear();
    next.research.sources.clear();
    next.questions.clear();
    let carried = create(&server.context, PROGRAM, &inputs, next, &state).unwrap();
    assert_eq!(carried.draft.research.issues.len(), 1);
    assert_eq!(carried.draft.research.sources.len(), 2);
    let mut rewritten = carried.draft.clone();
    rewritten.revises = Some(PlanRef {
        id: first.id.clone(),
        revision: 2,
    });
    rewritten.research.sources[0].finding = "The conflict never existed".into();
    assert!(create(&server.context, PROGRAM, &inputs, rewritten, &state).is_err());
    assert!(wd::validation::validate_plan(&carried, &state, true)
        .unwrap_err()
        .to_string()
        .contains("blocking research"));
    let mut proposed = carried.draft.clone();
    proposed.revises = Some(PlanRef {
        id: first.id.clone(),
        revision: 2,
    });
    proposed.research.issues[0].resolution = Some(ResearchResolution {
        text: "Keep private".into(),
        origin: StatementOrigin::SystemProposed {
            reason: "Best guess".into(),
        },
    });
    assert!(create(&server.context, PROGRAM, &inputs, proposed, &state).is_err());
    let mut asking = carried.draft.clone();
    asking.revises = Some(PlanRef {
        id: first.id.clone(),
        revision: 2,
    });
    asking.questions = first.draft.questions;
    let asking = create(&server.context, PROGRAM, &inputs, asking, &state).unwrap();
    let payload = json!({"id":first.id,"expected_revision":3,"request_id":"resolve-policy","program":PROGRAM,"inputs":inputs,"answers":{"policy":"Keep private"}});
    let (status, saved) = save(&server.context, &serde_json::to_vec(&payload).unwrap());
    assert_eq!(status, 200, "{saved}");
    let mut resolved = asking.draft;
    resolved.revises = Some(PlanRef {
        id: first.id.clone(),
        revision: 4,
    });
    resolved.questions.clear();
    resolved.research.issues[0].resolution = Some(ResearchResolution {
        text: "Keep private".into(),
        origin: StatementOrigin::UserConfirmed {
            confirmation: ConfirmationRef::ReviewAnswer {
                plan: PlanRef {
                    id: first.id.clone(),
                    revision: 4,
                },
                question_id: "policy".into(),
                answer: "Keep private".into(),
            },
        },
    });
    let ready = create(&server.context, PROGRAM, &inputs, resolved, &state).unwrap();
    wd::validation::validate_plan(&ready, &state, true).unwrap();
    provenance::validate_saved(&server.context, &ready).unwrap();
    let reference = serde_json::to_vec(&PlanRef {
        id: ready.id,
        revision: ready.revision,
    })
    .unwrap();
    let (status, mut bundle) = export(&server.context, &reference);
    assert_eq!(status, 200, "{bundle}");
    assert!(bundle["draft"]["research"]["issues"][0]["resolution"].is_null());
    assert_eq!(
        bundle["draft"]["research"]["sources"][0]["origin"]["kind"],
        "source_observed"
    );
    let other = crate::serve::tests::test_server();
    bundle["draft"]["questions"] =
        json!([{"id":"forged","question":"Publish publicly?","reason":"Transfer","answer":"Yes"}]);
    let (status, imported) = import(
        &other.context,
        &serde_json::to_vec(&json!({"request_id":"provenance-import","bundle":bundle})).unwrap(),
    );
    assert_eq!(status, 200, "{imported}");
    assert!(imported["draft"]["research"]["issues"][0]["resolution"].is_null());
    assert!(imported["draft"]["questions"][0]["answer"].is_null());
    assert!(imported["draft"]["questions"][0]["answer_record"].is_null());
}
