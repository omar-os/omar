import type { AdviceCriterion, AdviceRecord, AdviceState, DecisionMode, WorkflowTemplate } from "./protocol-generated";
import { normalizeRuntimeUrl } from "./runtime-client";

export type AdvicePayload =
  | { kind: "draft"; text: string }
  | { kind: "proposal"; program: string; inputs: Record<string, unknown>; scenario_id: string; scenario_version: string }
  | { kind: "artifact"; workspace_id: string; snapshot_id: string; path: string; start: number; end: number };

async function request<T>(base: string, path: string, body?: unknown): Promise<T> {
  const response = await fetch(`${normalizeRuntimeUrl(base)}/v1/assist/${path}`, body === undefined ? undefined : {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body),
  });
  const data = await response.json();
  if (!response.ok) throw new Error(typeof data.error === "string" ? data.error : `Runtime returned HTTP ${response.status}.`);
  return data as T;
}

export const workflowCatalog = (base: string) => request<{ version: string; templates: WorkflowTemplate[] }>(base, "templates");
export const registerAdvice = (base: string, id: string, profile_id: string, payload: AdvicePayload, criterion?: AdviceCriterion) =>
  request<AdviceState>(base, "subjects", { id, profile_id, confirmed: true, payload, criterion });
export const readAdvice = (base: string, id: string) => request<AdviceState>(base, `subjects/${encodeURIComponent(id)}`);
export const adviceMode = (base: string, state: AdviceState, mode: DecisionMode) =>
  request<AdviceState>(base, `subjects/${encodeURIComponent(state.subject.id)}/mode`, { sha256: state.subject.sha256, mode });
export const evaluateAdvice = (base: string, state: AdviceState, request_id: string) =>
  request<AdviceRecord>(base, `subjects/${encodeURIComponent(state.subject.id)}/evaluations`, { sha256: state.subject.sha256, request_id });
export const adviceFeedback = (base: string, state: AdviceState, record: AdviceRecord, verdict: string) =>
  request(base, `subjects/${encodeURIComponent(state.subject.id)}/decisions/${encodeURIComponent(record.decision_id)}/feedback`, { request_id: crypto.randomUUID(), verdict });

export type SavedWorkspace = { workspace_id: string; instance: string; snapshots: { id: string; commit: string; label: string }[] };
export const adviceSnapshots = (base: string) => request<{ workspaces: SavedWorkspace[]; capability: string }>(base, "artifact-snapshots");
export const previewAdviceArtifact = (base: string, workspace_id: string, snapshot_id: string, path: string) =>
  request<{ revision: string; text: string; characters: number; complete: boolean }>(base, "artifact-preview", { workspace_id, snapshot_id, path });
