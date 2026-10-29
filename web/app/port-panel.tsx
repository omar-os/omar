"use client";

import { type FormEvent, useEffect, useId, useMemo, useRef, useState } from "react";
import { formatDuration, parseInputValue, type DiagramSnapshot, type PendingInvocation } from "./lib/protocol";
import { exclusiveOutputs, fieldLabel, prettyValue, responseGuide, type ResponseGuide } from "./lib/response-guide";

type Answer = (invocation: PendingInvocation, values: Record<string, unknown>) => Promise<void>;
export function PortPanel({ agent, snapshot, pending, sending, guides = {}, onAnswer, onClose }: {
  agent: string; snapshot: DiagramSnapshot; pending: PendingInvocation[]; sending: boolean;
  guides?: Record<string, ResponseGuide>; onAnswer: Answer; onClose: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    const element = dialog.current;
    element?.showModal();
    return () => { element?.close(); if (previous?.isConnected) previous.focus(); };
  }, []);
  const wiring = useMemo(() => {
    const id = snapshot.agents.find(a => a.name === agent)?.id;
    const mine = snapshot.reactions.filter(r => r.agent === id);
    const ports = (ids: string[]) => snapshot.ports.filter(p => ids.includes(p.id));
    return { reads: ports(mine.flatMap(r => r.triggers)), writes: ports(mine.flatMap(r => r.effects)) };
  }, [agent, snapshot]);
  const first = pending[0] ? responseGuide(pending[0], guides[agent]) : null;
  return (
    <dialog ref={dialog} className="port-panel" aria-labelledby={titleId} onCancel={onClose}>
      <header className="port-panel-head">
        <div><span className="eyebrow">{first ? "NEEDS YOUR RESPONSE" : "STEP DETAILS"}</span><h2 id={titleId}>{first?.title || "No response needed"}</h2></div>
        <button type="button" className="port-panel-close" onClick={onClose} autoFocus>Close</button>
      </header>
      <div className="port-panel-body">
        {pending.map(invocation => <Waiting key={invocation.task_id} invocation={invocation} sending={sending} guide={guides[agent]} onAnswer={onAnswer} />)}
        {!pending.length ? <p className="port-panel-idle">This step is not waiting for a response. You can close this panel.</p> : null}
        <details className="port-details"><summary>Technical details</summary>
          <p>Step: <code>{agent}</code></p>
          <PortList label="Input values" ports={wiring.reads} /><PortList label="Output values" ports={wiring.writes} />
        </details>
      </div>
    </dialog>
  );
}
function PortList({ label, ports }: { label: string; ports: DiagramSnapshot["ports"] }) {
  if (!ports.length) return null;
  return <div className="port-list"><h3>{label}</h3>{ports.map(port => <div key={port.id} className="port-row">
    <span className="port-row-name">{port.name}</span><span className="port-row-type">{port.type}</span>
    <pre className="port-row-value">{port.value === null ? "—" : prettyValue(port.value)}</pre>
    <span className="port-row-tag">{port.last_tag ? `${formatDuration(port.last_tag.timestamp)}:${port.last_tag.microstep}` : ""}</span>
  </div>)}</div>;
}
function Waiting({ invocation, sending, guide, onAnswer }: { invocation: PendingInvocation; sending: boolean; guide?: ResponseGuide; onAnswer: Answer }) {
  const copy = responseGuide(invocation, guide);
  const alternatives = exclusiveOutputs(invocation);
  const [selected, setSelected] = useState(copy.setupOnly ? copy.blocked! : alternatives?.find(p => p !== copy.blocked) || "");
  const [draft, setDraft] = useState<Record<string, string>>({});
  const [signals, setSignals] = useState<Record<string, boolean>>({});
  const [error, setError] = useState("");
  const [submitted, setSubmitted] = useState(false);
  const effects = Object.entries(invocation.allowed_effects).filter(([port]) => !alternatives || port === selected);
  const bad: string[] = [];
  const values: Record<string, unknown> = {};
  for (const [port, type] of effects) {
    if (type === "signal") { if (signals[port]) values[port] = null; continue; }
    const text = draft[port] ?? "";
    if (!text.trim()) continue;
    const parsed = parseInputValue(type, text);
    const expectsJson = copy.outcomes[port]?.format === "json" || (copy.setupOnly && port === copy.blocked);
    let invalid = parsed === undefined;
    if (expectsJson) { try { JSON.parse(text); } catch { invalid = true; } }
    if (invalid) bad.push(port); else values[port] = parsed;
  }
  const valid = !bad.length && (!alternatives || Object.keys(values).length === 1)
    && (!copy.setupOnly || (Object.hasOwn(values, copy.blocked!) && Object.keys(values).length === 1));
  const blockedSelected = selected === copy.blocked;
  const button = copy.setupOnly ? "Report setup blocker" : copy.outcomes[selected]?.button || (blockedSelected ? "Report blocker" : alternatives ? `Submit ${fieldLabel(selected).toLowerCase()} & continue` : "Submit response & continue");
  async function submit(event: FormEvent) {
    event.preventDefault(); if (!valid || sending || submitted) return;
    setError("");
    try { await onAnswer(invocation, values); setSubmitted(true); setDraft({}); }
    catch (cause) {
      const message = cause instanceof Error ? cause.message : "Submission failed. Your response has been kept.";
      setError(message.includes("effect contract") && message.includes("not satisfied") ? "This step still needs a required output. Complete the remaining fields and try again. Your response has been kept." : message);
    }
  }
  return <form className="port-waiting" onSubmit={submit}>
    <p className="port-response-summary">{copy.summary}</p>
    <ol className="port-instructions">{copy.instructions.map((line, i) => <li key={i}>{line}</li>)}</ol>
    <details className="port-details"><summary>Full instructions</summary><p className="port-waiting-prompt">{invocation.prompt}</p></details>
    {alternatives ? <fieldset className="port-outcomes"><legend>What are you submitting?</legend>{alternatives.map(port => <label key={port}>
      <input type="radio" name={`outcome-${invocation.task_id}`} value={port} checked={selected === port} disabled={sending || (copy.setupOnly && port !== copy.blocked)} onChange={() => { setSelected(port); setError(""); }} />
      {copy.outcomes[port]?.label || fieldLabel(port)}
    </label>)}</fieldset> : null}
    {effects.map(([port, type]) => <label className={`port-field${bad.includes(port) ? " invalid" : ""}`} key={port}>
      <span className="port-field-name">{copy.outcomes[port]?.label || fieldLabel(port)}</span>
      <span className="port-field-help">{copy.outcomes[port]?.help || (copy.setupOnly ? "Paste a JSON failure report describing the missing requirements." : type === "signal" ? "Check to send this signal." : `Enter the requested ${type === "string" ? "text" : type} value.`)}</span>
      {type === "signal" ? <input type="checkbox" checked={!!signals[port]} onChange={e => setSignals(v => ({ ...v, [port]: e.target.checked }))} disabled={sending} /> :
        <textarea rows={4} value={draft[port] ?? ""} disabled={sending || submitted} onChange={e => { setDraft(v => ({ ...v, [port]: e.target.value })); setError(""); }} aria-invalid={bad.includes(port) || undefined} placeholder={copy.setupOnly ? '{"status":"blocked","reason":"Describe the missing requirements"}' : "Paste your response here…"} />}
      {bad.includes(port) ? <span className="port-field-problem" role="alert">{copy.outcomes[port]?.format === "json" || copy.setupOnly ? "Enter valid JSON." : `Enter a valid ${type} value.`}</span> : null}
    </label>)}
    <div className="port-response-footer"><p>{copy.next}</p>
      {error ? <p role="alert" className="port-field-problem">{error}</p> : null}
      {submitted ? <p role="status">Response accepted.</p> : null}
      <button className="primary-button" type="submit" disabled={sending || submitted || !valid}>{sending ? "Submitting…" : button}</button>
    </div>
    <details className="port-details"><summary>Connection rules</summary><p className="port-waiting-contract">{invocation.contract}</p></details>
  </form>;
}
