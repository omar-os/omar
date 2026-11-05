import test from 'node:test';
import assert from 'node:assert/strict';
import { responseGuide, exclusiveOutputs, prettyValue, guideForFiles } from '../app/lib/response-guide.ts';
const invocation = { agent:'prepare.chatgpt', prompt:'Prepare the report. Full details follow.', contract:'( prepare.packet | prepare.blocked )', allowed_effects:{'prepare.packet':'string','prepare.blocked':'string'}, trigger_values:{} };
test('recognizes a setup-only blocker through historical double-encoded input', () => {
  const value = JSON.stringify(JSON.stringify({ mode:'setup_review_only', productionAuthorized:false, projectRoot:null, dateNewYork:null, slot:null }));
  const guide = responseGuide({...invocation,trigger_values:{request:value}});
  assert.equal(guide.title, 'Project details required');
  assert.equal(guide.setupOnly, true);
  assert.equal(guide.blocked, 'prepare.blocked');
  assert.match(guide.next, /does not submit/);
  assert.doesNotMatch(prettyValue(value), /\\"/);
});
test('alternative outcomes do not misrepresent conjunctions or nested contracts', () => {
  assert.deepEqual(exclusiveOutputs(invocation), ['prepare.packet','prepare.blocked']);
  assert.equal(exclusiveOutputs({...invocation,contract:'prepare.packet , prepare.blocked'}), null);
  assert.equal(exclusiveOutputs({...invocation,contract:'( prepare.packet | prepare.blocked ), other'}), null);
});
test('explicit response descriptions do not invent a schema for legacy string fields', () => {
  const guides=guideForFiles({'omar.response.json':JSON.stringify({steps:{'prepare.chatgpt':{title:'Paste your approved report',outcomes:{'prepare.packet':{format:'json'}}}}})});
  assert.equal(responseGuide(invocation,guides['prepare.chatgpt']).title, 'Paste your approved report');
  assert.deepEqual(responseGuide(invocation).outcomes, {});
});

test('malformed optional metadata falls back without crashing the panel', () => {
  assert.deepEqual(guideForFiles({'omar.response.json':'{"steps":{"a":null,"b":{"title":{},"outcomes":{"x":{"label":{}}}}}}'}), {b:{outcomes:{x:{}}}});
});

test('unrelated setup workflows do not inherit campaign-specific requirements', () => {
  const guide = responseGuide({...invocation, trigger_values:{request:{productionAuthorized:false}}});
  assert.equal(guide.title, 'Setup details required');
  assert.doesNotMatch(guide.instructions.join(' '), /New York|story|project folder/);
});
