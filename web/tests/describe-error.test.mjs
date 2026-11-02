/**
 * A request that never reached the runtime reads the same in every browser;
 * anything the runtime itself said is passed through.
 */

import assert from "node:assert/strict";
import test from "node:test";

import { describeError } from "../app/lib/runtime-client.ts";

test("each browser's network failure becomes one sentence", () => {
  for (const message of [
    "Load failed",
    "Failed to fetch",
    "NetworkError when attempting to fetch resource.",
    "network error",
  ]) {
    assert.equal(describeError(new TypeError(message)), "Can't reach the runtime.");
  }
});

test("the runtime's own messages pass through", () => {
  assert.equal(describeError(new Error("compile failed: unknown port 'x'")), "compile failed: unknown port 'x'");
  assert.equal(describeError("Runtime returned HTTP 500."), "Runtime returned HTTP 500.");
});
