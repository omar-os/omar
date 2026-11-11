"use client";

import { useEffect, useRef, useState } from "react";
import { useIntegrationLibrary } from "./integration-library";
import { ConnectionForm } from "./task-connections";
import { connectionDetail, connectionTypes, type TaskConnection } from "./lib/task-connections";
import { WorkflowIcon } from "./workflow-icon";

function credentialLabel(entry: TaskConnection) {
  if (entry.type === "mcp") return "Uses the installed server’s sign-in";
  switch (entry.auth.type) {
    case "none": return "No authentication";
    case "bearer": return `Bearer token from ${entry.auth.variable}`;
    case "api-key": return `${entry.auth.header} from ${entry.auth.variable}`;
  }
}

export function IntegrationsPage({ onClose }: { onClose: () => void }) {
  const library = useIntegrationLibrary();
  const [form, setForm] = useState<{ type: TaskConnection["type"]; entry?: TaskConnection } | null>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const element = dialog.current;
    const opener = document.activeElement as HTMLElement | null;
    element?.showModal();
    return () => {
      element?.close();
      opener?.focus();
    };
  }, []);
  return <dialog ref={dialog} className="integrations-dialog" aria-labelledby="integrations-title" onCancel={onClose}>
    <header><h2 id="integrations-title">Integrations</h2><button type="button" onClick={onClose} aria-label="Close integrations">Close</button></header>
    <p>Save a connection here, then ask for it by name when creating a workflow. You can also choose it from Saved integrations when editing a task.</p>
    {library.error ? <div role="alert">{library.error} <button type="button" onClick={library.refresh}>Retry</button></div> : null}
    {!form ? <>
      {library.entries.length ? <ul className="integration-list">{library.entries.map(entry => {
        const kind = connectionTypes.find(item => item.type === entry.type)!;
        return <li key={entry.id}><details>
          <summary><WorkflowIcon name={kind.icon} size={18} /><strong>{entry.name}</strong><span>{kind.title}</span></summary>
          <p>{connectionDetail(entry)}</p><p>{credentialLabel(entry)}</p>
          <button type="button" onClick={() => setForm({ type: entry.type, entry })}>Edit integration</button>
        </details></li>;
      })}</ul> : !library.error && <p role="status">{library.loading ? "Loading integrations…" : "No saved integrations yet."}</p>}
      <div className="integration-add"><h3>Add an integration</h3>{connectionTypes.map(kind =>
        <button type="button" key={kind.type} onClick={() => setForm({type:kind.type})}>{kind.title}</button>)}</div>
    </> : <div className="integration-editor">
      {form.entry ? <p>Updated settings apply to future selections. Existing tasks keep their selected endpoint and credential variable.</p> : null}
      <ConnectionForm key={form.entry?.id ?? form.type} type={form.type} connection={form.entry ?? null}
        connections={library.entries} submitLabel="Save integration" onCancel={() => setForm(null)} onSave={async connection => {
          await library.save([{ connection, before: form.entry }]);
          setForm(null);
        }} />
    </div>}
  </dialog>;
}
