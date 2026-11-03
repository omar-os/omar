//! A declaration shortlists a candidate. Only a current, scope-bound reviewed
//! tool attempt satisfies a required connection at admission. This is operator
//! curation, not an automated credential probe or proof of future availability.
use super::*;
use anyhow::{ensure, Result};
use std::collections::{BTreeMap, BTreeSet};

/// A reviewed attempt is only a short-lived observation of one environment.
pub const MAX_CONNECTIVITY_AGE_SECONDS: u64 = 86_400;

fn digest_valid(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
}

pub fn validate_profile(profile: &ExecutionProfile) -> Result<()> {
    let mut declared = BTreeSet::new();
    for tool in &profile.tools {
        ensure!(
            store::identifier(tool) && declared.insert(tool),
            "invalid or duplicate tool ID"
        );
    }
    let mut detailed = BTreeSet::new();
    for tool in &profile.tool_capabilities {
        ensure!(
            declared.contains(&tool.id) && detailed.insert(&tool.id),
            "tool capability must have a unique declared tool ID: {}",
            tool.id
        );
        ensure!(
            tool.version > 0 && !tool.native_name.trim().is_empty(),
            "tool capability needs a version and native name"
        );
        ensure!(
            tool.permission_scope.iter().all(|s| !s.trim().is_empty()),
            "empty tool permission scope"
        );
        ensure!(
            tool.input_schema
                .as_ref()
                .is_none_or(serde_json::Value::is_object)
                && tool
                    .output_schema
                    .as_ref()
                    .is_none_or(serde_json::Value::is_object),
            "tool schemas must be objects or null"
        );
        if let Some(observation) = &tool.connectivity {
            ensure!(
                store::identifier(&observation.connection_scope)
                    && digest_valid(&observation.profile_digest)
                    && digest_valid(&observation.report_digest),
                "tool observation requires a connection scope and configuration/report digests"
            );
            ensure!(
                [
                    &observation.backend_version,
                    &observation.runtime_contract,
                    &observation.reviewer,
                    &observation.report,
                    &observation.attempt_id,
                    &observation.limitations
                ]
                .iter()
                .all(|s| !s.trim().is_empty()),
                "tool observation needs a reviewed actual attempt and limitations"
            );
            ensure!(
                observation.observed_at > 0
                    && observation.observed_at <= observation.reviewed_at
                    && observation.reviewed_at < observation.expires_at
                    && observation.expires_at - observation.observed_at
                        <= MAX_CONNECTIVITY_AGE_SECONDS,
                "tool observations require ordered dates and expire within 24 hours of the attempt"
            );
            if observation.outcome == ToolConnectionOutcome::Available {
                ensure!(tool.authentication != ToolAuthentication::Unknown && tool.effect != ToolEffect::Unknown
                    && !tool.permission_scope.is_empty(),
                    "available tool observations require declared authentication, effects and permission scope");
            }
        }
    }
    Ok(())
}

pub fn assess_tool(
    profile: &ExecutionProfile,
    tool: &str,
    connection_scope: Option<&str>,
    runtime_versions: &BTreeMap<String, String>,
    at: u64,
) -> ToolAccessDecision {
    let observation = profile
        .tool_capabilities
        .iter()
        .find(|t| t.id == tool)
        .and_then(|t| t.connectivity.as_ref());
    let (status, reason) = match observation {
        None => (ToolAccessStatus::Unknown, "Declared support only; no reviewed connection attempt."),
        Some(o) if connection_scope != Some(o.connection_scope.as_str()) =>
            (ToolAccessStatus::ConnectionScopeChanged, "The observation belongs to a different local connection scope."),
        Some(o) if o.profile_digest != profile.configuration_digest() =>
            (ToolAccessStatus::ConfigurationChanged, "The model, isolation or tool configuration changed after this observation."),
        Some(o) if o.runtime_contract != profiles::RUNTIME_CONTRACT_REVISION
            || runtime_versions.get(&profile.backend) != Some(&o.backend_version) =>
            (ToolAccessStatus::RuntimeChanged, "The installed backend version is unknown or differs from the observed runtime."),
        Some(o) if o.reviewed_at > at =>
            (ToolAccessStatus::NotYetValid, "The observation is not yet valid; check its dates and the local clock."),
        Some(o) if o.expires_at <= at =>
            (ToolAccessStatus::Expired, "The reviewed connection attempt has expired."),
        Some(o) if o.outcome == ToolConnectionOutcome::Unavailable =>
            (ToolAccessStatus::ReviewedUnavailable, "The current reviewed attempt reported this tool unavailable."),
        Some(_) => (ToolAccessStatus::ReviewedAvailable,
            "A current operator-reviewed tool attempt matches this profile, runtime and connection scope; availability can still change."),
    };
    ToolAccessDecision {
        tool: tool.into(),
        status,
        reason: reason.into(),
    }
}

pub fn required_tool_access(
    profile: &ExecutionProfile,
    steps: &[&StepSpec],
    connection_scope: Option<&str>,
    runtime_versions: &BTreeMap<String, String>,
    at: u64,
) -> Vec<ToolAccessDecision> {
    steps
        .iter()
        .flat_map(|s| s.required_tools.iter())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|tool| assess_tool(profile, tool, connection_scope, runtime_versions, at))
        .collect()
}

pub fn blockers(access: &[ToolAccessDecision]) -> Vec<String> {
    access
        .iter()
        .filter(|a| a.status != ToolAccessStatus::ReviewedAvailable)
        .map(|a| format!("Required tool '{}': {}", a.tool, a.reason))
        .collect()
}

pub fn require_launchable(decisions: &[SelectionDecision]) -> Result<()> {
    for decision in decisions {
        ensure!(
            decision.profile_id.is_some(),
            "missing execution profile for {}",
            decision.agent
        );
        // Recomputed by the server at admission, never trusted from a request.
        ensure!(
            decision.capability_blockers.is_empty(),
            "execution capability is unresolved for {}: {}",
            decision.agent,
            decision.capability_blockers.join(" ")
        );
        ensure!(
            decision
                .tool_access
                .iter()
                .all(|a| a.status == ToolAccessStatus::ReviewedAvailable),
            "required tool access is unresolved for {}",
            decision.agent
        );
    }
    Ok(())
}
