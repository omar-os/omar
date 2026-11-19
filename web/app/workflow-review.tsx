"use client";

import { useEffect, useRef, useState } from "react";
import type { ChatMessage, ModelCatalog, ProposedDesign, WorkflowPlan } from "./lib/protocol-generated";
import { normalizeRuntimeUrl } from "./lib/runtime-client";

export function workflowReady(plan: WorkflowPlan): boolean {
  return plan.review.confirmed && !plan.review.needs_redesign && plan.questions.length === 0
    && plan.assumptions.every(a => plan.review.accepted_assumptions.includes(a.id));
}

export function WorkflowReview({ design, sequence, source, url, disabled, onSaved, onDirty, onAsk, onStep }: {
  design: ProposedDesign; sequence: number; source: string; url: string; disabled: boolean;
  onSaved: (message: ChatMessage) => void; onDirty: (dirty: boolean) => void;
  onAsk: (text: string) => void; onStep: (reaction: string) => void;
}) {
  const plan = design.workflow!;
  const [catalog, setCatalog] = useState<ModelCatalog | null>(null);
  const [answers, setAnswers] = useState(Object.fromEntries(plan.questions.map(q => [q.id, plan.review.answers[q.id] ?? ""])));
  const [accepted, setAccepted] = useState(plan.review.accepted_assumptions);
  const [choices, setChoices] = useState(Object.fromEntries(Object.entries(plan.choices).map(([id, c]) => [id, c.profile_id])));
  const [confirmed, setConfirmed] = useState(plan.review.confirmed);
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const requests = useRef<AbortController | null>(null);
  useEffect(() => {
    const controller = new AbortController(); requests.current = controller;
    fetch(`${normalizeRuntimeUrl(url)}/v1/workflow/catalog`, { signal: controller.signal })
      .then(async r => { const value = await r.json(); if (!r.ok) throw Error(value.error); return value as ModelCatalog; })
      .then(setCatalog).catch(e => { if (!controller.signal.aborted) setError(String(e)); });
    return () => controller.abort();
  }, [url]);
  useEffect(() => { onDirty(dirty || busy); return () => onDirty(false); }, [dirty, busy, onDirty]);
  function changed() { setDirty(true); setConfirmed(false); }
  async function save() {
    setBusy(true); setError("");
    const signal = requests.current?.signal;
    try {
      const response = await fetch(`${normalizeRuntimeUrl(url)}/v1/workflow/review`, {
        method: "POST", headers: { "content-type": "application/json" }, signal,
        body: JSON.stringify({ sequence, program: source, files: design.files ?? {}, inputs: design.inputs,
          answers, accepted_assumptions: accepted, choices, confirmed }),
      });
      const message = await response.json();
      if (!response.ok) throw Error(message.error);
      if (!signal?.aborted) { setDirty(false); onSaved(message); }
    } catch (e) { if (!signal?.aborted) setError(e instanceof Error ? e.message : String(e)); }
    finally { if (!signal?.aborted) setBusy(false); }
  }
  return <section className="workflow-review" aria-label="Workflow review">
    <header><strong>Workflow review</strong><span>Revision {sequence}</span></header>
    <p>{plan.outcome}</p>
    <details><summary>Research and success criteria</summary>
      {plan.constraints.length > 0 && <ul>{plan.constraints.map((c, i) => <li key={i}>{c}</li>)}</ul>}
      {plan.sources.map((s, i) => <p key={i}><strong>{s.location}</strong>: {s.finding}<br /><small>{s.limitation}</small></p>)}
      {plan.research_gaps.map((g, i) => <p key={i}>Research gap: {g}</p>)}
      {plan.steps.map(step => <div key={step.reaction}><button type="button" className="secondary-button" onClick={() => onStep(step.reaction)}>{step.purpose}</button><p>{step.task} · {step.complexity}</p><ul>{step.success_criteria.map((c, i) => <li key={i}>{c}</li>)}</ul></div>)}
      <small>These criteria guide review. The existing compiler and runtime checks still apply; semantic quality needs evaluation.</small>
    </details>
    <fieldset disabled={disabled || busy}>
      {plan.assumptions.map(a => <label key={a.id}><input type="checkbox" checked={accepted.includes(a.id)} onChange={e => { setAccepted(e.target.checked ? [...accepted, a.id] : accepted.filter(id => id !== a.id)); changed(); }} />{a.text}<small>Proposed assumption — confirm or revise with OMAR.</small></label>)}
      {plan.questions.map(q => <label key={q.id}>{q.text}<input value={answers[q.id] ?? ""} onChange={e => { setAnswers({ ...answers, [q.id]: e.target.value }); changed(); }} />{q.reactions.map(r => <button type="button" key={r} onClick={() => onStep(r)}>View affected step</button>)}</label>)}
      <details><summary>Agents and model choices</summary>
        <p>One runtime agent uses one model across its steps. All choices are provisional; published benchmarks do not prove local task quality.</p>
        {Object.entries(plan.choices).map(([agent, choice]) => <label key={agent}>{agent}
          <select aria-label={`Model for ${agent}`} value={choices[agent]} disabled={!catalog} onChange={e => { setChoices({ ...choices, [agent]: e.target.value }); changed(); }}>
            {(catalog?.profiles ?? Object.values(plan.profiles)).filter(p => p.backend === plan.profiles[agent]?.backend).map(p => <option key={p.id} value={p.id}>{p.model}{p.effort ? ` · ${p.effort}` : ""} · {p.tier}</option>)}
          </select><small>{choice.reason}</small>
          {choice.evidence_ids.map(id => { const e = catalog?.evidence.find(e => e.id === id); return e ? <small key={id}><a href={e.source} target="_blank" rel="noreferrer">Published source</a>: {e.finding}<br />{Date.now() / 1000 >= e.expires_at ? "Expired" : "Candidate evidence"}. {e.limitations}</small> : <small key={id}>Evidence {id} is unavailable.</small>; })}
        </label>)}
        <small>These are requested settings. Provider-reported identity, access and actual cost are not verified here.</small>
      </details>
      <label><input type="checkbox" checked={confirmed} onChange={e => { setConfirmed(e.target.checked); setDirty(true); }} />I reviewed the workflow and model choices</label>
      <div className="workflow-review-actions"><button type="button" className="primary-button" onClick={() => void save()}>{busy ? "Saving…" : "Save review"}</button>
        {plan.review.needs_redesign && <button type="button" className="secondary-button" disabled={dirty} onClick={() => onAsk(`Update this workflow using my saved answers. Revise the executable workflow and remove resolved questions. Saved answers: ${JSON.stringify(plan.review.answers)}`)}>Update workflow from answers</button>}
      </div>
    </fieldset>
    {plan.review.needs_redesign && <p role="status">Answers saved. Update the workflow before running.</p>}
    {!plan.review.needs_redesign && workflowReady(plan) && !dirty && <p role="status">Review saved. Ready for confirmation.</p>}
    {error && <p role="alert">{error}</p>}
  </section>;
}
