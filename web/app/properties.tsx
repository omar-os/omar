"use client";

import { useMemo } from "react";
import { tokenizeLean } from "./lib/lean-syntax";
import type { PropertyVerdict } from "./lib/protocol";

/** A theorem as Lean source, numbered, with the lines Lean objected to marked. */
function LeanCode({ source, problems }: { source: string; problems: Set<number> }) {
  const lines = useMemo(() => {
    // Tokenized whole, then split, so a comment spanning lines stays one.
    const rows: { kind: string; text: string }[][] = [[]];
    for (const token of tokenizeLean(source.replace(/\n$/, ""))) {
      const parts = token.text.split("\n");
      parts.forEach((part, index) => {
        if (index > 0) rows.push([]);
        if (part) rows[rows.length - 1].push({ kind: token.kind, text: part });
      });
    }
    return rows;
  }, [source]);
  return (
    <pre className="lean-code" aria-label="Lean proof">
      <code>
        {lines.map((tokens, index) => (
          <span key={index} className={`lean-line${problems.has(index + 1) ? " has-problem" : ""}`}>
            <span className="lean-line-number" aria-hidden="true">{index + 1}</span>
            {tokens.map((token, at) => (
              <span key={at} className={`omar-tok-${token.kind}`}>{token.text}</span>
            ))}
            {"\n"}
          </span>
        ))}
      </code>
    </pre>
  );
}

export function PropertiesPanel({
  verdicts,
  selected,
  onSelect,
  stale,
  checking,
  onRecheck,
  onOpenEditor,
  opening,
  error,
}: {
  verdicts: PropertyVerdict[];
  selected: string | null;
  onSelect: (name: string | null) => void;
  /** The source has changed since Lean checked these. */
  stale: boolean;
  checking: boolean;
  onRecheck?: () => void;
  onOpenEditor?: () => void;
  opening: boolean;
  error: string;
}) {
  const proved = verdicts.filter((verdict) => verdict.proved).length;
  return (
    <section className="properties" role="tabpanel" aria-label="Formal properties">
      <header className="properties-header">
        <strong>Formal guarantees</strong>
        <span className={proved === verdicts.length ? "all-proved" : ""}>
          {proved} of {verdicts.length} proved by Lean
        </span>
      </header>
      <p className="properties-guidance">
        Each holds for every run of this program, whatever its agents answer: it is proved from
        the reactor semantics and the effect contracts, never from what a prompt asks. Select one to
        see the components it is about.
      </p>
      {stale ? (
        <div className="properties-stale" role="status">
          <span>The source has changed since Lean checked these.</span>
          {onRecheck ? (
            <button type="button" className="secondary-button" disabled={checking} onClick={onRecheck}>
              {checking ? "Checking…" : "Check again"}
            </button>
          ) : null}
        </div>
      ) : null}
      {onOpenEditor ? (
        <button type="button" className="secondary-button properties-editor" disabled={opening} onClick={onOpenEditor}>
          {opening ? "Starting editor…" : "Open in Web VS Code"}
        </button>
      ) : null}
      {error ? <p className="properties-error" role="alert">{error}</p> : null}
      <ol className="property-list">
        {verdicts.map((verdict) => {
          const open = selected === verdict.name;
          const problemLines = new Set(verdict.problems.map((problem) => problem.line));
          return (
            <li key={verdict.name} className={`property${verdict.proved ? " proved" : " unproved"}${open ? " selected" : ""}`}>
              <button
                type="button"
                className="property-summary"
                aria-expanded={open}
                onClick={() => onSelect(open ? null : verdict.name)}
              >
                <span className="property-status">{verdict.proved ? "Proved" : "Not proved"}</span>
                <span className="property-description">{verdict.description || verdict.name}</span>
                <code className="property-name">{verdict.name}</code>
              </button>
              {open ? (
                <div className="property-detail">
                  {verdict.mentions.length ? (
                    <p className="property-mentions">
                      About {verdict.mentions.map((name, index) => (
                        <span key={name}>{index ? ", " : ""}<code>{name}</code></span>
                      ))}
                    </p>
                  ) : null}
                  <LeanCode source={verdict.theorem} problems={problemLines} />
                  {verdict.problems.length ? (
                    <ul className="property-problems" aria-label="What Lean objected to">
                      {verdict.problems.map((problem, index) => (
                        <li key={index}>
                          {problem.line ? <b>Line {problem.line}: </b> : null}
                          <span>{problem.message}</span>
                        </li>
                      ))}
                    </ul>
                  ) : null}
                </div>
              ) : null}
            </li>
          );
        })}
      </ol>
    </section>
  );
}
