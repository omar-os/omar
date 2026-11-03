import type { CapabilityEvidence, ExecutionProfile, ExecutionReceipt, NativeAccessCandidate, PlanRef, WorkflowBundle, WorkflowPlanRevision } from "./protocol-generated";
import { normalizeRuntimeUrl } from "./runtime-client";

async function request<T>(url: string, path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
  const response = await fetch(`${normalizeRuntimeUrl(url)}/v1/workflow/${path}`, {
    method: body === undefined ? "GET" : "POST",
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body), signal,
  });
  const value = await response.json();
  if (!response.ok) throw new Error(value.error ?? `Workflow request failed (${response.status})`);
  return value as T;
}

export const fetchWorkflowPlan = (url: string, id: string, signal?: AbortSignal) => request<WorkflowPlanRevision>(url, "plan", { id }, signal);
export const fetchExecutionProfiles = (url: string, signal?: AbortSignal) => request<{profiles: ExecutionProfile[]}>(url, "profiles", undefined, signal);
export const fetchCapabilityEvidence = (url: string, signal?: AbortSignal) => request<{evidence: CapabilityEvidence[]; revision: string}>(url, "evidence", undefined, signal);
export const fetchExecutionReceipt = (url: string, run_id: string, signal?: AbortSignal) => request<ExecutionReceipt>(url, "receipts", { run_id }, signal);
export const inspectNativeAccess = (url: string, run_id: string, agent: string) => request<NativeAccessCandidate>(url, "access-evidence", {run_id, agent});
export const exportWorkflowBundle = (url: string, reference: PlanRef) => request<WorkflowBundle>(url, "export", reference);
export const importWorkflowBundle = (url: string, bundle: unknown, request_id = crypto.randomUUID()) => request<WorkflowPlanRevision>(url, "import", {bundle, request_id});

export function saveWorkflowPlan(url: string, plan: WorkflowPlanRevision, program: string, inputs: Record<string, unknown>, changes?: {
  answers?: Record<string, string>; accepted_assumptions?: string[]; overrides?: Record<string, string>;
}) {
  return request<WorkflowPlanRevision>(url, "plans", {
    id: plan.id, expected_revision: plan.revision, request_id: crypto.randomUUID(), program, inputs,
    answers: changes?.answers ?? Object.fromEntries(plan.draft.questions.filter(q => q.answer).map(q => [q.id, q.answer])),
    accepted_assumptions: changes?.accepted_assumptions ?? plan.draft.intent.assumptions.filter(a => a.accepted).map(a => a.id),
    overrides: changes?.overrides ?? plan.draft.preferences.overrides,
  });
}
