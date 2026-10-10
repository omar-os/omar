import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { locateStep, replaceStep } from '../app/lib/step-source.ts';
import { readConnectionPrompt, writeConnectionPrompt, validateConnection } from '../app/lib/task-connections.ts';
import { configurationDraftKey, readConfigurationDraft, saveConfigurationDraft } from '../app/lib/configuration-draft.ts';
const snapshot = JSON.parse(readFileSync(new URL('./fixtures/diagram-snapshot.v1.json', import.meta.url)));
const program = `team ReviewFlow[planner:Codex, reviewer:Codex] {
 input request:string
 output result:string
 action plan:string
 action critique:string
 /* prompt fake() -> result "fake" /* nested */ */
 prompt planner(request) -> plan "Use $(request). A \\"quoted\\" word and prompt text."
 prompt reviewer(plan) -> critique within(30s) "Inspect $(plan)."
 prompt planner(critique) -> result "Return $(critique)."
}
main ReviewFlow { flow = ReviewFlow() }`;

test('step spans distinguish repeated agents, deadlines, comments and escaped quotes', () => {
 const steps = snapshot.reactions.map(reaction => locateStep(program, snapshot, reaction));
 assert.ok(steps.every(Boolean));
 assert.equal(steps[0].prompt, 'Use $(request). A "quoted" word and prompt text.');
 assert.equal(steps[1].contract, 'critique');
 const changed = replaceStep(program, steps[1], { backend:'ClaudeCode', prompt:'Check "the plan" at C:\\work', triggers:['plan','request'], contract:'critique?' });
 const edited = locateStep(changed, snapshot, snapshot.reactions[1]);
 assert.equal(edited.backend, 'ClaudeCode');
 assert.equal(edited.prompt, 'Check "the plan" at C:\\work');
 assert.deepEqual(edited.triggers, ['plan','request']);
 assert.equal(edited.contract, 'critique?');
 assert.match(changed, /within\(30s\)/);
 assert.equal(locateStep(changed, snapshot, snapshot.reactions[0]).prompt, steps[0].prompt);
 assert.equal(locateStep(changed, snapshot, snapshot.reactions[2]).prompt, steps[2].prompt);
});

test('qualified reaction names select the correct team definition', () => {
 const reaction = {...snapshot.reactions[0], name:'flow.reaction.0'};
 const source = 'team Unrelated[a:Web] { input x:string output y:string prompt a(x) -> y "Unrelated" }\n'+program;
 assert.equal(locateStep(source, snapshot, reaction).prompt, 'Use $(request). A "quoted" word and prompt text.');
});

test('unsupported or ambiguous source remains for the source editor', () => {
 assert.equal(locateStep(program.replace('"Inspect $(plan)."','{= println!("hi"); =}'), snapshot, snapshot.reactions[0]), null);
 assert.equal(locateStep(program, snapshot, {...snapshot.reactions[0], name:'reaction.99'}), null);
 assert.equal(locateStep(program, snapshot, {...snapshot.reactions[0], agent:'agent::unknown'}), null);
});

const api = { id: 'api-1', name: 'Content API', type: 'http', url: 'https://api.example.com/v1/items?limit=10', method: 'GET', auth: { type: 'bearer', variable: 'CONTENT_API_TOKEN' } };
const webhook = { id: 'hook-1', name: 'Notifications', type: 'webhook', url: 'https://example.com/hooks/team', method: 'POST', auth: { type: 'api-key', variable: 'HOOK_KEY', header: 'X-API-Key' } };
const mcp = { id: 'mcp-1', name: 'Project tools', type: 'mcp', server: 'github' };

test('external connections round trip through executable source without changing triggers or other tasks', () => {
 const step = locateStep(program, snapshot, snapshot.reactions[1]);
 const connections = [api, webhook, mcp];
 const changed = replaceStep(program, step, { ...step, connections });
 const edited = locateStep(changed, snapshot, snapshot.reactions[1]);
 assert.deepEqual(edited.connections, connections);
 assert.equal(edited.prompt, step.prompt);
 assert.deepEqual(edited.triggers, step.triggers);
 assert.match(changed, /CONTENT_API_TOKEN/);
 assert.match(changed, /execution agent using its network tools/);
 assert.equal(locateStep(changed, snapshot, snapshot.reactions[0]).prompt, locateStep(program, snapshot, snapshot.reactions[0]).prompt);
 const changedPrompt = replaceStep(changed, edited, { backend: edited.backend, prompt: 'New task', contract: edited.contract, triggers: edited.triggers });
 assert.deepEqual(locateStep(changedPrompt, snapshot, snapshot.reactions[1]).connections, connections);
 const removed = replaceStep(changed, edited, { ...edited, connections: [] });
 assert.equal(removed, program);
});

test('connection configuration rejects invalid endpoints, credentials, duplicates and prompt interpolation', () => {
 for (const connection of [api, webhook, mcp]) assert.equal(validateConnection(connection), null);
 for (const url of ['example.com', 'javascript:alert(1)', 'https://user:secret@example.com', 'https://example.com/#fragment']) assert.ok(validateConnection({ ...api, url }));
 assert.ok(validateConnection({ ...api, id: 'other', name: ' content api ' }, [api]));
 assert.equal(validateConnection(api, [api]), null);
 assert.ok(validateConnection({ ...api, name: '$(request)' }));
 assert.ok(validateConnection({ ...api, auth: { type: 'bearer', variable: 'sk-secret-value' } }));
 assert.ok(validateConnection({ ...webhook, auth: { ...webhook.auth, header: 'X-API-Key\r\nInjected: yes' } }));
 assert.ok(validateConnection({ ...webhook, method: 'GET' }));
 assert.ok(validateConnection({ ...mcp, server: '' }));
 assert.ok(validateConnection({ ...mcp, server: undefined }));
 assert.throws(() => writeConnectionPrompt('Task', [{ ...api, url: 'invalid' }]));
});

test('malformed connection metadata is preserved for source editing instead of being silently dropped', () => {
 const prompt = 'Task with a "quote", slash \\, and $(request).';
 assert.deepEqual(readConnectionPrompt(writeConnectionPrompt(prompt, [api])), { prompt, connections: [api] });
 assert.deepEqual(readConnectionPrompt(prompt), { prompt, connections: [] });
 const malformed = writeConnectionPrompt(prompt, [api]).replace('"bearer"', '"unknown"');
 assert.throws(() => readConnectionPrompt(malformed), /source editor/);
 const step = locateStep(program, snapshot, snapshot.reactions[0]);
 const source = replaceStep(program, step, { ...step, connections: [api] }).replace('CONTENT_API_TOKEN', 'invalid-token');
 assert.equal(locateStep(source, snapshot, snapshot.reactions[0]), null);
});

test('applied configurations survive reloads and are isolated by runtime, chat and proposal', () => {
 const values = new Map();
 const storage = { getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, value) };
 const key = configurationDraftKey('http://localhost:7340/', 'chat-1', 4);
 assert.equal(key, configurationDraftKey('http://localhost:7340', 'chat-1', 4));
 saveConfigurationDraft(storage, key, program, 'edited program');
 assert.equal(readConfigurationDraft(storage, key, program), 'edited program');
 assert.equal(readConfigurationDraft(storage, key, 'new proposal'), 'new proposal');
 for (const other of [configurationDraftKey('http://localhost:7341', 'chat-1', 4), configurationDraftKey('http://localhost:7340', 'chat-2', 4), configurationDraftKey('http://localhost:7340', 'chat-1', 5)]) assert.equal(readConfigurationDraft(storage, other, program), program);
 values.set(key, 'corrupt');
 assert.equal(readConfigurationDraft(storage, key, program), program);
 assert.throws(() => saveConfigurationDraft({ setItem() { throw new Error('quota'); } }, key, program, 'edit'), /Could not save/);
});
