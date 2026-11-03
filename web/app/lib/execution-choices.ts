import type { ExecutionProfile, WorkflowPlanRevision } from "./protocol-generated";

function canonicalBackend(value: string) {
  const name = value.toLowerCase();
  return name === "claudecode" || name === "claude code" ? "claude" : name;
}

/** Eligibility covers every step sharing a session; the server revalidates it. */
export function eligibleExecutionProfiles(plan: WorkflowPlanRevision, agent: string, catalog: ExecutionProfile[], backend?: string) {
  const steps = plan.draft.steps.filter(step => step.agent === agent && step.operation !== "exact_validation" && step.operation !== "exact_transformation");
  const expected = backend ?? plan.profiles[agent]?.backend;
  return catalog.filter(profile => profile.enabled && profile.access !== "unavailable"
    && (!expected || profile.backend === canonicalBackend(expected))
    && (!plan.draft.preferences.allowed_backends.length || plan.draft.preferences.allowed_backends.includes(profile.backend))
    && steps.every(step => profile.operations.includes(step.operation) && step.required_tools.every(tool => profile.tools.includes(tool))
      && (!step.qualification_scope || (step.qualification_scope.input_modalities.every(modality => profile.input_modalities?.includes(modality))
        && step.qualification_scope.output_modalities.every(modality => profile.output_modalities?.includes(modality))))));
}
