"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { DecisionCapabilities, DecisionRecord, DecisionSource } from "./lib/protocol";
import {
  evaluateDecision,
  fetchDecisions,
  fetchDecisionSources,
  recordDecisionFeedback,
  setDecisionMode,
} from "./lib/decision-client";

type Props = { serveUrl: string; runId: string; capabilities: DecisionCapabilities };

export function Suggestions({ serveUrl, runId, capabilities }: Props) {
  const [sources, setSources] = useState<DecisionSource[]>([]);
  const [decisions, setDecisions] = useState<DecisionRecord[]>([]);
  const [selected, setSelected] = useState<string>("");
  const [message, setMessage] = useState("");
  const excerpt = useRef<HTMLTextAreaElement>(null);
  const source = useMemo(
    () => sources.find((item) => item.source_id === selected) ?? sources[0],
    [sources, selected],
  );
  const refresh = useCallback(async () => {
    const [nextSources, nextDecisions] = await Promise.all([
      fetchDecisionSources(serveUrl, runId),
      fetchDecisions(serveUrl, runId),
    ]);
    setSources(nextSources);
    setDecisions(nextDecisions);
  }, [serveUrl, runId]);
  useEffect(() => {
    const load = () => void refresh().catch((error) => setMessage(String(error)));
    const initial = window.setTimeout(load, 0);
    const timer = window.setInterval(load, 2000);
    return () => { window.clearTimeout(initial); window.clearInterval(timer); };
  }, [refresh]);
  const enable = async () => {
    try {
      await setDecisionMode(serveUrl, runId, "suggest");
      setMessage("Suggestions enabled for this run.");
    } catch (error) {
      setMessage(String(error));
    }
  };
  const copyHandoffNote = async (decision: DecisionRecord) => {
    const source = sources.find((item) => item.source_id === decision.source_id);
    const excerpt = source?.text ?? "";
    await navigator.clipboard.writeText(
      `Suggested owner: ${decision.suggestion}\nFinding: ${excerpt}\nSource: ${source?.reaction_id ?? "review"} · ${source?.port ?? "review"} handoff · invocation ${source?.invocation_id ?? "unknown"}\nReason: ${decision.reason_code}`,
    );
    setMessage("Handoff note copied. OMAR did not send it anywhere.");
  };
  const evaluate = async () => {
    const input = excerpt.current;
    if (!source || !input) return;
    const start = [...source.text.slice(0, input.selectionStart)].length;
    const end = [...source.text.slice(0, input.selectionEnd)].length;
    if (start === end) {
      setMessage("Select the review excerpt to evaluate.");
      return;
    }
    try {
      await evaluateDecision(serveUrl, runId, {
        request_id: crypto.randomUUID(), profile_id: "review-owner-v1",
        source_id: source.source_id, source_sha256: source.sha256,
        selection: { start, end },
      });
      setMessage("Evaluation queued.");
      await refresh();
    } catch (error) {
      setMessage(String(error));
    }
  };
  if (!capabilities.configured) {
    return <div className="suggestions"><p>Decision support is disabled in this runtime.</p></div>;
  }
  return <div className="suggestions" role="tabpanel">
    <div className="suggestions-heading">
      <b>Suggestions</b><button type="button" onClick={() => void enable()} disabled={!capabilities.key_present}>Enable for this run</button>
    </div>
    <p>Advisory only. OMAR will not route work, message agents, or change this run.</p>
    <p className="suggestions-message">
      Jev {capabilities.model} · {capabilities.key_present ? "provider key available" : "provider key unavailable"}
    </p>
    {!capabilities.key_present ? <p>Add <code>TYPESAFE_API_KEY</code> to the daemon environment before enabling suggestions.</p> : null}
    {sources.length ? <>
      <label>Review output
        <select value={source?.source_id ?? ""} onChange={(event) => setSelected(event.target.value)}>
          {sources.map((item) => <option key={item.source_id} value={item.source_id}>{item.reaction_id} · {item.port}</option>)}
        </select>
      </label>
      <textarea ref={excerpt} defaultValue={source?.text} key={source?.source_id} aria-label="Review excerpt" readOnly />
      <button type="button" onClick={() => void evaluate()}>Evaluate selection</button>
    </> : <p>Waiting for eligible review output.</p>}
    {message ? <p className="suggestions-message">{message}</p> : null}
    {decisions.map((decision) => <article className="suggestion-card" key={decision.decision_id}>
      <b>{decision.status === "suggested" ? `Suggested owner: ${decision.suggestion.replaceAll("_", " ")}` : decision.status.replaceAll("_", " ")}</b>
      <span>{decision.status} · {decision.confidence === null ? "confidence unavailable" : `confidence ${Math.round(decision.confidence * 100)}%`}</span>
      <small>{decision.reason_code} · {decision.coverage.replaceAll("_", " ")} · {decision.freshness}</small>
      {decision.model ? <small>Jev {decision.model}</small> : null}
      {decision.status === "suggested" || decision.status === "needs_review" ? <div>
        <button type="button" onClick={() => void copyHandoffNote(decision)}>Copy handoff note</button>
        <button type="button" onClick={() => void recordDecisionFeedback(serveUrl, runId, decision.decision_id, "useful")}>Helpful</button>
        <button type="button" onClick={() => void recordDecisionFeedback(serveUrl, runId, decision.decision_id, "not_useful")}>Not helpful</button>
        <button type="button" onClick={() => void recordDecisionFeedback(serveUrl, runId, decision.decision_id, "dismissed")}>Dismiss</button>
      </div> : null}
    </article>)}
  </div>;
}
