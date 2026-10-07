import type {
  DecisionCapabilities,
  DecisionMode,
  DecisionRecord,
  DecisionSource,
} from "./protocol";
import { normalizeRuntimeUrl } from "./runtime-client";

async function readError(response: Response): Promise<string> {
  try {
    const body = (await response.json()) as { error?: unknown };
    if (typeof body.error === "string") return body.error;
  } catch { /* status fallback */ }
  return `Runtime returned HTTP ${response.status}.`;
}

export async function fetchDecisionCapabilities(serveUrl: string): Promise<DecisionCapabilities | null> {
  const response = await fetch(`${normalizeRuntimeUrl(serveUrl)}/v1/assist/capabilities`);
  if (response.status === 204 || response.status === 404) return null;
  if (!response.ok) throw new Error(await readError(response));
  return (await response.json()) as DecisionCapabilities;
}

export async function setDecisionMode(serveUrl: string, runId: string, mode: DecisionMode): Promise<void> {
  const response = await fetch(`${normalizeRuntimeUrl(serveUrl)}/v1/assist/runs/${encodeURIComponent(runId)}/mode`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ mode, profile_id: "review-owner-v1" }),
  });
  if (!response.ok) throw new Error(await readError(response));
}

async function fetchDecisionPages<T>(url: string, field: "sources" | "decisions"): Promise<T[]> {
  const records: T[] = [];
  let cursor: string | null = null;
  for (let page = 0; page < 3; page += 1) {
    const response = await fetch(cursor ? `${url}?cursor=${encodeURIComponent(cursor)}` : url);
    if (!response.ok) throw new Error(await readError(response));
    const body = (await response.json()) as {
      sources?: DecisionSource[]; decisions?: DecisionRecord[]; next_cursor?: unknown;
    };
    const values = body[field];
    if (Array.isArray(values)) records.push(...(values as T[]));
    cursor = typeof body.next_cursor === "string" && body.next_cursor.length > 0 ? body.next_cursor : null;
    if (!cursor) return records;
  }
  throw new Error("Decision support returned too many pages.");
}

export async function fetchDecisionSources(serveUrl: string, runId: string): Promise<DecisionSource[]> {
  return fetchDecisionPages<DecisionSource>(
    `${normalizeRuntimeUrl(serveUrl)}/v1/assist/runs/${encodeURIComponent(runId)}/sources`, "sources",
  );
}

export async function fetchDecisions(serveUrl: string, runId: string): Promise<DecisionRecord[]> {
  return fetchDecisionPages<DecisionRecord>(
    `${normalizeRuntimeUrl(serveUrl)}/v1/assist/runs/${encodeURIComponent(runId)}/decisions`, "decisions",
  );
}

export async function evaluateDecision(serveUrl: string, runId: string, request: {
  request_id: string; profile_id: string; source_id: string; source_sha256: string;
  selection: { start: number; end: number };
}): Promise<void> {
  const response = await fetch(`${normalizeRuntimeUrl(serveUrl)}/v1/assist/runs/${encodeURIComponent(runId)}/evaluations`, {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(request),
  });
  if (!response.ok) throw new Error(await readError(response));
}

export async function recordDecisionFeedback(serveUrl: string, runId: string, decisionId: string, verdict: "useful" | "wrong_owner" | "not_useful" | "dismissed"): Promise<void> {
  const response = await fetch(`${normalizeRuntimeUrl(serveUrl)}/v1/assist/runs/${encodeURIComponent(runId)}/decisions/${encodeURIComponent(decisionId)}/feedback`, {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ request_id: crypto.randomUUID(), verdict }),
  });
  if (!response.ok) throw new Error(await readError(response));
}
