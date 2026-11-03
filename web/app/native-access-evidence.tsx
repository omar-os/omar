"use client";
import { useState } from "react";
import type { NativeAccessCandidate } from "./lib/protocol-generated";
import { inspectNativeAccess } from "./lib/workflow-client";

export function NativeAccessEvidence({url,runId,agents}:{url:string;runId:string;agents:string[]}) {
  const [reports,setReports]=useState<Record<string,NativeAccessCandidate>>({});
  const [busy,setBusy]=useState<string | null>(null);
  const [error,setError]=useState("");
  async function inspect(agent:string) {
    setBusy(agent);setError("");
    try {const report=await inspectNativeAccess(url,runId,agent);setReports(current=>({...current,[agent]:report}));}
    catch(cause){setError(cause instanceof Error ? cause.message : String(cause));}
    finally{setBusy(null);}
  }
  return <details><summary>Model access evidence</summary>
    <p>Inspect existing native run records. This makes no new model requests, changes no account settings and does not automatically verify access.</p>
    {agents.map(agent=><div key={agent}><button type="button" className="secondary-button" disabled={busy!==null} onClick={()=>void inspect(agent)}>{busy===agent ? "Inspecting…" : `Inspect access evidence for ${agent}`}</button>
      {reports[agent] ? <><p>{reports[agent].status.replaceAll("_"," ")} · {reports[agent].profile_id}<br />Reported model: {reports[agent].observed_model ?? "Unknown"}. Native requests made by this inspection: {reports[agent].new_model_requests}.</p>
        <small>Runtime: {reports[agent].backend_version ?? "Unknown"}. Scope: {reports[agent].connection_scope ?? "Unknown"}.<br />Evidence digest: {reports[agent].report_digest}</small>
        <ul>{reports[agent].limitations.map(reason=><li key={reason}>{reason}</li>)}</ul></> : null}
    </div>)}
    {error ? <p role="alert">{error}</p> : null}
  </details>;
}
