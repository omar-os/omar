/**
 * The countdown under a live timer: physical time to its next firing, read at
 * a glance. These pin the units it picks, since a reading in the wrong unit
 * is the kind of thing nobody notices until it says `90000 ms`.
 */

import assert from "node:assert/strict";
import test from "node:test";

import { formatCountdown, runClockAt } from "../app/lib/protocol.ts";

const SECOND = 1_000_000_000;
const MINUTE = 60 * SECOND;

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

test("the reading is to the next physical firing, lag included", () => {
  // A timer every 2 minutes fired at logical 10 min, a minute late: physical
  // time then stood at 11 min, so the next firing is 1 minute away, not 2.
  const tagAt = 1_700_000_000_000;
  const snapshot = { current_tag: { timestamp: 10 * MINUTE, microstep: 0 }, lag: MINUTE, tag_at: tagAt };
  const next = 12 * MINUTE;
  assert.equal(formatCountdown(next - runClockAt(snapshot, tagAt, tagAt)), "1 m");
  assert.equal(formatCountdown(next - runClockAt(snapshot, tagAt + 1000, tagAt)), "59 s");
  assert.equal(formatCountdown(next - runClockAt(snapshot, tagAt + 2000, tagAt)), "58 s");
  assert.equal(formatCountdown(next - runClockAt(snapshot, tagAt + 60_000, tagAt)), "due");
  // Events published within the tag do not move the reading: the origin is
  // when the tag advanced, however late this client saw the snapshot.
  assert.equal(runClockAt(snapshot, tagAt + 30_000, tagAt + 30_000), runClockAt(snapshot, tagAt + 30_000, tagAt));
  // Without `tag_at` the client's first sight of the tag stands in.
  const older = { current_tag: snapshot.current_tag, lag: MINUTE };
  assert.equal(formatCountdown(next - runClockAt(older, tagAt + 1000, tagAt)), "59 s");
  // Before the first tag the clock is just the lag-free wall time since.
  assert.equal(runClockAt({ current_tag: null, lag: null }, 5000, 0), 5 * SECOND);
});

test("at or past the firing it is due", () => {
  assert.equal(formatCountdown(0), "due");
  assert.equal(formatCountdown(-5 * SECOND), "due");
  assert.equal(formatCountdown(Number.NaN), "—");
});
