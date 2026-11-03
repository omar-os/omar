//! Reviewed published scores are source-bound candidate evidence, not local
//! qualification or a ranking across incomparable scales and harnesses.
use super::*;
use anyhow::{ensure, Context, Result};

pub const MAX_SNAPSHOT_BYTES: usize = 65_536;

pub fn content_digest(evidence: &CapabilityEvidence) -> String {
    let mut unsigned = evidence.clone();
    unsigned.reviewed_at = 0;
    if let Some(result) = &mut unsigned.benchmark_result {
        result.review = None;
    }
    digest(&serde_json::to_vec(&unsigned).expect("serializable evidence"))
}

fn sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn validate(evidence: &CapabilityEvidence) -> Result<()> {
    let Some(b) = &evidence.benchmark_result else {
        return Ok(());
    };
    ensure!(
        matches!(
            evidence.source_kind,
            EvidenceSourceKind::ProviderClaim | EvidenceSourceKind::PublicBenchmark
        ) && !evidence.locally_qualified
            && evidence.qualification.is_none(),
        "published scores cannot carry local qualification"
    );
    ensure!(
        store::identifier(&evidence.id)
            && evidence
                .author
                .as_ref()
                .is_some_and(|s| !s.trim().is_empty()),
        "published results need an evidence ID and source author"
    );
    ensure!(
        [
            &evidence.source,
            &evidence.finding,
            &evidence.limitations,
            &b.benchmark,
            &b.task_slice,
            &b.measured_model,
            &b.measurement_conditions,
            &b.applicability,
            &b.dataset_overlap,
            &b.contamination,
            &b.source_row.locator
        ]
        .iter()
        .all(|s| !s.trim().is_empty()),
        "published result provenance and transfer limitations are required"
    );
    b.target_scope.validate()?;
    ensure!(
        sha256(&b.profile_digest),
        "published result needs an exact target profile digest"
    );
    ensure!(
        b.retrieved_at > 0
            && b.published_at
                .is_none_or(|at| at > 0 && at <= b.retrieved_at),
        "published/retrieval dates are invalid"
    );
    ensure!(
        [
            &b.benchmark_version,
            &b.measured_backend,
            &b.measured_harness,
            &b.measured_effort,
            &b.unit
        ]
        .iter()
        .all(|s| s.as_ref().is_none_or(|s| !s.trim().is_empty())),
        "unknown benchmark settings must be null"
    );
    ensure!(
        b.measured_tools
            .as_ref()
            .is_none_or(|tools| tools.iter().all(|s| !s.trim().is_empty())),
        "unknown tools must be null; an empty list means explicitly no tools"
    );
    ensure!(
        b.sample_size.is_none_or(|n| n > 0)
            && b.score.is_finite()
            && [b.cost_usd, b.latency_ms]
                .iter()
                .all(|n| n.is_none_or(|n| n.is_finite() && n >= 0.0)),
        "invalid benchmark score, sample size, cost or latency"
    );
    if let Some(u) = &b.uncertainty {
        ensure!(
            u.lower.is_finite()
                && u.upper.is_finite()
                && u.lower <= b.score
                && b.score <= u.upper
                && !u.method.trim().is_empty(),
            "invalid benchmark uncertainty interval"
        );
    }
    let source = &b.source_row;
    ensure!(
        !source.snapshot.is_empty()
            && source.snapshot.len() <= MAX_SNAPSHOT_BYTES
            && source.snapshot_digest == digest(source.snapshot.as_bytes()),
        "source snapshot size or digest mismatch"
    );
    let snapshot: serde_json::Value =
        serde_json::from_str(&source.snapshot).context("benchmark snapshot must be JSON")?;
    let row = snapshot
        .pointer(&source.row_pointer)
        .context("source row pointer does not resolve")?;
    ensure!(row.is_object(), "source row must be a JSON object");
    let descendant = |pointer: &str| pointer.starts_with(&(source.row_pointer.clone() + "/"));
    ensure!(
        descendant(&source.score_pointer)
            && descendant(&source.model_pointer)
            && source.unit_pointer.as_ref().is_none_or(|p| descendant(p)),
        "score/model/unit pointers must belong to the same source row"
    );
    ensure!(
        snapshot
            .pointer(&source.score_pointer)
            .and_then(|v| v.as_f64())
            == Some(b.score),
        "score differs from its source row"
    );
    ensure!(
        snapshot
            .pointer(&source.model_pointer)
            .and_then(|v| v.as_str())
            == Some(&b.measured_model),
        "model differs from its source row"
    );
    ensure!(
        match (&source.unit_pointer, &b.unit) {
            (None, None) => true,
            (Some(p), Some(unit)) => snapshot.pointer(p).and_then(|v| v.as_str()) == Some(unit),
            _ => false,
        },
        "unit differs from its source row; unreported units must remain null"
    );
    if let Some(review) = &b.review {
        ensure!(
            !review.reviewer.trim().is_empty()
                && !review.rationale.trim().is_empty()
                && review.reviewed_at >= b.retrieved_at
                && review.reviewed_at == evidence.reviewed_at
                && review.content_digest == content_digest(evidence),
            "benchmark review is incomplete or its content changed"
        );
    } else {
        ensure!(
            evidence.reviewed_at == 0,
            "unreviewed published results must not claim a review date"
        );
    }
    Ok(())
}

pub fn assess(
    e: &CapabilityEvidence,
    p: &ExecutionProfile,
    at: u64,
) -> CapabilityEvidenceAssessment {
    let result = |status, reasons: Vec<String>| CapabilityEvidenceAssessment { status, reasons };
    if e.expires_at <= at {
        return result(
            CapabilityEvidenceStatus::Stale,
            vec!["Evidence has expired.".into()],
        );
    }
    if e.profile_id != p.id || e.profile_version != p.version || e.reviewed_at > at {
        return result(
            CapabilityEvidenceStatus::Insufficient,
            vec!["Profile binding or review date does not match.".into()],
        );
    }
    if let Some(b) = &e.benchmark_result {
        if b.profile_digest != p.configuration_digest() {
            return result(
                CapabilityEvidenceStatus::Stale,
                vec!["Target profile configuration changed after import.".into()],
            );
        }
        let mut gaps = vec![];
        if b.review.as_ref().is_none_or(|r| !r.accepted) {
            gaps.push("No accepted operator review of this exact source record.".into());
        }
        if b.retrieved_at > at {
            gaps.push("Retrieval date is in the future.".into());
        }
        if b.benchmark_version.is_none() {
            gaps.push("Benchmark version is unknown.".into());
        }
        if b.unit.is_none() {
            gaps.push("Score unit is unknown.".into());
        }
        if b.sample_size.is_none() {
            gaps.push("Sample size is unknown.".into());
        }
        if b.measured_harness.is_none() {
            gaps.push("Measured harness is unknown.".into());
        }
        if !gaps.is_empty() {
            return result(CapabilityEvidenceStatus::Insufficient, gaps);
        }
        return result(CapabilityEvidenceStatus::Candidate, vec!["Reviewed published result for candidate selection only; task transfer, account access and local quality are not established.".into()]);
    }
    if e.locally_qualified
        && e.qualification.as_ref().is_some_and(|q| {
            q.profile_digest == p.configuration_digest()
                && q.runtime_contract == profiles::RUNTIME_CONTRACT_REVISION
        })
    {
        return result(CapabilityEvidenceStatus::LocallyValidated, vec!["Reviewed local record; routing still requires exact task/compiled contract and installed CLI matching.".into()]);
    }
    if e.locally_qualified {
        return result(
            CapabilityEvidenceStatus::Stale,
            vec!["Local qualification configuration or runtime changed.".into()],
        );
    }
    if e.reviewed_at == 0 || e.source_kind == EvidenceSourceKind::Unknown {
        return result(
            CapabilityEvidenceStatus::Insufficient,
            vec!["Evidence provenance is unreviewed or unclassified.".into()],
        );
    }
    result(CapabilityEvidenceStatus::Candidate, vec!["Descriptive candidate evidence; not a local qualification or a source-bound numerical result.".into()])
}

pub fn applies(
    e: &CapabilityEvidence,
    p: &ExecutionProfile,
    scope: Option<&QualificationScope>,
    at: u64,
) -> bool {
    matches!(
        assess(e, p, at).status,
        CapabilityEvidenceStatus::Candidate | CapabilityEvidenceStatus::LocallyValidated
    ) && e
        .benchmark_result
        .as_ref()
        .is_none_or(|b| Some(&b.target_scope) == scope)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub const AT: u64 = 1793650000;

    pub fn fixture() -> (profiles::Registry, CapabilityEvidence) {
        let registry: profiles::Registry =
            serde_json::from_str(include_str!("../../config/execution-profiles.example.json"))
                .unwrap();
        let mut record: CapabilityEvidence = serde_json::from_str(include_str!(
            "../../evals/workflow-quality/benchmark-import/record.example.json"
        ))
        .unwrap();
        let b = record.benchmark_result.as_mut().unwrap();
        b.profile_digest = registry.profiles[0].configuration_digest();
        b.source_row.snapshot =
            include_str!("../../evals/workflow-quality/benchmark-import/snapshot.example.json")
                .into();
        b.source_row.snapshot_digest = digest(b.source_row.snapshot.as_bytes());
        (registry, record)
    }

    pub fn review(e: &mut CapabilityEvidence, accepted: bool) {
        e.reviewed_at = AT;
        let content_digest = content_digest(e);
        e.benchmark_result.as_mut().unwrap().review = Some(BenchmarkReview {
            accepted,
            reviewer: "Synthetic reviewer".into(),
            reviewed_at: AT,
            rationale: "Test fixture only".into(),
            content_digest,
        });
    }

    #[test]
    fn scores_models_and_units_are_bound_to_the_same_source_row_and_review() {
        let (_, original) = fixture();
        validate(&original).unwrap();
        for mutation in [
            "score",
            "model",
            "unit",
            "digest",
            "row",
            "review",
            "qualification",
        ] {
            let mut e = original.clone();
            review(&mut e, true);
            validate(&e).unwrap();
            let b = e.benchmark_result.as_mut().unwrap();
            match mutation {
                "score" => b.score = 99.0,
                "model" => b.measured_model = "different-model".into(),
                "unit" => b.unit = Some("fraction".into()),
                "digest" => b.source_row.snapshot.push(' '),
                "row" => b.source_row.score_pointer = "/outside/score".into(),
                "review" => e.finding.push_str(" changed after review"),
                "qualification" => e.locally_qualified = true,
                _ => unreachable!(),
            }
            assert!(validate(&e).is_err(), "{mutation}");
        }
        let mut e = original;
        e.benchmark_result.as_mut().unwrap().uncertainty = Some(BenchmarkUncertainty {
            lower: 45.0,
            upper: 60.0,
            method: "synthetic".into(),
        });
        assert!(validate(&e).is_err());
    }

    #[test]
    fn review_expiry_settings_and_missing_information_have_distinct_states() {
        let (registry, mut e) = fixture();
        let p = &registry.profiles[0];
        assert_eq!(
            assess(&e, p, AT).status,
            CapabilityEvidenceStatus::Insufficient
        );
        review(&mut e, false);
        assert_eq!(
            assess(&e, p, AT).status,
            CapabilityEvidenceStatus::Insufficient
        );
        review(&mut e, true);
        validate(&e).unwrap();
        assert_eq!(
            assess(&e, p, AT).status,
            CapabilityEvidenceStatus::Candidate
        );
        assert_eq!(
            assess(&e, p, e.expires_at).status,
            CapabilityEvidenceStatus::Stale
        );
        let mut changed = p.clone();
        changed.effort = Some("low".into());
        assert_eq!(
            assess(&e, &changed, AT).status,
            CapabilityEvidenceStatus::Stale
        );
        let mut incomplete = e.clone();
        incomplete.benchmark_result.as_mut().unwrap().sample_size = None;
        review(&mut incomplete, true);
        assert_eq!(
            assess(&incomplete, p, AT).status,
            CapabilityEvidenceStatus::Insufficient
        );
        let scope = &e.benchmark_result.as_ref().unwrap().target_scope;
        assert!(applies(&e, p, Some(scope), AT));
        let mut unrelated = scope.clone();
        unrelated.task_slice = "other-task".into();
        assert!(!applies(&e, p, Some(&unrelated), AT));
        assert!(!applies(&e, p, None, AT));
    }

    #[test]
    fn published_results_never_qualify_economy_and_only_link_matching_steps() {
        let (mut registry, mut e) = fixture();
        review(&mut e, true);
        registry.evidence.push(e.clone());
        let mut step = crate::workflow_design::recovery_tests::fixture(false)
            .0
            .steps
            .remove(0);
        step.operation = Operation::StructuredExtraction;
        step.required_tools.clear();
        step.qualification_scope = Some(e.benchmark_result.as_ref().unwrap().target_scope.clone());
        let backend = std::collections::BTreeMap::from([(step.agent.clone(), "codex".into())]);
        let select = |registry: &profiles::Registry, step: StepSpec| {
            registry
                .select(
                    &[step],
                    &RoutingPreferences::default(),
                    &backend,
                    &Default::default(),
                    &Default::default(),
                    AT,
                )
                .unwrap()
        };
        let (decisions, _) = select(&registry, step.clone());
        assert_eq!(decisions[0].profile_id.as_deref(), Some("codex-quality"));
        assert!(decisions[0].provisional);
        assert!(decisions[0].evidence_ids.contains(&e.id));
        step.qualification_scope.as_mut().unwrap().task_slice = "unrelated".into();
        assert!(!select(&registry, step.clone()).0[0]
            .evidence_ids
            .contains(&e.id));
        registry.profiles[0].tier = ProfileTier::Economy;
        assert!(select(&registry, step).0[0].profile_id.is_none());
    }
}
