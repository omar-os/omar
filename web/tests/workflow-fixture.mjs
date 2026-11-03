import { createHash, randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
export const catalog = JSON.parse(readFileSync(new URL("../../config/execution-profiles.example.json", import.meta.url)));
export const sourceDigest = source => createHash("sha256").update(source).digest("hex");
export function workflowFixture(program, inputs, preview, conversation, answered = false) {
  const profile = catalog.profiles.find(p => p.id === "codex-quality");
  const agents = preview.agents.filter(a => a.backend.toLowerCase() !== "web").map(a => a.id.replace(/^agent::?/, ""));
  return { schema_version:1, id:randomUUID(),revision:1,conversation_id:conversation,created_at:Date.now()/1000,request_id:randomUUID(),program,inputs,source_digest:sourceDigest(program),evidence_revision:"test-fixture",routing_policy_revision:"workflow-routing-v10",
    draft:{intent:{outcome:"A grounded release recommendation",message_sequences:[1],constraints:[],requirements:[],assumptions:[{id:"internal",text:"The brief is for an internal audience",provenance:"designer inference",origin:{kind:"system_proposed",reason:"Review the intended audience."},confirmation:null,accepted:false}],acceptance_criteria:["Every recommendation is supported by supplied evidence"]},research:{session_id:"request-1",usage:researchUsage(),summary:"Review the supplied release evidence and its limitations.",sources:[{id:"request",location:"input:flow.request",finding:inputs["flow.request"] ?? "",provenance:"supplied input snapshot",origin:{kind:"source_observed",input_port:"flow.request",json_pointer:"",quote:inputs["flow.request"] ?? "",content_digest:sourceDigest(inputs["flow.request"] ?? ""),source_reported_date:null}}],gaps:[],issues:[],capability_evidence_ids:[]},steps:preview.reactions.map((r,index)=>({logical_id:`fixture-step-${index}`,decomposition_reason:"Separate evidence review from the independent acceptance check.",interface:stepInterface(r,index,preview),resources:{max_input_bytes:262144,max_prompt_bytes:524288,max_output_bytes:262144,max_model_input_tokens:null,max_model_output_tokens:null,max_cost_usd:null},reaction:r.id.replace(/^reaction::?/, ""),agent:r.agent.replace(/^agent::?/, ""),operation:"structured_extraction",purpose:"Review evidence",acceptance_criteria:["Supported claims"],required_tools:[],evidence_ids:[],verification_reaction:null,execution:{effect:"repeatable",max_invocations:1,deadline_ms:null},recovery:null,qualification_scope:{task_family:"grounded-documents",task_slice:"supplied-fact-extraction",contract_version:"v1",complexity:"routine",consequence:"low",input_modalities:["text"],output_modalities:["structured_data"]}})),questions:answered?[]:[{id:"audience",question:"Who will use this brief?",reason:"The audience changes the level of detail.",target_reactions:[preview.reactions[0].id.replace(/^reaction::?/, "")],options:["Engineering leads","Product team"],proposed_default:"Engineering leads",required:true,answer:null}],preferences:{allowed_backends:[],overrides:{}}},
    profiles:Object.fromEntries(agents.map(a=>[a,profile])),selections:agents.map(agent=>({agent,profile_id:profile.id,evidence_ids:[],reason:"Provisional quality candidate; local qualification is pending.",provisional:true,tool_access:[],capability_blockers:[],repair_routes:[],model_access:{status:"unknown",reason:"No reviewed native request observation.",last_success_at:null,expires_at:null,observed_model:null,observed_effort:null},facts:{advertised_limits:"unknown",economics:"unknown",session_behavior:"persistent_per_runtime_agent",structured_output_control:"host_checks_before_handoff_when_declared",token_limits_enforced:false,spend_limits_enforced:false}}))};
}

// Synthetic host-ledger fixture for rendering coverage and limits; no model measurement.
function researchUsage() {
  return {session_id:"request-1",operator_sequence:1,limits:{catalog_calls:2,source_calls:3,evidence_bytes:65536,per_source_bytes:16384,wall_time_ms:300000,adapter_spend_usd:0},started_at_ms:1793610000000,finished_at_ms:1793610010000,catalog_calls:2,source_calls:2,failed_calls:1,evidence_bytes:800,stop_reason:"proposal_submitted",adapter_cost_usd:0,designer_cost_usd:null,external_retrieval_supported:false,captures:[{request_id:"source",location:"Supplied release notes",captured_at_ms:1793610005000,content_digest:sourceDigest("partial evidence"),full_content_digest:sourceDigest("partial evidence and more"),evidence_bytes:16,full_content_bytes:20000,offset:0,truncated:true}],errors:["external: External retrieval is unsupported; no network request was made."]};
}

// Rendering fixture only; these are not actual provider prices, quotas or limits.
export function fixtureProfileFacts(plan, status, lifetimeSeconds = 3600) {
  const at=Math.floor(Date.now()/1000);
  const source={source:"test://synthetic-profile-facts",source_digest:sourceDigest("fixture"),locator:"Synthetic row 1",observed_at:at-100,reviewed_at:at-90,expires_at:status==="current" ? at+lifetimeSeconds : at-1,reviewer:"Fixture reviewer",conditions:"Synthetic data for UI testing only."};
  for(const [agent,original] of Object.entries(plan.profiles)) {
    const profile=structuredClone(original);
    profile.identity={declared_provider:"fixture-provider",model_reference:"alias"};
    profile.advertised_limits={profile_digest:"fixture",context_window_tokens:8000,max_output_tokens:2000,source};
    profile.economics={profile_digest:"fixture",billing_model:"subscription",subscription_plan:"Fixture plan",prices:[{unit:"input_token",quantity:1000,price_usd:0.001,conditions:"Synthetic supplemental rate"}],quota:{unit:"fixture calls",limit:10,remaining:0,window_seconds:3600,resets_at:null},connection_scope:"fixture",source};
    plan.profiles[agent]=profile;
    const selection=plan.selections.find(s=>s.agent===agent);
    if(selection) selection.facts={advertised_limits:status,economics:status,session_behavior:"persistent_per_runtime_agent",structured_output_control:"host_checks_before_handoff_when_declared",token_limits_enforced:false,spend_limits_enforced:false};
  }
}

function stepInterface(reaction,index,preview) {
  const clean = id => id.replace(/^(port|reaction|agent)::?/, "");
  const logical = id => `fixture-step-${preview.reactions.findIndex(r=>r.id===id)}`;
  const inputs=reaction.triggers.map(id=>({port:clean(id),wire_type:"string",schema:{type:"string"},producer_steps:preview.reactions.filter(r=>r.effects.includes(id)).map(r=>logical(r.id)),supplied_input:preview.ports.some(p=>p.id===id && p.kind==="input"),timer:false}));
  return {version:1,taxonomy_revision:"workflow-operations-v1",owner:"model",anchor_digest:"fixture",implementation_digest:"fixture",inputs,outputs:reaction.effects.map(id=>({port:clean(id),wire_type:"string",schema:{type:"string"},consumer_steps:preview.reactions.filter(r=>r.triggers.includes(id)).map(r=>logical(r.id)),exposed_outputs:preview.ports.filter(p=>p.id===id && p.kind==="output").map(p=>clean(p.id)),checks:[]})),dependencies:inputs.flatMap(i=>i.producer_steps),state_variables:[],parameters:[],shared_state_steps:[],activation:"any_present_trigger",effect_contract:reaction.contract,prompt_template_bytes:32,known_initial_trigger_bytes:index===0 ? 20 : null,context_coverage:"Invocation payload only; native session history, system prompts, tools, provider tokens and cost are not bounded or measured here."};
}

// Source-row display fixture only; this is not an imported real benchmark.
export function fixtureBenchmarkEvidence() {
  const record=JSON.parse(readFileSync(new URL("../../evals/workflow-quality/benchmark-import/record.example.json",import.meta.url),"utf8"));
  const at=Math.floor(Date.now()/1000);
  record.reviewed_at=at-1;record.expires_at=at+4;
  record.benchmark_result.review={accepted:true,reviewer:"Synthetic browser reviewer",reviewed_at:at-1,rationale:"Synthetic display check only",content_digest:"fixture"};
  return record;
}
