//! A reviewed native request observation is separate from installation, login
//! status, tool connectivity, task quality and the operator's legacy access flag.
use super::*;
use anyhow::{ensure, Result};
use std::collections::{BTreeMap, BTreeSet};

pub fn validate(profile: &ExecutionProfile) -> Result<()> {
    let Some(o) = &profile.access_observation else {
        return Ok(());
    };
    ensure!(
        store::identifier(&o.connection_scope),
        "model access observation needs a local connection scope"
    );
    ensure!(
        [&o.profile_digest, &o.report_digest]
            .iter()
            .all(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())),
        "model access requires configuration and report SHA-256 digests"
    );
    ensure!(
        [
            &o.backend_version,
            &o.runtime_contract,
            &o.reviewer,
            &o.report,
            &o.attempt_id,
            &o.limitations
        ]
        .iter()
        .all(|s| !s.trim().is_empty()),
        "model access needs a reviewed native request, runtime and limitations"
    );
    ensure!(
        o.observed_at > 0
            && o.observed_at <= o.reviewed_at
            && o.reviewed_at < o.expires_at
            && o.expires_at - o.observed_at <= capabilities::MAX_CONNECTIVITY_AGE_SECONDS,
        "model access observations require ordered dates and expire within 24 hours"
    );
    ensure!(
        [&o.observed_model, &o.observed_effort]
            .iter()
            .all(|s| s.as_ref().is_none_or(|s| !s.trim().is_empty())),
        "unreported native model/effort must remain null"
    );
    Ok(())
}

pub fn settings_mismatch(
    profile: &ExecutionProfile,
    model: Option<&str>,
    effort: Option<&str>,
) -> bool {
    (profile.identity.model_reference == ModelReferenceKind::Versioned
        && model.is_some_and(|m| m != profile.model))
        || (profile
            .effort
            .as_deref()
            .zip(effort)
            .is_some_and(|(requested, reported)| requested != reported))
}

pub fn assess(
    profile: &ExecutionProfile,
    scope: Option<&str>,
    versions: &BTreeMap<String, String>,
    at: u64,
) -> ModelAccessAssessment {
    let (status,reason)=match &profile.access_observation {
        _ if !profile.enabled || profile.access==ProfileAccess::Unavailable =>
            (ModelAccessStatus::OperatorUnavailable,"The operator has disabled this profile or marked access unavailable."),
        None => (ModelAccessStatus::Unknown,"No reviewed native request observation. Installation, login status and a verified flag do not establish model access."),
        Some(o) if Some(o.connection_scope.as_str())!=scope =>
            (ModelAccessStatus::ConnectionScopeChanged,"The observation belongs to a different local account/connection scope."),
        Some(o) if o.profile_digest!=profile.configuration_digest() =>
            (ModelAccessStatus::ConfigurationChanged,"The model settings or execution contract changed after the observed request."),
        Some(o) if o.runtime_contract!=profiles::RUNTIME_CONTRACT_REVISION || versions.get(&profile.backend)!=Some(&o.backend_version) =>
            (ModelAccessStatus::RuntimeChanged,"The observed native runtime differs from the installed version or its version is unknown."),
        Some(o) if o.reviewed_at>at => (ModelAccessStatus::NotYetValid,"The observation is not yet valid; check its dates and the local clock."),
        Some(o) if o.expires_at<=at => (ModelAccessStatus::Expired,"The reviewed model-access observation has expired."),
        Some(o) if settings_mismatch(profile,o.observed_model.as_deref(),o.observed_effort.as_deref()) =>
            (ModelAccessStatus::SettingsMismatch,"The native request reported settings that differ from the explicit requested configuration."),
        Some(o) => match o.outcome {
            ModelAccessOutcome::Succeeded => (ModelAccessStatus::ReviewedAvailable,"A current reviewed native request matches this configuration, runtime and local scope. This does not qualify output quality, tool access, immutable identity or cost."),
            ModelAccessOutcome::Unavailable => (ModelAccessStatus::ReviewedUnavailable,"A current reviewed native request reported model/account access unavailable."),
            ModelAccessOutcome::Inconclusive => (ModelAccessStatus::Inconclusive,"The reviewed native request did not establish availability; it remains a provisional candidate."),
        },
    };
    let current = matches!(
        status,
        ModelAccessStatus::ReviewedAvailable
            | ModelAccessStatus::ReviewedUnavailable
            | ModelAccessStatus::Inconclusive
            | ModelAccessStatus::SettingsMismatch
    );
    ModelAccessAssessment {
        status,
        reason: reason.into(),
        last_success_at: profile
            .access_observation
            .as_ref()
            .filter(|o| current && o.outcome == ModelAccessOutcome::Succeeded)
            .map(|o| o.observed_at),
        expires_at: profile
            .access_observation
            .as_ref()
            .filter(|_| current)
            .map(|o| o.expires_at),
        observed_model: profile
            .access_observation
            .as_ref()
            .filter(|_| current)
            .and_then(|o| o.observed_model.clone()),
        observed_effort: profile
            .access_observation
            .as_ref()
            .filter(|_| current)
            .and_then(|o| o.observed_effort.clone()),
    }
}

pub fn unavailable(status: ModelAccessStatus) -> bool {
    matches!(
        status,
        ModelAccessStatus::ReviewedUnavailable
            | ModelAccessStatus::OperatorUnavailable
            | ModelAccessStatus::SettingsMismatch
    )
}

/// Only consume host receipt/ledger artifacts, never model-authored claims.
/// The caller authenticates chat ownership and hashes the exact artifact bytes.
pub fn inspect_run(
    receipt: &ExecutionReceipt,
    agent: &str,
    invocations: &[serde_json::Value],
    turns: &[serde_json::Value],
    artifact_digests: BTreeMap<String, String>,
    at: u64,
) -> Result<NativeAccessCandidate> {
    let p = receipt
        .profiles
        .get(agent)
        .ok_or_else(|| anyhow::anyhow!("agent has no saved native profile"))?;
    let mut gaps = vec![];
    if receipt.status != "completed" || receipt.error.is_some() || receipt.finished_at.is_none() {
        gaps.push("The run has no completed, error-free terminal receipt.".into());
    }
    if receipt.connection_scope.is_none() {
        gaps.push("The run did not capture its local account/connection scope.".into());
    }
    if receipt.runtime_contract != profiles::RUNTIME_CONTRACT_REVISION {
        gaps.push("The run did not capture the current native execution contract.".into());
    }
    let version = receipt.backend_versions.get(&p.backend).cloned().flatten();
    if version.is_none() {
        gaps.push("The run did not capture its installed native CLI version.".into());
    }
    let invoked: Vec<_> = invocations
        .iter()
        .filter(|r| r["agent"] == agent && r["invoked"] == true)
        .collect();
    let ids: Vec<_> = invoked
        .iter()
        .filter_map(|r| r["invocation_id"].as_str().map(str::to_owned))
        .collect();
    if invoked.is_empty()
        || ids.len() != invoked.len()
        || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
        || invoked.iter().any(|r| {
            r["resources"]["owner"] != "model"
                || r["status"] != "contract_satisfied"
                || !r["error"].is_null()
        })
    {
        gaps.push("There are no complete, successful model invocation records for this agent, or the records are ambiguous.".into());
    }
    let mut requests = BTreeSet::new();
    let mut models = BTreeSet::new();
    let mut efforts = BTreeSet::new();
    let mut latest = None;
    let mut all_models = true;
    let mut all_efforts = true;
    let mut mismatch = false;
    for turn in turns {
        let time = turn["native_finished_at"].as_u64();
        let request = turn["native_request_id"].as_str();
        if turn["native_event"] != "turn.completed"
            || turn["process_success"] != true
            || request.is_none_or(|id| !store::identifier(id))
            || time.is_none_or(|t| t < receipt.started_at || t > at)
            || !requests.insert(request.unwrap_or_default().to_owned())
        {
            gaps.push(
                "The native ledger contains an unfinished, failed, duplicate or unbound request."
                    .into(),
            );
        }
        latest = latest.max(time);
        if let Some(model) = turn["model"].as_str().filter(|s| !s.is_empty()) {
            models.insert(model.to_owned());
        } else {
            all_models = false;
        }
        if let Some(effort) = turn["effort"].as_str().filter(|s| !s.is_empty()) {
            efforts.insert(effort.to_owned());
        } else {
            all_efforts = false;
        }
        mismatch |= settings_mismatch(p, turn["model"].as_str(), turn["effort"].as_str());
    }
    if turns.len() != invoked.len() || turns.is_empty() {
        gaps.push("Native completion evidence does not cover exactly the invoked model turns; missing usage is not success evidence.".into());
    }
    if latest
        .is_some_and(|time| at.saturating_sub(time) >= capabilities::MAX_CONNECTIVITY_AGE_SECONDS)
    {
        gaps.push("The observed native completion is too old to support a current 24-hour access observation.".into());
    }
    let status = if mismatch {
        AccessCandidateStatus::SettingsMismatch
    } else if gaps.is_empty() {
        AccessCandidateStatus::ReadyForReview
    } else {
        AccessCandidateStatus::InsufficientEvidence
    };
    if mismatch {
        gaps.push("A native turn reported settings that differ from the explicit request.".into());
    }
    gaps.push("Existing run artifacts only; no new model requests or account probes. Operator review is required before registry use. This is not evidence of task quality, subscription billing, tool connectivity or immutable model identity.".into());
    if !all_models || models.len() != 1 {
        gaps.push(
            "A single provider-reported model identity is unavailable for these turns.".into(),
        );
    }
    let mut invocation_ids = ids;
    invocation_ids.sort();
    let report_digest = digest(&serde_json::to_vec(&artifact_digests)?);
    Ok(NativeAccessCandidate {
        run_id: receipt.run_id.clone(),
        agent: agent.into(),
        profile_id: p.id.clone(),
        profile_digest: p.configuration_digest(),
        connection_scope: receipt.connection_scope.clone(),
        backend_version: version,
        runtime_contract: receipt.runtime_contract.clone(),
        status,
        observed_at: if status == AccessCandidateStatus::ReadyForReview {
            latest
        } else {
            None
        },
        observed_model: if all_models && models.len() == 1 {
            models.into_iter().next()
        } else {
            None
        },
        observed_effort: if all_efforts && efforts.len() == 1 {
            efforts.into_iter().next()
        } else {
            None
        },
        invocation_ids,
        native_request_ids: requests.into_iter().collect(),
        report_digest,
        artifact_digests,
        limitations: gaps,
        new_model_requests: 0,
        adapter_cost_usd: 0.0,
        recorded_run_cost_usd: receipt.cost_usd,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn observe(p: &mut ExecutionProfile, at: u64, version: &str) {
        p.access_observation = Some(ModelAccessObservation {
            outcome: ModelAccessOutcome::Succeeded,
            connection_scope: "test-environment".into(),
            profile_digest: p.configuration_digest(),
            backend_version: version.into(),
            runtime_contract: profiles::RUNTIME_CONTRACT_REVISION.into(),
            observed_at: at,
            reviewed_at: at,
            expires_at: at + 3600,
            reviewer: "Synthetic test reviewer".into(),
            report: "test://native-request".into(),
            report_digest: digest(b"fixture"),
            attempt_id: "test-request".into(),
            observed_model: None,
            observed_effort: None,
            limitations: "Synthetic policy fixture, not a real native request".into(),
        });
    }

    #[test]
    fn native_access_needs_reviewed_matching_scope_runtime_settings_and_dates() {
        let mut p = profiles::Registry::load(tempfile::tempdir().unwrap().path())
            .unwrap()
            .profiles
            .remove(0);
        let versions = BTreeMap::from([(p.backend.clone(), "test-cli".into())]);
        p.access = ProfileAccess::Verified;
        assert_eq!(
            assess(&p, Some("test-environment"), &versions, 110).status,
            ModelAccessStatus::Unknown
        );
        let original = p.configuration_digest();
        observe(&mut p, 100, "test-cli");
        assert_eq!(original, p.configuration_digest());
        p.validate().unwrap();
        let checked = assess(&p, Some("test-environment"), &versions, 110);
        assert_eq!(checked.status, ModelAccessStatus::ReviewedAvailable);
        assert_eq!(checked.last_success_at, Some(100));
        assert!(checked.observed_model.is_none());
        for case in 0..7 {
            let mut changed = p.clone();
            let status = match case {
                0 => assess(&changed, None, &versions, 110).status,
                1 => assess(&changed, Some("test-environment"), &BTreeMap::new(), 110).status,
                2 => assess(&changed, Some("test-environment"), &versions, 99).status,
                3 => assess(&changed, Some("test-environment"), &versions, 3700).status,
                4 => {
                    changed.model = "other".into();
                    assess(&changed, Some("test-environment"), &versions, 110).status
                }
                5 => {
                    changed.access_observation.as_mut().unwrap().outcome =
                        ModelAccessOutcome::Unavailable;
                    assess(&changed, Some("test-environment"), &versions, 110).status
                }
                _ => {
                    changed.access_observation.as_mut().unwrap().observed_effort =
                        Some("unexpected".into());
                    assess(&changed, Some("test-environment"), &versions, 110).status
                }
            };
            assert_ne!(status, ModelAccessStatus::ReviewedAvailable, "case {case}");
        }
        p.access = ProfileAccess::Unavailable;
        assert_eq!(
            assess(&p, Some("test-environment"), &versions, 110).status,
            ModelAccessStatus::OperatorUnavailable
        );
    }

    #[test]
    fn access_attestations_reject_missing_review_or_unbounded_freshness() {
        let mut original = profiles::Registry::load(tempfile::tempdir().unwrap().path())
            .unwrap()
            .profiles
            .remove(0);
        observe(&mut original, 100, "test-cli");
        for case in 0..6 {
            let mut p = original.clone();
            let o = p.access_observation.as_mut().unwrap();
            match case {
                0 => o.report_digest.clear(),
                1 => o.reviewer.clear(),
                2 => o.attempt_id.clear(),
                3 => o.expires_at = 86501,
                4 => o.reviewed_at = 99,
                _ => o.observed_model = Some(String::new()),
            }
            assert!(p.validate().is_err(), "case {case}");
        }
    }

    #[test]
    fn completed_native_request_is_reviewable_but_not_quality_cost_or_identity_proof() {
        let p = profiles::Registry::load(tempfile::tempdir().unwrap().path())
            .unwrap()
            .profiles
            .remove(0);
        let receipt:ExecutionReceipt=serde_json::from_value(json!({"schema_version":1,"run_id":"test-run","plan":null,"source_digest":"test",
            "profiles":{"flow.worker":p},"connection_scope":"test-environment","runtime_contract":profiles::RUNTIME_CONTRACT_REVISION,
            "started_at":100,"finished_at":110,"status":"completed","error":null,"observed_models":{},"backend_versions":{"codex":"test-cli"},
            "input_tokens":null,"output_tokens":null,"cost_usd":null})).unwrap();
        let invocation = json!({"invocation_id":"step-1","agent":"flow.worker","invoked":true,"status":"contract_satisfied","error":null,"resources":{"owner":"model"}});
        let turn = json!({"native_request_id":"request-1","native_event":"turn.completed","process_success":true,"native_finished_at":111,"model":null,"effort":null});
        let candidate = inspect_run(
            &receipt,
            "flow.worker",
            &[invocation.clone()],
            &[turn.clone()],
            BTreeMap::new(),
            120,
        )
        .unwrap();
        assert_eq!(candidate.status, AccessCandidateStatus::ReadyForReview);
        assert!(candidate.observed_model.is_none() && candidate.recorded_run_cost_usd.is_none());
        assert_eq!(candidate.new_model_requests, 0);
        assert_eq!(candidate.observed_at, Some(111));
        for case in 0..10 {
            let mut r = receipt.clone();
            let mut i = invocation.clone();
            let mut t = turn.clone();
            match case {
                0 => r.connection_scope = None,
                1 => r.runtime_contract.clear(),
                2 => r.status = "failed".into(),
                3 => i["invoked"] = json!(false),
                4 => i["resources"]["owner"] = json!("code"),
                5 => t["process_success"] = json!(false),
                6 => t["native_event"] = json!("unknown"),
                7 => t["native_finished_at"] = json!(200),
                8 => t["effort"] = json!("wrong"),
                _ => r.backend_versions.clear(),
            }
            assert_ne!(
                inspect_run(&r, "flow.worker", &[i], &[t], BTreeMap::new(), 120)
                    .unwrap()
                    .status,
                AccessCandidateStatus::ReadyForReview,
                "case {case}"
            );
        }
        assert_eq!(
            inspect_run(
                &receipt,
                "flow.worker",
                &[invocation],
                &[],
                BTreeMap::new(),
                120
            )
            .unwrap()
            .status,
            AccessCandidateStatus::InsufficientEvidence
        );
    }
}
