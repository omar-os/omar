type ConfigurationDraft = { version: 1; original: string; program: string };

export function configurationDraftKey(runtime: string, conversation: string, proposal: number): string {
  return `omar:configuration:v1:${JSON.stringify([runtime.replace(/\/$/, ""), conversation, proposal])}`;
}

/** Applied task settings survive a reload, but never replace a new proposal. */
export function readConfigurationDraft(storage: Pick<Storage, "getItem">, key: string, original: string): string {
  try {
    const value = JSON.parse(storage.getItem(key) ?? "null") as ConfigurationDraft | null;
    if (value?.version === 1 && value.original === original && typeof value.program === "string") return value.program;
  } catch { /* An unavailable or invalid local draft leaves the proposal intact. */ }
  return original;
}

export function saveConfigurationDraft(storage: Pick<Storage, "setItem">, key: string, original: string, program: string): void {
  try { storage.setItem(key, JSON.stringify({ version: 1, original, program } satisfies ConfigurationDraft)); }
  catch { throw new Error("Could not save configuration in this browser. Free browser storage or allow site storage, then try again."); }
}
