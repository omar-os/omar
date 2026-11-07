import type { TaskConnection } from "./task-connections.ts";
import { connectionsInSource } from "./step-source.ts";
import { normalizeRuntimeUrl } from "./runtime-client.ts";

export type IntegrationChange = { connection: TaskConnection; before?: TaskConnection };

export function changedIntegrations(before: TaskConnection[], after: TaskConnection[]): IntegrationChange[] {
  return after.flatMap(connection => {
    const previous = before.find(item => item.id === connection.id);
    return JSON.stringify(previous) === JSON.stringify(connection) ? [] : [{ connection, before: previous }];
  });
}

/** Older releases saved applied changes only as browser drafts. Import solely
 * drafts for the selected runtime, filling missing entries without overwriting edits. */
export function previousIntegrations(storage: Storage, runtime: string): TaskConnection[] {
  const prefix = "omar:configuration:v1:";
  const result = new Map<string, TaskConnection>();
  for (let i = 0; i < storage.length; i++) {
    const key = storage.key(i);
    if (!key?.startsWith(prefix)) continue;
    try {
      const [owner] = JSON.parse(key.slice(prefix.length));
      if (owner !== runtime.replace(/\/$/, "")) continue;
      const draft = JSON.parse(storage.getItem(key) ?? "null");
      if (draft?.version !== 1 || typeof draft.program !== "string") continue;
      for (const item of connectionsInSource(draft.program)) result.set(item.id, item);
    } catch { /* Ignore malformed records, leaving the original draft untouched. */ }
  }
  return [...result.values()];
}

export async function catalogRequest(runtime: string, changes?: IntegrationChange[], importOnly = false): Promise<TaskConnection[]> {
  const response = await fetch(`${normalizeRuntimeUrl(runtime)}/v1/task-integrations`, changes ? {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ changes, importOnly }),
  } : { cache: "no-store" });
  const data = await response.json();
  if (!response.ok) throw new Error(data.error || "Could not load integrations.");
  if (!Array.isArray(data.connections)) throw new Error("The runtime returned an invalid integration catalog.");
  return data.connections;
}
