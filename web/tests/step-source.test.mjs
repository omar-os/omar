import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { locateStep, replaceStep } from '../app/lib/step-source.ts';
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
