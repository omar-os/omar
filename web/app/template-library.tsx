"use client";

import { useEffect, useRef, useState } from "react";
import { downloadArtifact, fetchRepositoryInput, fetchTemplates, parseTemplateData, prepareTemplate, type TemplateSource, type WorkflowTemplate } from "./lib/template-client";
import { TemplateFields } from "./template-fields";
import type { ConversationSummary } from "./lib/protocol";
import { selectConversation } from "./lib/runtime-client";
import "./template-library.css";

export function TemplateLibrary({url,onClose,onSelect}:{url:string;onClose:()=>void;onSelect:(chat:ConversationSummary)=>void}) {
  const dialog=useRef<HTMLDialogElement>(null);
  const [catalog,setCatalog]=useState<WorkflowTemplate[]>([]);
  const [selected,setSelected]=useState("");
  const [query,setQuery]=useState("");
  const [category,setCategory]=useState("All");
  const [goal,setGoal]=useState("");
  const [sources,setSources]=useState<TemplateSource[]>([{id:"source-1",title:"Source 1",text:""}]);
  const [fields,setFields]=useState<Record<string,string>>({});
  const [backend,setBackend]=useState("codex");
  const [busy,setBusy]=useState(false);
  const [error,setError]=useState("");
  const [loading,setLoading]=useState(true);
  const [attempt,setAttempt]=useState(0);
  const [verifiers,setVerifiers]=useState<{id:string;description:string}[]>([]);
  const importInput=useRef<HTMLInputElement>(null);
  const preparation=useRef<{fingerprint:string;id:string}|null>(null);
  const mounted=useRef(false);
  const template=catalog.find(t=>t.id===selected);
  useEffect(()=>{
    const opener=document.activeElement as HTMLElement|null;
    const element=dialog.current;
    mounted.current=true;element?.showModal();
    return ()=>{mounted.current=false;element?.close();opener?.focus();};
  },[]);
  useEffect(()=>{
    const abort=new AbortController();
    fetchTemplates(url,abort.signal).then(value=>{
      if(abort.signal.aborted)return;
      setCatalog(value.templates);setVerifiers(value.repository_verifiers??[]);setSelected(current=>current||"questionnaire");setLoading(false);setError("");
    }).catch(cause=>{if(!abort.signal.aborted){setError(cause.message);setLoading(false);}});
    return ()=>abort.abort();
  },[url,attempt]);
  function choose(id:string){setSelected(id);setGoal("");setFields({});setSources([{id:"source-1",title:"Source 1",text:""}]);setError("");}
  function sample(){if(!template)return;setGoal(template.sample.goal);setSources(structuredClone(template.sample.sources));setFields(Object.fromEntries(Object.entries(template.sample.data).map(([key,v])=>[key,JSON.stringify(v,null,2)])));setError("");}
  async function repository(id:string){try{const input=await fetchRepositoryInput(url,id);setFields(current=>({...current,repository:JSON.stringify(input,null,2)}));setError("");}catch(cause){setError(cause instanceof Error?cause.message:String(cause));}}
  async function importConfiguration(file:File){
    try{if(file.size>150_000)throw new Error("Configuration exceeds 150 KB.");const config=JSON.parse(await file.text());
      const target=catalog.find(t=>t.id===config.template_id&&t.digest===config.template_digest);
      if(config.schema_version!==1||!target||typeof config.input?.goal!=="string"||!Array.isArray(config.input.sources)||!config.input.sources.every((s:TemplateSource)=>typeof s.id==="string"&&typeof s.title==="string"&&typeof s.text==="string")||!config.input.data||typeof config.input.data!=="object")throw new Error("Invalid or outdated configuration. Reload its template and review the inputs.");
      setSelected(target.id);setGoal(config.input.goal);setSources(config.input.sources);setFields(Object.fromEntries(Object.entries(config.input.data).map(([key,v])=>[key,JSON.stringify(v,null,2)])));setError("");
    }catch(cause){setError(cause instanceof Error?cause.message:String(cause));}finally{if(importInput.current)importInput.current.value="";}
  }
  function exportConfiguration(){if(!template)return;try{const content=JSON.stringify({schema_version:1,template_id:template.id,template_digest:template.digest,input:{goal,sources,data:parseTemplateData(template,fields)}},null,2);downloadArtifact({name:`${template.id}-inputs.json`,content,digest:""});setError("");}catch(cause){setError(cause instanceof Error?cause.message:String(cause));}}
  async function upload(files:FileList|null){
    if(!files)return;
    try{
      const imported:TemplateSource[]=[];
      for(const file of Array.from(files)){
        if(!/\.(md|txt)$/i.test(file.name)||file.size>100_000)throw new Error("Choose Markdown or text files up to 100 KB. Put structured data in the fields below.");
        imported.push({id:`source-${crypto.randomUUID()}`,title:file.name,text:await file.text()});
      }
      setSources(current=>[...current.filter(s=>s.text.trim()),...imported]);setError("");
    }catch(cause){setError(cause instanceof Error?cause.message:String(cause));}
  }
  async function prepare(){
    if(!template||busy)return;
    setBusy(true);setError("");
    try{
      const input={goal,sources,data:parseTemplateData(template,fields)};
      const fingerprint=JSON.stringify({id:template.id,digest:template.digest,input,backend});
      if(preparation.current?.fingerprint!==fingerprint)preparation.current={fingerprint,id:crypto.randomUUID()};
      const prepared=await prepareTemplate(url,template,input,backend,preparation.current.id);
      const conversation=await selectConversation(url,prepared.conversation.id);
      if(mounted.current){onSelect(conversation);onClose();}
    }catch(cause){if(mounted.current)setError(cause instanceof Error?cause.message:String(cause));}
    finally{if(mounted.current)setBusy(false);}
  }
  const visible=catalog.filter(t=>(category==="All"||t.category===category)&&`${t.title} ${t.job} ${t.persona} ${t.artifacts}`.toLowerCase().includes(query.toLowerCase()));
  return <dialog ref={dialog} className="template-library" aria-labelledby="template-library-title" onCancel={event=>{if(busy)event.preventDefault();else onClose();}}>
    <header><div><span className="eyebrow">WORKFLOW MARKETPLACE</span><h2 id="template-library-title">Start with the result you need</h2><p>Explore examples, use your sources, then review before running.</p></div><button type="button" className="secondary-button" disabled={busy} onClick={onClose}>Close</button></header>
    <input ref={importInput} hidden type="file" accept=".json" aria-label="Import workflow inputs file" onChange={e=>{if(e.target.files?.[0])void importConfiguration(e.target.files[0]);}}/>
    {error?<div className="template-error"><p role="alert">{error}</p>{!catalog.length?<button type="button" onClick={()=>{setLoading(true);setAttempt(x=>x+1);}}>Retry</button>:null}</div>:null}
    {loading?<p role="status">Loading workflows…</p>:<div className="template-layout">
      <aside aria-label="Template catalog"><label>Find a workflow<input type="search" value={query} onChange={e=>setQuery(e.target.value)} placeholder="Proposal, pipeline, account research…"/></label><label>Category<select value={category} onChange={e=>setCategory(e.target.value)}>{["All","Revenue","Operations","Development","Security"].map(c=><option key={c}>{c}</option>)}</select></label>
        <p className="template-meta">{visible.length} of {catalog.length} workflows · {catalog.filter(t=>t.status==="runnable").length} available</p>
        <nav aria-label="Choose a template">{visible.map(t=><button type="button" key={t.id} disabled={busy} aria-current={selected===t.id?"true":undefined} onClick={()=>choose(t.id)}><strong>{t.title}</strong><span>{t.job}</span><small>{t.status==="runnable"?"Available · supplied data":"Preview · adapter in development"}</small></button>)}</nav>{!visible.length?<p>No matching workflows.</p>:null}
      </aside>
      {template?<article className="template-detail" aria-label="Template details"><span className="eyebrow">{template.category} · {template.publisher} · v{template.version}</span><h3>{template.title}</h3><p>{template.job}</p><p className="template-meta">For {template.persona}</p>
        <section className="template-example" aria-label="Example outcome"><h4>Example outcome</h4><p>{template.illustrative_example}</p><small>Illustrative example. A run on your inputs produces its own evidence and artifacts.</small></section>
        {template.example_result?<details className="template-sample"><summary>Open sample deliverable</summary><p>{template.example_result.summary??template.example_result.explanation}</p>{template.example_result.rows?.map(row=><div className="template-result-row" key={row.id}><strong>{row.id}</strong> <small>{row.status}</small><p>{row.text}</p></div>)}{template.example_result.changes?.map(change=><div key={change.path}><strong>{change.path}</strong><pre>{change.content}</pre></div>)}<small>Manually authored illustration using the bundled fictional sample. No quality or speed comparison has been measured for this package.</small></details>:null}
        <details><summary>What this workflow does</summary><p>{template.execution}</p><h4>Checks</h4><p>{template.acceptance}</p><h4>Missing information</h4><p>{template.failure}</p><h4>Intended deliverables</h4><p>{template.artifacts}</p></details>
        {template.status==="preview"?<p className="template-unavailable" role="status">This job is defined, but its tested real-input adapter is still in development. It cannot be run yet.</p>:<>
          <div className="template-actions"><h4>Use your data</h4><button type="button" className="secondary-button" disabled={busy} onClick={sample}>Use sample inputs</button><button type="button" className="secondary-button" disabled={busy} onClick={()=>importInput.current?.click()}>Import saved inputs</button></div>
          {template.id==="test"?<label>Installed repository verifier<select defaultValue="" disabled={busy||!verifiers.length} onChange={e=>void repository(e.target.value)}><option value="" disabled>Choose a reviewed snapshot</option>{verifiers.map(v=><option key={v.id} value={v.id}>{v.description}</option>)}</select><small>{verifiers.length?"Loads the exact reviewed files and patch permissions. Protected expectations stay private.":"Install a reviewed Python API verifier in OMAR’s workflow-verifiers directory first. The templates/repository-example.json fixture shows the required format and needs a locally installed, digest-pinned Docker image."}</small></label>:null}
          <fieldset disabled={busy}><label>What should this accomplish?<textarea value={goal} maxLength={4000} onChange={e=>setGoal(e.target.value)} placeholder={template.job}/></label>
            <label>Add source files<input type="file" accept=".md,.txt" multiple onChange={e=>void upload(e.target.files)}/></label>
            {sources.map((source,index)=><div className="template-source" key={source.id}><div className="template-actions"><strong>{source.title} <small>({source.id})</small></strong><button type="button" className="secondary-button" disabled={sources.length===1} onClick={()=>setSources(current=>current.filter((_,i)=>i!==index))}>Remove source</button></div><label>Source {index+1} text<textarea value={source.text} onChange={e=>setSources(current=>current.map((s,i)=>i===index?{...s,text:e.target.value}:s))}/></label><label>Evidence valid until (optional)<input type="date" value={source.valid_until??""} onChange={e=>setSources(current=>current.map((s,i)=>i===index?{...s,valid_until:e.target.value||null}:s))}/></label></div>)}
            <button type="button" className="secondary-button" onClick={()=>setSources(current=>[...current,{id:`source-${crypto.randomUUID()}`,title:`Source ${current.length+1}`,text:""}])}>Add another source</button>
            <TemplateFields key={template.id} template={template} fields={fields} sources={sources} onChange={(key,value)=>setFields(current=>({...current,[key]:value}))}/>
            <label>Agent backend<select value={backend} onChange={e=>setBackend(e.target.value)}><option value="codex">Codex</option><option value="claude">Claude</option></select></label>
          </fieldset>
          <p className="template-meta">Requires a configured backend and its supplied-data execution profile. Sources are passed to that provider when you confirm the run. Up to four agent steps, one correction, and 100 KB of inputs. No sending or publishing.</p>
          <div className="template-prepare"><button type="button" className="primary-button" disabled={busy||!goal.trim()||sources.some(s=>!s.text.trim())||template.fields.some(f=>!fields[f.key]?.trim())||template.id==="test"&&!verifiers.length} onClick={()=>void prepare()}>{busy?"Preparing…":"Prepare workflow"}</button><button type="button" className="secondary-button" disabled={busy} onClick={exportConfiguration}>Save inputs</button><small>Creates a named review chat. You confirm execution there.</small></div>
        </>}
      </article>:null}
    </div>}
  </dialog>;
}
