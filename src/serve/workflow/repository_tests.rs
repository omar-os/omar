use super::*;
const PROGRAM: &str = include_str!("../../../tests/topology/src/ValidatedHandoff.omar");

#[test]
fn repository_review_binds_input_suite_and_staged_admission_without_exposing_tests() {
    let server = crate::serve::tests::test_server();
    let suite = wd::repository::tests::fixture();
    let path = server
        .context
        .omar_dir
        .join("workflow-verifiers/schedule.json");
    topology::write_json_atomic(&path, &suite).unwrap();
    let (mut draft, state, mut inputs) = wd::recovery_tests::fixture(false);
    inputs.insert("flow.request".into(),json!({"files":suite.files,"allowed_patch_paths":suite.allowed_patch_paths,"entrypoint":suite.entrypoint}).to_string().into());
    for step in &mut draft.steps {
        if !step.output_checks.is_empty() {
            step.output_checks = vec![OutputCheck::RepositoryPatch {
                port: "flow.result".into(),
                verifier_id: suite.id.clone(),
                verifier_digest: suite.digest().unwrap(),
                repository_input: "flow.request".into(),
            }];
        }
    }
    let plan = create(&server.context, PROGRAM, &inputs, draft.clone(), &state).unwrap();
    let (status, value) = evidence(&server.context);
    assert_eq!(status, 200);
    assert_eq!(
        value["repository_verifiers"][0]["digest"],
        suite.digest().unwrap()
    );
    assert!(value["repository_verifiers"][0]
        .get("protected_tests")
        .is_none());
    let request: StartRunRequest = serde_json::from_value(
        json!({"program":PROGRAM,"inputs":inputs,"plan":{"id":plan.id,"revision":plan.revision}}),
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    admit(
        &server.context,
        &request,
        &state,
        "repository",
        directory.path(),
    )
    .unwrap();
    let staged: wd::repository::Suite = serde_json::from_slice(
        &fs::read(directory.path().join("repository-verifiers/schedule.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(staged.digest().unwrap(), suite.digest().unwrap());
    let mut modified = suite.clone();
    modified.protected_tests[0].expected = json!("changed");
    topology::write_json_atomic(&path, &modified).unwrap();
    assert!(admit(
        &server.context,
        &request,
        &state,
        "changed",
        directory.path()
    )
    .unwrap_err()
    .to_string()
    .contains("verifier changed"));
    topology::write_json_atomic(&path, &suite).unwrap();
    inputs.insert("flow.request".into(),json!({"files":{},"allowed_patch_paths":suite.allowed_patch_paths,"entrypoint":suite.entrypoint}).to_string().into());
    assert!(create(&server.context, PROGRAM, &inputs, draft, &state)
        .unwrap_err()
        .to_string()
        .contains("snapshot"));
    let mut native = state.clone();
    native.agents.get_mut("flow.writer").unwrap().backend = "Codex".into();
    assert!(
        wd::repository::resolve(&server.context.omar_dir, &plan, &native)
            .unwrap_err()
            .to_string()
            .contains("ports_only")
    );
}
