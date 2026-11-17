"use client";

import { useRef, useState, type FormEvent } from "react";

type Entry = { id: string; key: string; value: {
  revision: number; status: "candidate" | "confirmed" | "forgotten";
  text: string; source: string; expires_at_ms: number | null;
} };
type Page = { records: Entry[]; review_required: boolean; next_offset: number | null };
type Projects = { projects: Record<string, string>; linked_project: string | null };

export function MemoryManager({ url }: { url: string }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [key, setKey] = useState("");
  const [page, setPage] = useState<Page>();
  const [projects, setProjects] = useState<Projects>();
  const [offset, setOffset] = useState(0);
  const [edit, setEdit] = useState<Entry | "new">();
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  async function request(action: object) {
    const response = await fetch(url.replace(/\/$/, "") + "/v1/memory", {
      method: "POST", redirect: "error", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ key: key.trim(), request: action }),
    });
    const data = await response.json();
    if (!response.ok) throw new Error(data.error || "Memory request failed");
    return data;
  }

  async function refresh(action?: object, start = offset) {
    setBusy(true);
    setError("");
    try {
      if (action) await request(action);
      const [records, links] = await Promise.all([request({action:"list", offset:start}), request({action:"project_list"})]);
      setPage(records); setProjects(links); setOffset(start); setEdit(undefined);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not load memory");
    } finally { setBusy(false); }
  }

  function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const values = Object.fromEntries(new FormData(event.currentTarget));
    void refresh(edit && edit !== "new"
      ? { action:"edit", id:edit.id, revision:edit.value.revision, expires_at_ms:edit.value.expires_at_ms, ...values }
      : { action:"save", ...values });
  }

  return <>
    <button type="button" onClick={() => dialog.current?.showModal()}>Memory</button>
    <dialog className="memory-manager" ref={dialog} aria-label="Assistant memory" onCancel={e => { if (busy) e.preventDefault(); }}
      onClose={() => { setKey(""); setPage(undefined); setEdit(undefined); setProjects(undefined); setError(""); setOffset(0); }}>
      <header><h2>Assistant memory</h2><button type="button" disabled={busy} onClick={() => dialog.current?.close()}>Close</button></header>
      <fieldset disabled={busy}>
        {!page ? <form onSubmit={e => { e.preventDefault(); void refresh(); }}>
          <label>Memory operator key<input type="password" value={key} onChange={e => setKey(e.target.value)} required autoComplete="off" /></label>
          <p>Get your key the first time you open <code>omar -s &lt;runtime&gt; memory</code> in your terminal.</p>
          <button>Unlock memory</button>
        </form> : <>
          <label><input type="checkbox" checked={page.review_required} onChange={e => void refresh({action:"review", enabled:e.target.checked})} /> Review new memories before this assistant can recall them</label>
          <details><summary>Project sharing</summary>
            <label>Linked project<select value={projects?.linked_project ?? ""} onChange={e => void refresh(e.target.value ? {action:"project_link",id:e.target.value} : {action:"project_unlink"})}>
              <option value="">No project</option>{Object.entries(projects?.projects ?? {}).map(([id,name]) => <option key={id} value={id}>{name}</option>)}
            </select></label>
            <form onSubmit={e => {e.preventDefault(); void refresh({action:"project_create", name:new FormData(e.currentTarget).get("name")});}}>
              <label>New project name<input required name="name" maxLength={256} /></label><button>Create project</button>
            </form>
          </details>
          <div><button onClick={() => setEdit("new")}>Add memory</button> <button onClick={() => void refresh()}>Refresh</button></div>
          {edit && <form key={edit === "new" ? "new" : edit.id} onSubmit={save}>
            {edit === "new" && <>
              <label>Name<input required name="key" /></label>
              <label>Scope<select name="scope"><option value="ea">This assistant</option><option value="project">Linked project</option><option value="user">Runtime preference</option></select></label>
              <label>Kind<select name="kind">{["preference","decision","observation","procedure"].map(k => <option key={k}>{k}</option>)}</select></label>
            </>}
            <label>Memory text<textarea required name="text" defaultValue={edit === "new" ? "" : edit.value.text}/></label>
            <label>Source<input required name="source" defaultValue={edit === "new" ? "" : edit.value.source}/></label>
            <div><button>Save memory</button> <button type="button" onClick={() => setEdit(undefined)}>Cancel</button></div>
          </form>}
          {page.records.length === 0 && <p>No memories saved yet.</p>}
          {page.records.map(entry => <article key={entry.id}>
            <h3>{entry.key}</h3><small>{entry.value.status === "candidate" ? "Awaiting review" : entry.value.status}</small>
            {entry.value.status !== "forgotten" && <><p>{entry.value.text}</p><small>Source · {entry.value.source}</small><div>
              {entry.value.status === "candidate" && <button onClick={() => void refresh({action:"confirm",id:entry.id,revision:entry.value.revision})}>Accept</button>}
              <button onClick={() => setEdit(entry)}>Edit</button>
              <button onClick={() => { if (window.confirm("Erase this memory and its saved revisions?")) void refresh({action:"forget",id:entry.id,revision:entry.value.revision}); }}>{entry.value.status === "candidate" ? "Reject" : "Forget"}</button>
            </div></>}
          </article>)}
          <div>{offset > 0 && <button onClick={() => void refresh(undefined, Math.max(0,offset-32))}>Previous</button>}
          {page.next_offset !== null && <button onClick={() => void refresh(undefined,page.next_offset!)}>Next</button>}</div>
        </>}
      </fieldset>
      {error && <p role="alert">{error}</p>}
    </dialog>
  </>;
}
