import assert from "node:assert/strict";
import test from "node:test";

import { scanSecrets, templateInputs, templateProgram, templateTeam, templates } from "../app/lib/templates.ts";

test("all 24 jobs have distinct, runnable local workflow definitions", () => {
  assert.equal(templates.length, 24);
  assert.equal(new Set(templates.map((item) => item.id)).size, 24);
  assert.deepEqual(templates.map((item) => item.number), Array.from({ length: 24 }, (_, index) => index + 1));
  assert.deepEqual(new Set(templates.map((item) => item.group)), new Set(["development", "security", "revenue", "operations"]));
  assert.equal(templateTeam(templates.find((item) => item.id === "docs")), "Documentation");
  assert.equal(new Set(templates.filter((item) => item.id !== "secrets").map(templateTeam)).size, 23);
  for (const template of templates) {
    assert.ok(template.title && template.outcome && template.field && template.prereq);
    if (template.id === "secrets") continue; // browser-local deterministic scanner
    const program = templateProgram(template);
    const team = templateTeam(template);
    assert.match(program, new RegExp(`^team ${team}\\[`));
    assert.match(program, /prompt worker\(request\) -> draft/);
    assert.match(program, /prompt reviewer\(draft\) -> review/);
    assert.match(program, /prompt worker\(review\) -> result/);
    assert.ok(program.includes(`main { flow = ${team}() }`));
    assert.ok(program.includes(template.scope));
  }
  assert.deepEqual(templateInputs("my request"), { "flow.request": "my request" });
  assert.match(templateProgram(templates[0], "claude"), /worker: claude, reviewer: claude/);
  assert.throws(() => templateProgram(templates[0], "not-a-backend"), /supported agent backend/);
});

test("secret screening stays redacted and covers common high-confidence markers", () => {
  const github = `ghp_${"A".repeat(24)}`;
  const aws = `AKIA${"B".repeat(16)}`;
  const text = `safe\n${github}\n${aws}\n-----BEGIN PRIVATE KEY-----`;
  const findings = scanSecrets(text);
  assert.deepEqual(findings.map(({ line, kind }) => [line, kind]), [
    [2, "GitHub token"], [3, "AWS access key ID"], [4, "Private key header"],
  ]);
  assert.ok(!JSON.stringify(findings).includes(github));
  assert.ok(!JSON.stringify(findings).includes(aws));
  assert.deepEqual(scanSecrets("no credentials here"), []);
});
