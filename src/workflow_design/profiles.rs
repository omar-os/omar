use super::*;
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub const ROUTING_POLICY_REVISION: &str = "workflow-routing-v10";
pub const RUNTIME_CONTRACT_REVISION: &str = "native-profiles-v5";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub schema_version: u32,
    pub profiles: Vec<ExecutionProfile>,
    pub evidence: Vec<CapabilityEvidence>,
    /// Operator-owned, non-secret identity of the workspace/account/tool setup.
    /// Rotate when connections or permissions change. Never imported from a plan.
    #[serde(default)]
    pub connection_scope: Option<String>,
}

impl Registry {
    pub fn load(omar_dir: &Path) -> Result<Self> {
        let path = omar_dir.join("execution-profiles.json");
        let registry: Self = if path.exists() {
            serde_json::from_slice(&std::fs::read(path)?)?
        } else {
            serde_json::from_str(include_str!("../../config/execution-profiles.example.json"))?
        };
        registry.validate()?;
        Ok(registry)
    }

    pub fn revision(&self) -> String {
        super::digest(&serde_json::to_vec(self).expect("serializable registry"))
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1,
            "unsupported profile registry schema"
        );
        let mut ids = BTreeSet::new();
        ensure!(
            self.connection_scope
                .as_ref()
                .is_none_or(|s| store::identifier(s)),
            "invalid local connection scope"
        );
        for p in &self.profiles {
            p.validate()?;
            ensure!(ids.insert(p.id.as_str()), "duplicate profile ID: {}", p.id);
        }
        profile_contracts::validate_registry(&self.profiles)?;
        let mut evidence_ids = BTreeSet::new();
        for e in &self.evidence {
            ensure!(evidence_ids.insert(&e.id), "duplicate evidence ID");
            ensure!(
                self.profiles.iter().any(|p| p.id == e.profile_id
                    && p.version == e.profile_version
                    && p.operations.contains(&e.operation)),
                "evidence refers to unknown profile/version/operation"
            );
            ensure!(
                e.expires_at > e.reviewed_at && !e.source.is_empty() && !e.limitations.is_empty(),
                "invalid evidence provenance or validity window"
            );
            e.validate_qualification()?;
            benchmark::validate(e)?;
        }
        Ok(())
    }

    /// Select one profile per runtime agent. A shared session must satisfy ALL
    /// its operations; a conflicting step override cannot switch it mid-run.
    pub fn select(
        &self,
        steps: &[StepSpec],
        preferences: &RoutingPreferences,
        backends: &BTreeMap<String, String>,
        step_contracts: &BTreeMap<String, String>,
        runtime_versions: &BTreeMap<String, String>,
        at: u64,
    ) -> Result<(Vec<SelectionDecision>, BTreeMap<String, ExecutionProfile>)> {
        self.validate()?;
        let mut groups: BTreeMap<&str, Vec<&StepSpec>> = BTreeMap::new();
        for step in steps {
            if let Some(scope) = &step.qualification_scope {
                scope.validate()?;
            }
            if !matches!(
                step.operation,
                Operation::ExactValidation | Operation::ExactTransformation
            ) && !step.agent.is_empty()
                && backends
                    .get(&step.agent)
                    .is_none_or(|b| !crate::topology::is_web_backend(b))
            {
                groups.entry(&step.agent).or_default().push(step);
            }
        }
        for agent in preferences.overrides.keys() {
            ensure!(
                groups.contains_key(agent.as_str()),
                "profile override has no model-backed steps: {agent}"
            );
        }
        let mut selections = Vec::new();
        let mut bindings = BTreeMap::new();
        for (agent, steps) in groups {
            let override_id = preferences.overrides.get(agent);
            let model_access = |p: &ExecutionProfile| {
                access::assess(p, self.connection_scope.as_deref(), runtime_versions, at)
            };
            let access_ready = |p: &ExecutionProfile| {
                model_access(p).status == ModelAccessStatus::ReviewedAvailable
            };
            let tool_access = |p: &ExecutionProfile| {
                capabilities::required_tool_access(
                    p,
                    &steps,
                    self.connection_scope.as_deref(),
                    runtime_versions,
                    at,
                )
            };
            let tools_ready =
                |p: &ExecutionProfile| capabilities::blockers(&tool_access(p)).is_empty();
            let qualified = |p: &ExecutionProfile| {
                steps.iter().all(|s| {
                    self.evidence.iter().any(|e| {
                        e.profile_id == p.id
                            && e.profile_version == p.version
                            && e.operation == s.operation
                            && e.locally_qualified
                            && e.reviewed_at <= at
                            && e.expires_at > at
                            && e.qualification.as_ref().is_some_and(|q| {
                                Some(&q.scope) == s.qualification_scope.as_ref()
                                    && step_contracts.get(&s.reaction)
                                        == Some(&q.step_contract_digest)
                                    && q.profile_digest == p.configuration_digest()
                                    && q.runtime_contract == RUNTIME_CONTRACT_REVISION
                                    && runtime_versions.get(&p.backend) == Some(&q.backend_version)
                            })
                    })
                })
            };
            let routine_low_consequence = steps.iter().all(|step| {
                step.qualification_scope.as_ref().is_some_and(|scope| {
                    scope.complexity == TaskComplexity::Routine
                        && scope.consequence == TaskConsequence::Low
                })
            });
            let mut candidates: Vec<_> = self
                .profiles
                .iter()
                .filter(|p| {
                    let capable = steps.iter().all(|step| {
                        p.operations.contains(&step.operation)
                            && step.required_tools.iter().all(|t| p.tools.contains(t))
                            && step.qualification_scope.as_ref().is_none_or(|scope| {
                                scope
                                    .input_modalities
                                    .iter()
                                    .all(|m| p.input_modalities.contains(m))
                                    && scope
                                        .output_modalities
                                        .iter()
                                        .all(|m| p.output_modalities.contains(m))
                            })
                    });
                    // Explicit trials still require capability and availability;
                    // automatic economical routes additionally require evidence.
                    let economical_route_allowed = p.tier != ProfileTier::Economy
                        || override_id.is_some()
                        || (access_ready(p)
                            && qualified(p)
                            && tools_ready(p)
                            && routine_low_consequence);
                    p.enabled
                        && !access::unavailable(model_access(p).status)
                        && backends.get(agent).is_some_and(|b| b == &p.backend)
                        && (preferences.allowed_backends.is_empty()
                            || preferences.allowed_backends.contains(&p.backend))
                        && override_id.is_none_or(|id| id == &p.id)
                        && capable
                        && economical_route_allowed
                })
                .collect();
            candidates.sort_by_key(|p| {
                (
                    !tools_ready(p),
                    !qualified(p),
                    !access_ready(p),
                    p.tier != ProfileTier::Economy,
                    p.id.as_str(),
                )
            });
            let p = candidates.first().copied();
            let evidence_ids: Vec<String> = p
                .map(|p| {
                    self.evidence
                        .iter()
                        .filter(|e| {
                            e.profile_id == p.id
                                && e.profile_version == p.version
                                && e.reviewed_at <= at
                                && e.expires_at > at
                                && steps.iter().any(|s| s.operation == e.operation)
                                && steps.iter().any(|s| {
                                    s.operation == e.operation
                                        && benchmark::applies(
                                            e,
                                            p,
                                            s.qualification_scope.as_ref(),
                                            at,
                                        )
                                })
                                && e.qualification.as_ref().is_none_or(|q| {
                                    q.profile_digest == p.configuration_digest()
                                        && steps.iter().any(|s| {
                                            s.operation == e.operation
                                                && s.qualification_scope.as_ref() == Some(&q.scope)
                                        })
                                })
                        })
                        .map(|e| e.id.clone())
                        .collect()
                })
                .unwrap_or_default();
            let evidence_assessments = p
                .map(|p| {
                    self.evidence
                        .iter()
                        .filter(|e| evidence_ids.contains(&e.id))
                        .map(|e| (e.id.clone(), benchmark::assess(e, p, at)))
                        .collect()
                })
                .unwrap_or_default();
            let access = p.map(&tool_access).unwrap_or_default();
            let capability_blockers = capabilities::blockers(&access);
            selections.push(SelectionDecision {
                agent: agent.into(), profile_id: p.map(|p| p.id.clone()), evidence_ids, evidence_assessments,
                provisional: p.is_none_or(|p| !qualified(p) || !access_ready(p) || !tools_ready(p)),
                model_access: p.map(&model_access).unwrap_or_default(),
                tool_access: access,
                capability_blockers,
                facts: p.map(|p| profile_contracts::assess(p, self.connection_scope.as_deref(), at)).unwrap_or_default(),
                repair_routes: Vec::new(),
                reason: match p {
                    None => "Missing capability: no eligible configured profile meets every operation, tool, modality and current model-access requirement.".into(),
                    Some(p) if override_id.is_some() => format!("Operator selected {}. Model access evidence: {:?}; task qualification: {}.", p.label, model_access(p).status, qualified(p)),
                    Some(p) if qualified(p) => "Selected using current reviewed local evidence matching every task scope, profile setting and runtime version assigned to this agent.".into(),
                    Some(_) => "Provisional quality candidate. No current local qualification; account access and output quality still need verification.".into(),
                },
            });
            if let Some(p) = p {
                bindings.insert(agent.into(), p.clone());
            }
        }
        profile_contracts::bind_repairs(steps, &bindings, &mut selections);
        Ok((selections, bindings))
    }
}

impl ExecutionProfile {
    /// Excludes mutable labels and account availability. Includes every field
    /// affecting the executable/task contract, so reusing a version number
    /// cannot carry qualification across changed settings.
    pub fn configuration_digest(&self) -> String {
        let mut configuration = self.clone();
        configuration.label.clear();
        configuration.access = ProfileAccess::Unknown;
        configuration.access_observation = None;
        configuration.enabled = false;
        configuration.advertised_limits = None;
        configuration.economics = None;
        for tool in &mut configuration.tool_capabilities {
            tool.connectivity = None;
        }
        super::digest(&serde_json::to_vec(&configuration).expect("serializable profile"))
    }

    pub fn validate(&self) -> Result<()> {
        capabilities::validate_profile(self)?;
        access::validate(self)?;
        profile_contracts::validate_profile(self)?;
        ensure!(
            store::identifier(&self.id) && self.version > 0,
            "invalid profile ID/version"
        );
        ensure!(
            !self.model.is_empty()
                && self.model.len() <= 128
                && self
                    .model
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.:/".contains(&b)),
            "invalid model identifier"
        );
        ensure!(
            !self.operations.is_empty(),
            "profile must declare supported operations"
        );
        ensure!(
            self.isolation != ExecutionIsolation::PortsOnly
                || self.tools.iter().all(|t| t == "workflow_ports"),
            "ports-only profiles cannot advertise shell, file or external tools"
        );
        let efforts: &[&str] = match self.backend.as_str() {
            "claude" => &["low", "medium", "high", "xhigh", "max"],
            "codex" => &["low", "medium", "high", "xhigh"],
            _ => bail!(
                "explicit model settings are unsupported for backend '{}'",
                self.backend
            ),
        };
        ensure!(
            self.effort
                .as_ref()
                .is_none_or(|e| efforts.contains(&e.as_str())),
            "unsupported reasoning effort"
        );
        Ok(())
    }

    pub fn command(&self, compiled_backend: &str) -> Result<String> {
        self.validate()?;
        let backend = crate::backend::resolve(compiled_backend).map_err(anyhow::Error::msg)?;
        let selected = crate::backend::resolve(&self.backend).map_err(anyhow::Error::msg)?;
        ensure!(
            backend.kind() == selected.kind(),
            "profile '{}' does not match compiled backend '{}'",
            self.id,
            compiled_backend
        );
        ensure!(
            self.enabled && self.access != ProfileAccess::Unavailable,
            "profile '{}' is unavailable",
            self.id
        );
        let base = if self.isolation == ExecutionIsolation::PortsOnly {
            match self.backend.as_str() {
                "codex" => "codex --sandbox read-only --ignore-user-config --disable shell_tool --disable multi_agent -c web_search='\"disabled\"'",
                "claude" => "claude --tools '' --strict-mcp-config --disable-slash-commands",
                _ => unreachable!(),
            }
        } else {
            backend.default_command()
        };
        let mut command = format!(
            "{} --model {}",
            base,
            crate::manager::shell_single_quote(&self.model)
        );
        if let Some(effort) = &self.effort {
            match self.backend.as_str() {
                "claude" => command.push_str(&format!(
                    " --effort {}",
                    crate::manager::shell_single_quote(effort)
                )),
                "codex" => command.push_str(&format!(" -c model_reasoning_effort='\"{effort}\"'")),
                _ => unreachable!(),
            }
        }
        Ok(command)
    }
}

impl QualificationScope {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            [&self.task_family, &self.task_slice, &self.contract_version]
                .iter()
                .all(|value| store::identifier(value)),
            "qualification scope needs stable family, slice and contract identifiers"
        );
        ensure!(
            !self.input_modalities.is_empty() && !self.output_modalities.is_empty(),
            "qualification scope must declare input and output modalities"
        );
        Ok(())
    }
}

impl CapabilityEvidence {
    fn validate_qualification(&self) -> Result<()> {
        if self.locally_qualified {
            ensure!(self.source_kind == EvidenceSourceKind::LocalEvaluation && self.qualification.is_some(),
                "evidence '{}': qualification requires a reviewed local evaluation, not a boolean or provider claim", self.id);
        }
        let Some(q) = &self.qualification else {
            return Ok(());
        };
        q.scope.validate()?;
        ensure!(
            self.source_kind == EvidenceSourceKind::LocalEvaluation,
            "qualification metadata is only valid for local evaluations"
        );
        ensure!(self.author.as_ref().is_some_and(|s| !s.trim().is_empty()) &&
            [&q.runtime_contract, &q.backend_version, &q.benchmark, &q.benchmark_version, &q.report, &q.reviewer, &q.uncertainty_method]
                .iter().all(|s| !s.trim().is_empty()) && q.reviewed_at == self.reviewed_at,
            "qualification requires author, reviewer, dated report, runtime and benchmark provenance");
        ensure!(
            [
                &q.step_contract_digest,
                &q.profile_digest,
                &q.dataset_digest,
                &q.harness_digest,
                &q.grader_digest,
                &q.protocol_digest,
                &q.report_digest
            ]
            .iter()
            .all(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())),
            "qualification artifact digests must be SHA-256 values"
        );
        ensure!(
            q.total_cases > 0
                && q.accepted_cases <= q.total_cases
                && q.independent_groups > 0
                && q.independent_groups <= q.total_cases
                && q.minimum_cases >= q.minimum_independent_groups
                && q.minimum_independent_groups >= 2
                && q.required_acceptance_rate.is_finite()
                && q.required_acceptance_rate > 0.0
                && q.required_acceptance_rate <= 1.0
                && q.acceptance_rate_lower_bound.is_finite()
                && q.acceptance_rate_lower_bound >= 0.0
                && q.acceptance_rate_lower_bound
                    <= f64::from(q.accepted_cases) / f64::from(q.total_cases),
            "invalid qualification sample counts, thresholds or uncertainty bound"
        );
        if self.locally_qualified {
            ensure!(q.held_out && q.labels_reviewed && q.semantic_reviewed &&
                q.total_cases >= q.minimum_cases && q.independent_groups >= q.minimum_independent_groups &&
                q.acceptance_rate_lower_bound >= q.required_acceptance_rate,
                "qualification requires reviewed held-out results meeting the frozen sample and acceptance thresholds");
        }
        Ok(())
    }
}
