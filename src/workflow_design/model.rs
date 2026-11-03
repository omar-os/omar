use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct IntentRevision {
    pub outcome: String,
    #[serde(default)]
    pub message_sequences: Vec<u64>,
    #[serde(default)]
    pub constraints: Vec<String>,
    /// Typed counterparts of constraints; legacy strings remain readable.
    #[serde(default)]
    pub requirements: Vec<DesignRequirement>,
    #[serde(default)]
    pub assumptions: Vec<Assumption>,
    pub acceptance_criteria: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct Assumption {
    pub id: String,
    pub text: String,
    /// A message reference, source ID, or explicitly "designer inference".
    pub provenance: String,
    #[serde(default)]
    pub origin: Option<StatementOrigin>,
    /// Written by the review endpoint; retained separately from the original basis.
    #[serde(default)]
    pub confirmation: Option<ConfirmationRef>,
    #[serde(default)]
    pub accepted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ResearchBrief {
    pub summary: String,
    pub sources: Vec<ResearchSource>,
    #[serde(default)]
    pub gaps: Vec<String>,
    #[serde(default)]
    pub capability_evidence_ids: Vec<String>,
    #[serde(default)]
    pub issues: Vec<ResearchIssue>,
    /// Scope of metered host research. Native-agent activity is not included.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Host-derived immutable summary; proposal-supplied counters are ignored.
    #[serde(default)]
    pub usage: Option<ResearchUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ResearchSource {
    pub id: String,
    pub location: String,
    pub finding: String,
    /// User supplied, retrieved, or inferred. Never turn an inference into a fact.
    pub provenance: String,
    /// None denotes an older, unclassified record, never verified evidence.
    #[serde(default)]
    pub origin: Option<StatementOrigin>,
}

/// The revision's host-written created_at dates when this statement was
/// recorded. Source-reported dates are document metadata, not retrieval proof.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StatementOrigin {
    SystemProposed {
        reason: String,
    },
    UserStated {
        message_sequence: u64,
        quote: String,
    },
    SourceObserved {
        input_port: String,
        #[serde(default)]
        json_pointer: String,
        quote: String,
        #[serde(default)]
        content_digest: String,
        #[serde(default)]
        source_reported_date: Option<String>,
        /// Optional corroboration that this quote came through the metered
        /// host adapter. The input binding still carries the execution data.
        #[serde(default)]
        capture: Option<ResearchCaptureRef>,
    },
    UserConfirmed {
        confirmation: ConfirmationRef,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConfirmationRef {
    ReviewAnswer {
        plan: PlanRef,
        question_id: String,
        answer: String,
    },
    Assumption {
        plan: PlanRef,
        assumption_id: String,
        text_digest: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct DesignRequirement {
    pub id: String,
    pub text: String,
    pub origin: StatementOrigin,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ResearchIssue {
    pub id: String,
    pub kind: ResearchIssueKind,
    pub description: String,
    #[serde(default)]
    pub source_ids: Vec<String>,
    #[serde(default)]
    pub blocking: bool,
    #[serde(default)]
    pub resolution: Option<ResearchResolution>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum ResearchIssueKind {
    MissingInformation,
    Contradiction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ResearchResolution {
    pub text: String,
    pub origin: StatementOrigin,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ResearchCaptureRef {
    pub session_id: String,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ResearchLimits {
    pub catalog_calls: u32,
    pub source_calls: u32,
    pub evidence_bytes: usize,
    pub per_source_bytes: usize,
    pub wall_time_ms: u64,
    pub adapter_spend_usd: f64,
}

impl Default for ResearchLimits {
    fn default() -> Self {
        Self {
            catalog_calls: 2,
            source_calls: 3,
            evidence_bytes: 65_536,
            per_source_bytes: 16_384,
            wall_time_ms: 300_000,
            adapter_spend_usd: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum ResearchStop {
    ProposalSubmitted,
    SufficientEvidence,
    NeedsUserInput,
    MissingCapability,
    CallLimit,
    ContextLimit,
    TimeLimit,
    ClockChanged,
    Interrupted,
    Superseded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct ResearchCaptureSummary {
    pub request_id: String,
    pub location: String,
    pub captured_at_ms: u64,
    pub content_digest: String,
    pub full_content_digest: String,
    pub evidence_bytes: usize,
    pub full_content_bytes: usize,
    pub offset: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct ResearchUsage {
    pub session_id: String,
    pub operator_sequence: u64,
    pub limits: ResearchLimits,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub catalog_calls: u32,
    pub source_calls: u32,
    pub failed_calls: u32,
    pub evidence_bytes: usize,
    pub stop_reason: Option<ResearchStop>,
    pub captures: Vec<ResearchCaptureSummary>,
    pub errors: Vec<String>,
    /// Local adapter reads make no billable provider requests. This is not the
    /// cost of the native designer, which remains unknown.
    pub adapter_cost_usd: f64,
    pub designer_cost_usd: Option<f64>,
    pub external_retrieval_supported: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    ExactValidation,
    ExactTransformation,
    StructuredExtraction,
    ClassificationRouting,
    DocumentRetrieval,
    EvidenceSynthesis,
    Planning,
    Writing,
    ClaimVerification,
    CodeImplementation,
    CodeReview,
    CodeRepair,
    CodeVerification,
    ScriptStoryboard,
    RenderCodeGeneration,
    MediaGeneration,
    AudiovisualVerification,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct StepSpec {
    /// Persistent identity, independent of compiler reaction numbering.
    #[serde(default)]
    pub logical_id: String,
    pub reaction: String,
    pub agent: String,
    pub operation: Operation,
    pub purpose: String,
    /// Why this decomposition/handoff improves the requested outcome.
    #[serde(default)]
    pub decomposition_reason: Option<String>,
    /// Host-derived from the compiled source; never trusted from a proposal.
    #[serde(default)]
    pub interface: Option<StepInterface>,
    #[serde(default)]
    pub resources: StepResources,
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub required_tools: Vec<String>,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    /// A checker reaction that is the only consumer of this step's raw outputs.
    #[serde(default)]
    pub verification_reaction: Option<String>,
    #[serde(default)]
    pub output_checks: Vec<OutputCheck>,
    /// Missing scope is supported for older plans, but cannot qualify an
    /// automatic economical route or a task-specific performance claim.
    #[serde(default)]
    pub qualification_scope: Option<QualificationScope>,
    #[serde(default)]
    pub execution: StepExecution,
    /// An explicit additional repair and recheck, never a replay of the
    /// original agent after an uncertain provider/tool failure.
    #[serde(default)]
    pub recovery: Option<RecoveryPolicy>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum StepOwner {
    Code,
    Model,
    Human,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct StepInput {
    pub port: String,
    pub wire_type: String,
    #[ts(type = "Record<string, unknown>")]
    pub schema: serde_json::Value,
    pub producer_steps: Vec<String>,
    pub supplied_input: bool,
    pub timer: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct StepOutput {
    pub port: String,
    pub wire_type: String,
    #[ts(type = "Record<string, unknown>")]
    pub schema: serde_json::Value,
    pub consumer_steps: Vec<String>,
    pub exposed_outputs: Vec<String>,
    pub checks: Vec<OutputCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct StepInterface {
    pub version: u32,
    pub taxonomy_revision: String,
    pub owner: StepOwner,
    /// Anchor omits prompt/body and order, allowing edits with unchanged wiring.
    pub anchor_digest: String,
    /// Includes implementation, allowing exact matching when anchors repeat.
    pub implementation_digest: String,
    pub inputs: Vec<StepInput>,
    pub outputs: Vec<StepOutput>,
    /// Possible direct producers, not a claim that every trigger must arrive.
    pub dependencies: Vec<String>,
    pub state_variables: Vec<StepValueBinding>,
    pub parameters: Vec<StepValueBinding>,
    pub shared_state_steps: Vec<String>,
    pub activation: String,
    pub effect_contract: String,
    pub prompt_template_bytes: usize,
    pub known_initial_trigger_bytes: Option<usize>,
    pub context_coverage: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct StepValueBinding {
    pub name: String,
    pub wire_type: String,
    #[ts(type = "Record<string, unknown>")]
    pub schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(default, deny_unknown_fields)]
pub struct StepResources {
    /// UTF-8 JSON payload sizes, not provider token/context-window limits.
    pub max_input_bytes: usize,
    pub max_prompt_bytes: usize,
    pub max_output_bytes: usize,
    /// Unsupported native bounds must be explicitly rejected, never ignored.
    pub max_model_input_tokens: Option<u64>,
    pub max_model_output_tokens: Option<u64>,
    pub max_cost_usd: Option<f64>,
}
impl Default for StepResources {
    fn default() -> Self {
        Self {
            max_input_bytes: 262_144,
            max_prompt_bytes: 524_288,
            max_output_bytes: 262_144,
            max_model_input_tokens: None,
            max_model_output_tokens: None,
            max_cost_usd: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct StepResourceUsage {
    pub invocation_id: String,
    pub logical_step_id: String,
    pub reaction: String,
    pub owner: StepOwner,
    pub invoked: bool,
    pub input_bytes: usize,
    pub prompt_bytes: usize,
    pub raw_output_bytes: Option<usize>,
    pub returned_output_bytes: Option<usize>,
    pub limits: StepResources,
    pub context_coverage: String,
}

fn one_invocation() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct StepExecution {
    #[serde(default)]
    pub effect: StepEffect,
    #[serde(default = "one_invocation")]
    pub max_invocations: u32,
    #[serde(default)]
    pub deadline_ms: Option<u64>,
}

impl Default for StepExecution {
    fn default() -> Self {
        Self {
            effect: StepEffect::Unknown,
            max_invocations: 1,
            deadline_ms: None,
        }
    }
}

#[derive(Debug, Clone, Default, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum StepEffect {
    #[default]
    Unknown,
    Repeatable,
    ExternalAction,
    HumanDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct RecoveryPolicy {
    pub retry_port: String,
    pub repair_reaction: String,
    pub recheck_reaction: String,
    pub accepted_port: String,
    pub failure_port: String,
    #[serde(default = "one_invocation")]
    pub max_repair_attempts: u32,
    #[serde(default)]
    pub human_review: Option<HumanRecovery>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct HumanRecovery {
    pub reaction: String,
    pub blocked_port: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, TS)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStage {
    InitialCheck,
    RepairCheck,
    HumanReview,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum VerificationDecision {
    Accepted,
    RepairRequested,
    HumanReviewRequested,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct VerificationEvent {
    pub step: String,
    pub reaction: String,
    pub stage: VerificationStage,
    pub decision: VerificationDecision,
    pub validation_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(deny_unknown_fields)]
pub struct QualificationScope {
    pub task_family: String,
    pub task_slice: String,
    pub contract_version: String,
    pub complexity: TaskComplexity,
    pub consequence: TaskConsequence,
    pub input_modalities: Vec<Modality>,
    pub output_modalities: Vec<Modality>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum TaskComplexity {
    Routine,
    Ambiguous,
    Complex,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum TaskConsequence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    Text,
    StructuredData,
    Code,
    Image,
    Audio,
    Video,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutputCheck {
    JsonObject {
        port: String,
        required_fields: Vec<String>,
    },
    GroundedBrief {
        port: String,
        allowed_source_ids: Vec<String>,
        minimum_claims: usize,
    },
    /// Operator-owned, digest-bound Python JSON API regressions in a local
    /// container. The suite and expectations never come from the model.
    RepositoryPatch {
        port: String,
        verifier_id: String,
        verifier_digest: String,
        repository_input: String,
    },
}

impl OutputCheck {
    pub fn port(&self) -> &str {
        match self {
            Self::JsonObject { port, .. }
            | Self::GroundedBrief { port, .. }
            | Self::RepositoryPatch { port, .. } => port,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ReviewQuestion {
    pub id: String,
    pub question: String,
    pub reason: String,
    /// Compiled reaction IDs affected by the answer. Empty means the overall
    /// outcome/requirements; old questions remain readable.
    #[serde(default)]
    pub target_reactions: Vec<String>,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub proposed_default: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub answer: Option<String>,
    /// Host-written revision of the review action; legacy answer text alone
    /// cannot establish user-confirmed provenance.
    #[serde(default)]
    pub answer_record: Option<PlanRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ExecutionProfile {
    pub id: String,
    pub version: u32,
    pub label: String,
    pub backend: String,
    pub model: String,
    pub effort: Option<String>,
    pub tier: ProfileTier,
    pub operations: Vec<Operation>,
    pub tools: Vec<String>,
    /// Optional detailed declarations. Neither a tool name nor this declaration
    /// proves connectivity; reviewed observations are scoped to the local registry.
    #[serde(default)]
    pub tool_capabilities: Vec<ToolCapability>,
    /// Declared adapter capabilities, not proof of task quality. An empty
    /// list is unknown and cannot satisfy an explicit modality requirement.
    #[serde(default)]
    pub input_modalities: Vec<Modality>,
    #[serde(default)]
    pub output_modalities: Vec<Modality>,
    /// A configured candidate may be used before account access is probed.
    /// This legacy field is an operator declaration, not native access evidence.
    pub access: ProfileAccess,
    /// The legacy access flag is an operator declaration. Verification requires
    /// a current observation of an actual native model request.
    #[serde(default)]
    pub access_observation: Option<ModelAccessObservation>,
    pub enabled: bool,
    #[serde(default)]
    pub isolation: ExecutionIsolation,
    #[serde(default)]
    pub identity: ProfileIdentity,
    #[serde(default)]
    pub advertised_limits: Option<AdvertisedModelLimits>,
    #[serde(default)]
    pub economics: Option<ProfileEconomics>,
    #[serde(default)]
    pub fallback: Option<ProfileFallbackPolicy>,
}

/// Operator declarations about the requested configuration, never an assertion
/// that the provider reported an immutable model identity during execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileIdentity {
    pub declared_provider: Option<String>,
    pub model_reference: ModelReferenceKind,
}

#[derive(Debug, Clone, Default, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum ModelReferenceKind {
    #[default]
    Unknown,
    Alias,
    Versioned,
}

/// Traceable operator-reviewed source snapshot. The registry validates metadata,
/// not the truth of the source. No fetch or billable probe is implicit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ReviewedProfileSource {
    pub source: String,
    pub source_digest: String,
    pub locator: String,
    pub observed_at: u64,
    pub reviewed_at: u64,
    pub expires_at: u64,
    pub reviewer: String,
    pub conditions: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct AdvertisedModelLimits {
    pub profile_digest: String,
    pub context_window_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub source: ReviewedProfileSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ProfileEconomics {
    pub profile_digest: String,
    /// Describes the reviewed offering/setup; does not authorize billing or
    /// establish the marginal cost of this invocation.
    pub billing_model: BillingModel,
    pub subscription_plan: Option<String>,
    pub prices: Vec<ProfilePrice>,
    pub quota: Option<ProfileQuota>,
    pub connection_scope: Option<String>,
    pub source: ReviewedProfileSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum BillingModel {
    Unknown,
    Subscription,
    UsageBased,
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ProfilePrice {
    pub unit: PriceUnit,
    pub quantity: u64,
    pub price_usd: f64,
    pub conditions: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum PriceUnit {
    InputToken,
    CachedInputToken,
    OutputToken,
    Request,
    SubscriptionMonth,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ProfileQuota {
    pub unit: String,
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    pub window_seconds: Option<u64>,
    pub resets_at: Option<u64>,
}

#[derive(Debug, Clone, Default, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum ProfileFactStatus {
    #[default]
    Unknown,
    Current,
    Expired,
    NotYetValid,
    ConfigurationChanged,
    ConnectionScopeChanged,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS)]
pub struct ProfileFactAssessment {
    pub advertised_limits: ProfileFactStatus,
    pub economics: ProfileFactStatus,
    /// Supported native profiles retain one session per runtime agent. This
    /// assessment does not imply that complete session usage is measurable.
    pub session_behavior: String,
    pub structured_output_control: String,
    pub token_limits_enforced: bool,
    pub spend_limits_enforced: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ProfileFallbackPolicy {
    pub profile_id: String,
    pub trigger: FallbackTrigger,
    pub reason: String,
    pub max_attempts: u32,
    pub max_deadline_ms: u64,
    /// Non-null caps are rejected until the adapter can enforce them.
    pub max_cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum FallbackTrigger {
    ValidationRejected,
}

/// Resolved from a saved explicit repair graph, never an instruction to switch
/// a running native session after an uncertain tool/provider error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct ProfileRepairRoute {
    pub step: String,
    pub repair_reaction: String,
    pub repair_agent: String,
    pub profile_id: Option<String>,
    pub trigger: FallbackTrigger,
    pub max_attempts: u32,
    pub deadline_ms: Option<u64>,
    pub max_cost_usd: Option<f64>,
    pub permitted: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionIsolation {
    #[default]
    Native,
    PortsOnly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum ProfileTier {
    Economy,
    Quality,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum ProfileAccess {
    Unknown,
    Verified,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ModelAccessObservation {
    pub outcome: ModelAccessOutcome,
    pub connection_scope: String,
    pub profile_digest: String,
    pub backend_version: String,
    pub runtime_contract: String,
    pub observed_at: u64,
    pub reviewed_at: u64,
    pub expires_at: u64,
    pub reviewer: String,
    pub report: String,
    pub report_digest: String,
    pub attempt_id: String,
    pub observed_model: Option<String>,
    pub observed_effort: Option<String>,
    pub limitations: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum ModelAccessOutcome {
    Succeeded,
    Unavailable,
    Inconclusive,
}

#[derive(Debug, Clone, Default, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum ModelAccessStatus {
    #[default]
    Unknown,
    ReviewedAvailable,
    ReviewedUnavailable,
    Inconclusive,
    Expired,
    NotYetValid,
    ConfigurationChanged,
    RuntimeChanged,
    ConnectionScopeChanged,
    OperatorUnavailable,
    SettingsMismatch,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS)]
pub struct ModelAccessAssessment {
    pub status: ModelAccessStatus,
    pub reason: String,
    pub last_success_at: Option<u64>,
    pub expires_at: Option<u64>,
    pub observed_model: Option<String>,
    pub observed_effort: Option<String>,
}

/// Read-only inspection of existing owned run artifacts. ReadyForReview is not
/// a qualification or an approved registry observation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct NativeAccessCandidate {
    pub run_id: String,
    pub agent: String,
    pub profile_id: String,
    pub profile_digest: String,
    pub connection_scope: Option<String>,
    pub backend_version: Option<String>,
    pub runtime_contract: String,
    pub status: AccessCandidateStatus,
    pub observed_at: Option<u64>,
    pub observed_model: Option<String>,
    pub observed_effort: Option<String>,
    pub invocation_ids: Vec<String>,
    pub native_request_ids: Vec<String>,
    pub artifact_digests: BTreeMap<String, String>,
    pub report_digest: String,
    pub limitations: Vec<String>,
    pub new_model_requests: u32,
    pub adapter_cost_usd: f64,
    pub recorded_run_cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum AccessCandidateStatus {
    ReadyForReview,
    InsufficientEvidence,
    SettingsMismatch,
}

/// Operator-maintained description of a tool supplied by the native backend.
/// Permissions describe the observed setup; they do not sandbox the backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ToolCapability {
    pub id: String,
    pub version: u32,
    pub native_name: String,
    pub transport: ToolTransport,
    pub authentication: ToolAuthentication,
    pub effect: ToolEffect,
    pub permission_scope: Vec<String>,
    pub input_modalities: Vec<Modality>,
    pub output_modalities: Vec<Modality>,
    /// Unknown schemas stay null; these are descriptive, not runtime validators.
    #[ts(type = "Record<string, unknown> | null")]
    pub input_schema: Option<Value>,
    #[ts(type = "Record<string, unknown> | null")]
    pub output_schema: Option<Value>,
    #[serde(default)]
    pub connectivity: Option<ToolConnectivityObservation>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum ToolTransport {
    Native,
    NativeMcp,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum ToolAuthentication {
    Unknown,
    NotRequired,
    NativeSession,
    ExternalCredential,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum ToolEffect {
    Unknown,
    ReadOnly,
    LocalWrite,
    ExternalWrite,
}

/// Trusted operator attestation of an actual tool attempt, not a provider claim
/// or a CLI version probe. The designer has no API for writing this record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ToolConnectivityObservation {
    pub outcome: ToolConnectionOutcome,
    pub connection_scope: String,
    pub profile_digest: String,
    pub backend_version: String,
    pub runtime_contract: String,
    pub observed_at: u64,
    pub reviewed_at: u64,
    pub expires_at: u64,
    pub reviewer: String,
    pub report: String,
    pub report_digest: String,
    pub attempt_id: String,
    pub limitations: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum ToolConnectionOutcome {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum ToolAccessStatus {
    Unknown,
    ReviewedAvailable,
    ReviewedUnavailable,
    Expired,
    NotYetValid,
    ConfigurationChanged,
    RuntimeChanged,
    ConnectionScopeChanged,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct ToolAccessDecision {
    pub tool: String,
    pub status: ToolAccessStatus,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct CapabilityEvidence {
    pub id: String,
    pub profile_id: String,
    pub profile_version: u32,
    pub operation: Operation,
    #[serde(default)]
    pub source_kind: EvidenceSourceKind,
    #[serde(default)]
    pub author: Option<String>,
    pub source: String,
    pub finding: String,
    pub limitations: String,
    pub reviewed_at: u64,
    pub expires_at: u64,
    /// Kept for compatibility. True is valid only with a complete, reviewed
    /// local qualification; this flag never supplies qualification by itself.
    pub locally_qualified: bool,
    #[serde(default)]
    pub qualification: Option<LocalQualification>,
    /// A reviewed published result nominates a candidate, never a local default.
    #[serde(default)]
    pub benchmark_result: Option<PublishedBenchmarkResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct PublishedBenchmarkResult {
    pub benchmark: String,
    pub benchmark_version: Option<String>,
    pub published_at: Option<u64>,
    pub retrieved_at: u64,
    pub task_slice: String,
    pub target_scope: QualificationScope,
    pub profile_digest: String,
    pub measured_model: String,
    pub measured_backend: Option<String>,
    pub measured_harness: Option<String>,
    pub measured_effort: Option<String>,
    pub measured_tools: Option<Vec<String>>,
    pub sample_size: Option<u32>,
    pub score: f64,
    pub unit: Option<String>,
    pub direction: ScoreDirection,
    pub uncertainty: Option<BenchmarkUncertainty>,
    pub cost_usd: Option<f64>,
    pub latency_ms: Option<f64>,
    pub measurement_conditions: String,
    pub applicability: String,
    pub dataset_overlap: String,
    pub contamination: String,
    pub source_row: BenchmarkSourceRow,
    pub review: Option<BenchmarkReview>,
}

/// An operator-supplied JSON source extract. Hashes and pointers establish the
/// exact row, not independent retrieval, authorship or truth of the publication.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkSourceRow {
    pub snapshot: String,
    pub snapshot_digest: String,
    pub locator: String,
    pub row_pointer: String,
    pub score_pointer: String,
    pub model_pointer: String,
    pub unit_pointer: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum ScoreDirection {
    HigherIsBetter,
    LowerIsBetter,
    Descriptive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkUncertainty {
    pub lower: f64,
    pub upper: f64,
    pub method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkReview {
    pub accepted: bool,
    pub reviewer: String,
    pub reviewed_at: u64,
    pub rationale: String,
    pub content_digest: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityEvidenceStatus {
    Candidate,
    LocallyValidated,
    Stale,
    Insufficient,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct CapabilityEvidenceAssessment {
    pub status: CapabilityEvidenceStatus,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Default, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSourceKind {
    #[default]
    Unknown,
    ProviderClaim,
    PublicBenchmark,
    RuntimeObservation,
    LocalEvaluation,
}

/// Operator-curated attestation. The registry validates scope, bindings and
/// decision rules; it cannot attest that a human actually reviewed an artifact.
/// The designer cannot write this record through any workflow API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct LocalQualification {
    pub scope: QualificationScope,
    pub step_contract_digest: String,
    pub profile_digest: String,
    pub runtime_contract: String,
    pub backend_version: String,
    pub benchmark: String,
    pub benchmark_version: String,
    pub dataset_digest: String,
    pub harness_digest: String,
    pub grader_digest: String,
    /// Frozen before held-out evaluation, including thresholds and grouping.
    pub protocol_digest: String,
    pub report: String,
    pub report_digest: String,
    pub held_out: bool,
    pub labels_reviewed: bool,
    pub semantic_reviewed: bool,
    pub reviewer: String,
    pub reviewed_at: u64,
    /// All evaluated cases, including failed and interrupted attempts.
    pub total_cases: u32,
    pub accepted_cases: u32,
    pub independent_groups: u32,
    pub minimum_cases: u32,
    pub minimum_independent_groups: u32,
    pub required_acceptance_rate: f64,
    pub acceptance_rate_lower_bound: f64,
    pub uncertainty_method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct SelectionDecision {
    pub agent: String,
    pub profile_id: Option<String>,
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub evidence_assessments: BTreeMap<String, CapabilityEvidenceAssessment>,
    pub reason: String,
    pub provisional: bool,
    #[serde(default)]
    pub tool_access: Vec<ToolAccessDecision>,
    /// Candidate selection remains reviewable even if required connectivity is
    /// unknown. Every blocker must be resolved before a revision can launch.
    #[serde(default)]
    pub capability_blockers: Vec<String>,
    #[serde(default)]
    pub facts: ProfileFactAssessment,
    #[serde(default)]
    pub repair_routes: Vec<ProfileRepairRoute>,
    #[serde(default)]
    pub model_access: ModelAccessAssessment,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct RoutingPreferences {
    #[serde(default)]
    pub allowed_backends: Vec<String>,
    #[serde(default)]
    pub overrides: BTreeMap<String, String>,
}

/// Submitted by the designer. Authority-bearing revisions, digests and profile
/// snapshots are assigned by the server, not accepted from model output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct PlanDraft {
    #[serde(default)]
    pub revises: Option<PlanRef>,
    pub intent: IntentRevision,
    pub research: ResearchBrief,
    pub steps: Vec<StepSpec>,
    #[serde(default)]
    pub questions: Vec<ReviewQuestion>,
    #[serde(default)]
    pub preferences: RoutingPreferences,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct PlanRef {
    pub id: String,
    pub revision: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct WorkflowPlanRevision {
    pub schema_version: u32,
    pub id: String,
    pub revision: u32,
    pub conversation_id: String,
    pub created_at: u64,
    pub request_id: String,
    pub program: String,
    pub source_digest: String,
    /// Server-derived fingerprints of compiled prompts, port schemas and
    /// acceptance checks. A designer cannot preserve them across source edits.
    #[serde(default)]
    pub step_contracts: BTreeMap<String, String>,
    #[ts(type = "Record<string, unknown>")]
    pub inputs: BTreeMap<String, Value>,
    pub draft: PlanDraft,
    pub selections: Vec<SelectionDecision>,
    pub profiles: BTreeMap<String, ExecutionProfile>,
    pub evidence_revision: String,
    /// Older plans deserialize for review but require a new saved revision
    /// before admission under the current routing policy.
    #[serde(default)]
    pub routing_policy_revision: String,
}

/// Portable source and execution bindings. Ownership, message references and
/// approval state are re-established in the destination chat, never imported.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct WorkflowBundle {
    pub schema_version: u32,
    pub program: String,
    pub source_digest: String,
    #[ts(type = "Record<string, unknown>")]
    pub inputs: BTreeMap<String, Value>,
    pub draft: PlanDraft,
    pub profiles: BTreeMap<String, ExecutionProfile>,
    pub evidence_revision: String,
    #[serde(default)]
    pub routing_policy_revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct ExecutionReceipt {
    pub schema_version: u32,
    pub run_id: String,
    pub plan: Option<PlanRef>,
    pub source_digest: String,
    pub profiles: BTreeMap<String, ExecutionProfile>,
    #[serde(default)]
    pub connection_scope: Option<String>,
    #[serde(default)]
    pub runtime_contract: String,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub status: String,
    pub error: Option<String>,
    /// Checker decisions, not independent proof of semantic correctness or
    /// delivery. A cancelled/failed run does not become accepted from these.
    #[serde(default)]
    pub verification: Vec<VerificationEvent>,
    #[serde(default)]
    pub resources: Vec<StepResourceUsage>,
    #[serde(default)]
    pub repository_checks: Vec<RepositoryCheckReceipt>,
    /// Provider identity is separate from the requested profile. Missing data
    /// stays null; a successful process is not proof of the provider model.
    pub observed_models: BTreeMap<String, Option<String>>,
    #[serde(default)]
    pub observed_efforts: BTreeMap<String, Option<String>>,
    /// Local CLI versions keyed by backend, never provider model identity.
    #[serde(default)]
    pub backend_versions: BTreeMap<String, Option<String>>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct RepositoryCheckReceipt {
    pub invocation_id: String,
    pub reaction: String,
    pub verifier_id: String,
    pub verifier_digest: String,
    pub image: String,
    pub candidate_digest: String,
    pub status: String,
    pub test_count: usize,
    pub elapsed_ms: u64,
    /// Relative to the owned run directory. Contains the materialized patch
    /// and private diagnostic output, never returned to a model automatically.
    pub artifact: String,
}
