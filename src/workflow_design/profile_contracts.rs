//! Dated descriptive facts and explicit bounded model escalation. Facts never
//! become measured costs or enforced token limits merely by being configured.
use super::*;
use anyhow::{ensure, Context, Result};
use std::collections::BTreeMap;

fn digest_valid(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
}

fn validate_source(source: &ReviewedProfileSource, profile_digest: &str) -> Result<()> {
    ensure!(
        digest_valid(profile_digest) && digest_valid(&source.source_digest),
        "profile facts require SHA-256 configuration and source digests"
    );
    ensure!(
        [
            &source.source,
            &source.locator,
            &source.reviewer,
            &source.conditions
        ]
        .iter()
        .all(|s| !s.trim().is_empty()),
        "profile facts need a source, locator, reviewer and applicability conditions"
    );
    ensure!(
        source.observed_at > 0
            && source.observed_at <= source.reviewed_at
            && source.reviewed_at < source.expires_at,
        "profile facts require ordered observation, review and expiry dates"
    );
    Ok(())
}

pub fn validate_profile(profile: &ExecutionProfile) -> Result<()> {
    ensure!(
        profile
            .identity
            .declared_provider
            .as_ref()
            .is_none_or(|p| store::identifier(p)),
        "invalid declared provider identifier"
    );
    if let Some(limits) = &profile.advertised_limits {
        validate_source(&limits.source, &limits.profile_digest)?;
        ensure!(
            limits.context_window_tokens.is_some() || limits.max_output_tokens.is_some(),
            "empty advertised limit record; use null for unknown limits"
        );
        ensure!(
            limits.context_window_tokens.is_none_or(|n| n > 0)
                && limits.max_output_tokens.is_none_or(|n| n > 0),
            "advertised token limits must be positive"
        );
        if let (Some(context), Some(output)) =
            (limits.context_window_tokens, limits.max_output_tokens)
        {
            ensure!(
                output <= context,
                "advertised output limit exceeds context window"
            );
        }
    }
    if let Some(economics) = &profile.economics {
        validate_source(&economics.source, &economics.profile_digest)?;
        ensure!(
            economics
                .connection_scope
                .as_ref()
                .is_none_or(|s| store::identifier(s))
                && economics
                    .subscription_plan
                    .as_ref()
                    .is_none_or(|s| !s.trim().is_empty()),
            "invalid economics scope or subscription plan"
        );
        for price in &economics.prices {
            ensure!(
                price.quantity > 0
                    && price.price_usd.is_finite()
                    && price.price_usd >= 0.0
                    && !price.conditions.trim().is_empty(),
                "prices require finite nonnegative amounts, explicit units and conditions"
            );
        }
        if let Some(quota) = &economics.quota {
            ensure!(
                !quota.unit.trim().is_empty()
                    && (quota.limit.is_some() || quota.remaining.is_some()),
                "quota needs a unit and at least one known quantity"
            );
            ensure!(
                quota.window_seconds.is_none_or(|n| n > 0),
                "quota window must be positive"
            );
            if let (Some(limit), Some(remaining)) = (quota.limit, quota.remaining) {
                ensure!(remaining <= limit, "remaining quota exceeds its limit");
            }
            if quota.remaining.is_some() {
                ensure!(
                    economics.connection_scope.is_some()
                        && economics.source.expires_at - economics.source.observed_at
                            <= capabilities::MAX_CONNECTIVITY_AGE_SECONDS,
                    "remaining quota requires a local scope and expires within 24 hours"
                );
                if let Some(reset) = quota.resets_at {
                    ensure!(
                        reset > economics.source.observed_at
                            && economics.source.expires_at <= reset,
                        "remaining quota must expire by its next reset"
                    );
                }
            }
        }
    }
    if let Some(fallback) = &profile.fallback {
        ensure!(
            store::identifier(&fallback.profile_id)
                && fallback.profile_id != profile.id
                && !fallback.reason.trim().is_empty(),
            "fallback needs a different target profile and an escalation reason"
        );
        ensure!(
            fallback.max_attempts == 1
                && fallback.max_deadline_ms > 0
                && fallback.max_deadline_ms <= 3_600_000,
            "profile fallback supports one repair attempt with a deadline up to one hour"
        );
        ensure!(
            fallback.max_cost_usd.is_none(),
            "native fallback spend caps are unsupported; do not infer a cap from published prices"
        );
    }
    Ok(())
}

fn source_status(
    profile: &ExecutionProfile,
    digest: &str,
    source: &ReviewedProfileSource,
    at: u64,
) -> ProfileFactStatus {
    if digest != profile.configuration_digest() {
        ProfileFactStatus::ConfigurationChanged
    } else if source.reviewed_at > at {
        ProfileFactStatus::NotYetValid
    } else if source.expires_at <= at {
        ProfileFactStatus::Expired
    } else {
        ProfileFactStatus::Current
    }
}

pub fn assess(
    profile: &ExecutionProfile,
    connection_scope: Option<&str>,
    at: u64,
) -> ProfileFactAssessment {
    ProfileFactAssessment {
        advertised_limits: profile
            .advertised_limits
            .as_ref()
            .map_or(ProfileFactStatus::Unknown, |r| {
                source_status(profile, &r.profile_digest, &r.source, at)
            }),
        economics: profile
            .economics
            .as_ref()
            .map_or(ProfileFactStatus::Unknown, |r| {
                if r.connection_scope
                    .as_deref()
                    .is_some_and(|scope| Some(scope) != connection_scope)
                {
                    ProfileFactStatus::ConnectionScopeChanged
                } else {
                    source_status(profile, &r.profile_digest, &r.source, at)
                }
            }),
        session_behavior: "persistent_per_runtime_agent".into(),
        structured_output_control: "host_checks_before_handoff_when_declared".into(),
        token_limits_enforced: false,
        spend_limits_enforced: false,
    }
}

/// The launched backend and isolation flags cannot widen. Tool permissions are
/// reviewed declarations, not proof that a native process is sandboxed.
fn compatible_fallback(original: &ExecutionProfile, target: &ExecutionProfile) -> Result<()> {
    ensure!(
        original.backend == target.backend && original.isolation == target.isolation,
        "fallback cannot change the native backend or isolation contract"
    );
    ensure!(
        original.identity.declared_provider.is_some()
            && original.identity.declared_provider == target.identity.declared_provider,
        "fallback requires the same explicitly declared provider"
    );
    ensure!(
        target.tools.iter().all(|t| original.tools.contains(t)),
        "fallback cannot add tool capabilities"
    );
    for id in &target.tools {
        if id == "workflow_ports" && target.isolation == ExecutionIsolation::PortsOnly {
            continue;
        }
        let from = original
            .tool_capabilities
            .iter()
            .find(|t| &t.id == id)
            .context("fallback requires detailed original tool permissions")?;
        let to = target
            .tool_capabilities
            .iter()
            .find(|t| &t.id == id)
            .context("fallback requires detailed target tool permissions")?;
        ensure!(
            from.native_name == to.native_name
                && from.version == to.version
                && from.transport == to.transport
                && from.authentication == to.authentication
                && from.authentication != ToolAuthentication::Unknown
                && from.effect == to.effect
                && from.effect != ToolEffect::Unknown
                && !from.permission_scope.is_empty()
                && !to.permission_scope.is_empty()
                && to
                    .permission_scope
                    .iter()
                    .all(|p| from.permission_scope.contains(p)),
            "fallback cannot expand or leave unknown the declared tool permission contract"
        );
    }
    Ok(())
}

pub fn validate_registry(profiles: &[ExecutionProfile]) -> Result<()> {
    for original in profiles {
        if let Some(policy) = &original.fallback {
            let target = profiles
                .iter()
                .find(|p| p.id == policy.profile_id)
                .context("unknown fallback profile")?;
            ensure!(target.fallback.is_none(), "fallback chains and cycles are unsupported; at most one model escalation is permitted");
            compatible_fallback(original, target)?;
        }
    }
    Ok(())
}

/// Attach routes after selecting every runtime agent, so a repair is tied to
/// its actual saved binding. This never creates a repair or changes a profile.
pub fn bind_repairs(
    steps: &[StepSpec],
    bindings: &BTreeMap<String, ExecutionProfile>,
    decisions: &mut [SelectionDecision],
) {
    for step in steps {
        if step
            .interface
            .as_ref()
            .is_some_and(|i| i.owner != StepOwner::Model)
            || matches!(
                step.operation,
                Operation::ExactValidation | Operation::ExactTransformation
            )
        {
            continue;
        }
        let Some(recovery) = &step.recovery else {
            continue;
        };
        let Some(original) = bindings.get(&step.agent) else {
            continue;
        };
        let Some(repair) = steps
            .iter()
            .find(|s| s.reaction == recovery.repair_reaction)
        else {
            continue;
        };
        // Code and actual Web repair steps do not switch a model profile.
        if repair
            .interface
            .as_ref()
            .is_some_and(|i| i.owner != StepOwner::Model)
        {
            continue;
        }
        let target = bindings.get(&repair.agent);
        let check = (|| -> Result<String> {
            let target = target.context("repair has no eligible execution profile")?;
            ensure!(
                recovery.max_repair_attempts == 1
                    && step.execution.max_invocations == 1
                    && repair.execution.max_invocations == 1
                    && step.execution.effect == StepEffect::Repeatable
                    && repair.execution.effect == StepEffect::Repeatable,
                "model repair requires repeatable steps and one invocation per step"
            );
            ensure!(
                repair
                    .required_tools
                    .iter()
                    .all(|t| step.required_tools.contains(t)),
                "model repair cannot add required tools to the original step"
            );
            if original.id == target.id {
                return Ok("The declared graph permits one repair using the same profile, then the protected recheck.".into());
            }
            let policy = original
                .fallback
                .as_ref()
                .context("this profile does not permit a different repair model")?;
            ensure!(
                policy.profile_id == target.id,
                "repair model differs from the permitted fallback profile"
            );
            ensure!(
                repair
                    .execution
                    .deadline_ms
                    .is_some_and(|d| d > 0 && d <= policy.max_deadline_ms),
                "repair step needs a deadline within its profile fallback bound"
            );
            compatible_fallback(original, target)?;
            Ok(format!("{} One separate repair invocation after validation rejection, followed by the protected recheck.", policy.reason))
        })();
        let permitted = check.is_ok();
        let reason = match check {
            Ok(reason) => reason,
            Err(error) => format!("{error:#}"),
        };
        if let Some(decision) = decisions.iter_mut().find(|d| d.agent == step.agent) {
            if !permitted {
                decision
                    .capability_blockers
                    .push(format!("Repair for '{}': {reason}", step.reaction));
                decision.provisional = true;
            }
            decision.repair_routes.push(ProfileRepairRoute {
                step: step.reaction.clone(),
                repair_reaction: repair.reaction.clone(),
                repair_agent: repair.agent.clone(),
                profile_id: target.map(|p| p.id.clone()),
                trigger: FallbackTrigger::ValidationRejected,
                max_attempts: 1,
                deadline_ms: repair.execution.deadline_ms,
                max_cost_usd: None,
                permitted,
                reason,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn registry() -> profiles::Registry {
        profiles::Registry::load(tempfile::tempdir().unwrap().path()).unwrap()
    }

    fn source() -> ReviewedProfileSource {
        ReviewedProfileSource {
            source: "test://profile-facts".into(),
            source_digest: digest(b"synthetic source"),
            locator: "Synthetic row 1".into(),
            observed_at: 100,
            reviewed_at: 110,
            expires_at: 200,
            reviewer: "Test reviewer".into(),
            conditions: "Synthetic fixture; not provider prices or limits".into(),
        }
    }

    pub(crate) fn add_facts(p: &mut ExecutionProfile) {
        let binding = p.configuration_digest();
        p.advertised_limits = Some(AdvertisedModelLimits {
            profile_digest: binding.clone(),
            context_window_tokens: Some(8000),
            max_output_tokens: Some(2000),
            source: source(),
        });
        p.economics = Some(ProfileEconomics {
            profile_digest: binding,
            billing_model: BillingModel::Subscription,
            subscription_plan: Some("Synthetic plan".into()),
            prices: vec![],
            quota: Some(ProfileQuota {
                unit: "test calls".into(),
                limit: Some(10),
                remaining: Some(0),
                window_seconds: Some(3600),
                resets_at: Some(200),
            }),
            connection_scope: Some("test-scope".into()),
            source: source(),
        });
    }

    #[test]
    fn profile_facts_preserve_unknown_cost_and_expire_without_becoming_enforced_limits() {
        let mut p = registry().profiles.remove(0);
        let unknown = assess(&p, None, 120);
        assert_eq!(unknown.economics, ProfileFactStatus::Unknown);
        assert!(!unknown.token_limits_enforced && !unknown.spend_limits_enforced);
        let before = p.configuration_digest();
        add_facts(&mut p);
        p.validate().unwrap();
        assert_eq!(
            before,
            p.configuration_digest(),
            "a source update is not an executable configuration change"
        );
        let current = assess(&p, Some("test-scope"), 120);
        assert_eq!(current.advertised_limits, ProfileFactStatus::Current);
        assert_eq!(current.economics, ProfileFactStatus::Current);
        assert!(!current.token_limits_enforced && !current.spend_limits_enforced);
        assert!(
            p.economics.as_ref().unwrap().prices.is_empty(),
            "subscription access is not a zero-dollar per-call price"
        );
        assert_eq!(
            assess(&p, None, 120).economics,
            ProfileFactStatus::ConnectionScopeChanged
        );
        assert_eq!(
            assess(&p, Some("test-scope"), 109).advertised_limits,
            ProfileFactStatus::NotYetValid
        );
        assert_eq!(
            assess(&p, Some("test-scope"), 200).economics,
            ProfileFactStatus::Expired
        );
        p.model = "another-model".into();
        assert_eq!(
            assess(&p, Some("test-scope"), 120).advertised_limits,
            ProfileFactStatus::ConfigurationChanged
        );
    }

    #[test]
    fn facts_reject_unsourced_values_bad_units_and_unscoped_remaining_quota() {
        let mut original = registry().profiles.remove(0);
        add_facts(&mut original);
        for case in 0..11 {
            let mut p = original.clone();
            match case {
                0 => p
                    .advertised_limits
                    .as_mut()
                    .unwrap()
                    .source
                    .source_digest
                    .clear(),
                1 => p
                    .advertised_limits
                    .as_mut()
                    .unwrap()
                    .source
                    .conditions
                    .clear(),
                2 => p.advertised_limits.as_mut().unwrap().max_output_tokens = Some(9000),
                3 => p.advertised_limits.as_mut().unwrap().context_window_tokens = Some(0),
                4 => p.economics.as_mut().unwrap().connection_scope = None,
                5 => {
                    p.economics
                        .as_mut()
                        .unwrap()
                        .quota
                        .as_mut()
                        .unwrap()
                        .remaining = Some(11)
                }
                6 => p.economics.as_mut().unwrap().source.expires_at = 201,
                7 => p.economics.as_mut().unwrap().prices.push(ProfilePrice {
                    unit: PriceUnit::Request,
                    quantity: 0,
                    price_usd: 1.0,
                    conditions: "test".into(),
                }),
                8 => p.economics.as_mut().unwrap().prices.push(ProfilePrice {
                    unit: PriceUnit::InputToken,
                    quantity: 1000,
                    price_usd: f64::NAN,
                    conditions: "test".into(),
                }),
                9 => p.economics.as_mut().unwrap().prices.push(ProfilePrice {
                    unit: PriceUnit::OutputToken,
                    quantity: 1000,
                    price_usd: -1.0,
                    conditions: "test".into(),
                }),
                _ => p.economics.as_mut().unwrap().source.reviewed_at = 99,
            }
            assert!(p.validate().is_err(), "invalid facts case {case}");
        }
    }

    fn fallback_registry() -> profiles::Registry {
        let mut r = registry();
        r.evidence.clear();
        r.profiles.retain(|p| p.backend == "codex");
        for p in &mut r.profiles {
            p.tools = vec!["workflow_ports".into()];
            p.isolation = ExecutionIsolation::PortsOnly;
            p.identity.declared_provider = Some("test-provider".into());
        }
        r.profiles
            .iter_mut()
            .find(|p| p.id == "codex-economy")
            .unwrap()
            .fallback = Some(ProfileFallbackPolicy {
            profile_id: "codex-quality".into(),
            trigger: FallbackTrigger::ValidationRejected,
            reason: "Use the quality candidate for one rejected draft".into(),
            max_attempts: 1,
            max_deadline_ms: 5000,
            max_cost_usd: None,
        });
        r
    }

    #[test]
    fn profile_fallback_rejects_cycles_provider_permission_changes_and_unenforced_spend() {
        let original = fallback_registry();
        original.validate().unwrap();
        for case in 0..8 {
            let mut r = original.clone();
            let quality = r
                .profiles
                .iter_mut()
                .find(|p| p.id == "codex-quality")
                .unwrap();
            match case {
                0 => quality.identity.declared_provider = Some("different-provider".into()),
                1 => quality.identity.declared_provider = None,
                2 => quality.isolation = ExecutionIsolation::Native,
                3 => {
                    quality.isolation = ExecutionIsolation::Native;
                    quality.tools.push("shell".into());
                }
                4 => {
                    quality.fallback = Some(ProfileFallbackPolicy {
                        profile_id: "codex-economy".into(),
                        trigger: FallbackTrigger::ValidationRejected,
                        reason: "A cycle".into(),
                        max_attempts: 1,
                        max_deadline_ms: 5000,
                        max_cost_usd: None,
                    });
                }
                _ => {
                    let fallback = r
                        .profiles
                        .iter_mut()
                        .find(|p| p.id == "codex-economy")
                        .unwrap()
                        .fallback
                        .as_mut()
                        .unwrap();
                    match case {
                        5 => fallback.max_attempts = 2,
                        6 => fallback.max_cost_usd = Some(0.01),
                        _ => fallback.profile_id = "missing".into(),
                    }
                }
            }
            assert!(r.validate().is_err(), "invalid fallback case {case}");
        }
        let mut r = original;
        for p in &mut r.profiles {
            p.isolation = ExecutionIsolation::Native;
            p.tools = vec!["files".into()];
        }
        assert!(
            r.validate().is_err(),
            "unknown native permission scopes cannot permit fallback"
        );
        for p in &mut r.profiles {
            capabilities_tests::observe(p, "files", 100, "test-cli");
        }
        r.validate().unwrap();
        r.profiles
            .iter_mut()
            .find(|p| p.id == "codex-quality")
            .unwrap()
            .tool_capabilities[0]
            .permission_scope
            .push("wider".into());
        assert!(r.validate().is_err());
    }

    #[test]
    fn repair_uses_only_the_explicit_bound_target_and_keeps_one_attempt_and_deadline() {
        let mut r = fallback_registry();
        let first:StepSpec=serde_json::from_value(json!({"reaction":"flow.work","agent":"flow.worker",
            "operation":"structured_extraction","purpose":"Extract","acceptance_criteria":["Supported"],
            "execution":{"effect":"repeatable","max_invocations":1},
            "recovery":{"retry_port":"flow.retry","repair_reaction":"flow.repair","recheck_reaction":"flow.check",
                "accepted_port":"flow.accepted","failure_port":"flow.blocked","max_repair_attempts":1}})).unwrap();
        let mut repair = first.clone();
        repair.agent = "flow.repairer".into();
        repair.reaction = "flow.repair".into();
        repair.recovery = None;
        repair.execution.deadline_ms = Some(5000);
        let mut steps = vec![first, repair];
        let backends = BTreeMap::from([
            ("flow.worker".into(), "codex".into()),
            ("flow.repairer".into(), "codex".into()),
        ]);
        let prefs = RoutingPreferences {
            allowed_backends: vec![],
            overrides: BTreeMap::from([
                ("flow.worker".into(), "codex-economy".into()),
                ("flow.repairer".into(), "codex-quality".into()),
            ]),
        };
        let select = |r: &profiles::Registry, steps: &[StepSpec]| {
            r.select(
                steps,
                &prefs,
                &backends,
                &BTreeMap::new(),
                &BTreeMap::new(),
                120,
            )
            .unwrap()
            .0
        };
        let decisions = select(&r, &steps);
        capabilities::require_launchable(&decisions).unwrap();
        let route = &decisions
            .iter()
            .find(|d| d.agent == "flow.worker")
            .unwrap()
            .repair_routes[0];
        assert_eq!(route.profile_id.as_deref(), Some("codex-quality"));
        assert_eq!(route.max_attempts, 1);
        assert_eq!(route.deadline_ms, Some(5000));
        assert!(route.max_cost_usd.is_none());
        steps[1].execution.deadline_ms = Some(5001);
        assert!(capabilities::require_launchable(&select(&r, &steps)).is_err());
        steps[1].execution.deadline_ms = None;
        assert!(capabilities::require_launchable(&select(&r, &steps)).is_err());
        steps[1].execution.deadline_ms = Some(5000);
        r.profiles
            .iter_mut()
            .find(|p| p.id == "codex-economy")
            .unwrap()
            .fallback = None;
        assert!(capabilities::require_launchable(&select(&r, &steps)).is_err());
    }
}
