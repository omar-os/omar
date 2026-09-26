/** Real OMAR + code-server smoke test. Requires UI-enabled OMAR and code-server.
 * A compiler fixture avoids Lean/model credentials; runtime, Rust, files, IDE
 * and browser are real. Keeps screenshots/logs in a disposable test directory. */
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, readFile } from 'node:fs/promises';
import { tmpdir, homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import { chromium } from '@playwright/test';
const root = await mkdtemp(join(tmpdir(), 'omar-editor-smoke-'));
const home = join(root, 'home'), source = join(root, 'source');
await mkdir(home); await mkdir(source);
await writeFile(join(source, 'seed.txt'), 'Unchanged source\n');
const binary = resolve(process.env.OMAR_BIN || '../target/debug/omar');
const compiler = join(root, 'omarc');
const bytecode = {version:1, team:'Artifacts', instructions:[
  {op:'begin_plan',team:'Artifacts'},
  {op:'declare_instance',name:'writer',team:'Writer',parent:''},
  {op:'define_port',kind:'input',name:'writer.tick',type:'int',instance:'writer'},
  {op:'define_port',kind:'output',name:'writer.out',type:'string',instance:'writer'},
  {op:'install_reaction',id:'writer.report',instance:'writer',agent:'',triggers:['writer.tick'],effects:['writer.out'],contract:'writer.out',prompt:'',body:'std::fs::write("report.md", "# Agent report\\nCreated by the topology.\\n").unwrap(); out = Some("done".to_string());'},
  {op:'commit_plan'},
]};
await writeFile(compiler, `#!${process.execPath}\nrequire('fs').writeFileSync(process.argv[3], ${JSON.stringify(JSON.stringify(bytecode))});\n`, {mode:0o700});
const program = join(root,'artifacts.omar'); await writeFile(program,'// Compiler fixture\n');
const env = {...process.env,HOME:home,OMARC_BIN:compiler,CARGO_HOME:process.env.CARGO_HOME||join(homedir(),'.cargo'),RUSTUP_HOME:process.env.RUSTUP_HOME||join(homedir(),'.rustup')};
const result = spawnSync(binary,['run',program,'--input','writer.tick=1','--fast'],{cwd:source,env,encoding:'utf8',timeout:180000});
assert.equal(result.status,0,result.stderr+result.stdout);
await writeFile(join(root,'topology.log'),result.stdout+result.stderr);
const runtime = spawn(binary,['serve','--address','127.0.0.1:0','--no-ea'],{cwd:source,env});
let logs='',base;
runtime.stdout.on('data',c=>{logs+=c;base=/OMAR serve: (http:\/\/[^\s]+)/.exec(logs)?.[1];});
runtime.stderr.on('data',c=>{logs+=c;});
const wait=ms=>new Promise(r=>setTimeout(r,ms));
for(let i=0;!base&&i<100;i++)await wait(100);
assert.ok(base,logs);
const api=async(path,body)=>{
  const r=await fetch(`${base}/v1/workspaces${path}`,body?{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body)}:{});
  const data=await r.json();assert.ok(r.ok,JSON.stringify(data));return data;
};
let editorId,browser;
try {
  const list=(await api('')).workspaces;assert.equal(list.length,1);editorId=list[0].id;
  const worktree=join(home,'.omar','workspaces',editorId,'worktree');
  browser=await chromium.launch({headless:true});
  const context=await browser.newContext({viewport:{width:1440,height:1000}});
  context.setDefaultTimeout(30000);
  console.log('Runtime:',base);
  const page=await context.newPage();await page.goto(base);
  await page.getByRole('button',{name:'Files & versions',exact:true}).click();
  await page.getByRole('button',{name:'· report.md',exact:true}).click();
  await page.getByLabel('File preview').getByText('Created by the topology.',{exact:false}).waitFor();
  await page.screenshot({path:join(root,'01-artifacts.png')});
  const popup=context.waitForEvent('page');
  await page.getByRole('button',{name:'Open in Web VS Code',exact:true}).click();
  const editor=await popup;
  editor.on('console', message => { if(message.type()==='error') console.log('Editor console:',message.text()); });
  editor.on('requestfailed', request => console.log('Editor request failed:', request.url(), request.failure()?.errorText));
  console.log('Editor opened');
  await editor.waitForURL(/http:\/\/127\.0\.0\.1:/,{timeout:60000});
  await editor.locator('.monaco-workbench').waitFor({timeout:60000});
  console.log('Workbench ready');
  const trust=editor.getByRole('button',{name:/Yes, I trust the authors/});
  await trust.waitFor({timeout:30000}); await trust.click();
  const gitTrust = editor.getByRole('button',{name:'Trust Folder & Continue',exact:true});
  await gitTrust.waitFor({timeout:10000}).then(()=>gitTrust.click()).catch(()=>{});
  const modifier=process.platform==='darwin'?'Meta':'Control';
  await editor.getByText('report.md',{exact:true}).first().dblclick();
  const textInput = editor.getByRole('textbox',{name:'report.md',exact:true});
  await textInput.waitFor({timeout:30000});
  await textInput.focus();
  await editor.keyboard.press(`${modifier}+a`);
  await editor.keyboard.insertText('# Agent report\nEdited by the operator in Web VS Code.\n');
  await editor.keyboard.press(`${modifier}+s`);
  for(let i=0;i<100;i++){if((await readFile(join(worktree,'report.md'),'utf8')).includes('Edited by the operator'))break;await wait(100);}
  assert.match(await readFile(join(worktree,'report.md'),'utf8'),/Edited by the operator/);
  await editor.screenshot({path:join(root,'02-web-vscode.png')});
  await page.getByRole('button',{name:'Refresh',exact:true}).click();
  await page.getByLabel('File preview').getByText('Edited by the operator',{exact:false}).waitFor();
  const snapshots=(await api(`/${editorId}`)).snapshots;
  const final=snapshots.find(s=>s.label==='Final topology files');assert.ok(final);
  await page.getByLabel('File version',{exact:true}).selectOption(final.id);
  await page.getByRole('button',{name:'· report.md',exact:true}).click();
  await page.getByLabel('Compare with worktree',{exact:true}).check();
  await page.getByRole('heading',{name:'Current worktree',exact:true}).waitFor();
  await page.screenshot({path:join(root,'03-compare.png')});
  await page.getByRole('button',{name:'Restore as new workspace',exact:true}).click();
  await page.getByText('Restored into a new workspace.',{exact:false}).waitFor();
  const restored=(await api('')).workspaces.find(w=>w.restored_from);assert.ok(restored);
  assert.match(await readFile(join(home,'.omar','workspaces',restored.id,'worktree','report.md'),'utf8'),/Created by the topology/);
  assert.match(await readFile(join(worktree,'report.md'),'utf8'),/Edited by the operator/);
  assert.equal(await readFile(join(source,'seed.txt'),'utf8'),'Unchanged source\n');
  assert.equal((await fetch(new URL('/',editor.url()))).status,403);
  await page.close();
  await wait(11000); // Past Mission Control's 10-second grace period.
  assert.equal((await fetch(`${base}/health`)).status,200,'editor connection must keep runtime alive');
  await editor.close();await api(`/${editorId}/editor/stop`,{});
  for(let i=0;runtime.exitCode===null&&i<150;i++)await wait(100);
  assert.equal(runtime.exitCode,0,'runtime should exit after the last editor disconnects');
  console.log(`PASS: topology, previews, authenticated code-server, browser save, compare, restore and lifecycle. Evidence: ${root}`);
} catch(error) {
  if(browser) for(const [i,p] of browser.contexts()[0].pages().entries()) { await p.screenshot({path:join(root,`failure-${i}.png`)}).catch(()=>{}); await writeFile(join(root,`failure-${i}.html`),await p.content()); console.log((await p.locator('body').innerText().catch(()=>'' )).slice(0,4000)); }
  throw error;
} finally {
  if(editorId)await api(`/${editorId}/editor/stop`,{}).catch(()=>{});
  await browser?.close();runtime.kill('SIGTERM');await writeFile(join(root,'runtime.log'),logs);
  console.log(`Evidence: ${root}`);
}
