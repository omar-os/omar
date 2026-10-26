import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { locateStep, replaceStep } from '../app/lib/step-source.ts';
import { taskCopy, shortTaskText } from '../app/lib/task-copy.ts';
const snapshot = JSON.parse(readFileSync(new URL('./fixtures/diagram-snapshot.v1.json', import.meta.url)));
const program = `team ReviewFlow[planner:Codex, reviewer:Codex] {
 input request:string
 output result:string
 action plan:string
 action critique:string
 /* task fake() -> result "fake" /* nested */ */
 task planner(request) -> plan "Use $(request). A \\"quoted\\" word and prompt text."
 task reviewer(plan) -> critique within(30s) "Inspect $(plan)."
 task planner(critique) -> result "Return $(critique)."
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
 const source = 'team Unrelated[a:Web] { input x:string output y:string task a(x) -> y "Unrelated" }\n'+program;
 assert.equal(locateStep(source, snapshot, reaction).prompt, 'Use $(request). A "quoted" word and prompt text.');
});

test('unsupported or ambiguous source remains for the source editor', () => {
 assert.equal(locateStep(program.replace('"Inspect $(plan)."','{= println!("hi"); =}'), snapshot, snapshot.reactions[0]), null);
 assert.equal(locateStep(program, snapshot, {...snapshot.reactions[0], name:'reaction.99'}), null);
 assert.equal(locateStep(program, snapshot, {...snapshot.reactions[0], agent:'agent::unknown'}), null);
});

test('editing task configuration preserves metadata and the other steps', () => {
 const metadata = 'display("Review campaign script", "Inspect the prompt, identify gaps, and return a critique.")';
 for (const deadline of ['', 'within(30s)']) {
  const source = program.replace('within(30s)', `${deadline} ${metadata}`);
  const step = locateStep(source, snapshot, snapshot.reactions[1]);
  assert.equal(step.prompt, 'Inspect $(plan).');
  assert.equal(step.contract, 'critique');
  const changed = replaceStep(source, step, {...step, prompt:'Review $(plan) carefully.', contract:'critique?'});
  assert.ok(changed.includes(metadata));
  const edited = locateStep(changed, snapshot, snapshot.reactions[1]);
  assert.equal(edited.prompt, 'Review $(plan) carefully.');
  assert.equal(edited.contract, 'critique?');
  assert.equal(locateStep(changed, snapshot, snapshot.reactions[0]).prompt, locateStep(program, snapshot, snapshot.reactions[0]).prompt);
 }
});

test('task copy is shared without exposing identifiers as descriptions', () => {
 const reaction = snapshot.reactions[0];
 const agent = snapshot.agents.find(agent => agent.id === reaction.agent);
 assert.deepEqual(taskCopy({...reaction, title:' Prepare reel content ', description:' Research, write and narrate the story. '}, agent), {
  title:'Prepare reel content', description:'Research, write and narrate the story.'
 });
 assert.equal(taskCopy(reaction, agent).title, 'Draft a release plan for request');
 assert.match(taskCopy(reaction, agent).description, /five numbered steps/);
 assert.equal(shortTaskText('Render and review reel', 12), 'Render and…');
 assert.equal(shortTaskText('Render\n  reel', 20), 'Render reel');
 assert.equal(shortTaskText('🎬🎬🎬', 2), '🎬…');
});


test('legacy code and manual steps describe known behavior without agent placeholders', () => {
 const code = {...snapshot.reactions[0], prompt:undefined, agent:'agent::', triggers:['port::route.evidence'], effects:['port::route.approved','port::route.rejected']};
 const copy = taskCopy(code);
 assert.equal(copy.title, 'Produce approved and rejected');
 assert.match(copy.description, /Uses evidence/);
 assert.match(copy.description, /May produce approved and rejected/);
 assert.doesNotMatch(copy.description, /confidence|0.95|automatically approves/);
 const manual = taskCopy({...code, prompt:'Review the draft.'}, {backend:'Web'});
 assert.match(manual.title, /Request human response/);
 assert.match(manual.description, /Waits for a person/);
});

test('editing instructions clears stale copy while backend-only changes preserve it', () => {
 const source = program.replace('within(30s)', 'within(30s) display("Review campaign script", "Inspect the draft and return a critique.")');
 const step = locateStep(source, snapshot, snapshot.reactions[1]);
 const backendOnly = {backend:'ClaudeCode', prompt:step.prompt, contract:step.contract, triggers:step.triggers};
 assert.match(replaceStep(source, step, backendOnly), /display\("Review campaign script"/);
 const changed = replaceStep(source, step, {...backendOnly, prompt:'Translate the plan into French.'});
 assert.doesNotMatch(changed, /display\(/);
 assert.equal(locateStep(changed, snapshot, snapshot.reactions[1]).prompt, 'Translate the plan into French.');
});
