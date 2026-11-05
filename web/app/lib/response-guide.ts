import type { PendingInvocation } from "./protocol";

export type ResponseGuide = {
  title?: string; summary?: string; instructions?: string[]; next?: string;
  outcomes?: Record<string, { label?: string; help?: string; format?: "json" | "text"; button?: string }>;
};
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
export function guideForFiles(files?: Record<string, string>): Record<string, ResponseGuide> {
  try {
    const value = JSON.parse(files?.["omar.response.json"] || "{}");
    if (!value.steps || typeof value.steps !== "object") return {};
    return Object.fromEntries(Object.entries(value.steps).filter(([, guide]) => guide && typeof guide === "object" && !Array.isArray(guide)).map(([name, raw]) => {
      const guide = raw as Record<string, unknown>;
      const result: ResponseGuide = {};
      for (const key of ["title", "summary", "next"] as const) if (typeof guide[key] === "string") result[key] = guide[key];
      if (Array.isArray(guide.instructions)) result.instructions = guide.instructions.filter((v): v is string => typeof v === "string");
      if (guide.outcomes && typeof guide.outcomes === "object") {
        result.outcomes = Object.fromEntries(Object.entries(guide.outcomes).filter(([, value]) => value && typeof value === "object").map(([port, value]) => {
          const fields = value as Record<string, unknown>;
          return [port, Object.fromEntries(Object.entries(fields).filter(([key, text]) => ["label", "help", "button"].includes(key) ? typeof text === "string" : key === "format" && ["json", "text"].includes(String(text))))];
        }));
      }
      return [name, result];
    }));
  } catch { return {}; }
}
export function exclusiveOutputs(invocation: PendingInvocation): string[] | null {
  const text = invocation.contract.trim().replace(/^\(\s*([^()]*)\s*\)$/, "$1").trim();
  if (!/^[\w.]+(?:\s*\|\s*[\w.]+)+$/.test(text)) return null;
  const names = text.split("|").map(s => s.trim());
  return names.every(name => Object.hasOwn(invocation.allowed_effects, name)) ? names : null;
}
export function fieldLabel(port: string): string {
  const name = port.split(".").at(-1) || port;
  return ({ packet: "Prepared packet", blocked: "Blocker report", receipt: "Publication receipt" } as Record<string, string>)[name]
    || name.replace(/[_-]/g, " ").replace(/^./, c => c.toUpperCase());
}
export function responseGuide(invocation: PendingInvocation, explicit: ResponseGuide = {}) {
  const request = Object.values(invocation.trigger_values).map(readableValue).find(value =>
    value && typeof value === "object" && !Array.isArray(value) &&
    ((value as Record<string, unknown>).mode === "setup_review_only" || (value as Record<string, unknown>).productionAuthorized === false),
  ) as Record<string, unknown> | undefined;
  const blocked = Object.keys(invocation.allowed_effects).find(name => name.split(".").at(-1) === "blocked");
  const setupOnly = !!request && !!blocked;
  // Legacy requests may describe setup requirements in prose. Only surface
  // requirements actually named in the input or instructions; unrelated
  // workflows must not inherit a publication campaign's checklist.
  const setupText = `${invocation.prompt} ${JSON.stringify(request ?? {})}`;
  const needsProject = !!request && ("projectRoot" in request || /projectRoot|project (?:folder|root|directory)/i.test(setupText));
  const needsSlot = !!request && ("dateNewYork" in request || /New York.*(?:date|slot)|date.*slot/i.test(setupText));
  const needsStory = !!request && ("storyBrief" in request || /story.selection|story brief/i.test(setupText));
  const missing = [
    ...(needsProject ? ["Provide the approved project folder with its playbook and production files."] : []),
    ...(needsSlot ? ["Specify the New York publication date and slot."] : []),
    ...(needsStory ? ["Provide the story brief."] : []),
    ...(!needsProject && !needsSlot && !needsStory ? ["Review the missing requirements in Full instructions."] : []),
    "Replace the setup-only request with an approved production request before continuing production.",
  ];
  const promptSummary = invocation.prompt.split(/\n|(?<=[.!?])\s/)[0].trim();
  const fields = Object.keys(invocation.allowed_effects);
  return {
    title: setupOnly ? (needsProject ? "Project details required" : "Setup details required") : (typeof explicit.title === "string" ? explicit.title : fields.length === 1 ? `Provide ${fieldLabel(fields[0]).toLowerCase()}` : "Submit a result or report a blocker"),
    summary: setupOnly ? "Production is on hold. This request is in setup-review mode and is not authorized for production."
      : typeof explicit.summary === "string" ? explicit.summary : promptSummary.length <= 220 ? promptSummary : "Complete the task in Full instructions, then submit its result below.",
    instructions: setupOnly ? missing : Array.isArray(explicit.instructions) ? explicit.instructions.filter(x => typeof x === "string") : [invocation.agent.split(".").at(-1)?.toLowerCase() === "chatgpt" ? "Complete this step in ChatGPT. OMAR does not run ChatGPT automatically." : "Complete this manual step using the full instructions below.", "Return here and submit the requested result, or describe what prevents completion."],
    next: setupOnly ? "Report the missing requirements below. Production remains on hold; this does not submit a production packet."
      : typeof explicit.next === "string" ? explicit.next : "Submitting completes this step. The workflow continues along the selected output connection.",
    setupOnly, blocked, outcomes: explicit.outcomes || {},
  };
}
