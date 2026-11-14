import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { locateStep, replacePrompt, replaceStep } from '../app/lib/step-source.ts';
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
 assert.equal(locateStep(program.replace('"Inspect $(plan)."','{= println!("hi"); =}'), snapshot, snapshot.reactions[1]), null);
 assert.equal(locateStep(program, snapshot, {...snapshot.reactions[0], name:'reaction.99'}), null);
 assert.equal(locateStep(program, snapshot, {...snapshot.reactions[0], agent:'agent::unknown'}), null);
});

test('prompt-only save preserves adjacent Rust and the other role configuration', () => {
 const body = 'reaction(plan) -> critique {= /* nested /* =} */ */ let s = r#"=}"#; critique = Some(s.into()); =}';
 const mixed = program.replace('task reviewer(plan) -> critique within(30s) "Inspect $(plan)."', body)
   .replace('"Return $(critique)."', '"""\n    Return $(critique).\n    Add a "quote".\n    """');
 assert.equal(locateStep(mixed, snapshot, snapshot.reactions[1]), null);
 const last = locateStep(mixed, snapshot, snapshot.reactions[2]);
 assert.equal(last.prompt, 'Return $(critique).\nAdd a "quote".');
 const edited = replacePrompt(mixed, last, 'Use $(critique).\nReview "again".');
 assert.equal(edited.slice(0, last.promptSpan.start), mixed.slice(0, last.promptSpan.start));
 assert.equal(locateStep(edited, snapshot, snapshot.reactions[2]).prompt, 'Use $(critique).\nReview "again".');
 assert.ok(edited.includes(body));
});

import { graphChange, splitConnection } from '../app/lib/graph-edits.ts';
const insert = {position:'before',name:'checked',role:'planner',prompt:'Check the value',repairs:{},port:'port::flow.request'};
test('inserting and removing graph steps preserves typed ports and downstream branches', () => {
 const first=snapshot.reactions[0];
 const before=graphChange(program,snapshot,first,insert);
 assert.match(before,/task planner\(request\) -> checked "Check the value"/);
 assert.match(before,/task planner\(checked\) -> plan "Use \$\(checked\)/);
 const after=graphChange(program,snapshot,first,{...insert,position:'after',port:first.effects[0]});
 assert.match(after,/task planner\(request\) -> checked/);
 assert.match(after,/task planner\(checked\) -> plan/);
 assert.match(after,/task reviewer\(plan\) -> critique/);
 const remove={...insert,position:'remove'};
 assert.throws(()=>graphChange(program,snapshot,first,remove),/compatible replacement/);
 const repaired=graphChange(program,snapshot,first,{...remove,repairs:{[first.effects[0]]:first.triggers[0]}});
 assert.match(repaired,/request -> plan/);
 assert.match(repaired,/task reviewer\(plan\) -> critique/);
 assert.throws(()=>graphChange(program,snapshot,first,{...insert,name:'plan'}),/unique identifier/);
});
test('an inter-instance connection is split in its enclosing block and keeps its delay',()=>{
 const workflow=program.replace('flow = ReviewFlow()','flow = ReviewFlow()\nother = ReviewFlow()\nflow.result -> other.request after 2s');
 const port={...snapshot.ports.find(p=>p.name==='flow.request'),id:'port::other.request',name:'other.request',instance:'other'};
 const snap={...snapshot,ports:[...snapshot.ports,port],instances:[...snapshot.instances,{id:'instance::other',name:'other',team:'ReviewFlow'}]};
 const graph=splitConnection(workflow,snap,{kind:'connection',source:'port::flow.result',target:port.id},{...insert,name:'audit',role:'Web'});
 assert.match(graph,/team auditStep\[worker : Web\]/);
 assert.match(graph,/flow.result -> audit.value\s+audit.result -> other.request after 2s/);
});
