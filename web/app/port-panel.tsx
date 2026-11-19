"use client";

import { type FormEvent, useState } from "react";
import { parseInputValue, type DiagramSnapshot, type PendingInvocation } from "./lib/protocol";
import { approvalFor, exclusiveOutputs, fieldLabel, nextSteps, prettyValue, responseGuide, type ResponseGuide } from "./lib/response-guide";

export type Answer = (invocation: PendingInvocation, values: Record<string, unknown>, receipt: string) => Promise<void>;

/** The selected workflow step owns its controls. No separate modal or focus trap. */
export function StepResponse({ invocation, snapshot, sending, guide = {}, onAnswer, onCheck, onConfigure }: {
  invocation: PendingInvocation; snapshot: DiagramSnapshot; sending: boolean; guide?: ResponseGuide;
  onAnswer: Answer; onCheck: () => Promise<boolean>; onConfigure: () => void;
}) {
  const copy = responseGuide(invocation, guide);
  const [checking, setChecking] = useState(false);
  const [checkResult, setCheckResult] = useState("");
  async function check() {
    setChecking(true); setCheckResult("");
    try { setCheckResult(await onCheck() ? "This step is still blocked. Complete the setup above, then check again." : "This step is no longer waiting for setup. Check its current status in the workflow."); }
    catch { setCheckResult("Could not check the runtime. Try again when it is connected."); }
    finally { setChecking(false); }
  }
  return <section className="step-response" aria-label={copy.label}>
    <span className="interaction-kind" data-kind={copy.kind}>{copy.label}</span>
    <p className="port-response-summary">{copy.summary}</p>
    <p className="response-required">{copy.required}</p>
    {copy.kind === "setup" ? <>
      <ol className="port-instructions">{copy.instructions.map((line, i) => <li key={i}>{line}</li>)}</ol>
      <div className="response-actions">
        {copy.setupUrl ? <a className="primary-button" href={copy.setupUrl} target="_blank" rel="noopener noreferrer">Open setup</a> : <button type="button" className="primary-button" onClick={onConfigure}>Configure with OMAR</button>}
        <button type="button" className="secondary-button" disabled={checking} onClick={() => void check()}>{checking ? "Checking…" : "Check status"}</button>
      </div>
      <p className="response-next">Checking reads the runtime status. The step continues only when its requirements are resolved.</p>
      {checkResult ? <p role="status">{checkResult}</p> : null}
    </> : copy.kind === "approval" ? <Approval invocation={invocation} snapshot={snapshot} sending={sending} guide={guide} onAnswer={onAnswer} onConfigure={onConfigure} />
      : <ResponseFields invocation={invocation} snapshot={snapshot} sending={sending} guide={guide} onAnswer={onAnswer} />}
    <details className="port-details"><summary>Full instructions</summary><p className="port-waiting-prompt">{invocation.prompt}</p></details>
    <details className="port-details"><summary>Technical details</summary>
      <p>Step: <code>{invocation.agent}</code></p>
      {Object.entries(invocation.trigger_values).map(([name, value]) => <div className="port-row" key={name}><span className="port-row-name">{name}</span><pre className="port-row-value">{prettyValue(value)}</pre></div>)}
      <p className="port-waiting-contract">{invocation.contract}</p>
    </details>
  </section>;
}

function Approval({ invocation, snapshot, sending, guide, onAnswer, onConfigure }: {
  invocation: PendingInvocation; snapshot: DiagramSnapshot; sending: boolean; guide: ResponseGuide; onAnswer: Answer; onConfigure: () => void;
}) {
  const binding = approvalFor(invocation, guide);
  const [revising, setRevising] = useState(false);
  const [feedback, setFeedback] = useState("");
  const [error, setError] = useState("");
  const [submitted, setSubmitted] = useState(false);
  if (!binding) return <div className="response-configuration"><p>This approval needs a reviewable result and valid decision outputs before it can be submitted.</p><button type="button" className="secondary-button" onClick={onConfigure}>Configure approval</button></div>;
  const revisions = nextSteps(snapshot, binding.changes);
  const following = nextSteps(snapshot, binding.approve);
  async function decide(changes: boolean) {
    if (!binding || sending || submitted || (changes && (!feedback.trim() || !revisions.length))) return;
    setError("");
    const port = changes ? binding.changes : binding.approve;
    const receipt = changes ? `Changes requested. Feedback sent to ${revisions.join(", ")}.`
      : following.length ? `Approved. Result sent to ${following.join(", ")}.` : "Approved. The result was accepted by the workflow.";
    try { await onAnswer(invocation, { [port]: changes ? feedback.trim() : binding.value }, receipt); setSubmitted(true); }
    catch (cause) { setError(`${cause instanceof Error ? cause.message : "Submission failed."} Your feedback has been kept.`); }
  }
  return <div className="approval-response">
    <h4>Result to review</h4>
    <div className="approval-artifact">{prettyValue(binding.value)}</div>
    <p className="response-next">{following.length ? `Approval sends this result to ${following.join(", ")}.` : "Approval accepts this result as the workflow output."}</p>
    {revising ? <div className="revision-feedback">
      <label className="port-field"><span className="port-field-name">What should change?</span><textarea autoFocus rows={3} value={feedback} onChange={event => setFeedback(event.target.value)} disabled={sending || submitted} placeholder="Describe the changes you need…" /></label>
      <p className="response-next">Your feedback goes to {revisions.join(", ")}.</p>
      <div className="response-actions"><button type="button" className="primary-button" disabled={sending || submitted || !feedback.trim()} onClick={() => void decide(true)}>{sending ? "Sending…" : "Send change request"}</button><button type="button" className="secondary-button" disabled={sending || submitted} onClick={() => setRevising(false)}>Cancel</button></div>
    </div> : <div className="response-actions">
      <button type="button" className="primary-button" disabled={sending || submitted} onClick={() => void decide(false)}>{sending ? "Submitting…" : "Approve"}</button>
      <button type="button" className="secondary-button" disabled={sending || submitted || !revisions.length} onClick={() => setRevising(true)}>Request changes</button>
    </div>}
    {!revisions.length ? <p className="response-next">This workflow has no revision path. <button type="button" className="link-button" onClick={onConfigure}>Configure a revision step</button> to enable change requests.</p> : null}
    {error ? <p className="port-field-problem" role="alert">{error}</p> : null}
  </div>;
}

function ResponseFields({ invocation, snapshot, sending, guide, onAnswer }: {
  invocation: PendingInvocation; snapshot: DiagramSnapshot; sending: boolean; guide: ResponseGuide; onAnswer: Answer;
}) {
  const copy = responseGuide(invocation, guide);
  const alternatives = exclusiveOutputs(invocation);
  const [selected, setSelected] = useState(alternatives?.find(p => p !== copy.blocked) || "");
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
    let invalid = parsed === undefined;
    if (copy.outcomes[port]?.format === "json") { try { JSON.parse(text); } catch { invalid = true; } }
    if (invalid) bad.push(port); else values[port] = parsed;
  }
  const valid = !bad.length && Object.keys(values).length > 0 && (!alternatives || Object.keys(values).length === 1);
  const button = copy.outcomes[selected]?.button || (selected === copy.blocked ? "Report blocker" : copy.kind === "input" ? "Save and continue" : "Submit result and continue");
  async function submit(event: FormEvent) {
    event.preventDefault(); if (!valid || sending || submitted) return;
    setError("");
    const destinations = [...new Set(Object.keys(values).flatMap(port => nextSteps(snapshot, port)))];
    const receipt = `${copy.kind === "input" ? "Information saved" : "Response accepted"}.${destinations.length ? ` Sent to ${destinations.join(", ")}.` : " The workflow received your result."}`;
    try { await onAnswer(invocation, values, receipt); setSubmitted(true); }
    catch (cause) { setError(`${cause instanceof Error ? cause.message : "Submission failed."} Your response has been kept.`); }
  }
  return <form className="port-waiting" onSubmit={submit}>
    {copy.kind === "manual" ? <ol className="port-instructions">{copy.instructions.map((line, i) => <li key={i}>{line}</li>)}</ol> : null}
    {alternatives ? <fieldset className="port-outcomes"><legend>What are you submitting?</legend>{alternatives.map(port => <label key={port}>
      <input type="radio" name={`outcome-${invocation.task_id}`} value={port} checked={selected === port} disabled={sending || submitted} onChange={() => { setSelected(port); setError(""); }} />{copy.outcomes[port]?.label || fieldLabel(port)}
    </label>)}</fieldset> : null}
    {effects.map(([port, type]) => <label className={`port-field${bad.includes(port) ? " invalid" : ""}`} key={port}>
      <span className="port-field-name">{copy.outcomes[port]?.label || fieldLabel(port)}</span>
      <span className="port-field-help">{copy.outcomes[port]?.help || (type === "signal" ? "Check to send this signal." : `Enter the requested ${type === "string" ? "text" : type} value.`)}</span>
      {type === "signal" ? <input type="checkbox" checked={!!signals[port]} onChange={e => setSignals(v => ({ ...v, [port]: e.target.checked }))} disabled={sending || submitted} /> : <textarea rows={3} value={draft[port] ?? ""} disabled={sending || submitted} onChange={e => { setDraft(v => ({ ...v, [port]: e.target.value })); setError(""); }} aria-invalid={bad.includes(port) || undefined} placeholder="Enter your response…" />}
      {bad.includes(port) ? <span className="port-field-problem" role="alert">{copy.outcomes[port]?.format === "json" ? "Enter valid JSON." : `Enter a valid ${type} value.`}</span> : null}
    </label>)}
    <p className="response-next">{copy.next}</p>
    {error ? <p role="alert" className="port-field-problem">{error}</p> : null}
    <button className="primary-button" type="submit" disabled={sending || submitted || !valid}>{sending ? "Submitting…" : button}</button>
  </form>;
}
