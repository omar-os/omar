import { normalizeRuntimeUrl } from "./runtime-client";
import type { ConversationSummary } from "./protocol";

export interface TemplateSource { id:string; title:string; text:string; valid_until?:string|null }
export interface TemplateInput { goal:string; sources:TemplateSource[]; data:Record<string,unknown> }
export interface WorkflowTemplate {
  id:string; number:number; title:string; category:string; wave:number; persona:string;
  job:string; inputs:string; artifacts:string; execution:string; acceptance:string; failure:string;
  implementation_delta:string; illustrative_example:string; version:string; digest:string;
  publisher:string; status:"preview"|"runnable"; adapter:string; sample:TemplateInput;
  fields:{key:string;label:string;help:string}[];
  example_result?:{summary?:string;explanation?:string;rows?:{id:string;text:string;status:string}[];changes?:{path:string;content:string}[]};
}
export interface TemplateArtifact {name:string;content:string;digest:string}
export interface TemplateResult {
  status:string; reason?:string; template_id?:string; run_id?:string; exact_checks?:string;
  semantic_acceptance?:string; calculated?:Record<string,unknown>;
  draft?:{summary:string;rows:{id:string;text:string;status:string;citations:{source_id:string;quote:string}[]}[];limitations:string[]};
  artifacts:TemplateArtifact[];
  edit_revision?:number; copies?:{revision:number;artifact:TemplateArtifact}[];
}
async function request<T>(url:string,path:string,body?:unknown,signal?:AbortSignal):Promise<T> {
  const response=await fetch(`${normalizeRuntimeUrl(url)}/v1/templates${path}`,{method:body===undefined?"GET":"POST",headers:body===undefined?undefined:{"content-type":"application/json"},body:body===undefined?undefined:JSON.stringify(body),signal:signal??AbortSignal.timeout(30_000),cache:"no-store"});
  const value=await response.json();
  if(!response.ok)throw new Error(value.error??`Template request failed (${response.status})`);
  return value as T;
}
export const fetchTemplates=(url:string,signal?:AbortSignal)=>request<{templates:WorkflowTemplate[];repository_verifiers?:{id:string;description:string}[];repository_error?:string}>(url,"",undefined,signal);
export const prepareTemplate=(url:string,template:WorkflowTemplate,input:TemplateInput,backend:string,request_id:string)=>request<{conversation:ConversationSummary;replayed:boolean}>(url,"/prepare",{template_id:template.id,template_digest:template.digest,request_id,backend,input});
export const fetchTemplateResult=(url:string,run_id:string,signal?:AbortSignal)=>request<TemplateResult>(url,"/result",{run_id},signal);
export const saveTemplateCopy=(url:string,run_id:string,revision:number,content:string)=>request<TemplateResult>(url,"/copy",{run_id,revision,content});
export const fetchRepositoryInput=(url:string,id:string)=>request<Record<string,unknown>>(url,"/repository",{id});

export function parseTemplateData(template:WorkflowTemplate,fields:Record<string,string>):Record<string,unknown> {
  return Object.fromEntries(template.fields.map(field=>{
    try {return [field.key,JSON.parse(fields[field.key]??"")] as const;}
    catch {throw new Error(`${field.label}: enter valid JSON or load the sample inputs.`);}
  }));
}

export function downloadArtifact(artifact:TemplateArtifact) {
  const media=artifact.name.endsWith(".json")?"application/json":artifact.name.endsWith(".csv")?"text/csv":"text/markdown";
  const url=URL.createObjectURL(new Blob([artifact.content],{type:`${media};charset=utf-8`}));
  const link=document.createElement("a");link.href=url;link.download=artifact.name;link.click();
  setTimeout(()=>URL.revokeObjectURL(url),1000);
}
