import type { DiagramReaction, DiagramSnapshot } from "./protocol";

type Annotations = { title?: string; description?: string; spans: Span[] };
type Token = { text: string; start: number; end: number; string: boolean; annotations?: Annotations };
type Span = { start: number; end: number };
export type StepSource = {
  title?: string;
  description?: string;
  metadataSpans: Span[];
  declarationStart: number;
  team: string;
  backend: string;
  prompt: string;
  triggers: string[];
  contract: string;
  backendSpan: Span;
  promptSpan: Span;
  triggersSpan: Span;
  contractSpan: Span;
};

/** Locate editable spans, never decide whether a workflow is valid. The runtime
 * compiler validates every replacement before it becomes the current draft. */
function tokens(source: string): Token[] {
  const result: Token[] = [];
  let i = 0;
  while (i < source.length) {
    if (/\s/.test(source[i])) { i++; continue; }
    if (source.startsWith("//", i)) {
      const annotations: Annotations = { spans: [] };
      let continuing = false;
      while (source.startsWith("//", i)) {
        const newline = source.indexOf("\n", i);
        const end = newline < 0 ? source.length : newline;
        const line = source.slice(i + 2, end).trim();
        const tag = /^@(title|desc)\s+(.*)$/.exec(line);
        if (tag) {
          continuing = tag[1] === "desc";
          annotations[continuing ? "description" : "title"] = tag[2];
        } else if (continuing) {
          annotations.description = (annotations.description + " " + line).trim();
        }
        if (tag || continuing) annotations.spans.push({ start: i, end });
        i = Math.min(source.length, end + 1);
        while (i < source.length && /[ \t\r]/.test(source[i])) i++;
      }
      if (annotations.spans.length && /^task\b/.test(source.slice(i))) {
        result.push({ text: "task", start: i, end: i + 4, string: false, annotations });
        i += 4;
      }
      continue;
    }
    if (source.startsWith("/*", i)) {
      let depth = 1; i += 2;
      while (depth && i < source.length) {
        if (source.startsWith("/*", i)) { depth++; i += 2; }
        else if (source.startsWith("*/", i)) { depth--; i += 2; }
        else i++;
      }
      if (depth) throw new Error("Unclosed comment");
      continue;
    }
    // Rust code has its own lexer. Leave these programs to the source editor.
    if (source.startsWith("{=", i)) throw new Error("Code reaction");
    const start = i;
    if (source[i] === '"') {
      i++;
      while (i < source.length && source[i] !== '"') {
        if (source[i] === "\\" && /["\\]/.test(source[i + 1] ?? "")) i += 2;
        else i++;
      }
      if (i === source.length) throw new Error("Unclosed prompt");
      i++;
      result.push({ text: source.slice(start + 1, i - 1).replace(/\\(["\\])/g, "$1"), start, end: i, string: true });
    } else {
      const word = /^[A-Za-z_][A-Za-z_0-9]*/.exec(source.slice(i));
      i += word?.[0].length ?? (source.startsWith("->", i) ? 2 : 1);
      result.push({ text: source.slice(start, i), start, end: i, string: false });
    }
  }
  return result;
}

export function locateStep(source: string, snapshot: DiagramSnapshot, reaction: DiagramReaction): StepSource | null {
  try {
    const all = tokens(source);
    const instance = snapshot.instances.find((item) => item.id.replace(/^instance::/, "") === reaction.instance);
    const team = instance?.team ?? snapshot.team;
    const index = Number(/(?:^|\.)reaction\.(\d+)$/.exec(reaction.name)?.[1]);
    if (!Number.isInteger(index)) return null;
    const begin = all.findIndex((token, i) => !token.string && token.text === "team" && all[i + 1]?.text === team);
    if (begin < 0) return null;
    const open = all.findIndex((token, i) => i > begin && !token.string && token.text === "{");
    if (open < 0) return null;
    const end = all.findIndex((token, i) => i > open && !token.string && token.text === "}");
    if (end < 0) return null;
    const prompts = all.slice(open + 1, end).filter((token) => !token.string && token.text === "task");
    const declaration = prompts[index];
    if (!declaration) return null;
    const position = all.indexOf(declaration);
    const owner = all[position + 1]?.text;
    const actualOwner = snapshot.agents.find((agent) => agent.id === reaction.agent)?.name.split(".").at(-1);
    if (owner !== actualOwner || all[position + 2]?.text !== "(") return null;
    const agent = all.findIndex((token, i) => i > begin && i < open && token.text === owner && all[i + 1]?.text === ":");
    if (agent < 0) return null;
    const backend = all[agent + 2];
    const close = all.findIndex((token, i) => i > position + 2 && !token.string && token.text === ")");
    if (close < 0 || all[close + 1]?.text !== "->") return null;
    const firstString = all.findIndex((token, i) => i > close + 1 && token.string);
    // Constant strings in output contracts are deliberately left to source editing.
    if (firstString < 0 || all[firstString - 1]?.text === "=" || firstString >= end) return null;
    const body = firstString;
    const deadline = all.findIndex((token, i) => i > close + 1 && i < body && !token.string && token.text === "within");
    const contractEnd = deadline < 0 ? body : deadline;
    const triggersSpan = { start: all[position + 2].end, end: all[close].start };
    const contractSpan = { start: all[close + 2].start, end: all[contractEnd - 1].end };
    return {
      title: declaration.annotations?.title,
      description: declaration.annotations?.description,
      metadataSpans: declaration.annotations?.spans ?? [],
      declarationStart: declaration.start,
      team, backend: backend.text, prompt: all[body].text,
      triggers: source.slice(triggersSpan.start, triggersSpan.end).split(",").map((s) => s.trim()).filter(Boolean),
      contract: source.slice(contractSpan.start, contractSpan.end),
      backendSpan: backend, promptSpan: all[body], triggersSpan, contractSpan,
    };
  } catch { return null; }
}

export function replaceStep(source: string, step: StepSource, changes: { backend: string; prompt: string; triggers: string[]; contract: string; title?: string; description?: string }): string {
  if (!/^[A-Za-z_][A-Za-z_0-9]*$/.test(changes.backend)) throw new Error("Choose a supported execution backend.");
  const replacements = [
    { ...step.backendSpan, value: changes.backend },
    { ...step.promptSpan, value: `"${changes.prompt.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"` },
    { ...step.triggersSpan, value: changes.triggers.join(", ") },
    { ...step.contractSpan, value: changes.contract },
  ];
  // Changed instructions invalidate old copy unless the editor explicitly
  // supplies a revised title and description. Backend-only edits preserve it.
  const explicitCopy = changes.title !== undefined && changes.description !== undefined;
  const changedInstructions = changes.prompt !== step.prompt || changes.contract !== step.contract || changes.triggers.join(",") !== step.triggers.join(",");
  if (explicitCopy || changedInstructions) {
    for (const span of step.metadataSpans) replacements.push({ ...span, value: "" });
    if (explicitCopy) {
      const indent = source.slice(source.lastIndexOf("\n", step.declarationStart - 1) + 1, step.declarationStart).match(/^[ \t]*/)?.[0] ?? "";
      const lines = [["title", changes.title], ["desc", changes.description]]
        .filter(([, value]) => value?.trim())
        .map(([tag, value]) => "// @" + tag + " " + value!.trim().replace(/[\r\n]+/g, " ") + "\n" + indent);
      replacements.push({start: step.declarationStart, end: step.declarationStart, value: lines.join("")});
    }
  }
  replacements.sort((a, b) => b.start - a.start || b.end - a.end);
  return replacements.reduce((text, edit) => text.slice(0, edit.start) + edit.value + text.slice(edit.end), source);
}
