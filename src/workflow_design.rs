//! Planning metadata lives with the existing saved proposal. Model choices are
//! execution configuration; published benchmark notes never establish local quality.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ModelProfile {
    pub id: String,
    pub backend: String,
    pub model: String,
    pub effort: Option<String>,
    /// A routing hint, not a measured price or quality guarantee.
    pub tier: String,
}
impl ModelProfile {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            [&self.id, &self.model].iter().all(|s| !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))),
            "invalid model profile identifier"
        );
        ensure!(
            matches!(self.backend.as_str(), "codex" | "claude"),
            "explicit profiles support Codex and Claude only"
        );
        ensure!(
            matches!(self.effort.as_deref(), Some("low" | "medium" | "high"))
                || (self.backend == "codex" && self.effort.as_deref() == Some("xhigh"))
                || (self.backend == "claude" && self.effort.is_none()),
            "unsupported effort"
        );
        ensure!(
            matches!(self.tier.as_str(), "economy" | "quality"),
            "unknown model tier"
        );
        Ok(())
    }
    pub fn command(&self, backend: &str) -> Result<String> {
        self.validate()?;
        let backend = if backend.eq_ignore_ascii_case("ClaudeCode") {
            "claude"
        } else {
            backend
        };
        ensure!(
            crate::backend::resolve(backend)
                .map_err(anyhow::Error::msg)?
                .kind()
                .name()
                == self.backend,
            "model profile does not match the execution backend"
        );
        let base = crate::backend::resolve(backend)
            .map_err(anyhow::Error::msg)?
            .default_command();
        let model = crate::manager::shell_single_quote(&self.model);
        Ok(if self.backend == "codex" {
            format!(
                "{base} --model {model} -c {}",
                crate::manager::shell_single_quote(&format!(
                    "model_reasoning_effort=\"{}\"",
                    self.effort.as_deref().unwrap_or("high")
                ))
            )
        } else {
            format!(
                "{base} --model {model}{}",
                self.effort
                    .as_ref()
                    .map(|e| format!(" --effort {e}"))
                    .unwrap_or_default()
            )
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ModelEvidence {
    pub id: String,
    pub profile_id: String,
    pub task: String,
    pub finding: String,
    pub source: String,
    pub observed_at: u64,
    pub expires_at: u64,
    pub limitations: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ModelCatalog {
    pub profiles: Vec<ModelProfile>,
    pub evidence: Vec<ModelEvidence>,
}
impl ModelCatalog {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("workflow-models.json");
        let catalog: Self = if path.exists() {
            serde_json::from_slice(&std::fs::read(path)?)?
        } else {
            serde_json::from_str(include_str!("../config/workflow-models.example.json"))?
        };
        let mut ids = BTreeSet::new();
        ensure!(
            catalog.profiles.len() <= 32 && catalog.evidence.len() <= 64,
            "model catalog is too large"
        );
        for p in &catalog.profiles {
            p.validate()?;
            ensure!(ids.insert(&p.id), "duplicate profile ID");
        }
        let mut evidence_ids = BTreeSet::new();
        for e in &catalog.evidence {
            ensure!(
                ids.contains(&e.profile_id) && evidence_ids.insert(&e.id),
                "invalid evidence profile or duplicate ID"
            );
            ensure!(
                e.source.starts_with("https://")
                    && e.expires_at > e.observed_at
                    && !e.limitations.is_empty(),
                "evidence needs a source, expiry and limitations"
            );
        }
        Ok(catalog)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesignAssumption {
    pub id: String,
    pub text: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesignQuestion {
    pub id: String,
    pub text: String,
    pub reactions: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesignStep {
    pub reaction: String,
    pub purpose: String,
    pub task: String,
    pub complexity: String,
    pub success_criteria: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesignSource {
    pub location: String,
    pub finding: String,
    pub limitation: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ModelChoice {
    pub profile_id: String,
    pub reason: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
pub struct DesignReview {
    #[ts(type = "Record<string, string>")]
    pub answers: BTreeMap<String, String>,
    pub accepted_assumptions: Vec<String>,
    pub confirmed: bool,
    pub needs_redesign: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct WorkflowPlan {
    pub outcome: String,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub assumptions: Vec<DesignAssumption>,
    #[serde(default)]
    pub questions: Vec<DesignQuestion>,
    pub steps: Vec<DesignStep>,
    #[serde(default)]
    pub sources: Vec<DesignSource>,
    #[serde(default)]
    pub research_gaps: Vec<String>,
    #[ts(type = "Record<string, ModelChoice>")]
    pub choices: BTreeMap<String, ModelChoice>,
    /// Host-owned: assistant proposals cannot confirm a plan or manufacture answers.
    #[serde(default)]
    pub review: DesignReview,
    /// Host-owned exact profile snapshots, so a later catalog edit cannot silently change a run.
    #[serde(default)]
    #[ts(type = "Record<string, ModelProfile>")]
    pub profiles: BTreeMap<String, ModelProfile>,
}
impl WorkflowPlan {
    pub fn ready(&self) -> bool {
        self.review.confirmed
            && !self.review.needs_redesign
            && self.questions.is_empty()
            && self
                .assumptions
                .iter()
                .all(|a| self.review.accepted_assumptions.contains(&a.id))
    }
    pub fn resolve(
        &mut self,
        state: &crate::topology::VmState,
        catalog: &ModelCatalog,
    ) -> Result<()> {
        ensure!(
            !self.outcome.trim().is_empty() && self.outcome.len() <= 4000,
            "a bounded outcome is required"
        );
        ensure!(
            self.steps.len() <= 64
                && self.questions.len() <= 12
                && self.sources.len() <= 12
                && self.assumptions.len() <= 12,
            "plan exceeds the supported planning budget"
        );
        let reactions: BTreeSet<_> = state.reactions.keys().collect();
        let mut covered = BTreeSet::new();
        for step in &self.steps {
            ensure!(
                reactions.contains(&step.reaction) && covered.insert(&step.reaction),
                "unknown or duplicate step"
            );
            ensure!(
                !step.purpose.trim().is_empty()
                    && !step.success_criteria.is_empty()
                    && step.success_criteria.iter().all(|c| !c.trim().is_empty()),
                "each step needs a purpose and success criteria"
            );
        }
        ensure!(covered == reactions, "explain every compiled step");
        let mut ids = BTreeSet::new();
        for question in &self.questions {
            ensure!(
                !question.id.is_empty()
                    && ids.insert(&question.id)
                    && !question.text.trim().is_empty(),
                "invalid question ID/text"
            );
            ensure!(
                question.reactions.iter().all(|r| reactions.contains(r)),
                "question refers to an unknown step"
            );
        }
        ids.clear();
        for assumption in &self.assumptions {
            ensure!(
                !assumption.id.is_empty()
                    && ids.insert(&assumption.id)
                    && !assumption.text.trim().is_empty(),
                "invalid assumption"
            );
        }
        let required: BTreeSet<_> = state
            .agents
            .iter()
            .filter(|(_, a)| {
                matches!(
                    a.backend.to_lowercase().as_str(),
                    "codex" | "claudecode" | "claude"
                )
            })
            .map(|(id, _)| id)
            .collect();
        ensure!(
            self.choices.keys().collect::<BTreeSet<_>>() == required,
            "choose one profile per Codex/Claude runtime agent; shared steps share that model"
        );
        self.profiles.clear();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        for (agent, choice) in &self.choices {
            let profile = catalog
                .profiles
                .iter()
                .find(|p| p.id == choice.profile_id)
                .context("unknown profile")?;
            profile.command(&state.agents[agent].backend)?;
            ensure!(
                !choice.reason.trim().is_empty(),
                "explain each model choice"
            );
            for id in &choice.evidence_ids {
                let e = catalog
                    .evidence
                    .iter()
                    .find(|e| &e.id == id)
                    .context("unknown evidence")?;
                ensure!(
                    e.profile_id == profile.id && e.observed_at <= now && now < e.expires_at,
                    "model evidence is expired or mismatched"
                );
                ensure!(
                    self.steps
                        .iter()
                        .any(|s| state.reactions[&s.reaction].agent == *agent && s.task == e.task),
                    "evidence does not match this agent's task"
                );
            }
            self.profiles.insert(agent.clone(), profile.clone());
        }
        Ok(())
    }
}
