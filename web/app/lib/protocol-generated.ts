// Generated from the Rust wire types by `cargo test protocol`. Do not edit.
//
// The runtime is the single definition of what the protocol is. Editing this
// file makes the client disagree with the daemon, which is the failure the
// generator exists to make impossible.

/** Every status `omar serve` can report a run in. */
export const RUN_STATUSES = ["starting", "running", "stopping", "completed", "stopped", "failed"] as const;
export type RunStatus = (typeof RUN_STATUSES)[number];

/** Where a drawing stands. `ready` is compiled but never run, which is what a proposal's preview is. */
export const DIAGRAM_STATUSES = ["ready", "running", "completed", "failed"] as const;
export type DiagramStatus = (typeof DIAGRAM_STATUSES)[number];

/** Where one reaction stands. */
export const REACTION_STATUSES = ["idle", "running", "completed"] as const;
export type ReactionStatus = (typeof REACTION_STATUSES)[number];

/** What an edge means, which decides how it is drawn. */
export const EDGE_KINDS = ["connection", "trigger", "effect"] as const;
export type EdgeKind = (typeof EDGE_KINDS)[number];

/** What a port is for. */
export const PORT_KINDS = ["input", "output", "action"] as const;
export type PortKind = (typeof PORT_KINDS)[number];

/** What happened, as the event stream names it. */
export const DIAGRAM_EVENT_KINDS = ["run_started", "tag_advanced", "reaction_started", "reaction_completed", "run_completed", "run_failed"] as const;
export type DiagramEventKind = (typeof DIAGRAM_EVENT_KINDS)[number];

/** Who spoke. */
export const CHAT_ROLES = ["operator", "assistant"] as const;
export type ChatRole = (typeof CHAT_ROLES)[number];

export type IntentRevision = { outcome: string, message_sequences: Array<number>, constraints: Array<string>,
/**
 * Typed counterparts of constraints; legacy strings remain readable.
 */
requirements: Array<DesignRequirement>, assumptions: Array<Assumption>, acceptance_criteria: Array<string>, };

export type Assumption = { id: string, text: string,
/**
 * A message reference, source ID, or explicitly "designer inference".
 */
provenance: string, origin: StatementOrigin | null,
/**
 * Written by the review endpoint; retained separately from the original basis.
 */
confirmation: ConfirmationRef | null, accepted: boolean, };

export type ResearchBrief = { summary: string, sources: Array<ResearchSource>, gaps: Array<string>, capability_evidence_ids: Array<string>, issues: Array<ResearchIssue>,
/**
 * Scope of metered host research. Native-agent activity is not included.
 */
session_id: string | null,
/**
 * Host-derived immutable summary; proposal-supplied counters are ignored.
 */
usage: ResearchUsage | null, };

export type ResearchSource = { id: string, location: string, finding: string,
/**
 * User supplied, retrieved, or inferred. Never turn an inference into a fact.
 */
provenance: string,
/**
 * None denotes an older, unclassified record, never verified evidence.
 */
origin: StatementOrigin | null, };

export type StatementOrigin = { "kind": "system_proposed", reason: string, } | { "kind": "user_stated", message_sequence: number, quote: string, } | { "kind": "source_observed", input_port: string, json_pointer: string, quote: string, content_digest: string, source_reported_date: string | null,
/**
 * Optional corroboration that this quote came through the metered
 * host adapter. The input binding still carries the execution data.
 */
capture: ResearchCaptureRef | null, } | { "kind": "user_confirmed", confirmation: ConfirmationRef, };

export type ConfirmationRef = { "kind": "review_answer", plan: PlanRef, question_id: string, answer: string, } | { "kind": "assumption", plan: PlanRef, assumption_id: string, text_digest: string, };

export type DesignRequirement = { id: string, text: string, origin: StatementOrigin, };

export type ResearchIssue = { id: string, kind: ResearchIssueKind, description: string, source_ids: Array<string>, blocking: boolean, resolution: ResearchResolution | null, };

export type ResearchIssueKind = "missing_information" | "contradiction";

export type ResearchResolution = { text: string, origin: StatementOrigin, };

export type ResearchCaptureRef = { session_id: string, request_id: string, };

export type ResearchLimits = { catalog_calls: number, source_calls: number, evidence_bytes: number, per_source_bytes: number, wall_time_ms: number, adapter_spend_usd: number, };

export type ResearchStop = "proposal_submitted" | "sufficient_evidence" | "needs_user_input" | "missing_capability" | "call_limit" | "context_limit" | "time_limit" | "clock_changed" | "interrupted" | "superseded";

export type ResearchCaptureSummary = { request_id: string, location: string, captured_at_ms: number, content_digest: string, full_content_digest: string, evidence_bytes: number, full_content_bytes: number, offset: number, truncated: boolean, };

export type ResearchUsage = { session_id: string, operator_sequence: number, limits: ResearchLimits, started_at_ms: number, finished_at_ms: number | null, catalog_calls: number, source_calls: number, failed_calls: number, evidence_bytes: number, stop_reason: ResearchStop | null, captures: Array<ResearchCaptureSummary>, errors: Array<string>,
/**
 * Local adapter reads make no billable provider requests. This is not the
 * cost of the native designer, which remains unknown.
 */
adapter_cost_usd: number, designer_cost_usd: number | null, external_retrieval_supported: boolean, };

export type Operation = "exact_validation" | "exact_transformation" | "structured_extraction" | "classification_routing" | "document_retrieval" | "evidence_synthesis" | "planning" | "writing" | "claim_verification" | "code_implementation" | "code_review" | "code_repair" | "code_verification" | "script_storyboard" | "render_code_generation" | "media_generation" | "audiovisual_verification";

export type StepSpec = {
/**
 * Persistent identity, independent of compiler reaction numbering.
 */
logical_id: string, reaction: string, agent: string, operation: Operation, purpose: string,
/**
 * Why this decomposition/handoff improves the requested outcome.
 */
decomposition_reason: string | null,
/**
 * Host-derived from the compiled source; never trusted from a proposal.
 */
interface: StepInterface | null, resources: StepResources, acceptance_criteria: Array<string>, required_tools: Array<string>, evidence_ids: Array<string>,
/**
 * A checker reaction that is the only consumer of this step's raw outputs.
 */
verification_reaction: string | null, output_checks: Array<OutputCheck>,
/**
 * Missing scope is supported for older plans, but cannot qualify an
 * automatic economical route or a task-specific performance claim.
 */
qualification_scope: QualificationScope | null, execution: StepExecution,
/**
 * An explicit additional repair and recheck, never a replay of the
 * original agent after an uncertain provider/tool failure.
 */
recovery: RecoveryPolicy | null, };

export type StepOwner = "code" | "model" | "human";

export type StepInput = { port: string, wire_type: string, schema: Record<string, unknown>, producer_steps: Array<string>, supplied_input: boolean, timer: boolean, };

export type StepOutput = { port: string, wire_type: string, schema: Record<string, unknown>, consumer_steps: Array<string>, exposed_outputs: Array<string>, checks: Array<OutputCheck>, };

export type StepInterface = { version: number, taxonomy_revision: string, owner: StepOwner,
/**
 * Anchor omits prompt/body and order, allowing edits with unchanged wiring.
 */
anchor_digest: string,
/**
 * Includes implementation, allowing exact matching when anchors repeat.
 */
implementation_digest: string, inputs: Array<StepInput>, outputs: Array<StepOutput>,
/**
 * Possible direct producers, not a claim that every trigger must arrive.
 */
dependencies: Array<string>, state_variables: Array<StepValueBinding>, parameters: Array<StepValueBinding>, shared_state_steps: Array<string>, activation: string, effect_contract: string, prompt_template_bytes: number, known_initial_trigger_bytes: number | null, context_coverage: string, };

export type StepValueBinding = { name: string, wire_type: string, schema: Record<string, unknown>, };

export type StepResources = {
/**
 * UTF-8 JSON payload sizes, not provider token/context-window limits.
 */
max_input_bytes: number, max_prompt_bytes: number, max_output_bytes: number,
/**
 * Unsupported native bounds must be explicitly rejected, never ignored.
 */
max_model_input_tokens: number | null, max_model_output_tokens: number | null, max_cost_usd: number | null, };

export type StepResourceUsage = { invocation_id: string, logical_step_id: string, reaction: string, owner: StepOwner, invoked: boolean, input_bytes: number, prompt_bytes: number, raw_output_bytes: number | null, returned_output_bytes: number | null, limits: StepResources, context_coverage: string, };

export type StepExecution = { effect: StepEffect, max_invocations: number, deadline_ms: number | null, };

export type StepEffect = "unknown" | "repeatable" | "external_action" | "human_decision";

export type RecoveryPolicy = { retry_port: string, repair_reaction: string, recheck_reaction: string, accepted_port: string, failure_port: string, max_repair_attempts: number, human_review: HumanRecovery | null, };

export type HumanRecovery = { reaction: string, blocked_port: string, };

export type VerificationStage = "initial_check" | "repair_check" | "human_review";

export type VerificationDecision = "accepted" | "repair_requested" | "human_review_requested" | "blocked";

export type VerificationEvent = { step: string, reaction: string, stage: VerificationStage, decision: VerificationDecision, validation_error: string | null, };

export type QualificationScope = { task_family: string, task_slice: string, contract_version: string, complexity: TaskComplexity, consequence: TaskConsequence, input_modalities: Array<Modality>, output_modalities: Array<Modality>, };

export type TaskComplexity = "routine" | "ambiguous" | "complex";

export type TaskConsequence = "low" | "medium" | "high";

export type Modality = "text" | "structured_data" | "code" | "image" | "audio" | "video";

export type OutputCheck = { "kind": "json_object", port: string, required_fields: Array<string>, } | { "kind": "grounded_brief", port: string, allowed_source_ids: Array<string>, minimum_claims: number, } | { "kind": "repository_patch", port: string, verifier_id: string, verifier_digest: string, repository_input: string, };

export type ReviewQuestion = { id: string, question: string, reason: string,
/**
 * Compiled reaction IDs affected by the answer. Empty means the overall
 * outcome/requirements; old questions remain readable.
 */
target_reactions: Array<string>, options: Array<string>, proposed_default: string | null, required: boolean, answer: string | null,
/**
 * Host-written revision of the review action; legacy answer text alone
 * cannot establish user-confirmed provenance.
 */
answer_record: PlanRef | null, };

export type ExecutionProfile = { id: string, version: number, label: string, backend: string, model: string, effort: string | null, tier: ProfileTier, operations: Array<Operation>, tools: Array<string>,
/**
 * Optional detailed declarations. Neither a tool name nor this declaration
 * proves connectivity; reviewed observations are scoped to the local registry.
 */
tool_capabilities: Array<ToolCapability>,
/**
 * Declared adapter capabilities, not proof of task quality. An empty
 * list is unknown and cannot satisfy an explicit modality requirement.
 */
input_modalities: Array<Modality>, output_modalities: Array<Modality>,
/**
 * A configured candidate may be used before account access is probed.
 * This legacy field is an operator declaration, not native access evidence.
 */
access: ProfileAccess,
/**
 * The legacy access flag is an operator declaration. Verification requires
 * a current observation of an actual native model request.
 */
access_observation: ModelAccessObservation | null, enabled: boolean, isolation: ExecutionIsolation, identity: ProfileIdentity, advertised_limits: AdvertisedModelLimits | null, economics: ProfileEconomics | null, fallback: ProfileFallbackPolicy | null, };

export type ProfileIdentity = { declared_provider: string | null, model_reference: ModelReferenceKind, };

export type ModelReferenceKind = "unknown" | "alias" | "versioned";

export type ReviewedProfileSource = { source: string, source_digest: string, locator: string, observed_at: number, reviewed_at: number, expires_at: number, reviewer: string, conditions: string, };

export type AdvertisedModelLimits = { profile_digest: string, context_window_tokens: number | null, max_output_tokens: number | null, source: ReviewedProfileSource, };

export type ProfileEconomics = { profile_digest: string,
/**
 * Describes the reviewed offering/setup; does not authorize billing or
 * establish the marginal cost of this invocation.
 */
billing_model: BillingModel, subscription_plan: string | null, prices: Array<ProfilePrice>, quota: ProfileQuota | null, connection_scope: string | null, source: ReviewedProfileSource, };

export type BillingModel = "unknown" | "subscription" | "usage_based" | "hybrid";

export type ProfilePrice = { unit: PriceUnit, quantity: number, price_usd: number, conditions: string, };

export type PriceUnit = "input_token" | "cached_input_token" | "output_token" | "request" | "subscription_month";

export type ProfileQuota = { unit: string, limit: number | null, remaining: number | null, window_seconds: number | null, resets_at: number | null, };

export type ProfileFactStatus = "unknown" | "current" | "expired" | "not_yet_valid" | "configuration_changed" | "connection_scope_changed";

export type ProfileFactAssessment = { advertised_limits: ProfileFactStatus, economics: ProfileFactStatus,
/**
 * Supported native profiles retain one session per runtime agent. This
 * assessment does not imply that complete session usage is measurable.
 */
session_behavior: string, structured_output_control: string, token_limits_enforced: boolean, spend_limits_enforced: boolean, };

export type ProfileFallbackPolicy = { profile_id: string, trigger: FallbackTrigger, reason: string, max_attempts: number, max_deadline_ms: number,
/**
 * Non-null caps are rejected until the adapter can enforce them.
 */
max_cost_usd: number | null, };

export type FallbackTrigger = "validation_rejected";

export type ProfileRepairRoute = { step: string, repair_reaction: string, repair_agent: string, profile_id: string | null, trigger: FallbackTrigger, max_attempts: number, deadline_ms: number | null, max_cost_usd: number | null, permitted: boolean, reason: string, };

export type ExecutionIsolation = "native" | "ports_only";

export type ProfileTier = "economy" | "quality";

export type ProfileAccess = "unknown" | "verified" | "unavailable";

export type ModelAccessObservation = { outcome: ModelAccessOutcome, connection_scope: string, profile_digest: string, backend_version: string, runtime_contract: string, observed_at: number, reviewed_at: number, expires_at: number, reviewer: string, report: string, report_digest: string, attempt_id: string, observed_model: string | null, observed_effort: string | null, limitations: string, };

export type ModelAccessOutcome = "succeeded" | "unavailable" | "inconclusive";

export type ModelAccessStatus = "unknown" | "reviewed_available" | "reviewed_unavailable" | "inconclusive" | "expired" | "not_yet_valid" | "configuration_changed" | "runtime_changed" | "connection_scope_changed" | "operator_unavailable" | "settings_mismatch";

export type ModelAccessAssessment = { status: ModelAccessStatus, reason: string, last_success_at: number | null, expires_at: number | null, observed_model: string | null, observed_effort: string | null, };

export type NativeAccessCandidate = { run_id: string, agent: string, profile_id: string, profile_digest: string, connection_scope: string | null, backend_version: string | null, runtime_contract: string, status: AccessCandidateStatus, observed_at: number | null, observed_model: string | null, observed_effort: string | null, invocation_ids: Array<string>, native_request_ids: Array<string>, artifact_digests: { [key in string]: string }, report_digest: string, limitations: Array<string>, new_model_requests: number, adapter_cost_usd: number, recorded_run_cost_usd: number | null, };

export type AccessCandidateStatus = "ready_for_review" | "insufficient_evidence" | "settings_mismatch";

export type ToolCapability = { id: string, version: number, native_name: string, transport: ToolTransport, authentication: ToolAuthentication, effect: ToolEffect, permission_scope: Array<string>, input_modalities: Array<Modality>, output_modalities: Array<Modality>,
/**
 * Unknown schemas stay null; these are descriptive, not runtime validators.
 */
input_schema: Record<string, unknown> | null, output_schema: Record<string, unknown> | null, connectivity: ToolConnectivityObservation | null, };

export type ToolTransport = "native" | "native_mcp";

export type ToolAuthentication = "unknown" | "not_required" | "native_session" | "external_credential";

export type ToolEffect = "unknown" | "read_only" | "local_write" | "external_write";

export type ToolConnectivityObservation = { outcome: ToolConnectionOutcome, connection_scope: string, profile_digest: string, backend_version: string, runtime_contract: string, observed_at: number, reviewed_at: number, expires_at: number, reviewer: string, report: string, report_digest: string, attempt_id: string, limitations: string, };

export type ToolConnectionOutcome = "available" | "unavailable";

export type ToolAccessStatus = "unknown" | "reviewed_available" | "reviewed_unavailable" | "expired" | "not_yet_valid" | "configuration_changed" | "runtime_changed" | "connection_scope_changed";

export type ToolAccessDecision = { tool: string, status: ToolAccessStatus, reason: string, };

export type CapabilityEvidence = { id: string, profile_id: string, profile_version: number, operation: Operation, source_kind: EvidenceSourceKind, author: string | null, source: string, finding: string, limitations: string, reviewed_at: number, expires_at: number,
/**
 * Kept for compatibility. True is valid only with a complete, reviewed
 * local qualification; this flag never supplies qualification by itself.
 */
locally_qualified: boolean, qualification: LocalQualification | null,
/**
 * A reviewed published result nominates a candidate, never a local default.
 */
benchmark_result: PublishedBenchmarkResult | null, };

export type PublishedBenchmarkResult = { benchmark: string, benchmark_version: string | null, published_at: number | null, retrieved_at: number, task_slice: string, target_scope: QualificationScope, profile_digest: string, measured_model: string, measured_backend: string | null, measured_harness: string | null, measured_effort: string | null, measured_tools: Array<string> | null, sample_size: number | null, score: number, unit: string | null, direction: ScoreDirection, uncertainty: BenchmarkUncertainty | null, cost_usd: number | null, latency_ms: number | null, measurement_conditions: string, applicability: string, dataset_overlap: string, contamination: string, source_row: BenchmarkSourceRow, review: BenchmarkReview | null, };

export type BenchmarkSourceRow = { snapshot: string, snapshot_digest: string, locator: string, row_pointer: string, score_pointer: string, model_pointer: string, unit_pointer: string | null, };

export type ScoreDirection = "higher_is_better" | "lower_is_better" | "descriptive";

export type BenchmarkUncertainty = { lower: number, upper: number, method: string, };

export type BenchmarkReview = { accepted: boolean, reviewer: string, reviewed_at: number, rationale: string, content_digest: string, };

export type CapabilityEvidenceStatus = "candidate" | "locally_validated" | "stale" | "insufficient";

export type CapabilityEvidenceAssessment = { status: CapabilityEvidenceStatus, reasons: Array<string>, };

export type EvidenceSourceKind = "unknown" | "provider_claim" | "public_benchmark" | "runtime_observation" | "local_evaluation";

export type LocalQualification = { scope: QualificationScope, step_contract_digest: string, profile_digest: string, runtime_contract: string, backend_version: string, benchmark: string, benchmark_version: string, dataset_digest: string, harness_digest: string, grader_digest: string,
/**
 * Frozen before held-out evaluation, including thresholds and grouping.
 */
protocol_digest: string, report: string, report_digest: string, held_out: boolean, labels_reviewed: boolean, semantic_reviewed: boolean, reviewer: string, reviewed_at: number,
/**
 * All evaluated cases, including failed and interrupted attempts.
 */
total_cases: number, accepted_cases: number, independent_groups: number, minimum_cases: number, minimum_independent_groups: number, required_acceptance_rate: number, acceptance_rate_lower_bound: number, uncertainty_method: string, };

export type SelectionDecision = { agent: string, profile_id: string | null, evidence_ids: Array<string>, evidence_assessments: { [key in string]: CapabilityEvidenceAssessment }, reason: string, provisional: boolean, tool_access: Array<ToolAccessDecision>,
/**
 * Candidate selection remains reviewable even if required connectivity is
 * unknown. Every blocker must be resolved before a revision can launch.
 */
capability_blockers: Array<string>, facts: ProfileFactAssessment, repair_routes: Array<ProfileRepairRoute>, model_access: ModelAccessAssessment, };

export type RoutingPreferences = { allowed_backends: Array<string>, overrides: { [key in string]: string }, };

export type PlanDraft = { revises: PlanRef | null, intent: IntentRevision, research: ResearchBrief, steps: Array<StepSpec>, questions: Array<ReviewQuestion>, preferences: RoutingPreferences, };

export type PlanRef = { id: string, revision: number, };

export type WorkflowPlanRevision = { schema_version: number, id: string, revision: number, conversation_id: string, created_at: number, request_id: string, program: string, source_digest: string,
/**
 * Server-derived fingerprints of compiled prompts, port schemas and
 * acceptance checks. A designer cannot preserve them across source edits.
 */
step_contracts: { [key in string]: string }, inputs: Record<string, unknown>, draft: PlanDraft, selections: Array<SelectionDecision>, profiles: { [key in string]: ExecutionProfile }, evidence_revision: string,
/**
 * Older plans deserialize for review but require a new saved revision
 * before admission under the current routing policy.
 */
routing_policy_revision: string, };

export type WorkflowBundle = { schema_version: number, program: string, source_digest: string, inputs: Record<string, unknown>, draft: PlanDraft, profiles: { [key in string]: ExecutionProfile }, evidence_revision: string, routing_policy_revision: string, };

export type RepositoryCheckReceipt = { invocation_id: string, reaction: string, verifier_id: string, verifier_digest: string, image: string, candidate_digest: string, status: string, test_count: number, elapsed_ms: number,
/**
 * Relative to the owned run directory. Contains the materialized patch
 * and private diagnostic output, never returned to a model automatically.
 */
artifact: string, };

export type ExecutionReceipt = { schema_version: number, run_id: string, plan: PlanRef | null, source_digest: string, profiles: { [key in string]: ExecutionProfile }, connection_scope: string | null, runtime_contract: string, started_at: number, finished_at: number | null, status: string, error: string | null,
/**
 * Checker decisions, not independent proof of semantic correctness or
 * delivery. A cancelled/failed run does not become accepted from these.
 */
verification: Array<VerificationEvent>, resources: Array<StepResourceUsage>, repository_checks: Array<RepositoryCheckReceipt>,
/**
 * Provider identity is separate from the requested profile. Missing data
 * stays null; a successful process is not proof of the provider model.
 */
observed_models: { [key in string]: string | null }, observed_efforts: { [key in string]: string | null },
/**
 * Local CLI versions keyed by backend, never provider model identity.
 */
backend_versions: { [key in string]: string | null }, input_tokens: number | null, output_tokens: number | null, cost_usd: number | null, };

export type DiagramTag = { timestamp: number, microstep: number, };

export type DiagramInstance = { id: string, name: string,
/**
 * The team it was instantiated from.
 */
team: string,
/**
 * The container this one is drawn inside, or empty for a top-level one.
 *
 * A team can instantiate another team, so containers nest. The id is
 * given rather than the name so a client never has to re-derive one from
 * the other.
 */
parent: string, };

export type DiagramAgent = { id: string, name: string, backend: string,
/**
 * The container it is drawn inside.
 */
instance: string, };

export type DiagramPort = { id: string, name: string, kind: PortKind, type: string, delay: number | null, value: unknown | null, last_tag: DiagramTag | null, instance: string, };

export type DiagramTimer = { id: string, name: string, offset: number,
/**
 * `0` fires once; anything else re-arms forever.
 */
period: number, last_tag: DiagramTag | null, instance: string, };

export type DiagramReaction = { id: string, name: string,
/**
 * Human-readable task copy; absent in workflows created before task metadata.
 */
title?: string, description?: string, agent: string, order: number, triggers: Array<string>, effects: Array<string>, contract: string, status: ReactionStatus, invocation_id: string | null, instance: string,
/**
 * Nanoseconds this reaction gave itself, or `None` for one bounded only by
 * the run. Carried so a client can draw the bound rather than leaving the
 * operator to read it out of the source.
 */
within: number | null, };

export type DiagramEdge = { id: string, kind: EdgeKind, source: string, target: string,
/**
 * Null when the hop costs nothing. `0` is `after 0`, which costs a
 * microstep; larger values are nanoseconds. Trigger and effect edges
 * carry no delay of their own and are always null.
 */
delay: number | null, };

export type DiagramSnapshot = { protocol_version: number, team: string, sequence: number, status: DiagramStatus, current_tag: DiagramTag | null,
/**
 * Nanoseconds physical time had run past `current_tag` when it executed.
 * `None` before the first tag. A sequence number says how much has been
 * published; this says whether the run is keeping its promises.
 */
lag: number | null,
/**
 * The containers to draw. Empty for a program compiled before instances
 * were carried through, which is how a client tells the difference.
 */
instances: Array<DiagramInstance>, agents: Array<DiagramAgent>, ports: Array<DiagramPort>,
/**
 * Empty for a program with no timer, and for bytecode that predates them.
 */
timers: Array<DiagramTimer>, reactions: Array<DiagramReaction>, edges: Array<DiagramEdge>, };

export type DiagramEvent = { protocol_version: number, sequence: number, team: string, tag: DiagramTag | null, kind: DiagramEventKind, payload: Record<string, unknown>, };

export type ProposedDesign = { plan?: PlanRef, program: string, inputs: Record<string, unknown>,
/**
 * The compiled topology, so the operator sees what they are approving
 * before any run exists.
 */
preview: DiagramSnapshot, };

export type ChatMessage = { sequence: number, role: ChatRole, text: string,
/**
 * Commentary while working, rather than something awaiting an answer.
 */
progress: boolean, design: ProposedDesign | null,
/**
 * Diagram components the operator had selected when they sent this.
 * "this one" is unresolvable in text; a selection says which.
 */
selection: Array<string>, };

export type Conversation = { id: string, title: string, ea_id: number | null, created_at: number, updated_at: number, messages: Array<ChatMessage>, };

export type ConversationSummary = { id: string, title: string, created_at: number, updated_at: number, message_count: number, ea_id: number | null, busy: boolean, run: RunRecord | null, };

export type RunRecord = { run_id: string, team: string, status: RunStatus, diagram_address: string | null, started_at: number, finished_at: number | null, error: string | null, };

