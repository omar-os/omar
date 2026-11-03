import type { DiagramAgent, DiagramReaction } from "./protocol";

/** Shared by the card and inspector. Legacy workflows keep their old labels. */
export function taskCopy(reaction: DiagramReaction, agent?: DiagramAgent) {
  const generated = /(?:^|\.)reaction\.\d+$/.test(reaction.name);
  return {
    title: reaction.title?.trim() || (generated ? agent?.name ?? reaction.name : reaction.name),
    description: reaction.description?.trim() || "",
  };
}

/** SVG text cannot wrap; keep the full copy in the inspector and tooltip. */
export function shortTaskText(text: string, maxCharacters: number): string {
  const characters = Array.from(text.replace(/\s+/g, " ").trim());
  return characters.length <= maxCharacters
    ? characters.join("")
    : `${characters.slice(0, Math.max(0, maxCharacters - 1)).join("").trimEnd()}…`;
}
