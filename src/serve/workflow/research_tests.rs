use super::*;
const PROGRAM: &str = include_str!("../../../tests/topology/src/ValidatedHandoff.omar");

fn operator(context: &Context_, text: &str) -> ChatMessage {
    context
        .publish(ChatRole::Operator, text.into(), false, None)
        .unwrap()
}
fn lookup(context: &Context_, sequence: u64, id: &str, kind: Value) -> Value {
    let body = json!({"token":*context.agent_token.lock().unwrap(),"session_id":wd::research::Store::session_id(sequence),"request_id":id,"lookup":kind});
    let (status, reply) = research::lookup(context, &serde_json::to_vec(&body).unwrap());
    assert_eq!(status, 200, "{reply}");
    reply
}

#[test]
fn host_research_reads_owned_inputs_and_proposals_cannot_forge_usage() {
    let server = crate::serve::tests::test_server();
    let (draft, state, inputs) = wd::recovery_tests::fixture(false);
    let original = create(&server.context, PROGRAM, &inputs, draft.clone(), &state).unwrap();
    assert!(
        original.draft.research.usage.is_none(),
        "prepared designs are not metered native research"
    );
    let message = operator(&server.context, "Use supplied release evidence");
    let read = lookup(
        &server.context,
        message.sequence,
        "source",
        json!({"kind":"plan_input","plan":{"id":original.id,"revision":1},"input_port":"flow.request"}),
    );
    assert_eq!(read["data"], inputs["flow.request"]);
    assert_eq!(read["research"]["source_calls"], 1);
    let mut draft = draft;
    draft.research.usage = Some(serde_json::from_value(read["research"].clone()).unwrap());
    draft.research.usage.as_mut().unwrap().source_calls = 999;
    let saved = create(&server.context, PROGRAM, &inputs, draft, &state).unwrap();
    let usage = saved.draft.research.usage.as_ref().unwrap();
    assert_eq!(usage.source_calls, 1);
    assert_eq!(usage.stop_reason, Some(ResearchStop::ProposalSubmitted));
    research::validate_saved(&server.context, &saved.draft).unwrap();
    let mut forged = saved.draft.clone();
    forged.research.usage.as_mut().unwrap().adapter_cost_usd = 42.0;
    assert!(research::validate_saved(&server.context, &forged).is_err());
    let closed = lookup(
        &server.context,
        message.sequence,
        "new",
        json!({"kind":"operator_message","message_sequence":message.sequence}),
    );
    assert!(closed["data"].is_null());
    assert_eq!(closed["research"]["source_calls"], 1);
}

#[test]
fn new_operator_scope_supersedes_the_previous_budget_and_external_reads_are_explicit() {
    let server = crate::serve::tests::test_server();
    let first = operator(&server.context, "First request");
    let one = lookup(
        &server.context,
        first.sequence,
        "source",
        json!({"kind":"operator_message","message_sequence":first.sequence}),
    );
    assert_eq!(one["data"], "First request");
    let second = operator(&server.context, "Revised request");
    let external = lookup(
        &server.context,
        second.sequence,
        "external",
        json!({"kind":"external","location":"https://example.invalid/source"}),
    );
    assert!(external["error"].as_str().unwrap().contains("unsupported"));
    assert_eq!(external["research"]["failed_calls"], 1);
    assert_eq!(external["research"]["adapter_cost_usd"], 0.0);
    assert_eq!(
        research::ledger(&server.context)
            .unwrap()
            .summary(&wd::research::Store::session_id(first.sequence))
            .unwrap()
            .stop_reason,
        Some(ResearchStop::Superseded)
    );
    let stale = json!({"token":server.agent_token,"session_id":one["research"]["session_id"],"request_id":"stale","lookup":{"kind":"operator_message","message_sequence":first.sequence}});
    assert_eq!(
        research::lookup(&server.context, &serde_json::to_vec(&stale).unwrap()).0,
        400
    );
    let fallback = lookup(
        &server.context,
        second.sequence,
        "local",
        json!({"kind":"operator_message","message_sequence":second.sequence}),
    );
    assert_eq!(fallback["data"], "Revised request");
}

#[test]
fn captured_quotes_are_checked_and_portable_bundles_drop_host_authority() {
    let server = crate::serve::tests::test_server();
    let message = operator(&server.context, "Observed release evidence");
    let read = lookup(
        &server.context,
        message.sequence,
        "task",
        json!({"kind":"operator_message","message_sequence":message.sequence}),
    );
    let (mut draft, state, mut inputs) = wd::recovery_tests::fixture(false);
    inputs.insert(
        "flow.request".into(),
        json!("Observed release evidence plus uncaptured words"),
    );
    let reference = json!({"session_id":read["research"]["session_id"],"request_id":"task"});
    draft.research.sources=serde_json::from_value(json!([{"id":"doc1","location":"supplied","finding":"Observed release evidence","provenance":"host read",
        "origin":{"kind":"source_observed","input_port":"flow.request","quote":"Observed release evidence","capture":reference}}])).unwrap();
    let mut bad = draft.clone();
    if let Some(StatementOrigin::SourceObserved { quote, .. }) = &mut bad.research.sources[0].origin
    {
        *quote = "uncaptured words".into();
    }
    assert!(create(&server.context, PROGRAM, &inputs, bad, &state)
        .unwrap_err()
        .to_string()
        .contains("host capture"));
    let saved = create(&server.context, PROGRAM, &inputs, draft, &state).unwrap();
    provenance::validate_saved(&server.context, &saved).unwrap();
    let mut portable = saved.draft.clone();
    provenance::detach_chat(&mut portable);
    assert!(portable.research.usage.is_none());
    assert!(portable.research.session_id.is_none());
    assert!(matches!(
        &portable.research.sources[0].origin,
        Some(StatementOrigin::SourceObserved { capture: None, .. })
    ));
    let other = crate::serve::tests::test_server();
    assert!(provenance::validate_saved(&other.context, &saved).is_err());
}

#[test]
fn research_http_routes_authenticate_and_catalog_replays_preserve_the_budget() {
    let server = crate::serve::tests::test_server();
    let message = operator(&server.context, "Research this workflow");
    for path in [
        "/v1/agent/research",
        "/v1/agent/profiles",
        "/v1/agent/evidence",
    ] {
        let response = crate::serve::tests::request(
            server.address(),
            "POST",
            path,
            Some(r#"{"token":"wrong"}"#),
        );
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    }
    let body = json!({"token":server.agent_token}).to_string();
    let mut last = Value::Null;
    for path in [
        "/v1/agent/profiles",
        "/v1/agent/evidence",
        "/v1/agent/profiles",
    ] {
        let response = crate::serve::tests::request(server.address(), "POST", path, Some(&body));
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        last = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert!(last["error"].is_null(), "{last}");
    }
    assert_eq!(last["research"]["catalog_calls"], 2);
    assert_eq!(last["replayed"], true);
    assert!(last["data"]["profiles"].is_array());
    let body=json!({"token":server.agent_token,"session_id":last["research"]["session_id"],"request_id":"task","lookup":{"kind":"operator_message","message_sequence":message.sequence}}).to_string();
    let response =
        crate::serve::tests::request(server.address(), "POST", "/v1/agent/research", Some(&body));
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.contains("Research this workflow"));
    let before = research::ledger(&server.context)
        .unwrap()
        .summary(&wd::research::Store::session_id(message.sequence))
        .unwrap();
    let public =
        crate::serve::tests::request(server.address(), "GET", "/v1/workflow/profiles", None);
    assert!(public.starts_with("HTTP/1.1 200"));
    assert_eq!(
        research::ledger(&server.context)
            .unwrap()
            .summary(&before.session_id)
            .unwrap(),
        before
    );
}
