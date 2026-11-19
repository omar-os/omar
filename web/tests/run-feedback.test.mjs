import test from "node:test";
import assert from "node:assert/strict";
import { describeRunFailure } from "../app/lib/run-feedback.ts";

test("missing Cargo explains the setup failure and the recovery path", () => {
  const feedback = describeRunFailure("failed to invoke cargo: No such file or directory", []);
  assert.match(feedback.summary, /No workflow agents started/);
  assert.match(feedback.guidance, /Inspect terminal/);
});

test("manual timeouts explain the handoff instead of suggesting an agent repair", () => {
  const feedback = describeRunFailure("'prepare.chatgpt' did not answer within 3600s", [
    { name: "prepare.chatgpt", backend: "Web" },
  ]);
  assert.match(feedback.summary, /60 minutes/);
  assert.match(feedback.guidance, /submit the result through the response panel/);
});

test("unknown failures point to automatic investigation and its visible progress", () => {
  assert.match(describeRunFailure("unexpected error", []).guidance, /automatically when the assistant is available/);
});
