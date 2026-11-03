"use client";

import { useEffect, useState } from "react";
import type { CapabilityEvidence, ExecutionProfile, WorkflowPlanRevision } from "./lib/protocol-generated";
import { eligibleExecutionProfiles } from "./lib/execution-choices";
import { BenchmarkEvidenceDetails } from "./benchmark-evidence-details";
import { ExecutionProfileDetails } from "./execution-profile-details";

export function StepExecutionChoice({ plan, agent, backend, catalog, evidence, evidenceRevision, disabled, onApply, onPending }: {
  plan: WorkflowPlanRevision; agent: string; backend: string; catalog: ExecutionProfile[];
  evidence: CapabilityEvidence[]; evidenceRevision?: string; disabled: boolean; onApply: (agent: string, profile: string) => Promise<void>;
  onPending: (pending: boolean) => void;
}) {
  const current = plan.profiles[agent];
  const [selected, setSelected] = useState(current?.id ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const choice = plan.selections.find(s => s.agent === agent);
  const steps = plan.draft.steps.filter(s => s.agent === agent);
  const tools = [...new Set(steps.flatMap(s => s.required_tools))];
  useEffect(() => {
    onPending(selected !== (current?.id ?? ""));
    return () => onPending(false);
  }, [selected, current?.id, onPending]);
  if (!choice) return null;
  async function apply() {
    setBusy(true); setError("");
    try { await onApply(agent, selected); }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); }
    finally { setBusy(false); }
  }
  return <details className="step-technical-details" open>
    <summary>Model choice</summary>
    <p className="step-edit-note">{steps.length > 1 ? `This agent runs ${steps.length} steps. Its model choice applies to all of them.` : "Model and effort for this step."}</p>
    <label className="inspector-field"><span>Execution profile</span><select aria-label="Step model profile" value={selected} disabled={disabled || busy} onChange={event => setSelected(event.target.value)}>
      <option value="">Choose a supported profile</option>
      {eligibleExecutionProfiles(plan, agent, catalog, backend).map(profile => <option key={profile.id} value={profile.id}>{profile.model}{profile.effort ? ` · ${profile.effort}` : ""} · {profile.tier}</option>)}
    </select></label>
    <p className="step-edit-note">{current ? `Saved: ${current.model}${current.effort ? ` · ${current.effort} effort` : ""}.` : "A compatible profile is required before running."}</p>
    <button type="button" className="step-save" disabled={disabled || busy || !selected || selected === current?.id} onClick={() => void apply()}>{busy ? "Saving…" : "Apply model choice"}</button>
    {choice.capability_blockers?.length ? <div className="step-config-error" role="status"><strong>{choice.repair_routes?.some(route => !route.permitted) ? "Repair configuration needs review before running." : "Tool access needs review before running."}</strong><ul>{choice.capability_blockers.map(blocker => <li key={blocker}>{blocker}</li>)}</ul></div> : null}
    {current ? <ExecutionProfileDetails profile={current} facts={choice.facts} access={choice.model_access} /> : null}
    <details><summary>Selection evidence</summary><p>{choice.reason}</p><p>Required tools: {tools.length ? tools.join(", ") : "Supplied inputs and workflow ports only"}.</p>
      {choice.tool_access?.map(access => <p key={access.tool}><strong>{access.tool}: {access.status.replaceAll("_", " ")}</strong><br />{access.reason}</p>)}
      <p>Tool declarations describe the native setup. They do not restrict its permissions. Connection observations are reviewed attempts, not a guarantee that the next call will succeed.</p>
      <p>Tool or provider failures do not automatically switch profiles or retry an action. A model repair must be bound in the saved graph and follow a rejected validation check.</p>
      {choice.repair_routes?.length ? <ul>{choice.repair_routes.map(route => <li key={route.step}>{route.step} → {route.repair_agent}: {route.profile_id ?? "Missing model profile"}. {route.permitted ? "Permitted" : "Blocked"}. {route.reason}<br /><small>Up to {route.max_attempts} repair attempt. {route.deadline_ms ? `Deadline: ${route.deadline_ms} ms.` : "No step deadline declared."} Spend cap: unsupported.</small></li>)}</ul> : <p>No model repair route is bound for this agent.</p>}
      {choice.evidence_ids.map(id => { const record = evidence.find(e => e.id === id); return <div key={id}>{record ? <>{record.finding}<br /><small>{record.source_kind?.replaceAll("_", " ") ?? "Unclassified evidence"} · {record.source}<br />{record.limitations}{record.qualification ? <><br />{record.qualification.benchmark} ({record.qualification.benchmark_version}): {record.qualification.accepted_cases}/{record.qualification.total_cases} accepted cases across {record.qualification.independent_groups} groups. Reviewed by {record.qualification.reviewer}.<br />Report: {record.qualification.report}</> : null}</small><BenchmarkEvidenceDetails evidence={record} assessment={evidenceRevision === plan.evidence_revision ? choice.evidence_assessments?.[id] : undefined} /></> : id}</div>; })}
      <ul>{steps.map(step => <li key={step.reaction}>{step.purpose}<br /><small>{step.qualification_scope ? <>{step.qualification_scope.complexity} · {step.qualification_scope.consequence} consequence<br />{step.qualification_scope.task_family} / {step.qualification_scope.task_slice}<br />{step.qualification_scope.input_modalities.join(", ")} → {step.qualification_scope.output_modalities.join(", ")}</> : "Task scope is unspecified; selection remains provisional."}</small></li>)}</ul>
    </details>
    {error ? <p className="step-config-error" role="alert">{error}</p> : null}
  </details>;
}
