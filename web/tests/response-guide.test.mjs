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

test('interaction types are explicit; approval-like names do not grant approval controls', () => {
  assert.equal(responseGuide(invocation).kind, 'manual');
  assert.equal(responseGuide(invocation, {kind:'input'}).label, 'Information needed');
  assert.equal(responseGuide(invocation, {kind:'approval'}).action, 'Review step');
  assert.equal(responseGuide({...invocation,trigger_values:{request:{productionAuthorized:false}}}, {kind:'approval'}).kind, 'setup');
});

test('setup links only accept credential-free HTTPS navigation', () => {
  for (const setupUrl of ['javascript:alert(1)', 'data:text/html,test', '/fake', 'https://user:password@example.com', 'not a url']) {
    assert.equal(guideForFiles({'omar.response.json':JSON.stringify({steps:{s:{kind:'setup',setupUrl}}})}).s.setupUrl, undefined);
  }
  assert.equal(guideForFiles({'omar.response.json':JSON.stringify({steps:{s:{kind:'setup',setupUrl:'https://example.com/setup'}}})}).s.setupUrl, 'https://example.com/setup');
});

test('approval returns exactly the artifact for this invocation and requires two valid decision outputs', async () => {
  const {approvalFor} = await import('../app/lib/response-guide.ts');
  const request = {...invocation, contract:'( review.approved | review.changes )', allowed_effects:{'review.approved':'string','review.changes':'string'}, trigger_values:{'review.draft':'Original artifact'}};
  const guide = {kind:'approval',approval:{artifact:'review.draft',approve:'review.approved',changes:'review.changes'}};
  assert.equal(approvalFor(request,guide).value,'Original artifact');
  assert.equal(approvalFor({...request,trigger_values:{}},guide),null);
  assert.equal(approvalFor({...request,contract:'review.approved , review.changes'},guide),null);
  assert.equal(approvalFor({...request,allowed_effects:{...request.allowed_effects,'review.changes':'bool'}},guide),null);
  assert.equal(approvalFor(request,{...guide,approval:{...guide.approval,approve:'other.output'}}),null);
  assert.equal(approvalFor(request,{...guide,kind:'manual'}),null);
});

test('change requests use an actual downstream step, not an unconnected blocker output', async () => {
  const {nextSteps} = await import('../app/lib/response-guide.ts');
  const snapshot = {ports:[{id:'port::review.changes',name:'review.changes'},{id:'port::review.blocked',name:'review.blocked'}],edges:[{source:'port::review.changes',target:'port::revise.feedback'}],reactions:[{triggers:['port::revise.feedback'],title:'Revise the draft'}]};
  assert.deepEqual(nextSteps(snapshot,'review.changes'),['Revise the draft']);
  assert.deepEqual(nextSteps(snapshot,'review.blocked'),[]);
  assert.deepEqual(nextSteps(snapshot,'missing'),[]);
});
