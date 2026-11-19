import type { PendingInvocation } from "./protocol";

export function readableValue(value: unknown): unknown {
  // Strings can contain JSON, including a historically double-encoded request.
  // Decode for display only. The exact original is retained by the runtime.
  for (let i = 0; i < 3 && typeof value === "string"; i++) {
    try { value = JSON.parse(value); } catch { break; }
  }
  return value;
}
export function prettyValue(value: unknown): string {
  const decoded = readableValue(value);
  return typeof decoded === "string" ? decoded : JSON.stringify(decoded, null, 2) ?? "—";
}
export function exclusiveOutputs(invocation: PendingInvocation): string[] | null {
  const text = invocation.contract.trim().replace(/^\(\s*([^()]*)\s*\)$/, "$1").trim();
  if (!/^[\w.]+(?:\s*\|\s*[\w.]+)+$/.test(text)) return null;
  const names = text.split("|").map(s => s.trim());
  return names.every(name => Object.hasOwn(invocation.allowed_effects, name)) ? names : null;
}
export function fieldLabel(port: string): string {
  const name = port.split(".").at(-1) || port;
  return name.replace(/[_-]/g, " ").replace(/^./, c => c.toUpperCase());
}
/** Generic copy for a manual step, derived only from the invocation itself. */
export function responseGuide(invocation: PendingInvocation) {
  const promptSummary = invocation.prompt.split(/\n|(?<=[.!?])\s/)[0].trim();
  const fields = Object.keys(invocation.allowed_effects);
  return {
    title: fields.length === 1 ? `Provide ${fieldLabel(fields[0]).toLowerCase()}` : "Submit a response",
    summary: promptSummary.length <= 220 ? promptSummary : "Complete the task in Full instructions, then submit its result below.",
    instructions: ["Complete this manual step using the full instructions below.", "Return here and submit the requested result, or describe what prevents completion."],
    next: "Submitting completes this step. The workflow continues along the selected output connection.",
  };
}
