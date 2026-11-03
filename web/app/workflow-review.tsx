"use client";

import { NativeAccessEvidence } from "./native-access-evidence";

import { useEffect, useRef, useState } from "react";
import type { CapabilityEvidence, ExecutionProfile, ExecutionReceipt, ResearchUsage, StatementOrigin, WorkflowPlanRevision } from "./lib/protocol-generated";
import { fetchCapabilityEvidence, fetchExecutionProfiles, fetchExecutionReceipt, fetchWorkflowPlan, saveWorkflowPlan } from "./lib/workflow-client";
import { eligibleExecutionProfiles } from "./lib/execution-choices";

function Origin({value}: {value?: StatementOrigin | null}) {
  if (!value) return <small>Provenance not yet classified</small>;
  if (value.kind === "system_proposed") return <small>System proposed · {value.reason}</small>;
  if (value.kind === "user_stated") return <small>User stated · message {value.message_sequence}: “{value.quote}”</small>;
  if (value.kind === "user_confirmed") return <small>User confirmed · saved review revision {value.confirmation.plan.revision}</small>;
  return <small>Source observed · input {value.input_port}{value.json_pointer}{value.source_reported_date ? ` · source-reported date ${value.source_reported_date}` : ""}{value.capture ? " · matched to a recorded research read" : ""}<br />“{value.quote}”</small>;
}

function ResearchDetails({usage}: {usage?: ResearchUsage | null}) {
  const stops = {proposal_submitted:"Proposal submitted",sufficient_evidence:"Enough evidence collected",needs_user_input:"Needs your input",missing_capability:"Required access is unavailable",call_limit:"Lookup limit reached",context_limit:"Evidence size limit reached",time_limit:"Research time limit reached",clock_changed:"System clock changed",interrupted:"Research was interrupted",superseded:"Replaced by a newer request"};
  return <details><summary>Research used</summary>{usage ? <>
    <p>{usage.stop_reason ? stops[usage.stop_reason] : "Research in progress"}. Catalog reads: {usage.catalog_calls}/{usage.limits.catalog_calls}. Source reads: {usage.source_calls}/{usage.limits.source_calls}.</p>
    <p>Captured evidence: {usage.evidence_bytes.toLocaleString()} of {usage.limits.evidence_bytes.toLocaleString()} bytes. Time allowance: {Math.round(usage.limits.wall_time_ms / 1000)} seconds.</p>
    <p>These limits cover reads through OMAR’s research tool. The designer’s other tools and model usage are outside this measurement. Designer cost is unknown.</p>
    <p>Additional research adapter charges: ${usage.adapter_cost_usd.toFixed(2)}.{!usage.external_retrieval_supported ? " External retrieval is unavailable through this tool; research uses supplied context and configured catalogs." : ""}</p>
    {usage.captures.map(capture => <p key={capture.request_id}><strong>{capture.location}</strong><br /><small>Captured {new Date(capture.captured_at_ms).toISOString()} · {capture.evidence_bytes.toLocaleString()} bytes{capture.truncated ? ` · partial source (${capture.full_content_bytes.toLocaleString()} bytes total, starting at byte ${capture.offset})` : ""}</small></p>)}
    {usage.errors.map((error,index) => <p key={index}>Research gap: {error}</p>)}
  </> : <p>No metered research record is attached to this design. Research usage and designer cost are unknown.</p>}</details>;
}

export function WorkflowReview({ url, id, source, inputs, disabled, onPlan, onReady, onRestore, onRefine, onDirty, onCatalog, onInspectStep }: {
  url: string; id: string; source: string; inputs: Record<string, unknown>; disabled: boolean;
  onPlan: (plan: WorkflowPlanRevision) => void; onReady: (ready: boolean) => void;
  onRestore: (program: string, inputs: Record<string, unknown>) => void; onRefine: () => void;
  onDirty: (dirty: boolean) => void; onCatalog: (catalog: {profiles: ExecutionProfile[]; evidence: CapabilityEvidence[]; evidenceRevision: string}) => void;
  onInspectStep: (reaction: string) => void;
}) {
  const [plan, setPlan] = useState<WorkflowPlanRevision | null>(null);
  const [profiles, setProfiles] = useState<ExecutionProfile[]>([]);
  const [answers, setAnswers] = useState<Record<string, string>>({});
  const [accepted, setAccepted] = useState<string[]>([]);
  const [overrides, setOverrides] = useState<Record<string, string>>({});
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const mounted = useRef(false);
  const callbacks = useRef({ onPlan, onReady, onRestore, onCatalog });
  useEffect(() => { callbacks.current = { onPlan, onReady, onRestore, onCatalog }; });
  useEffect(() => { onDirty(dirty || busy); }, [dirty, busy, onDirty]);
  useEffect(() => {
    mounted.current = true;
    const abort = new AbortController();
    callbacks.current.onReady(false);
    Promise.all([fetchWorkflowPlan(url, id, abort.signal), fetchExecutionProfiles(url, abort.signal), fetchCapabilityEvidence(url, abort.signal)])
      .then(([saved, catalog, evidence]) => {
        if (abort.signal.aborted) return;
        setPlan(saved); setProfiles(catalog.profiles);
        callbacks.current.onCatalog({profiles:catalog.profiles, evidence:evidence.evidence, evidenceRevision:evidence.revision});
        setAnswers(Object.fromEntries(saved.draft.questions.map(q => [q.id, q.answer ?? ""])));
        setAccepted(saved.draft.intent.assumptions.filter(a => a.accepted).map(a => a.id));
        setOverrides(saved.draft.preferences.overrides);
        callbacks.current.onPlan(saved); callbacks.current.onRestore(saved.program, saved.inputs);
      }).catch(cause => { if (!abort.signal.aborted) setError(String(cause.message ?? cause)); });
    return () => { mounted.current=false; abort.abort(); };
  }, [url, id]);
  const unresolvedResearch = plan?.draft.research.issues?.some(issue => issue.blocking && !issue.resolution) ?? false;
  const unlinkedSources = plan?.draft.steps.some(step => (step.output_checks ?? []).some(check => check.kind === "grounded_brief" && check.allowed_source_ids.some(id => {
    const source = plan.draft.research.sources.find(source => source.id === id);
    return !source?.origin || source.origin.kind === "system_proposed";
  }))) ?? false;
  const incompleteContracts = plan?.draft.steps.some(step => !step.logical_id || !step.interface || (plan.draft.steps.length > 1 && !step.decomposition_reason?.trim())) ?? false;
  const ready = !!plan && !dirty && !busy && !unresolvedResearch && !unlinkedSources && !incompleteContracts && plan.draft.intent.assumptions.every(a => a.accepted)
    && plan.draft.questions.every(q => !q.required && !q.answer) && plan.selections.every(s => s.profile_id !== null && !s.capability_blockers?.length);
  useEffect(() => { onReady(ready); }, [ready, onReady]);

  async function save() {
    if (!plan || busy) return;
    setBusy(true); setError("");
    try {
      const saved = await saveWorkflowPlan(url, plan, source, inputs, { answers: Object.fromEntries(Object.entries(answers).filter(([,v]) => v.trim())), accepted_assumptions: accepted, overrides });
      if (!mounted.current) return;
      setPlan(saved); onPlan(saved); setDirty(false);
    } catch (cause) { if (mounted.current) setError(cause instanceof Error ? cause.message : String(cause)); }
    finally { if (mounted.current) setBusy(false); }
  }

  return <section className="workflow-review" aria-label="Workflow review">
    <div className="workflow-review-heading"><strong>Workflow review</strong><span>{plan ? `Revision ${plan.revision}` : "Loading…"}</span></div>
    {error ? <p role="alert">{error}</p> : null}
    {plan ? <>
      <p>{plan.draft.intent.outcome}</p>
      <details><summary>What a good result looks like</summary><ul>{plan.draft.intent.acceptance_criteria.map((criterion,i) => <li key={i}>{criterion}</li>)}</ul><p>{plan.draft.research.summary}</p>
        {(plan.draft.intent.requirements ?? []).map(r => <p key={r.id}><strong>Requirement:</strong> {r.text}<br /><Origin value={r.origin} /></p>)}
        {plan.draft.research.sources.map(s => <p key={s.id}><strong>{s.id}</strong>: {s.finding}<br /><small>{s.location} · {s.provenance}</small><br /><Origin value={s.origin} /></p>)}
        {plan.draft.research.sources.length ? <small>Recorded with this revision on {new Date(plan.created_at * 1000).toISOString().slice(0,10)}. Source links do not establish factual support by themselves.</small> : null}
        {plan.draft.research.gaps.map((gap,i) => <p key={i}>Open question: {gap}</p>)}
      </details>
      <ResearchDetails usage={plan.draft.research.usage} />
      {incompleteContracts ? <p role="status">Update this design to explain the steps and confirm their data handoffs before running.</p> : null}
      {incompleteContracts && !disabled ? <button type="button" className="secondary-button" disabled={dirty || busy} onClick={onRefine}>Complete the workflow review with OMAR</button> : null}
      {(plan.draft.research.issues ?? []).map(issue => <div className="workflow-question" key={issue.id}><strong>{issue.kind === "contradiction" ? "Conflicting evidence" : "Missing information"}{issue.blocking && !issue.resolution ? " · needs a decision" : ""}</strong><p>{issue.description}</p>{issue.source_ids.length ? <small>Sources: {issue.source_ids.join(", ")}</small> : null}{issue.resolution ? <p>Decision: {issue.resolution.text}<br /><Origin value={issue.resolution.origin} /></p> : <small>Unresolved</small>}</div>)}
      {unlinkedSources ? <p role="status">Update the research to link the sources used by the output checks before running.</p> : null}
      {unresolvedResearch && !disabled ? <button type="button" className="secondary-button" disabled={dirty || busy} onClick={onRefine}>Resolve research issues with OMAR</button> : null}
      {plan.draft.intent.assumptions.length ? <fieldset disabled={disabled || busy}><legend>Check these assumptions</legend>{plan.draft.intent.assumptions.map(a => <label key={a.id} className="workflow-assumption"><input type="checkbox" checked={accepted.includes(a.id)} onChange={e => { setAccepted(current => e.target.checked ? [...current, a.id] : current.filter(id => id !== a.id)); setDirty(true); }} /> <span>{a.text}{a.accepted && a.confirmation ? <small>User confirmed · saved review revision {a.confirmation.plan.revision}</small> : <Origin value={a.origin} />}</span></label>)}</fieldset> : null}
      {plan.draft.questions.map(q => <div key={q.id} className="workflow-question"><label htmlFor={`${id}-${q.id}-answer`}>{q.question}{q.required ? " *" : ""}</label><small>{q.reason}</small><input id={`${id}-${q.id}-answer`} value={answers[q.id] ?? ""} disabled={disabled || busy} onChange={e => { setAnswers({...answers, [q.id]:e.target.value}); setDirty(true); }} placeholder={q.proposed_default ? `Suggested: ${q.proposed_default}` : "Your answer"} list={`${id}-${q.id}`} /><datalist id={`${id}-${q.id}`}>{q.options.map(option => <option key={option} value={option} />)}</datalist>{q.target_reactions?.length ? <div className="workflow-review-actions">{q.target_reactions.map(reaction => <button key={reaction} type="button" className="secondary-button" onClick={() => onInspectStep(reaction)}>View affected step: {plan.draft.steps.find(step => step.reaction === reaction)?.purpose ?? reaction}</button>)}</div> : null}</div>)}
      <details><summary>Agents and model choices</summary>{plan.selections.map(s => <label className="workflow-question" key={s.agent}>{s.agent}<select value={overrides[s.agent] ?? s.profile_id ?? ""} disabled={disabled || busy} onChange={e => { setOverrides({...overrides,[s.agent]:e.target.value}); setDirty(true); }}><option value="">Choose a model profile</option>{eligibleExecutionProfiles(plan, s.agent, profiles).map(p => <option key={p.id} value={p.id}>{p.model}{p.effort ? ` · ${p.effort}` : ""} · {p.tier}</option>)}</select><small>{s.reason}</small>{s.capability_blockers?.map(blocker => <small className="step-config-error" key={blocker}>{blocker}</small>)}</label>)}</details>
      {!disabled ? <div className="workflow-review-actions"><button type="button" className="secondary-button" disabled={busy || (!dirty && source === plan.program)} onClick={() => void save()}>{busy ? "Saving…" : "Save review"}</button><button type="button" className="secondary-button" disabled={busy || dirty || source !== plan.program} onClick={() => void save()}>Refresh model choices</button>{plan.draft.questions.some(q => q.answer) ? <button type="button" className="secondary-button" disabled={dirty || busy} onClick={onRefine}>Update workflow from answers</button> : null}<small>{dirty ? "Save your changes before running." : ready ? "Review saved." : "Resolve the questions, assumptions, model choices and required tool access."}</small></div> : null}
    </> : null}
  </section>;
}

export function WorkflowResult({ url, runId, status }: {url:string; runId:string; status:string}) {
  const [receipt, setReceipt] = useState<ExecutionReceipt | null>(null);
  const [error,setError] = useState("");
  useEffect(() => {
    const controller = new AbortController();
    fetchExecutionReceipt(url, runId, controller.signal).then(value => { if (!controller.signal.aborted) { setReceipt(value);setError(""); } }).catch(cause => { if (!controller.signal.aborted) setError(String(cause.message ?? cause)); });
    return () => controller.abort();
  }, [url,runId,status]);
  return <details className="workflow-review"><summary>Execution evidence</summary>{error ? <p role="alert">{error}</p> : null}{receipt ? <><p>Run {receipt.status} · source {receipt.source_digest.slice(0,12)}{receipt.plan ? ` · revision ${receipt.plan.revision}` : ""}</p>{Object.entries(receipt.profiles).map(([agent,p]) => <p key={agent}>{agent}: requested {p.model}{p.effort ? ` (${p.effort})` : ""}<br /><small>Provider-reported model: {receipt.observed_models[agent] ?? "unavailable"}</small></p>)}<ul>{(receipt.verification ?? []).map((event, index) => <li key={`${event.reaction}:${index}`}>{event.step}: {event.stage.replaceAll("_", " ")} → {event.decision.replaceAll("_", " ")}{event.validation_error ? <small> · {event.validation_error}</small> : null}</li>)}</ul>{(receipt.repository_checks ?? []).length ? <details><summary>Repository regression checks</summary><p>These checks run before a patch is handed off. Passing covers the declared test suite; it does not establish overall code quality.</p>{(receipt.repository_checks ?? []).map((check, index) => <p key={`${check.invocation_id}:${index}`}>{check.reaction}: {check.status.replaceAll("_", " ")} · {check.test_count} declared cases<br /><small>Suite {check.verifier_id} · {check.verifier_digest.slice(0,12)} · {check.elapsed_ms} ms</small></p>)}</details> : null}<details><summary>Invocation payload measurements</summary><p>These byte counts cover declared payloads. Native session history, tools, provider tokens and cost remain outside this measurement.</p>{(receipt.resources ?? []).map(usage => <p key={usage.invocation_id}>{usage.logical_step_id || usage.reaction}: {usage.invoked ? "invoked" : "stopped before invocation"}<br /><small>Input {usage.input_bytes.toLocaleString()}/{usage.limits.max_input_bytes.toLocaleString()} bytes · prompt {usage.prompt_bytes.toLocaleString()}/{usage.limits.max_prompt_bytes.toLocaleString()} bytes · raw output {usage.raw_output_bytes?.toLocaleString() ?? "unknown"}/{usage.limits.max_output_bytes.toLocaleString()} bytes · returned payload {usage.returned_output_bytes?.toLocaleString() ?? "none"} bytes</small></p>)}</details><NativeAccessEvidence key={runId} url={url} runId={runId} agents={Object.keys(receipt.profiles)} /><p>Cost: {receipt.cost_usd === null ? "not reported" : `$${receipt.cost_usd.toFixed(4)}`}. Task quality requires the declared output checks.</p>{receipt.error ? <p role="alert">{receipt.error}</p> : null}</> : <p>Loading execution record…</p>}</details>;
}
