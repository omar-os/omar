//! Explicitly opted-in, bounded evaluation through the production provider and validators.
use super::*;
use crate::decisions::workflow::{
    AdviceCriterion, AdviceEvidence, AdviceInput, AdviceSubject, SubjectKind, CATALOG_VERSION,
};

#[derive(Deserialize)]
struct Corpus {
    schema_version: u32,
    version: String,
    label_status: String,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    id: String,
    profile_id: String,
    split: String,
    evidence: String,
    criterion: String,
    expected_outcome: String,
    tags: Vec<String>,
}

fn corpus() -> Corpus {
    serde_json::from_str(include_str!("../../../eval/jev-v1/corpus.json"))
        .expect("versioned evaluation corpus must decode")
}

fn input(case: &Case) -> AdviceInput {
    let kind = match case.profile_id.as_str() {
        "template-fit-v1" => SubjectKind::Draft,
        "scenario-coverage-v1" => SubjectKind::Proposal,
        _ => SubjectKind::Artifact,
    };
    AdviceInput {
        subject: AdviceSubject {
            id: Uuid::new_v4().to_string(),
            kind,
            chat_id: "synthetic-evaluation".into(),
            workspace_id: "synthetic-evaluation".into(),
            revision: case.id.clone(),
            sha256: sha256(&case.evidence),
        },
        profile_id: case.profile_id.clone(),
        criterion: AdviceCriterion {
            id: case.id.clone(),
            version: if kind == SubjectKind::Draft {
                CATALOG_VERSION
            } else {
                "1"
            }
            .into(),
            text: case.criterion.clone(),
            requires_complete: true,
        },
        evidence: vec![AdviceEvidence {
            id: "excerpt-1".into(),
            text: case.evidence.clone(),
            start: 0,
            end: case.evidence.chars().count(),
        }],
        complete: true,
        deterministic_failures: Vec::new(),
        responsibility_map: None,
        eligible_templates: vec![
            "launch-brief".into(),
            "supplied-research".into(),
            "writing-review".into(),
        ],
    }
}

#[test]
fn corpus_has_diverse_separate_splits_and_only_registry_labels() {
    let corpus = corpus();
    assert_eq!(corpus.schema_version, 1);
    assert_eq!(
        corpus.label_status,
        "synthetic-provisional-not-human-reviewed"
    );
    let mut ids = std::collections::BTreeSet::new();
    for case in &corpus.cases {
        assert!(ids.insert(&case.id));
        let request = workflow::prepare(&input(case)).unwrap();
        assert!(
            request["questions"]["outcome"]["criteria"]
                .get(&case.expected_outcome)
                .is_some(),
            "{}",
            case.id
        );
        assert!(matches!(case.split.as_str(), "development" | "held-out"));
    }
    for profile in [
        "template-fit-v1",
        "scenario-coverage-v1",
        "artifact-requirement-v1",
    ] {
        let cases: Vec<_> = corpus
            .cases
            .iter()
            .filter(|c| c.profile_id == profile)
            .collect();
        assert!(cases.len() >= 30);
        assert_eq!(cases.iter().filter(|c| c.split == "held-out").count(), 10);
        assert!(cases
            .iter()
            .any(|c| c.tags.iter().any(|t| t == "adversarial")));
        assert!(cases.iter().any(|c| c.tags.iter().any(|t| t == "chinese")));
    }
}

fn positive(outcome: &str) -> bool {
    matches!(outcome, "covered" | "appears_satisfied")
}
fn ratio(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator != 0).then(|| numerator as f64 / denominator as f64)
}

fn metrics(rows: &[Value]) -> Value {
    let surfaced: Vec<_> = rows.iter().filter(|r| r["status"] == "suggested").collect();
    let correct = surfaced
        .iter()
        .filter(|r| r["outcome"] == r["expected_outcome"])
        .count();
    let negative = rows
        .iter()
        .filter(|r| !positive(r["expected_outcome"].as_str().unwrap_or("")))
        .count();
    let false_positive = surfaced
        .iter()
        .filter(|r| {
            positive(r["outcome"].as_str().unwrap_or(""))
                && !positive(r["expected_outcome"].as_str().unwrap_or(""))
        })
        .count();
    json!({"attempted_cases": rows.len(), "surfaced": surfaced.len(), "precision_among_surfaced": ratio(correct, surfaced.len()),
        "coverage": ratio(surfaced.len(), rows.len()), "abstention_or_unavailability": ratio(rows.len()-surfaced.len(), rows.len()),
        "false_positive_covered_or_satisfied": false_positive, "false_positive_rate_among_expected_nonpositive": ratio(false_positive, negative),
        "labels": "provisional; these scores cannot authorize product enablement"})
}

#[test]
fn evaluation_metrics_count_false_reassurance_and_keep_abstention_separate() {
    let result = metrics(&[
        json!({"status":"suggested", "outcome":"covered", "expected_outcome":"missing_handoff"}),
        json!({"status":"needs_review", "outcome":"covered", "expected_outcome":"covered"}),
        json!({"status":"suggested", "outcome":"not_satisfied", "expected_outcome":"not_satisfied"}),
    ]);
    assert_eq!(result["precision_among_surfaced"], 0.5);
    assert_eq!(result["false_positive_covered_or_satisfied"], 1);
    assert_eq!(
        result["false_positive_rate_among_expected_nonpositive"],
        0.5
    );
    assert!(metrics(&[])["precision_among_surfaced"].is_null());
}

/// Ordinary CI (including `--ignored`) never dispatches. Only the wrapper
/// sets this opt-in and supplies all the request/estimated-cost bounds.
#[test]
#[ignore = "live provider evaluation requires scripts/jev-eval.sh and explicit limits"]
fn live_evaluation() {
    if std::env::var("OMAR_JEV_LIVE_EVAL").as_deref() != Ok("1") {
        eprintln!("Live Jev evaluation not opted in.");
        return;
    }
    run_live().expect("bounded live evaluation failed");
}

fn run_live() -> Result<()> {
    let limit: usize = std::env::var("OMAR_JEV_MAX_REQUESTS")?.parse()?;
    let max_cost: f64 = std::env::var("OMAR_JEV_MAX_ESTIMATED_USD")?.parse()?;
    let price: f64 = std::env::var("OMAR_JEV_INPUT_USD_PER_MILLION")?.parse()?;
    anyhow::ensure!(
        (1..=1000).contains(&limit)
            && max_cost.is_finite()
            && max_cost > 0.0
            && price.is_finite()
            && price > 0.0,
        "explicit positive request and estimated-cost limits are required"
    );
    let output = PathBuf::from(std::env::var("OMAR_JEV_REPORT")?);
    let split = std::env::var("OMAR_JEV_EVAL_SPLIT").unwrap_or_else(|_| "held-out".into());
    anyhow::ensure!(
        matches!(split.as_str(), "development" | "held-out"),
        "unknown evaluation split"
    );
    let corpus = corpus();
    let mut report = json!({"schema_version": 1, "corpus_version": corpus.version, "label_status": corpus.label_status,
        "model_requested": JEV_MODEL, "policy_version": crate::decisions::workflow::POLICY_VERSION,
        "profiles": ["template-fit-v1", "scenario-coverage-v1", "artifact-requirement-v1"], "split": split,
        "max_requests": limit, "max_estimated_usd": max_cost, "assumed_input_usd_per_million": price,
        "model_quality_validated": false, "product_enablement": "off",
        "cost_note": "Estimate uses request UTF-8 bytes as a conservative token proxy. Provider overhead, billing rules and taxes are unknown. This is not a provider-side spend cap. Actual invoice cost is unavailable.",
        "measured_invoice_cost_usd": null, "metrics": null, "by_profile": null, "cases": [],
        "baseline": {"policy": "always abstain", "coverage": 0.0, "precision_among_surfaced": null, "false_positive_covered_or_satisfied": 0}});
    if std::env::var_os("TYPESAFE_API_KEY").is_none() {
        report["status"] = json!("skipped_missing_credentials");
        report["requests_sent"] = json!(0);
        write_report(&output, &report)?;
        return Ok(());
    }
    let provider = TypeSafeProvider::new(&DecisionSupportConfig::default())?;
    let mut rows = Vec::new();
    let mut estimate = 0.0;
    let mut tokens = 0i64;
    let mut missing_usage = 0;
    let mut reason = "corpus_exhausted";
    for case in corpus.cases.iter().filter(|case| case.split == split) {
        if rows.len() >= limit {
            reason = "request_limit";
            break;
        }
        let input = input(case);
        let request = workflow::prepare(&input)?;
        let cost_bound = serde_json::to_vec(&request)?.len() as f64 * price / 1_000_000.0;
        if estimate + cost_bound > max_cost {
            reason = "estimated_cost_limit";
            break;
        }
        estimate += cost_bound;
        let started = std::time::Instant::now();
        let response = provider
            .evaluate(&request)
            .and_then(|response| workflow::response_result(&input, &request, response));
        let mut row = json!({"id": case.id, "profile_id": case.profile_id, "expected_outcome": case.expected_outcome,
            "latency_ms": started.elapsed().as_millis(), "estimated_cost_reserved_usd": cost_bound,
            "request_sha256": sha256(&serde_json::to_string(&request)?)});
        match response {
            Ok((status, result)) => {
                row["status"] = json!(status);
                row["outcome"] = json!(result.outcome);
                if let Some(used) = result.input_tokens {
                    tokens += used.max(0);
                } else {
                    missing_usage += 1;
                }
                row["result"] = json!(result);
            }
            Err(error) => {
                row["status"] = json!("unavailable");
                row["error"] = json!(error.to_string());
                missing_usage += 1;
            }
        }
        rows.push(row);
        // Persist after every paid call. No automatic retry on provider errors.
        report["status"] = json!("running_provisional");
        report["cases"] = json!(rows);
        report["requests_sent"] = json!(rows.len());
        write_report(&output, &report)?;
    }
    report["status"] = json!("completed_provisional_labels_unreviewed");
    report["stop_reason"] = json!(reason);
    report["metrics"] = metrics(&rows);
    let by_profile: BTreeMap<_, _> = [
        "template-fit-v1",
        "scenario-coverage-v1",
        "artifact-requirement-v1",
    ]
    .into_iter()
    .map(|profile| {
        let subset = rows
            .iter()
            .filter(|row| row["profile_id"] == profile)
            .cloned()
            .collect::<Vec<_>>();
        (profile, metrics(&subset))
    })
    .collect();
    report["by_profile"] = json!(by_profile);
    report["reported_input_tokens"] = json!(tokens);
    report["requests_missing_token_usage"] = json!(missing_usage);
    report["estimated_cost_reserved_usd"] = json!(estimate);
    report["estimated_cost_from_reported_tokens_usd"] = json!(tokens as f64 * price / 1_000_000.0);
    report["cases"] = json!(rows);
    report["requests_sent"] = json!(rows.len());
    write_report(&output, &report)
}

fn write_report(path: &Path, report: &Value) -> Result<()> {
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(directory)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&serde_json::to_vec_pretty(report)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}
