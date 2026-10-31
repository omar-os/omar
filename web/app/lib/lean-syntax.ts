/**
 * A tokenizer for the Lean a property is written in.
 *
 * It colours what a reader scans a proof for — keywords, tactics, names of
 * the program's components, comments — and leaves the rest as text. The
 * kinds are the OMAR tokenizer's, so the editor's colours apply unchanged.
 */

import type { Token, TokenKind } from "./omar-syntax";

const KEYWORDS = new Set([
  "theorem", "lemma", "def", "example", "fun", "let", "have", "show", "from", "by",
  "if", "then", "else", "match", "with", "at", "using", "in", "do", "open", "namespace",
  "end", "where", "calc", "this",
]);

const TACTICS = new Set([
  "intro", "intros", "rintro", "exact", "refine", "apply", "rcases", "obtain", "cases",
  "induction", "simp", "simp_all", "simpa", "subst", "rfl", "omega", "decide", "constructor",
  "left", "right", "exists", "use", "contradiction", "exfalso", "assumption", "trivial",
  "rw", "rwa", "unfold", "split", "by_cases", "first", "all_goals", "any_goals", "repeat",
  "specialize", "generalize", "funext", "ext", "congr", "norm_num", "linarith", "nofun",
]);

const IDENT_START = /[A-Za-z_α-ωΑ-Ω]/;
const IDENT_PART = /[A-Za-z0-9_'.!?α-ωΑ-Ω₀-₉]/;

export function tokenizeLean(source: string): Token[] {
  const tokens: Token[] = [];
  let index = 0;
  const push = (kind: TokenKind, text: string) => {
    if (!text) return;
    const last = tokens[tokens.length - 1];
    if (last && last.kind === kind) last.text += text;
    else tokens.push({ kind, text });
  };

  while (index < source.length) {
    const char = source[index];

    if (source.startsWith("--", index)) {
      const end = source.indexOf("\n", index);
      const stop = end === -1 ? source.length : end;
      push("comment", source.slice(index, stop));
      index = stop;
      continue;
    }

    // `/- … -/` and doc comments nest, as Lean's do.
    if (source.startsWith("/-", index)) {
      let depth = 0;
      let cursor = index;
      while (cursor < source.length) {
        if (source.startsWith("/-", cursor)) { depth += 1; cursor += 2; }
        else if (source.startsWith("-/", cursor)) {
          depth -= 1; cursor += 2;
          if (depth === 0) break;
        } else cursor += 1;
      }
      push("comment", source.slice(index, cursor));
      index = cursor;
      continue;
    }

    // A string is a component's name: what the property is about.
    if (char === '"') {
      let cursor = index + 1;
      while (cursor < source.length && source[cursor] !== '"') {
        cursor += source[cursor] === "\\" ? 2 : 1;
      }
      push("string", source.slice(index, cursor + 1));
      index = cursor + 1;
      continue;
    }

    if (/[0-9]/.test(char)) {
      let cursor = index;
      while (cursor < source.length && /[0-9]/.test(source[cursor])) cursor += 1;
      push("number", source.slice(index, cursor));
      index = cursor;
      continue;
    }

    if (IDENT_START.test(char)) {
      let cursor = index;
      while (cursor < source.length && IDENT_PART.test(source[cursor])) cursor += 1;
      const word = source.slice(index, cursor);
      const kind: TokenKind = KEYWORDS.has(word)
        ? "keyword"
        : TACTICS.has(word)
          ? "agent"
          : /^[A-Z]/.test(word)
            ? "type"
            : "text";
      push(kind, word);
      index = cursor;
      continue;
    }

    if ("∀∃λ".includes(char)) {
      push("keyword", char);
      index += 1;
      continue;
    }

    if ("()[]{}⟨⟩,:;|·→←↔∧∨¬≠≤≥=<>+-*/▸".includes(char)) {
      push("punctuation", char);
      index += 1;
      continue;
    }

    push("text", char);
    index += 1;
  }
  return tokens;
}
