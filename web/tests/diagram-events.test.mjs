/**
 * The stream hands the page every event kind the protocol names. An
 * `agent_attempt` left unlistened never reached it, so a window closed at
 * the run's end stayed "open" once the diagram server was gone.
 */

import assert from "node:assert/strict";
import test from "node:test";

import { subscribeToDiagram } from "../app/lib/runtime-client.ts";
import { DIAGRAM_EVENT_KINDS } from "../app/lib/protocol.ts";

test("every diagram event kind is listened for", () => {
  const listened = [];
  globalThis.EventSource = class {
    addEventListener(kind) {
      listened.push(kind);
    }
    close() {}
  };
  subscribeToDiagram("http://127.0.0.1:1", () => {}, () => {});
  assert.deepEqual([...listened].sort(), [...DIAGRAM_EVENT_KINDS].sort());
});
