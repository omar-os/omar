import type { ProposedDesign } from "./protocol";
import { normalizeRuntimeUrl } from "./runtime-client";

export type WorkflowDraft = { design: ProposedDesign; filename: string; proposal_sequence: number };
export type WorkflowState = { revision: number; name: string | null; draft: WorkflowDraft | null };
export async function fetchWorkflow(base: string): Promise<WorkflowState> {
  const response = await fetch(`${normalizeRuntimeUrl(base)}/v1/workflow`);
  if (!response.ok) throw new Error("Could not load your saved workflow. Reconnect and retry.");
  return response.json();
}
export async function updateWorkflow(base: string, update: { revision: number; name?: string; draft?: WorkflowDraft; clear_draft?: boolean }): Promise<WorkflowState> {
  const response = await fetch(`${normalizeRuntimeUrl(base)}/v1/workflow`, {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(update),
  });
  const data = await response.json();
  if (!response.ok) throw new Error(data.error || "Could not save your workflow.");
  return data;
}

/** One serial save queue per conversation. A recovery copy protects edits even
 * when a tab closes before the server acknowledges them. Revision conflicts
 * never silently overwrite a newer tab's draft. */
export class WorkflowDraftStore {
  private revision = 0;
  private pending: WorkflowDraft | null = null;
  private inflight: Promise<void> | null = null;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private listeners = new Set<() => void>();
  private status = "Saved";
  private blocked = false;
  readonly key: string;
  constructor(readonly base: string) { this.key = `omar:workflow-draft:${base}`; }
  subscribe = (fn: () => void) => { this.listeners.add(fn); return () => { this.listeners.delete(fn); }; };
  snapshot = () => this.status;
  private publish(status: string) { this.status = status; this.listeners.forEach(fn => fn()); }
  async load(): Promise<WorkflowState> {
    const saved = await fetchWorkflow(this.base);
    this.revision = saved.revision;
    let recovery: { revision: number; draft: WorkflowDraft } | null = null;
    try { recovery = JSON.parse(localStorage.getItem(this.key) || "null"); } catch { /* Server remains authoritative. */ }
    if (recovery?.draft?.design && typeof recovery.revision === "number") {
      if (JSON.stringify(recovery.draft) === JSON.stringify(saved.draft)) {
        try { localStorage.removeItem(this.key); } catch { /* The server copy is durable. */ }
      } else {
        this.pending = recovery.draft;
        if (recovery.revision !== saved.revision) {
          this.blocked = true;
          this.publish("A newer version was saved in another tab. Your edits are kept here; copy them before using the saved version.");
        } else {
          this.publish("Saving…");
          void this.flush().catch(() => {});
        }
        return { ...saved, draft: recovery.draft };
      }
    }
    return saved;
  }
  stage(draft: WorkflowDraft) {
    this.pending = draft;
    try { localStorage.setItem(this.key, JSON.stringify({ revision: this.revision, draft })); }
    catch { this.publish("Saving… Keep this tab open until saved."); }
    if (this.blocked) return;
    this.publish("Saving…");
    clearTimeout(this.timer);
    this.timer = setTimeout(() => { void this.flush().catch(() => {}); }, 300);
  }
  async flush(): Promise<void> {
    clearTimeout(this.timer);
    if (this.blocked) throw new Error(this.status);
    if (this.inflight) { await this.inflight; if (this.pending) return this.flush(); return; }
    const draft = this.pending;
    if (!draft) return;
    const operation = async () => {
      try {
        const saved = await updateWorkflow(this.base, { revision: this.revision, draft });
        this.revision = saved.revision;
        if (this.pending === draft) {
          this.pending = null;
          try { localStorage.removeItem(this.key); } catch { /* Already saved on the server. */ }
          this.publish("Saved");
        } else {
          try { localStorage.setItem(this.key, JSON.stringify({ revision: this.revision, draft: this.pending })); } catch { /* Next save still persists to the server. */ }
        }
      } catch (error) {
        this.blocked = /another tab|newer proposal/.test(String(error));
        this.publish(error instanceof Error ? error.message : "Could not save. Your edits are kept in this browser.");
        throw error;
      } finally { this.inflight = null; }
    };
    this.inflight = operation();
    await this.inflight;
    if (this.pending) await this.flush();
  }
  async useSaved(): Promise<WorkflowState> {
    if (this.inflight) await this.inflight.catch(() => {});
    clearTimeout(this.timer);
    const saved = await fetchWorkflow(this.base);
    this.pending = null; this.blocked = false; this.revision = saved.revision;
    try { localStorage.removeItem(this.key); } catch { /* The server copy is durable. */ }
    this.publish("Saved");
    return saved;
  }
}
