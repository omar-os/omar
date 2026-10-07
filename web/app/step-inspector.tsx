"use client";

import { useMemo, useState } from "react";
import { locateStep, replaceStep, type StepSource } from "./lib/step-source";
import type { DiagramSnapshot } from "./lib/protocol";
import { formatDuration } from "./lib/protocol";
import { componentName } from "./diagram/diagram-canvas";
import { WorkflowIcon } from "./workflow-icon";
import { taskCopy } from "./lib/task-copy";

/** Details come from the compiled topology, never from prototype sample data. */
export function StepInspector({ component, snapshot, source, editable, onApply, onClose, onEdit, onAsk }: {
  source: string;
  editable: boolean;
  onApply: (source: string) => Promise<void>;
  component: string;
  snapshot: DiagramSnapshot;
  onClose: () => void;
  onEdit: () => void;
  onAsk: (request: string) => void;
}) {
  const reaction = snapshot.reactions.find((item) => componentName(item.id) === component);
  const port = snapshot.ports.find((item) => componentName(item.id) === component);
  const timer = (snapshot.timers ?? []).find((item) => componentName(item.id) === component);
  const agent = snapshot.agents.find((item) => item.id === reaction?.agent);
  const configuration = useMemo(() => reaction ? locateStep(source, snapshot, reaction) : null, [source, snapshot, reaction]);
  const copy = reaction ? taskCopy(reaction, agent) : null;
  const name = copy?.title ?? port?.name ?? timer?.name ?? component;
  const label = (id: string) => snapshot.ports.find((item) => item.id === id)?.name ?? (snapshot.timers ?? []).find((item) => item.id === id)?.name ?? componentName(id);
  const connectionNames = (ids: string[]) => ids.length ? ids.map((id) => <span className="step-connection" key={id}><i />{label(id)}</span>) : <span className="step-empty">None declared</span>;

  return (
    <aside className="step-inspector" aria-label="Step details">
      <header><span className="inspector-type">{reaction ? "TASK" : timer ? "TIMER" : port?.kind === "action" ? "ACTION" : "CONNECTION"}</span><button type="button" aria-label="Close step details" onClick={onClose}><WorkflowIcon name="close" size={16} /></button></header>
      <h3>{name}</h3>
      <p className="inspector-description">{copy ? copy.description || "This workflow has no task description yet. Ask OMAR to describe this step." : component}</p>
      {reaction ? <>
        <details className="step-technical-details"><summary>Technical details</summary><div className="inspector-field"><span>Step ID</span><code>{component}</code></div><div className="inspector-field"><span>Agent</span><code>{agent?.name ?? reaction.agent}</code></div></details>
        {!configuration ? <>
        <div className="inspector-field"><span>Execution backend</span><div className="step-model"><i>{(agent?.backend ?? "?").slice(0, 1)}</i>{agent?.backend ?? "Not specified"}<span className={`step-status ${reaction.status}`}>{reaction.status}</span></div></div>
        <div className="inspector-field"><span>Output contract</span><div className="step-contract">{reaction.contract || "No contract declared"}</div></div>
        {reaction.within != null ? <div className="inspector-field"><span>Deadline</span><strong>{formatDuration(reaction.within)}</strong></div> : null}
        <div className="connections-heading"><span>Connections</span><button type="button" onClick={() => onAsk(`Update the connections for ${component}: `)}><WorkflowIcon name="plus" size={12} /> Edit</button></div>
        <div className="inspector-field"><span>Receives</span>{connectionNames(reaction.triggers)}</div>
        <div className="inspector-field"><span>Produces</span>{connectionNames(reaction.effects)}</div>
        </> : null}
        {configuration ? <StepEditor key={`${component}:${source}`} source={source} step={configuration} enabled={editable} onApply={onApply} ports={snapshot.ports.filter((item) => item.instance === reaction.instance && item.kind !== "output").map((item) => item.name.startsWith(`${reaction.instance}.`) ? item.name.slice(reaction.instance.length + 1) : item.name)} /> : <p className="step-empty">Use the source editor for this step’s configuration.</p>}
        <button type="button" className="step-edit" onClick={onEdit}>Edit prompt & backend in source <WorkflowIcon name="arrow" size={14} /></button>
        <button type="button" className="step-ask" onClick={() => onAsk(`Update the prompt or execution backend for ${component}: `)}>Edit with OMAR</button>
      </> : null}
      {port ? <>
        <div className="inspector-field"><span>Value type</span><strong>{port.type}</strong></div>
        <div className="inspector-field"><span>Current value</span><pre className="step-contract">{port.value === null ? "Waiting for input" : typeof port.value === "string" ? port.value : JSON.stringify(port.value, null, 2)}</pre></div>
        <div className="inspector-field"><span>Connections</span>{snapshot.edges.filter((edge) => edge.source === port.id || edge.target === port.id).map((edge) => <span className="step-connection" key={edge.id}>{label(edge.source)} → {label(edge.target)}</span>)}</div>
      </> : null}
      {timer ? <><div className="inspector-field"><span>Starts after</span><strong>{formatDuration(timer.offset)}</strong></div><div className="inspector-field"><span>Repeats</span><strong>{timer.period ? `Every ${formatDuration(timer.period)}` : "Once"}</strong></div></> : null}
    </aside>
  );
}

function StepEditor({ source, step, ports, enabled, onApply }: {
  source: string; step: StepSource; ports: string[]; enabled: boolean; onApply: (source: string) => Promise<void>;
}) {
  const [backend, setBackend] = useState(step.backend);
  const [prompt, setPrompt] = useState(step.prompt);
  const [contract, setContract] = useState(step.contract);
  const [triggers, setTriggers] = useState(step.triggers);
  const [adding, setAdding] = useState(false);
  const [connection, setConnection] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);
  const changed = backend !== step.backend || prompt !== step.prompt || contract !== step.contract || triggers.join(",") !== step.triggers.join(",");
  const backends = Array.from(new Set([step.backend, "ClaudeCode", "Codex", "Cursor", "OpenCode", "Pi", "Agy", "Web"]));
  async function apply() {
    setBusy(true); setError(""); setSaved(false);
    try { await onApply(replaceStep(source, step, { backend, prompt, triggers, contract })); setSaved(true); }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); }
    finally { setBusy(false); }
  }
  return <div className="step-editor">
    <h4>Configuration</h4>
    <p className="step-edit-note">Changes update the {step.team} definition and all its instances. The workflow still requires confirmation before running.</p>
    {!enabled ? <p role="status">Configuration is read-only while the workflow is running or the runtime is unavailable.</p> : null}
    <fieldset disabled={!enabled || busy}>
      <label className="inspector-field"><span>Execution agent</span><select aria-label="Step execution agent" value={backend} onChange={(event) => setBackend(event.target.value)}>{backends.map((value) => <option key={value} value={value}>{value === "ClaudeCode" ? "Claude Code" : value === "OpenCode" ? "opencode" : value === "Pi" ? "pi" : value === "Agy" ? "agy" : value}</option>)}</select></label>
      <p className="step-edit-note">Uses the model configured for this agent.</p>
      <label className="inspector-field"><span>Prompt</span><textarea aria-label="Step prompt" rows={7} value={prompt} onChange={(event) => setPrompt(event.target.value)} /></label>
      <label className="inspector-field"><span>Output connection / contract</span><input aria-label="Step output contract" value={contract} onChange={(event) => setContract(event.target.value)} /></label>
      <div className="inspector-field"><span>Input connections</span>{triggers.length ? triggers.map((name) => <div className="step-input-row" key={name}><span>{name}</span><button type="button" aria-label={`Remove input ${name}`} onClick={() => setTriggers((current) => current.filter((item) => item !== name))}>×</button></div>) : <small>No inputs selected</small>}</div>
      <button className="step-ask" type="button" aria-expanded={adding} onClick={() => setAdding((current) => !current)}>Add connection</button>
      {adding ? <div className="step-add-connection"><label className="inspector-field"><span>Existing input or task output</span><select aria-label="Input connection" value={connection} onChange={(event) => setConnection(event.target.value)}><option value="">Choose a connection…</option>{ports.filter((port) => !triggers.includes(port)).map((port) => <option key={port}>{port}</option>)}</select></label><button type="button" className="step-ask" disabled={!connection} onClick={() => { setTriggers((current) => [...current, connection]); setConnection(""); setAdding(false); }}>Add selected connection</button></div> : null}
      <button type="button" className="step-save" disabled={!changed || busy} onClick={() => void apply()}>{busy ? "Validating…" : "Apply configuration"}</button>
    </fieldset>
    {error ? <p className="step-config-error" role="alert">{error}</p> : null}
    {saved ? <p role="status">Configuration applied to the draft.</p> : null}
  </div>;
}
