import type { DiagramReaction, DiagramSnapshot } from "./protocol";

type Token = { text: string; start: number; end: number; string: boolean };
type Span = { start: number; end: number };
export type StepSource = {
  team: string;
  backend: string;
  model: string;
  role: string;
  bindingEnd: number;
  prompt: string;
  triggers: string[];
  contract: string;
  backendSpan: Span;
  promptSpan: Span;
  triggersSpan: Span;
  contractSpan: Span;
};

// The compiler treats Rust as opaque. A terminator in a comment, raw string
// or quoted literal belongs to Rust and cannot begin another OMAR declaration.
function codeEnd(source: string, start: number): number {
  const literal = /\/\/[^\n]*|\/\*|\b(?:br|cr|r)(#*)"|'(?:\\.|[^'\\])'|"(?:\\[\s\S]|[^"\\])*"|=}/gy;
  let i = start + 2;
  while (i < source.length) {
    literal.lastIndex = i;
    const match = literal.exec(source);
    if (!match) { i++; continue; }
    i = literal.lastIndex;
    if (match[0] === "=}") return i;
    if (match[1] !== undefined) {
      const delimiter = '"' + match[1];
      const end = source.indexOf(delimiter, i);
      if (end < 0) break;
      i = end + delimiter.length;
    } else if (match[0] === "/*") {
      let depth = 1;
      while (depth && i < source.length) {
        if (source.startsWith("/*", i)) { depth++; i += 2; }
        else if (source.startsWith("*/", i)) { depth--; i += 2; }
        else i++;
      }
    }
  }
  throw new Error("Unclosed code reaction");
}

/** Locate editable spans, never decide whether a workflow is valid. The runtime
 * compiler validates every replacement before it becomes the current draft. */
export function tokens(source: string): Token[] {
  const result: Token[] = [];
  let i = 0;
  while (i < source.length) {
    if (/\s/.test(source[i])) { i++; continue; }
    if (source.startsWith("//", i)) { const end = source.indexOf("\n", i); i = end < 0 ? source.length : end; continue; }
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
    const start = i;
    if (source.startsWith("{=", i)) {
      i = codeEnd(source, i);
      result.push({ text: "{=…=}", start, end: i, string: false });
    } else if (source.startsWith('"""', i)) {
      const close = source.indexOf('"""', i + 3);
      if (close < 0) throw new Error("Unclosed block prompt");
      const lines = source.slice(i + 3, close).replace(/\r+$/gm, "").split("\n");
      if (!lines[0]?.trim()) lines.shift();
      if (!lines.at(-1)?.trim()) lines.pop();
      const indent = Math.min(...lines.filter(line => line.trim()).map(line => /^[ \t]*/.exec(line)![0].length));
      i = close + 3;
      result.push({ text: lines.map(line => line.trim() ? line.slice(indent) : "").join("\n"), start, end: i, string: true });
    } else if (source[i] === '"') {
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
    const prompts = all.slice(open + 1, end).filter((token) => !token.string && (token.text === "task" || token.text === "reaction"));
    const declaration = prompts[index];
    if (!declaration || declaration.text !== "task") return null;
    const position = all.indexOf(declaration);
    const owner = all[position + 1]?.text;
    const actualOwner = snapshot.agents.find((agent) => agent.id === reaction.agent)?.name.split(".").at(-1);
    if (owner !== actualOwner || all[position + 2]?.text !== "(") return null;
    const agent = all.findIndex((token, i) => i > begin && i < open && token.text === owner && all[i + 1]?.text === ":");
    if (agent < 0) return null;
    const backend = all[agent + 2];
    let bindingEnd = backend.end;
    let model = "";
    if (all[agent + 3]?.text === "(") {
      const option = all.slice(agent + 4, agent + 8);
      if (option.length !== 4 || option[0].text !== "model" || option[1].text !== "=" || !option[2].string || option[3].text !== ")") return null;
      model = option[2].text;
      bindingEnd = option[3].end;
    }
    const close = all.findIndex((token, i) => i > position + 2 && !token.string && token.text === ")");
    if (close < 0 || all[close + 1]?.text !== "->") return null;
    const body = all.findIndex((token, i) => i > close + 1 && token.string);
    // Constant strings in output contracts are deliberately left to source editing.
    if (body < 0 || all[body - 1]?.text === "=" || body >= end ||
        all.slice(close + 2, body).some(token => ["task", "reaction", "{=…=}"].includes(token.text))) return null;
    const deadline = all.findIndex((token, i) => i > close + 1 && i < body && !token.string && token.text === "within");
    const contractEnd = deadline < 0 ? body : deadline;
    const triggersSpan = { start: all[position + 2].end, end: all[close].start };
    const contractSpan = { start: all[close + 2].start, end: all[contractEnd - 1].end };
    return {
      team, role: owner, model, bindingEnd, backend: backend.text, prompt: all[body].text,
      triggers: source.slice(triggersSpan.start, triggersSpan.end).split(",").map((s) => s.trim()).filter(Boolean),
      contract: source.slice(contractSpan.start, contractSpan.end),
      backendSpan: backend, promptSpan: all[body], triggersSpan, contractSpan,
    };
  } catch { return null; }
}

export function quotedPrompt(prompt: string): string {
  return '"' + prompt.replace(/\\/g, "\\\\").replace(/"/g, '\\"') + '"';
}

export function replacePrompt(source: string, step: StepSource, prompt: string): string {
  return source.slice(0, step.promptSpan.start) + quotedPrompt(prompt) + source.slice(step.promptSpan.end);
}

export function replaceStep(source: string, step: StepSource, changes: { backend: string; model?: string; prompt: string; triggers: string[]; contract: string }): string {
  if (!/^[A-Za-z_][A-Za-z_0-9]*$/.test(changes.backend)) throw new Error("Choose a supported execution backend.");
  const model = changes.model ?? step.model;
  if (model && (!/^[a-zA-Z0-9._:/@+\[\]-]+$/.test(model) || model.startsWith("-"))) throw new Error("Choose a valid backend model ID.");
  const binding = model ? `${changes.backend}(model = "${model}")` : changes.backend;
  const replacements = [
    model === step.model ? { ...step.backendSpan, value: changes.backend } : { start: step.backendSpan.start, end: step.bindingEnd, value: binding },
    { ...step.promptSpan, value: quotedPrompt(changes.prompt) },
    { ...step.triggersSpan, value: changes.triggers.join(", ") },
    { ...step.contractSpan, value: changes.contract },
  ].sort((a, b) => b.start - a.start);
  return replacements.reduce((text, edit) => text.slice(0, edit.start) + edit.value + text.slice(edit.end), source);
}
