use super::*;

fn sample(id: &str) -> (Value, Input) {
    let p = package(id, None).unwrap();
    let input = serde_json::from_value(p["sample"].clone()).unwrap();
    (p, input)
}

#[test]
fn catalog_preserves_thirty_unique_jobs_and_valid_samples() {
    let c = catalog().unwrap();
    let templates = c["templates"].as_array().unwrap();
    assert_eq!(templates.len(), 30);
    let mut ids = BTreeSet::new();
    for (i, t) in templates.iter().enumerate() {
        assert_eq!(t["number"], i + 1);
        assert!(ids.insert(t["id"].as_str().unwrap()));
        assert_eq!(t["digest"].as_str().unwrap().len(), 64);
        let input: Input = serde_json::from_value(t["sample"].clone()).unwrap();
        if t["status"] == "runnable" {
            prepare(
                t["id"].as_str().unwrap(),
                Some(t["digest"].as_str().unwrap()),
                &input,
            )
            .unwrap();
            result::check(
                t["id"].as_str().unwrap(),
                t["digest"].as_str().unwrap(),
                &serde_json::to_string(&input).unwrap(),
                &t["example_result"],
            )
            .unwrap();
        }
    }
}

#[test]
fn preflight_rejects_stale_packages_missing_data_and_duplicate_sources() {
    let (_, mut input) = sample("proposal");
    assert!(prepare("proposal", Some("stale"), &input).is_err());
    input.sources.push(input.sources[0].clone());
    assert!(prepare("proposal", None, &input).is_err());
    input.sources.pop();
    input.data.clear();
    assert!(prepare("proposal", None, &input).is_err());
    assert!(prepare("test", None, &input).is_err());
}

#[test]
fn proposal_prices_are_exact_and_unknown_lines_are_not_zero() {
    let (_, mut input) = sample("proposal");
    let prepared = prepare("proposal", None, &input).unwrap();
    assert_eq!(prepared["calculated"]["priced_subtotal_cents"], 600000);
    assert_eq!(
        prepared["calculated"]["priced_total_before_tax_cents"],
        540000
    );
    input.data.get_mut("pricing").unwrap()["lines"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"L3","service_id":"missing","quantity":1}));
    let p = prepare("proposal", None, &input).unwrap();
    assert_eq!(p["calculated"]["all_lines_priced"], false);
    assert!(p["calculated"]["lines"][2]["amount_cents"].is_null());
    input.data.get_mut("pricing").unwrap()["discount_bps"] = json!(10001);
    assert!(prepare("proposal", None, &input).is_err());
}

#[test]
fn duplicate_metric_rows_fail_and_missing_sources_remain_unknown() {
    let (_, mut input) = sample("weekly-review");
    let p = prepare("weekly-review", None, &input).unwrap();
    assert_eq!(p["calculated"]["metrics"][0]["current"], 120);
    assert!(p["calculated"]["metrics"][2]["current"].is_null());
    input.data.get_mut("metrics").unwrap()[0]["current"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"c1","value":100}));
    assert!(prepare("weekly-review", None, &input).is_err());
}

#[test]
fn account_scoring_preserves_unknowns_and_exclusions() {
    let (_, input) = sample("target-accounts");
    let p = prepare("target-accounts", None, &input).unwrap();
    let rows = p["calculated"]["accounts"].as_array().unwrap();
    assert_eq!(rows.len(), 25);
    assert_eq!(
        rows.iter().find(|r| r["id"] == "A02").unwrap()["unknown_weight"],
        30
    );
    assert_eq!(rows.last().unwrap()["excluded"], true);
}

fn questionnaire_result(input: &Input) -> Value {
    json!({"summary":"Questionnaire draft; review unknowns.","rows":input.data["questions"].as_array().unwrap().iter().map(|q|json!({"id":q["id"],"text":"Evidence needs review.","status":"unknown","citations":[]})).collect::<Vec<_>>(),"limitations":["Unsupported answers are unknown."]})
}

#[test]
fn answer_matrix_blocks_missing_reordered_and_unsupported_answers() {
    let (p, input) = sample("questionnaire");
    let digest = p["digest"].as_str().unwrap();
    let encoded = serde_json::to_string(&input).unwrap();
    let mut value = questionnaire_result(&input);
    result::check("questionnaire", digest, &encoded, &value).unwrap();
    value["rows"].as_array_mut().unwrap().swap(0, 1);
    assert!(result::check("questionnaire", digest, &encoded, &value).is_err());
    value = questionnaire_result(&input);
    value["rows"][9] = json!({"id":"Q-10","text":"Yes, certified","status":"cited","citations":[{"source_id":"access","quote":"Access reviews happen quarterly and when people leave."}]});
    assert!(result::check("questionnaire", digest, &encoded, &value).is_err());
    value["rows"].as_array_mut().unwrap().pop();
    assert!(result::check("questionnaire", digest, &encoded, &value).is_err());
}

#[test]
fn quotes_must_exist_and_expired_evidence_cannot_be_cited() {
    let (p, mut input) = sample("research");
    let digest = p["digest"].as_str().unwrap();
    let mut value = json!({"summary":"Compare options","rows":[{"id":"options","text":"Option A supports CSV.","status":"cited","citations":[{"source_id":"option-a","quote":"Invented quote"}]},{"id":"tradeoffs","text":"Compare needs","status":"inference","citations":[]},{"id":"recommendation","text":"Review options","status":"inference","citations":[]},{"id":"unknowns","text":"Unspecified limits","status":"unknown","citations":[]}],"limitations":[]});
    assert!(result::check(
        "research",
        digest,
        &serde_json::to_string(&input).unwrap(),
        &value
    )
    .is_err());
    value["rows"][0]["citations"][0]["quote"] =
        json!("Option A supports CSV imports and costs 10 units.");
    result::check(
        "research",
        digest,
        &serde_json::to_string(&input).unwrap(),
        &value,
    )
    .unwrap();
    input.sources[0].valid_until = Some("2020-01-01".into());
    assert!(result::check(
        "research",
        digest,
        &serde_json::to_string(&input).unwrap(),
        &value
    )
    .is_err());
}

#[test]
fn bundle_contains_digested_outputs_and_safe_csv() {
    let (p, input) = sample("questionnaire");
    let mut value = questionnaire_result(&input);
    value["rows"][0]["text"] = json!("=HYPERLINK(\"bad\")");
    let bundle = result::bundle(
        "questionnaire",
        p["digest"].as_str().unwrap(),
        &serde_json::to_string(&input).unwrap(),
        &value,
    )
    .unwrap();
    assert_eq!(bundle["status"], "needs_information");
    let artifacts = bundle["artifacts"].as_array().unwrap();
    assert_eq!(artifacts.len(), 6);
    assert!(artifacts
        .iter()
        .find(|a| a["name"] == "records.csv")
        .unwrap()["content"]
        .as_str()
        .unwrap()
        .contains("'=HYPERLINK"));
    for artifact in artifacts {
        assert_eq!(
            artifact["digest"],
            crate::template_library::digest(artifact["content"].as_str().unwrap().as_bytes())
        );
    }
}
