import test from "node:test";
import assert from "node:assert/strict";
import {parseCsv,importTemplateCsv} from "../app/lib/template-data.ts";

test("CSV preserves quoted delimiters, newlines and text that resembles formulas",()=>{
  assert.deepEqual(parseCsv('id,question\r\nQ1,"Is this, \"\"quoted\"\"?\nNext line"\r\nQ2,=1+1'),[{id:"Q1",question:'Is this, "quoted"?\nNext line'},{id:"Q2",question:"=1+1"}]);
});
test("CSV refuses ambiguous rows and duplicate question IDs",()=>{
  for(const text of ['id,id\na,b','id,question\nq1','id,question\nq1,"unterminated','id,question\nq1,"closed"junk'])assert.throws(()=>parseCsv(text));
  assert.throws(()=>importTemplateCsv("questions","id,question\nq1,one\nq1,two"));
});
test("CSV approval and unknown criteria remain explicit",()=>{
  assert.deepEqual(importTemplateCsv("questions","id,question\nq1,Certified?"),[{id:"q1",question:"Certified?",allowed_source_ids:[]}]);
  assert.deepEqual(importTemplateCsv("accounts","id,name,excluded,industry,size\na,Acme,false,true,"),[{id:"a",name:"Acme",excluded:false,criteria:{industry:true,size:null}}]);
  assert.throws(()=>importTemplateCsv("accounts","id,name,excluded,industry\na,Acme,false,probably"));
});
