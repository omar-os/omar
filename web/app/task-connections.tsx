"use client";

import { useState } from "react";
import { connectionDetail, connectionTypes, validateConnection, type TaskConnection } from "./lib/task-connections";
import { WorkflowIcon } from "./workflow-icon";

type ConnectionType = TaskConnection["type"] | "workflow";

export function TaskConnections({ connections, triggers, ports, onConnectionsChange, onTriggersChange }: {
  connections: TaskConnection[];
  triggers: string[];
  ports: string[];
  onConnectionsChange: (connections: TaskConnection[]) => void;
  onTriggersChange: (triggers: string[]) => void;
}) {
  const [adding, setAdding] = useState(false);
  const [type, setType] = useState<ConnectionType | null>(null);
  const [editing, setEditing] = useState<TaskConnection | null>(null);
  const [input, setInput] = useState("");
  const available = ports.filter((port) => !triggers.includes(port));

  function close() { setAdding(false); setType(null); setEditing(null); setInput(""); }
  function save(connection: TaskConnection) {
    onConnectionsChange(editing ? connections.map((item) => item.id === editing.id ? connection : item) : [...connections, connection]);
    close();
  }

  return <section className="task-connections" aria-label="Task connections">
    <div className="task-connections-heading"><h4>Connections <span>{connections.length}</span></h4><WorkflowIcon name="api" size={17} /></div>
    <p className="step-edit-note">Give this task access to external APIs and tools.</p>
    {connections.length ? <ul className="task-connection-list">{connections.map((connection) => {
      const kind = connectionTypes.find((item) => item.type === connection.type)!;
      return <li className="task-connection-card" key={connection.id}>
        <div className="task-connection-icon"><WorkflowIcon name={kind.icon} size={17} /></div>
        <div className="task-connection-info"><strong>{connection.name}</strong><small>{kind.title} · Configured</small><span title={connectionDetail(connection)}>{connectionDetail(connection)}</span></div>
        <div className="task-connection-actions"><button type="button" aria-label={`Edit connection ${connection.name}`} onClick={() => { setEditing(connection); setType(connection.type); setAdding(true); }}>Edit</button><button type="button" aria-label={`Remove connection ${connection.name}`} onClick={() => { onConnectionsChange(connections.filter((item) => item.id !== connection.id)); if (editing?.id === connection.id) close(); }}><WorkflowIcon name="close" size={13} /></button></div>
      </li>;
    })}</ul> : <div className="task-connections-empty"><WorkflowIcon name="api" size={22} /><strong>No external connections yet</strong><span>Add an API or tool this task can use.</span></div>}
    <button className="step-ask connection-add-button" type="button" aria-expanded={adding} onClick={() => adding ? close() : setAdding(true)}><WorkflowIcon name="plus" size={14} />Add connection</button>
    {adding ? <div className="connection-setup">
      <div className="connection-setup-heading"><strong>{editing ? "Edit connection" : type ? "Set up connection" : "Choose a connection type"}</strong><button type="button" aria-label="Cancel connection setup" onClick={close}><WorkflowIcon name="close" size={14} /></button></div>
      {!type ? <div className="connection-type-list">
        {connectionTypes.map((item) => <button className="connection-type-option" type="button" key={item.type} onClick={() => setType(item.type)}><i><WorkflowIcon name={item.icon} size={18} /></i><span><strong>{item.title}</strong><small>{item.description}</small></span><WorkflowIcon name="chevron" size={14} /></button>)}
        <button className="connection-type-option" type="button" onClick={() => setType("workflow")}><i><WorkflowIcon name="workflow" size={18} /></i><span><strong>Workflow input</strong><small>Use data from this workflow or another step.</small></span><WorkflowIcon name="chevron" size={14} /></button>
      </div> : <>
        {!editing ? <button className="connection-back" type="button" onClick={() => { setType(null); setInput(""); }}>← Connection types</button> : null}
        {type === "workflow" ? <>
          <p className="step-edit-note">Inputs determine when this task runs. External APIs are available to the agent while it runs.</p>
          {available.length ? <><label className="inspector-field"><span>Existing workflow input or step output</span><select aria-label="Input connection" value={input} onChange={(event) => setInput(event.target.value)}><option value="">Choose an input…</option>{available.map((port) => <option key={port}>{port}</option>)}</select></label><button type="button" className="step-save" disabled={!input} onClick={() => { onTriggersChange([...triggers, input]); close(); }}>Add selected connection</button></> : <p className="connection-empty-inputs" role="status">All available inputs are already added. Choose an external connection type to connect an API or tool.</p>}
        </> : <ConnectionForm key={editing?.id ?? type} type={type} connection={editing} connections={connections} onSave={save} onCancel={close} />}
      </>}
    </div> : null}
    <div className="workflow-input-heading"><strong>Workflow inputs</strong><small>{triggers.length}</small></div>
    <p className="step-edit-note">Data that triggers this task.</p>
    {triggers.length ? <div className="workflow-input-list">{triggers.map((name) => <div className="step-input-row" key={name}><WorkflowIcon name="workflow" size={14} /><span>{name}</span><button type="button" aria-label={`Remove input ${name}`} onClick={() => onTriggersChange(triggers.filter((item) => item !== name))}><WorkflowIcon name="close" size={13} /></button></div>)}</div> : <p className="step-empty">No workflow inputs selected.</p>}
  </section>;
}

function ConnectionForm({ type, connection, connections, onSave, onCancel }: {
  type: TaskConnection["type"];
  connection: TaskConnection | null;
  connections: TaskConnection[];
  onSave: (connection: TaskConnection) => void;
  onCancel: () => void;
}) {
  const http = connection && connection.type !== "mcp" ? connection : null;
  const [name, setName] = useState(connection?.name ?? "");
  const [url, setUrl] = useState(http?.url ?? "");
  const [method, setMethod] = useState(http?.method ?? (type === "webhook" ? "POST" : "GET"));
  const [auth, setAuth] = useState(http?.auth.type ?? "none");
  const [variable, setVariable] = useState(http?.auth.type !== "none" ? http?.auth.variable ?? "" : "");
  const [header, setHeader] = useState(http?.auth.type === "api-key" ? http.auth.header : "X-API-Key");
  const [server, setServer] = useState(connection?.type === "mcp" ? connection.server : "");
  const [error, setError] = useState("");
  const kind = connectionTypes.find((item) => item.type === type)!;

  function save() {
    const common = { id: connection?.id ?? crypto.randomUUID(), name: name.trim() };
    const next: TaskConnection = type === "mcp" ? { ...common, type, server: server.trim() } : {
      ...common, type, url: url.trim(), method,
      auth: auth === "none" ? { type: "none" } : auth === "bearer" ? { type: "bearer", variable: variable.trim() } : { type: "api-key", variable: variable.trim(), header: header.trim() },
    };
    const problem = validateConnection(next, connections);
    if (problem) { setError(problem); return; }
    onSave(next);
  }

  return <div className="connection-form" onKeyDown={(event) => {
    if (event.key === "Enter" && event.target instanceof HTMLInputElement) { event.preventDefault(); save(); }
  }}>
    <span className="connection-kind-label"><WorkflowIcon name={kind.icon} size={14} />{kind.title}</span>
    <label className="inspector-field"><span>Connection name</span><input aria-label="Connection name" autoFocus maxLength={80} value={name} placeholder={type === "mcp" ? "Project tools" : type === "webhook" ? "Team notifications" : "Content API"} onChange={(event) => setName(event.target.value)} /></label>
    {type === "mcp" ? <>
      <label className="inspector-field"><span>Installed server name</span><input aria-label="MCP server name" value={server} placeholder="e.g. github" onChange={(event) => setServer(event.target.value)} /></label>
      <p className="step-edit-note">Use the server name configured in your execution agent. Install and authenticate the server there first; adding it here makes its tools part of this task’s instructions.</p>
    </> : <>
      <label className="inspector-field"><span>{type === "webhook" ? "Webhook URL" : "Endpoint URL"}</span><input aria-label="Endpoint URL" type="url" value={url} placeholder="https://api.example.com/v1/items" onChange={(event) => setUrl(event.target.value)} /></label>
      {type === "http" ? <label className="inspector-field"><span>HTTP method</span><select aria-label="HTTP method" value={method} onChange={(event) => setMethod(event.target.value)}>{["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"].map((value) => <option key={value}>{value}</option>)}</select></label> : <p className="step-edit-note">Sends a POST request. Describe the payload in the task prompt.</p>}
      <label className="inspector-field"><span>Authentication</span><select aria-label="Connection authentication" value={auth} onChange={(event) => setAuth(event.target.value as typeof auth)}><option value="none">No authentication</option><option value="bearer">Bearer token</option><option value="api-key">API key header</option></select></label>
      {auth !== "none" ? <>
        {auth === "api-key" ? <label className="inspector-field"><span>Header name</span><input aria-label="API key header" value={header} onChange={(event) => setHeader(event.target.value)} /></label> : null}
        <label className="inspector-field"><span>Credential environment variable</span><input aria-label="Credential environment variable" value={variable} placeholder="SERVICE_API_KEY" autoComplete="off" spellCheck={false} onChange={(event) => setVariable(event.target.value)} /></label>
        <p className="step-edit-note">Enter the variable name, not the secret. Set its value in the execution agent’s environment.</p>
      </> : null}
      <p className="step-edit-note">Your agent calls this endpoint as needed by the task prompt. Connectivity is checked when it runs.</p>
    </>}
    {error ? <p className="step-config-error" role="alert">{error}</p> : null}
    <div className="connection-form-actions"><button type="button" className="step-ask" onClick={onCancel}>Cancel</button><button type="button" className="step-save" onClick={save}>{connection ? "Save connection" : "Add to task"}</button></div>
  </div>;
}
