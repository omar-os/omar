"use client";

import { useEffect, useRef, useState } from "react";
import type { ChatMessage, DiagramSnapshot } from "./lib/protocol";
import { OmarSource } from "./omar-source";
import { WorkflowIcon } from "./workflow-icon";
import type { WorkflowPlanRevision } from "./lib/protocol-generated";
import { exportWorkflowBundle, importWorkflowBundle } from "./lib/workflow-client";

/** Browse the real proposals retained in this conversation. */
export function FilesVersions({ source, filename, snapshot, messages, onClose, onEdit, runtimeUrl, plan, transferEnabled = false, reviewDirty = false }: {
  source: string;
  filename: string;
  snapshot: DiagramSnapshot | null;
  messages: ChatMessage[];
  onClose: () => void;
  onEdit: () => void;
  runtimeUrl?: string; plan?: WorkflowPlanRevision | null; transferEnabled?: boolean; reviewDirty?: boolean;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [version, setVersion] = useState("current");
  const [file, setFile] = useState("program");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const importInput = useRef<HTMLInputElement>(null);
  const proposals = messages.filter((message) => message.design);
  const proposal = proposals.find((message) => String(message.sequence) === version)?.design;
  const program = proposal?.program ?? source;
  const topology = proposal?.preview ?? snapshot;
  const reference = version === "current" ? plan : proposal?.plan;
  const unsaved = version === "current" && (!!plan && plan.program !== source || reviewDirty);
  const files = [
    { id: "program", name: filename, kind: "OMAR" },
    ...(topology ? [{ id: "topology", name: "topology.json", kind: "JSON" }] : []),
    ...(proposal ? [{ id: "inputs", name: "inputs.json", kind: "JSON" }] : []),
  ];
  const activeFile = files.find((item) => item.id === file) ?? files[0];

  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null;
    const element = dialog.current;
    element?.showModal();
    return () => { element?.close(); opener?.focus(); };
  }, []);

  async function exportBundle() {
    if (!runtimeUrl || !reference || busy) return;
    setBusy(true); setError("");
    try {
      const bundle = await exportWorkflowBundle(runtimeUrl, {id:reference.id, revision:reference.revision});
      const url = URL.createObjectURL(new Blob([JSON.stringify(bundle, null, 2)], {type:"application/json"}));
      const link = document.createElement("a"); link.href=url; link.download=`${topology?.team ?? "workflow"}.omar-workflow.json`; link.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); }
    finally { setBusy(false); }
  }

  async function importBundle(file: File) {
    if (!runtimeUrl || busy) return;
    setBusy(true); setError("");
    try {
      if (file.size > 2 * 1024 * 1024) throw new Error("This workflow bundle is too large.");
      const bundle: unknown = JSON.parse(await file.text());
      await importWorkflowBundle(runtimeUrl, bundle);
      onClose();
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); }
    finally { setBusy(false); if (importInput.current) importInput.current.value=""; }
  }

  return (
    <dialog ref={dialog} className="files-modal" aria-labelledby="files-title" onCancel={onClose} onClick={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <div className="files-modal-content">
        <header className="files-modal-header">
          <div><span className="eyebrow">WORKSPACE</span><h2 id="files-title">Files & versions</h2></div>
          <button type="button" className="secondary-button" onClick={onClose}>Close</button>
        </header>
        <div className="workspace-controls">
          <label><span>Workflow</span><div className="workspace-name"><WorkflowIcon name="workflow" size={16} />{topology?.team ?? "New workflow"}</div></label>
          <label><span>Version</span><select aria-label="Version" value={version} onChange={(event) => setVersion(event.target.value)}><option value="current">Current draft</option>{proposals.map((message, index) => <option key={message.sequence} value={message.sequence}>Proposal {index + 1}</option>)}</select></label>
          <button type="button" className="primary-button" onClick={onEdit} disabled={!snapshot}>Open source editor</button>
        </div>
        <p className="workspace-notice">{version === "current" ? "Current workflow source. Proposals are saved with this conversation; manual source edits remain in this session." : "Saved proposal from this conversation. Viewing a version does not change or deploy your workflow."}</p>
        {runtimeUrl ? <div className="workflow-transfer">
          <div className="workflow-review-actions">
            <button type="button" className="secondary-button" disabled={!transferEnabled || !reference || unsaved || busy} onClick={() => void exportBundle()}>Export workflow bundle</button>
            <button type="button" className="secondary-button" disabled={!transferEnabled || busy} onClick={() => importInput.current?.click()}>Import workflow bundle</button>
            <input ref={importInput} type="file" accept=".json" aria-label="Workflow bundle file" hidden onChange={event => {const selected=event.target.files?.[0];if(selected) void importBundle(selected);}} />
          </div>
          <p className="workspace-notice">{unsaved ? "Save the workflow review and source edits before exporting this draft." : "A bundle includes the saved source, inputs, research and exact model bindings. Importing creates a draft for review. Plain .omar source alone uses configured backend defaults."}</p>
          {busy ? <p role="status">Preparing workflow…</p> : null}{error ? <p role="alert">{error}</p> : null}
        </div> : null}
        <div className="file-browser">
          <nav className="file-tree" aria-label="Workflow files">
            <div className="tree-root">{topology?.team ?? "workflow"}/</div>
            {files.map((item) => <button key={item.id} type="button" aria-current={activeFile.id === item.id ? "true" : undefined} onClick={() => setFile(item.id)}><span className="file-type">{item.kind}</span>{item.name}</button>)}
          </nav>
          <section className="file-preview" aria-label="File preview">
            <div className="preview-header">{activeFile.name}<small>{version === "current" ? "Current draft" : "Saved proposal"}</small></div>
            {activeFile.id === "program" ? (program ? <OmarSource source={program} /> : <p className="file-empty">Start a conversation with OMAR to draft your first workflow.</p>) : <pre>{JSON.stringify(activeFile.id === "topology" ? topology : proposal?.inputs, null, 2)}</pre>}
          </section>
        </div>
      </div>
    </dialog>
  );
}
