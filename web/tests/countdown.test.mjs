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

test("the reading is to the next physical firing, from the run's clock origin", () => {
  // The runtime's logical zero stood at wall time T0. A timer every 2 minutes
  // fired at logical 10 min, a minute late, so physical time then stood at
  // 11 min: the next firing is a minute away, not two.
  const T0 = 1_700_000_000_000;
  const snapshot = { current_tag: { timestamp: 10 * MINUTE, microstep: 0 }, lag: MINUTE, clock_origin: T0 };
  const next = 12 * MINUTE;
  const at = (ms) => formatCountdown(next - runClockAt(snapshot, T0 + ms, 0));
  assert.equal(at(11 * 60_000), "1 m");
  assert.equal(at(11 * 60_000 + 1000), "59 s");
  assert.equal(at(11 * 60_000 + 2000), "58 s");
  assert.equal(at(12 * 60_000), "due");
  // Before the first tag the clock is still wall time since the origin: a
  // timer with a 1 min offset, 30 s after the clock started, is 30 s away,
  // however long before that the picture was first drawn.
  const fresh = { current_tag: null, lag: null, clock_origin: T0 };
  assert.equal(formatCountdown(MINUTE - runClockAt(fresh, T0 + 30_000, T0 - 60_000)), "30 s");
  // Events published within a tag do not move the reading: nothing the
  // client saw later changes where the origin stands.
  assert.equal(runClockAt(snapshot, T0 + 30_000, T0 + 30_000), runClockAt(snapshot, T0 + 30_000, T0));
  // Without an origin the client's first sight of the tag stands in.
  const older = { current_tag: snapshot.current_tag, lag: MINUTE };
  assert.equal(formatCountdown(next - runClockAt(older, T0 + 1000, T0)), "59 s");
  assert.equal(runClockAt({ current_tag: null, lag: null }, 5000, 0), 5 * SECOND);
});

test("at or past the firing it is due", () => {
  assert.equal(formatCountdown(0), "due");
  assert.equal(formatCountdown(-5 * SECOND), "due");
  assert.equal(formatCountdown(Number.NaN), "—");
});
