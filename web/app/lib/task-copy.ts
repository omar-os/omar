import type { DiagramAgent, DiagramReaction, DiagramSnapshot } from "./protocol";

/** Display copy is separate from runtime identity. Old saved snapshots have no
 * metadata: describe their actual prompt or declared inputs/outputs, never infer
 * approvals, integrations, or side effects from an agent's role. */
export function taskCopy(reaction: DiagramReaction, agent?: DiagramAgent, snapshot?: DiagramSnapshot) {
  const title = reaction.title?.trim();
  const description = reaction.description?.trim();
  const prompt = (reaction.prompt ?? "")
    .replace(/<omar-connections>[\s\S]*?<\/omar-connections>/g, "")
    .replace(/\$\(([^)]*)\)/g, (_, name: string) => humanize(name))
    .replace(/\s+/g, " ").trim();
  const inputs = reaction.triggers.map(humanize);
  const outputs = reaction.effects.map(humanize);
  const io = `${inputs.length ? `Uses ${join(inputs)}. ` : ""}${outputs.length ? `May produce ${join(outputs)} according to the step's output rules.` : "No output is declared."}`;
  // A meaningful team name is useful for old code reactions whose bodies were
  // never included in snapshots. It is a label, not a claim about the code.
  const team = snapshot?.instances.find((item) => item.id === `instance::${reaction.instance}`)?.team;
  const teamName = humanize(team ?? "");
  const purpose = prompt.replace(/^You are\b[^.!?]*[.!?]\s*/i, "")
    .replace(/^Judge this exact criterion:\s*/i, "Check that ");
  const firstSentence = purpose.match(/^.*?[.!?](?:\s|$)/)?.[0]?.trim() ?? purpose;
  const fallbackTitle = firstSentence && !isRoleName(firstSentence)
    ? shortTaskText(firstSentence.replace(/[.!?:;,]+$/, ""), 80)
    : teamName && !isRoleName(teamName)
      ? capitalize(teamName)
      : outputs.length ? `Produce ${join(outputs)}` : inputs.length ? `Handle ${join(inputs)}` : "Run configured code";
  const manual = agent?.backend.toLowerCase() === "web";
  return {
    title: title && !isRoleName(title) ? title : (manual ? `Request human response: ${shortTaskText(fallbackTitle, 52)}` : fallbackTitle),
    description: description || `${manual ? "Waits for a person to respond in the web panel. " : ""}${prompt || io}`,
  };
}

function isRoleName(value: string): boolean {
  return value.toLowerCase().split(/[^\p{L}\p{N}]+/u).filter(Boolean).every(word =>
    /^(team|node|worker|agent|review|reviewer|reasoner|step|task|planner|synthesizer|verifier|checker|writer|collector|native|codex|jev|\d+)$/.test(word));
}

function humanize(value: string): string {
  return (value.split("::").pop()?.split(".").pop() ?? value)
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/([A-Z])([A-Z][a-z])/g, "$1 $2")
    .replace(/[_-]+/g, " ").trim().toLowerCase();
}
function join(values: string[]): string {
  return values.length > 1 ? `${values.slice(0, -1).join(", ")} and ${values.at(-1)}` : values[0] ?? "";
}
function capitalize(value: string): string { return value.charAt(0).toUpperCase() + value.slice(1); }

/** SVG text cannot wrap automatically. The inspector and tooltip retain all copy. */
export function shortTaskText(text: string, maxCharacters: number): string {
  const characters = Array.from(text.replace(/\s+/g, " ").trim());
  return characters.length <= maxCharacters ? characters.join("")
    : `${characters.slice(0, Math.max(0, maxCharacters - 1)).join("").trimEnd()}…`;
}

/** Two title lines keep action and object visible without widening the graph. */
export function taskTitleLines(title: string, width: number): string[] {
  const limit = Math.max(8, Math.floor(width / 8.1));
  const words = title.split(/\s+/);
  let first = "";
  while (words.length && Array.from(`${first} ${words[0]}`.trim()).length <= limit) {
    first = `${first} ${words.shift()}`.trim();
  }
  if (!first) first = shortTaskText(words.shift() ?? title, limit);
  return words.length ? [first, shortTaskText(words.join(" "), limit)] : [first];
}
