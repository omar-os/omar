"use client";

import { useEffect, useState } from "react";
import { downloadArtifact, fetchTemplateResult, saveTemplateCopy, type TemplateResult } from "./lib/template-client";
import type { RunStatus } from "./lib/protocol";
import "./template-library.css";

export function TemplateResults({url,runId,status,primary=false}:{url:string;runId:string;status:RunStatus;primary?:boolean}) {
  const [result,setResult]=useState<TemplateResult|null>(null);
  const [error,setError]=useState("");
  const [retry,setRetry]=useState(0);
  const [editing,setEditing]=useState(false);
  const [content,setContent]=useState("");
  const [saving,setSaving]=useState(false);
  useEffect(()=>{
    if(!["completed","failed","stopped"].includes(status))return;
    const abort=new AbortController();
    // The run event can precede final receipt publication by a few milliseconds.
    let timer:ReturnType<typeof setTimeout>|undefined;
    async function load(attempt=0){
      try {const value=await fetchTemplateResult(url,runId,abort.signal);
        if(abort.signal.aborted)return;
        if(value.status==="not_available"&&attempt<5){timer=setTimeout(()=>void load(attempt+1),500);return;}
        setResult(value);setError("");
      }catch(cause){if(!abort.signal.aborted)setError(cause instanceof Error?cause.message:String(cause));}
    }
    void load();return ()=>{abort.abort();clearTimeout(timer);};
  },[url,runId,status,retry]);
  async function save(){
    if(!result||saving)return;setSaving(true);setError("");
    try{setResult(await saveTemplateCopy(url,runId,result.edit_revision??0,content));setEditing(false);}
    catch(cause){setError(cause instanceof Error?cause.message:String(cause));}
    finally{setSaving(false);}
  }
  if(!["completed","failed","stopped"].includes(status))return null;
  return <section className={`template-results${primary?" template-results-primary":""}`} aria-label="Workflow deliverables"><div className="template-actions"><h3>Your deliverables</h3>{result?.draft?<span className="template-result-status">{result.status==="needs_information"?"Needs information":"Draft for review"}</span>:null}</div>
    <p className="template-meta">Saved run {runId}. These files belong to that run’s input snapshot.</p>
    {error?<p role="alert">{error} <button type="button" onClick={()=>setRetry(n=>n+1)}>Reload result</button></p>:null}
    {!result&&!error?<p role="status">Loading deliverables…</p>:null}
    {result?.status==="not_available"?<p>No accepted deliverable was saved for this run. Review its execution evidence before trying again.</p>:null}
    {result?.status==="blocked"?<p role="status">Review required: {result.reason}</p>:null}
    {result?.draft?<>
      <p>{result.draft.summary}</p>
      <p className="template-meta">Structure, source quotes and declared calculations passed runtime checks. Answers and recommendations still require review; a matching quote does not prove a claim.</p>
      {result.calculated&&Object.keys(result.calculated).length?<details open><summary>Calculated from your inputs</summary><CalculationView value={result.calculated}/></details>:null}
      <div aria-label="Result records">{result.draft.rows.map(row=><article key={row.id} className="template-result-row"><header><strong>{row.id}</strong><small>{row.status==="cited"?"Source quoted":row.status==="unknown"?"Missing evidence":"Suggested interpretation"}</small></header><p>{row.text}</p>{row.citations.length?<details><summary>Inspect {row.citations.length} source quote{row.citations.length===1?"":"s"}</summary>{row.citations.map((citation,i)=><blockquote key={i}><strong>{citation.source_id}</strong><br/>{citation.quote}</blockquote>)}</details>:null}</article>)}</div>
      {result.draft.limitations.length?<details open><summary>Limitations and next steps</summary><ul>{result.draft.limitations.map((line,i)=><li key={i}>{line}</li>)}</ul></details>:null}
      <div className="template-downloads" aria-label="Download deliverables">{result.artifacts.map(artifact=><button key={artifact.name} type="button" className="secondary-button" onClick={()=>downloadArtifact(artifact)}>Download {artifact.name}</button>)}</div>
      <details><summary>Artifact versions</summary><p className="template-meta">The original is retained. Edited copies are saved separately and have no validation badge.</p>{result.copies?.map(copy=><div key={copy.revision} className="template-actions"><span>Edited copy {copy.revision} · unchecked</span><button type="button" className="secondary-button" onClick={()=>downloadArtifact(copy.artifact)}>Download copy {copy.revision}</button></div>)}</details>
      {!editing?<button type="button" className="secondary-button" onClick={()=>{setContent(result.copies?.at(-1)?.artifact.content??result.artifacts.find(a=>a.name==="report.md")?.content??"");setEditing(true);}}>Edit a copy</button>:<div><label>Edited report<textarea aria-label="Edited report" value={content} disabled={saving} onChange={e=>setContent(e.target.value)}/></label><div className="template-actions"><button type="button" className="primary-button" disabled={saving||!content.trim()} onClick={()=>void save()}>{saving?"Saving…":"Save unchecked copy"}</button><button type="button" disabled={saving} onClick={()=>setEditing(false)}>Cancel edit</button></div></div>}
    </>:null}
  </section>;
}

function CalculationView({value}:{value:Record<string,unknown>}) {
  const metrics=value.metrics as {id:string;label?:string;current:number|null;previous:number|null;delta:number|null}[]|undefined;
  if(metrics)return <table><thead><tr><th>Metric</th><th>Current</th><th>Previous</th><th>Change</th></tr></thead><tbody>{metrics.map(m=><tr key={m.id}><th>{m.label??m.id}</th><td>{m.current??"Unavailable"}</td><td>{m.previous??"Unavailable"}</td><td>{m.delta??"Unavailable"}</td></tr>)}</tbody></table>;
  const accounts=value.accounts as {id:string;name:string;excluded:boolean;confirmed_score:number;unknown_weight:number}[]|undefined;
  if(accounts)return <table><thead><tr><th>Account</th><th>Eligible</th><th>Confirmed score</th><th>Unknown weight</th></tr></thead><tbody>{accounts.map(a=><tr key={a.id}><th>{a.name}</th><td>{a.excluded?"Excluded":"Yes"}</td><td>{a.confirmed_score}</td><td>{a.unknown_weight}</td></tr>)}</tbody></table>;
  if(typeof value.priced_total_before_tax_cents==="number"){const cents=BigInt(value.priced_total_before_tax_cents);return <><p><strong>{value.currency as string} {`${cents/BigInt(100)}.${String(cents%BigInt(100)).padStart(2,"0")}`}</strong> for priced work, before tax.{value.all_lines_priced===false?" Some work is unpriced; this is not a complete total.":""}</p><details><summary>Pricing calculation and line items</summary><pre>{JSON.stringify(value,null,2)}</pre></details></>;}
  return <pre>{JSON.stringify(value,null,2)}</pre>;
}
