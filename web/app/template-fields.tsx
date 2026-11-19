"use client";
import type { TemplateSource, WorkflowTemplate } from "./lib/template-client";
import { useState } from "react";
import { importTemplateCsv } from "./lib/template-data";

type Question={id:string;question:string;allowed_source_ids?:string[]};

/** Ordinary questionnaire editing has named controls; JSON remains an import path. */
export function TemplateFields({template,fields,sources,onChange}:{template:WorkflowTemplate;fields:Record<string,string>;sources:TemplateSource[];onChange:(key:string,value:string)=>void}) {
  const [error,setError]=useState("");
  async function importCsv(key:string,file:File){try{if(file.size>100_000)throw new Error("CSV exceeds 100 KB.");onChange(key,JSON.stringify(importTemplateCsv(key,await file.text()),null,2));setError("");}catch(cause){setError(cause instanceof Error?cause.message:String(cause));}}
  let questions:Question[]|null=null;
  if(template.id==="questionnaire"){
    try{const data=JSON.parse(fields.questions||"[]");if(Array.isArray(data)&&data.every(q=>typeof q.id==="string"&&typeof q.question==="string"))questions=data;}catch{}
  }
  const updateQuestions=(value:Question[])=>onChange("questions",JSON.stringify(value,null,2));
  function addQuestion(){if(!questions)return;let index=questions.length+1;while(questions.some(q=>q.id===`Q-${index}`))index++;updateQuestions([...questions,{id:`Q-${index}`,question:"",allowed_source_ids:[]}]);}
  return <div>{questions?<section aria-label="Questionnaire questions"><h4>Questions and approved evidence</h4><p className="template-meta">Keep your customer’s question IDs. Only selected sources may support each answer; no selection means the answer must stay unknown.</p>{questions.map((question,index)=><div className="template-source" key={index}><label>Question ID<input value={question.id} onChange={e=>updateQuestions(questions!.map((q,i)=>i===index?{...q,id:e.target.value}:q))}/></label><label>Question<textarea value={question.question} onChange={e=>updateQuestions(questions!.map((q,i)=>i===index?{...q,question:e.target.value}:q))}/></label><p>Approved sources</p>{sources.map(source=><label className="template-source-choice" key={source.id}><input type="checkbox" checked={question.allowed_source_ids?.includes(source.id)??false} onChange={e=>updateQuestions(questions!.map((q,i)=>i===index?{...q,allowed_source_ids:e.target.checked?[...new Set([...(q.allowed_source_ids??[]),source.id])]:(q.allowed_source_ids??[]).filter(id=>id!==source.id)}:q))}/>{source.title} <small>({source.id})</small></label>)}<button type="button" className="secondary-button" onClick={()=>updateQuestions(questions!.filter((_,i)=>i!==index))}>Remove question</button></div>)}<button type="button" className="secondary-button" onClick={addQuestion}>Add question</button></section>:null}
    {error?<p role="alert">{error}</p>:null}
    {template.fields.map(field=><div key={field.key}>{["questions","accounts"].includes(field.key)?<label>Import {field.key} CSV<input type="file" aria-label={`Import ${field.key} CSV`} accept=".csv" onChange={e=>{if(e.target.files?.[0])void importCsv(field.key,e.target.files[0]);}}/><small>{field.key==="questions"?"Columns: id, question, allowed_source_ids (separated by semicolons).":"Columns: id, name, excluded, then one column per scoring criterion. Use true, false or blank for unknown criteria."}</small></label>:null}<details open={field.key!=="questions"||!questions}><summary>{field.key==="questions"?"Import or edit question JSON":field.label}</summary><label><small>{field.help}</small><textarea aria-label={field.label} className="template-json" spellCheck={false} value={fields[field.key]??""} onChange={e=>onChange(field.key,e.target.value)}/></label></details></div>)}
  </div>;
}
