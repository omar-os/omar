/** RFC-style CSV parsing for bounded, user-supplied tables. No formula evaluation. */
export function parseCsv(text:string):Record<string,string>[] {
  if(new TextEncoder().encode(text).length>100_000)throw new Error("CSV exceeds 100 KB.");
  const records:string[][]=[];let row:string[]=[];let value="";let quoted=false;let closed=false;
  const field=()=>{row.push(value);value="";closed=false;};
  const record=()=>{field();if(row.some(v=>v!==""))records.push(row);row=[];};
  for(let i=0;i<text.length;i++){
    const c=text[i];
    if(quoted){if(c==='"'){if(text[i+1]==='"'){value+='"';i++;}else{quoted=false;closed=true;}}else value+=c;continue;}
    if(c===","){field();continue;}
    if(c==="\n"||c==="\r"){if(c==="\r"&&text[i+1]==="\n")i++;record();continue;}
    if(closed)throw new Error("Unexpected characters after a quoted CSV field.");
    if(c==='"'){if(value)throw new Error("Quote inside an unquoted CSV field.");quoted=true;}else value+=c;
  }
  if(quoted)throw new Error("Unclosed CSV quote.");
  if(value||row.length||closed)record();
  const headers=records.shift()?.map((v,i)=>(i===0?v.replace(/^\uFEFF/,""):v).trim());
  if(!headers?.length||headers.some(h=>!h)||new Set(headers).size!==headers.length)throw new Error("CSV needs unique, nonempty column names.");
  if(!records.length||records.length>500)throw new Error("CSV needs 1–500 data rows.");
  return records.map((record,i)=>{if(record.length!==headers.length)throw new Error(`CSV row ${i+2} has the wrong number of cells.`);return Object.fromEntries(headers.map((h,i)=>[h,record[i]]));});
}

export function importTemplateCsv(field:string,text:string):unknown[] {
  const rows=parseCsv(text);const ids=new Set<string>();
  return rows.map(row=>{
    if(!row.id||ids.has(row.id))throw new Error("Every CSV row needs a unique id.");ids.add(row.id);
    if(field==="questions"){
      if(!row.question)throw new Error("Question CSV requires id,question; optional allowed_source_ids uses semicolons.");
      return {id:row.id,question:row.question,allowed_source_ids:(row.allowed_source_ids??"").split(";").map(s=>s.trim()).filter(Boolean)};
    }
    if(field==="accounts"){
      if(!row.name||!["true","false"].includes(row.excluded))throw new Error("Account CSV requires id,name,excluded (true/false), then criterion columns (true/false/blank).");
      const criteria=Object.fromEntries(Object.entries(row).filter(([key])=>!["id","name","excluded"].includes(key)).map(([key,value])=>{if(!["true","false",""].includes(value))throw new Error(`Criterion ${key} must be true, false or blank.`);return [key,value===""?null:value==="true"];}));
      return {id:row.id,name:row.name,excluded:row.excluded==="true",criteria};
    }
    throw new Error("This field does not support CSV import.");
  });
}
