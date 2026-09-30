"use client";

import { useEffect, useRef, useState } from "react";
import type { AdviceState, ProposedDesign, WorkflowTemplate } from "./lib/protocol-generated";
import { checkProgram } from "./lib/runtime-client";
import {
  adviceFeedback, adviceMode, adviceSnapshots, evaluateAdvice, previewAdviceArtifact,
  readAdvice, registerAdvice, workflowCatalog, type AdvicePayload, type SavedWorkspace,
} from "./lib/workflow-advice-client";

type Kind = "draft" | "proposal" | "artifact";
const profiles: Record<Kind, string> = { draft: "template-fit-v1", proposal: "scenario-coverage-v1", artifact: "artifact-requirement-v1" };

/** Opening or editing this panel never requests a provider evaluation. */
export function WorkflowAdvice({ serveUrl, program, inputs, canSelect, onSelect }: {
  serveUrl: string; program: string; inputs: Record<string, unknown>; canSelect: boolean;
  onSelect: (design: ProposedDesign) => void;
}) {
  const [kind, setKind] = useState<Kind>("draft");
  const [brief, setBrief] = useState("");
  const [templates, setTemplates] = useState<WorkflowTemplate[]>([]);
  const [workspaces, setWorkspaces] = useState<SavedWorkspace[]>([]);
  const [workspace, setWorkspace] = useState("");
  const [snapshot, setSnapshot] = useState("");
  const [path, setPath] = useState("");
  const [artifact, setArtifact] = useState<{ text: string; characters: number; revision: string; complete: boolean } | null>(null);
  const [range, setRange] = useState({ start: 0, end: 0 });
  const [requirement, setRequirement] = useState("");
  const [requireComplete, setRequireComplete] = useState(true);
  const [confirmed, setConfirmed] = useState(false);
  const [state, setState] = useState<AdviceState | null>(null);
  const [preparedKey, setPreparedKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [dismissed, setDismissed] = useState<string[]>([]);
  const ids = useRef<Partial<Record<Kind, string>>>({});
  const key = JSON.stringify(kind === "draft" ? [kind, brief] : kind === "proposal" ? [kind, program, inputs] : [kind, workspace, snapshot, path, range, requirement, requireComplete]);
  const stale = !!state && preparedKey !== key;
  const pending = state?.records.some((r) => r.status === "queued" || r.status === "evaluating");

  // Polling reads local records only. No retry or provider dispatch is implicit.
  useEffect(() => {
    if (!state || !pending || stale) return;
    let cancelled = false;
    const timer = setInterval(() => {
      readAdvice(serveUrl, state.subject.id).then((next) => { if (!cancelled) setState(next); })
        .catch((e: Error) => { if (!cancelled) setError(e.message); });
    }, 500);
    return () => { cancelled = true; clearInterval(timer); };
  }, [serveUrl, state?.subject.id, pending, stale]); // eslint-disable-line react-hooks/exhaustive-deps

  async function action(task: () => Promise<void>) {
    if (busy) return;
    setBusy(true); setError("");
    try { await task(); } catch (e) { setError(e instanceof Error ? e.message : "Advice is unavailable."); }
    finally { setBusy(false); }
  }

  async function prepare() {
    const id = ids.current[kind] ?? (ids.current[kind] = crypto.randomUUID());
    const payload: AdvicePayload = kind === "draft" ? { kind, text: brief }
      : kind === "proposal" ? { kind, program, inputs, scenario_id: "review-actual-output", scenario_version: "1" }
      : { kind, workspace_id: workspace, snapshot_id: snapshot, path, ...range };
    const criterion = kind === "artifact" ? { id: "operator-requirement", version: "1", text: requirement, requires_complete: requireComplete } : undefined;
    const next = await registerAdvice(serveUrl, id, profiles[kind], payload, criterion);
    setState(next); setPreparedKey(key); setConfirmed(false);
  }

  async function selectTemplate(template: WorkflowTemplate) {
    const checked = await checkProgram(serveUrl, template.program, `${template.id.replaceAll("-", "_")}.omar`);
    if (!checked.ok || !checked.preview) throw new Error(checked.errors?.join("\n") || "The template could not be compiled.");
    onSelect({ program: template.program, inputs: { "flow.brief": brief }, preview: checked.preview });
  }

  const recommendation = !stale ? state?.records.findLast((r) => r.status === "suggested" && r.input.profile_id === "template-fit-v1" && !dismissed.includes(r.decision_id))?.result?.outcome : undefined;
  return <details className="workflow-advice">
    <summary>Optional workflow assistance</summary>
    <p>Choose a workflow, check a proposal, or review saved text. Jev advice is optional; it never deploys or edits your work.</p>
    <label>Review stage <select value={kind} onChange={(e) => { setKind(e.target.value as Kind); setConfirmed(false); }} disabled={busy}>
      <option value="draft">Choose a workflow</option><option value="proposal">Check scenarios before running</option><option value="artifact">Review a saved text result</option>
    </select></label>
    {kind === "draft" ? <>
      <label>Workflow brief <textarea value={brief} onChange={(e) => { setBrief(e.target.value); setConfirmed(false); }} /></label>
      <button type="button" disabled={busy} onClick={() => void action(async () => setTemplates((await workflowCatalog(serveUrl)).templates))}>Browse supported workflows</button>
      {templates.map((template) => <div key={template.id} className="advice-template">
        <strong>{template.name}{recommendation === template.id ? " · Suggested" : ""}</strong><p>{template.purpose}</p>
        <small>{template.constraint} {template.eligible ? `Uses ${template.backend}.` : "A supported agent backend and tmux are required."}</small>
        <button type="button" disabled={busy || !canSelect || !template.eligible || !brief.trim()} onClick={() => void action(() => selectTemplate(template))}>Use {template.name}</button>
        <a download={`${template.id.replaceAll("-", "_")}.omar`} href={`data:text/plain;charset=utf-8,${encodeURIComponent(template.program)}`}>Export .omar source</a>
      </div>)}
    </> : kind === "proposal" ? <>
      <p><strong>Scenario: review actual output · v1</strong></p>
      <p>The reviewer assesses the writer&apos;s actual draft before final delivery, and the final writer uses that review. Applies to the starter workflows&apos; <code>flow</code> handoffs.</p>
      <details><summary>Proposal evidence</summary><pre>{program || "Select or author a proposal first."}</pre></details>
    </> : <>
      <p>Review immutable saved Markdown, plain text, extracted static HTML text, and text reports. Visual output and rich documents are unavailable. HTML with styles or dynamic content is incomplete evidence.</p>
      <button type="button" disabled={busy} onClick={() => void action(async () => setWorkspaces((await adviceSnapshots(serveUrl)).workspaces))}>Load saved revisions</button>
      <label>Workspace <select value={workspace} onChange={(e) => { setWorkspace(e.target.value); setSnapshot(""); setArtifact(null); setConfirmed(false); }}><option value="">Select a workspace</option>{workspaces.map((w) => <option key={w.workspace_id} value={w.workspace_id}>{w.instance || "Root"} · {w.workspace_id.slice(0, 8)}</option>)}</select></label>
      <label>Saved revision <select value={snapshot} onChange={(e) => { setSnapshot(e.target.value); setArtifact(null); setConfirmed(false); }}><option value="">Select a revision</option>{workspaces.find((w) => w.workspace_id === workspace)?.snapshots.map((s) => <option key={s.id} value={s.id}>{s.label} · {s.commit.slice(0, 8)}</option>)}</select></label>
      <label>Relative file path <input value={path} placeholder="result.md" onChange={(e) => { setPath(e.target.value); setArtifact(null); setConfirmed(false); }} /></label>
      <button type="button" disabled={busy || !workspace || !snapshot || !path} onClick={() => void action(async () => { const preview = await previewAdviceArtifact(serveUrl, workspace, snapshot, path); setArtifact(preview); setRange({ start: 0, end: preview.characters }); })}>Preview saved text</button>
      {artifact ? <><pre>{artifact.text}</pre><small>Revision {artifact.revision}; selection uses Unicode character offsets in the extracted text. {artifact.complete ? "Complete text is inspectable." : "Incomplete evidence: visual or dynamic content is uninspectable."}</small>
        <label>Selection start <input type="number" min={0} max={artifact.characters} value={range.start} onChange={(e) => { setRange({ ...range, start: Number(e.target.value) }); setConfirmed(false); }} /></label>
        <label>Selection end <input type="number" min={1} max={artifact.characters} value={range.end} onChange={(e) => { setRange({ ...range, end: Number(e.target.value) }); setConfirmed(false); }} /></label></> : null}
      <label>Requirement to confirm <textarea value={requirement} onChange={(e) => { setRequirement(e.target.value); setConfirmed(false); }} /></label>
      <label><input type="checkbox" checked={requireComplete} onChange={(e) => { setRequireComplete(e.target.checked); setConfirmed(false); }} /> This requirement needs the complete text.</label>
    </>}
    <label><input type="checkbox" checked={confirmed} onChange={(e) => setConfirmed(e.target.checked)} /> I confirm this evidence and criterion for review.</label>
    <button type="button" disabled={busy || !confirmed || (kind === "proposal" && !program) || (kind === "artifact" && (!artifact || !requirement.trim()))} onClick={() => void action(prepare)}>Prepare review locally</button>
    {state ? <div className="advice-evaluation">
      <p>Revision <code>{state.subject.revision}</code> · {stale ? "Stale — evidence changed. Prepare a fresh review." : `Mode: ${state.mode}`}</p>
      <details><summary>Exact evidence and criterion</summary><p>{state.input.criterion.text}</p>{state.input.evidence.map((e) => <pre key={e.id}>{e.id} [{e.start}, {e.end}){"\n"}{e.text}</pre>)}</details>
      <p>Enabling suggestions permits explicit requests to send this selected text and criterion to TypeSafe ({"jev-1.13.0"}). Off and shadow make no provider calls.</p>
      <button type="button" disabled={busy || stale} onClick={() => void action(async () => setState(await adviceMode(serveUrl, state, "suggest")))}>Enable suggestions for this revision</button>
      <button type="button" disabled={busy || stale} onClick={() => void action(async () => setState(await adviceMode(serveUrl, state, "shadow")))}>Shadow: keep local</button>
      <button type="button" disabled={busy} onClick={() => void action(async () => setState(await adviceMode(serveUrl, state, "off")))}>Turn assistance off</button>
      <button type="button" disabled={busy || stale || state.mode !== "suggest" || pending} onClick={() => void action(async () => { await evaluateAdvice(serveUrl, state, crypto.randomUUID()); setState(await readAdvice(serveUrl, state.subject.id)); })}>{kind === "draft" ? "Suggest a workflow" : kind === "proposal" ? "Check selected scenario" : "Review confirmed requirement"}</button>
      {state.records.filter((r) => !dismissed.includes(r.decision_id)).map((record) => <article key={record.decision_id} className="advice-card">
        <strong>{record.input.profile_id} · {record.status}</strong><p>{stale ? "stale" : record.freshness}</p>
        {record.error ? <p role="status">{record.error}</p> : null}
        {record.result ? <><p>{record.result.message}</p><p>Outcome: {record.result.outcome}</p>
          <small>Choice confidence {record.result.confidence.toFixed(2)} · Option probability {record.result.selected_probability.toFixed(2)} · Evidence sufficiency {record.result.sufficient_context.toFixed(2)}</small>
          {record.result.evidence_id ? <details><summary>Inspect evidence {record.result.evidence_id}</summary><pre>{record.input.evidence.find((e) => e.id === record.result?.evidence_id)?.text}</pre></details> : null}</> : null}
        <button type="button" disabled={busy} onClick={() => void action(async () => { await adviceFeedback(serveUrl, state, record, "dismissed"); setDismissed((old) => [...old, record.decision_id]); })}>Reject suggestion</button>
        <button type="button" disabled={busy} onClick={() => void action(async () => { await adviceFeedback(serveUrl, state, record, "useful"); })}>Mark useful</button>
      </article>)}
    </div> : null}
    {error ? <p role="alert">{error} Ordinary authoring and deployment remain available.</p> : null}
  </details>;
}
