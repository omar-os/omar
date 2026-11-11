import test from 'node:test';
import assert from 'node:assert/strict';
import { responseGuide, exclusiveOutputs, prettyValue } from '../app/lib/response-guide.ts';
const invocation = { agent:'prepare.writer', prompt:'Prepare the report. Full details follow.', contract:'( prepare.packet | prepare.blocked )', allowed_effects:{'prepare.packet':'string','prepare.blocked':'string'}, trigger_values:{} };
test('double-encoded values display decoded', () => {
  const value = JSON.stringify(JSON.stringify({ mode:'draft', projectRoot:null }));
  assert.doesNotMatch(prettyValue(value), /\\"/);
});
test('alternative outcomes do not misrepresent conjunctions or nested contracts', () => {
  assert.deepEqual(exclusiveOutputs(invocation), ['prepare.packet','prepare.blocked']);
  assert.equal(exclusiveOutputs({...invocation,contract:'prepare.packet , prepare.blocked'}), null);
  assert.equal(exclusiveOutputs({...invocation,contract:'( prepare.packet | prepare.blocked ), other'}), null);
});
test('copy comes only from the invocation, never from request fields or names', () => {
  const single = {...invocation, contract:'prepare.report_draft', allowed_effects:{'prepare.report_draft':'string'}};
  assert.equal(responseGuide(single).title, 'Provide report draft');
  assert.equal(responseGuide(invocation).title, 'Submit a response');
  assert.equal(responseGuide(invocation).summary, 'Prepare the report.');
  const request = {...invocation, agent:'prepare.chatgpt', trigger_values:{request:{productionAuthorized:false, dateNewYork:null, storyBrief:null}}};
  assert.deepEqual(responseGuide(request), responseGuide(invocation));
});
