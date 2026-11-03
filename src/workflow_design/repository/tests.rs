use super::*;

pub(crate) fn fixture() -> Suite {
    let task: Value = serde_json::from_str(include_str!(
        "../../../evals/workflow-quality/repository-repair/inputs/repo-schedule-01.json"
    ))
    .unwrap();
    let reference: Value = serde_json::from_str(include_str!(
        "../../../evals/workflow-quality/repository-repair/references/repo-schedule-01.json"
    ))
    .unwrap();
    Suite {
        schema_version: 1,
        id: "schedule".into(),
        description: "Owned diagnostic regression suite".into(),
        reviewed_by: "automated fixture review, not independent labels".into(),
        reviewed_at: 1,
        adapter: "python_json_api_v1".into(),
        image: format!("sha256:{}", "a".repeat(64)),
        entrypoint: task["entrypoint"].as_str().unwrap().into(),
        files: serde_json::from_value(task["files"].clone()).unwrap(),
        allowed_patch_paths: serde_json::from_value(task["allowed_patch_paths"].clone()).unwrap(),
        timeout_ms: 5000,
        protected_tests: serde_json::from_value(reference["protected_tests"].clone()).unwrap(),
    }
}

#[test]
fn suite_contract_rejects_unbounded_commands_paths_and_unreviewed_tests() {
    let original = fixture();
    original.validate().unwrap();
    for mutation in 0..12 {
        let mut suite = original.clone();
        match mutation {
            0 => suite.id = "../escape".into(),
            1 => suite.image = "python:latest".into(),
            2 => suite.entrypoint = "os:system;echo bad".into(),
            3 => suite.timeout_ms = 60001,
            4 => suite
                .files
                .insert("../outside".into(), "bad".into())
                .map(|_| ())
                .unwrap_or(()),
            5 => suite.allowed_patch_paths.push("new.py".into()),
            6 => suite.protected_tests.clear(),
            7 => suite.protected_tests.push(suite.protected_tests[0].clone()),
            8 => suite.reviewed_by.clear(),
            9 => suite.adapter = "host_shell".into(),
            10 => suite
                .files
                .insert("candidate_app/core.py/child".into(), "bad".into())
                .map(|_| ())
                .unwrap_or(()),
            _ => suite.reviewed_at = u64::MAX,
        }
        assert!(suite.validate().is_err(), "mutation {mutation}");
    }
    let mut changed = original.clone();
    changed.protected_tests[0].expected = json!("changed");
    assert_ne!(original.digest().unwrap(), changed.digest().unwrap());
    let catalog = original.summary().unwrap().to_string();
    assert!(!catalog.contains("protected_tests"));
    assert!(!catalog.contains("expected"));
}

#[test]
fn staged_suite_and_runtime_context_are_required_even_for_valid_json() {
    let suite = fixture();
    let check = OutputCheck::RepositoryPatch {
        port: "result".into(),
        verifier_id: suite.id.clone(),
        verifier_digest: suite.digest().unwrap(),
        repository_input: "request".into(),
    };
    let writes = BTreeMap::from([(
        "result".into(),
        json!({"changes":[],"explanation":"valid shape"}),
    )]);
    assert!(recovery::check_and_route("check", &[check.clone()], None, writes.clone()).is_err());
    let directory = tempfile::tempdir().unwrap();
    let invocations = directory.path().join("invocations");
    fs::create_dir(&invocations).unwrap();
    let mut receipts = vec![];
    let result = check_and_route(
        RuntimeContext {
            directory: Some(&invocations),
            invocation_id: "i",
            reaction: "check",
            stop_dir: None,
            deadline: Instant::now() + Duration::from_secs(1),
        },
        &[check.clone()],
        None,
        writes.clone(),
        &mut receipts,
    );
    assert!(result.is_err());
    assert!(receipts.is_empty());
    stage(
        directory.path(),
        &BTreeMap::from([(suite.id.clone(), suite.clone())]),
    )
    .unwrap();
    let mut tampered = suite;
    tampered.protected_tests[0].expected = json!("forged");
    crate::topology::write_json_atomic(
        &directory.path().join("repository-verifiers/schedule.json"),
        &tampered,
    )
    .unwrap();
    let result = check_and_route(
        RuntimeContext {
            directory: Some(&invocations),
            invocation_id: "i",
            reaction: "check",
            stop_dir: None,
            deadline: Instant::now() + Duration::from_secs(1),
        },
        &[check],
        None,
        writes,
        &mut receipts,
    );
    assert!(result.unwrap_err().to_string().contains("digest mismatch"));
}
