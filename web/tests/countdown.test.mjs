/**
 * The countdown under a live timer: physical time to its next firing, read at
 * a glance. These pin the units it picks, since a reading in the wrong unit
 * is the kind of thing nobody notices until it says `90000 ms`.
 */

import assert from "node:assert/strict";
import test from "node:test";

import { formatCountdown } from "../app/lib/protocol.ts";

const SECOND = 1_000_000_000;

test("a countdown reads in the largest units that say something", () => {
  assert.equal(formatCountdown(90 * SECOND), "1 m 30 s");
  assert.equal(formatCountdown(60 * SECOND), "1 m");
  assert.equal(formatCountdown(3600 * SECOND), "1 h");
  assert.equal(formatCountdown(3605 * SECOND), "1 h 5 s");
  assert.equal(formatCountdown((2 * 3600 + 3 * 60 + 4) * SECOND), "2 h 3 m 4 s");
});

test("whole seconds are never shown as milliseconds", () => {
  assert.equal(formatCountdown(SECOND), "1 s");
  assert.equal(formatCountdown(500_000_000), "500 ms");
  // Rounded up: the reading never says less time is left than there is.
  assert.equal(formatCountdown(1_500_000_000), "2 s");
  assert.equal(formatCountdown(999_999_999), "1 s");
  assert.equal(formatCountdown(10), "1 ms");
});

test("at or past the firing it is due", () => {
  assert.equal(formatCountdown(0), "due");
  assert.equal(formatCountdown(-5 * SECOND), "due");
  assert.equal(formatCountdown(Number.NaN), "—");
});
