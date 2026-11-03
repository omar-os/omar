use super::*;

#[test]
fn published_catalog_exposes_review_status_and_reassesses_changed_settings() {
    let server = crate::serve::tests::test_server();
    let (mut registry, mut record) = wd::benchmark::tests::fixture();
    wd::benchmark::tests::review(&mut record, true);
    record.reviewed_at = wd::now();
    record
        .benchmark_result
        .as_mut()
        .unwrap()
        .review
        .as_mut()
        .unwrap()
        .reviewed_at = record.reviewed_at;
    let id = record.id.clone();
    registry.evidence.push(record);
    let path = server.context.omar_dir.join("execution-profiles.json");
    fs::write(&path, serde_json::to_vec(&registry).unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    let response =
        crate::serve::tests::request(server.address(), "GET", "/v1/workflow/evidence", None);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let body: Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["assessments"][&id]["status"], "candidate");
    assert_eq!(
        body["evidence"].as_array().unwrap().last().unwrap()["locally_qualified"],
        false
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    registry.profiles[0].effort = Some("low".into());
    fs::write(&path, serde_json::to_vec(&registry).unwrap()).unwrap();
    assert_eq!(
        evidence(&server.context).1["assessments"][&id]["status"],
        "stale"
    );
}
