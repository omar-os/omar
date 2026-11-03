import test from "node:test";
import assert from "node:assert/strict";
import { eligibleExecutionProfiles } from "../app/lib/execution-choices.ts";
import { catalog } from "./workflow-fixture.mjs";

test("a shared session's profile must support every step and tool", () => {
  const plan = {profiles:{}, draft:{preferences:{allowed_backends:["codex"]},steps:[
    {agent:"flow.writer",operation:"structured_extraction",required_tools:["files"]},
    {agent:"flow.writer",operation:"evidence_synthesis",required_tools:["files"]},
  ]}};
  const eligible = () => eligibleExecutionProfiles(plan,"flow.writer",catalog.profiles,"Codex").map(p=>p.id);
  assert.deepEqual(eligible(),["codex-quality"]);
  plan.draft.steps.pop();
  assert.deepEqual(eligible(),["codex-quality","codex-economy"]);
  plan.draft.steps[0].required_tools.push("video-renderer");
  assert.deepEqual(eligible(),[]);
  plan.draft.steps[0].required_tools.pop();
  plan.draft.steps[0].qualification_scope = {input_modalities:["text"],output_modalities:["video"]};
  assert.deepEqual(eligible(),[]);
  plan.draft.steps[0].qualification_scope.output_modalities = ["structured_data"];
  assert.deepEqual(eligible(),["codex-quality","codex-economy"]);
});

test("unavailable profiles and backend restrictions are visible as missing choices", () => {
  const profiles=structuredClone(catalog.profiles);
  const plan={profiles:{},draft:{preferences:{allowed_backends:["claude"]},steps:[{agent:"flow.worker",operation:"code_repair",required_tools:[]}]}};
  assert.equal(eligibleExecutionProfiles(plan,"flow.worker",profiles,"ClaudeCode")[0].id,"claude-quality");
  profiles.find(p=>p.id==="claude-quality").access="unavailable";
  assert.deepEqual(eligibleExecutionProfiles(plan,"flow.worker",profiles,"ClaudeCode"),[]);
  assert.deepEqual(eligibleExecutionProfiles(plan,"flow.worker",profiles,"Codex"),[]);
});
