// exe and contextFile are generated JSON literals, never shell expressions.
import { execFile } from "node:child_process";
import { promisify } from "node:util";
const run = promisify(execFile);
async function snapshot(event) {
  const { stdout } = await run(exe, ["agent-hook", "--context-file", contextFile,
    "--format", "opencode", "--event", event], { timeout: 5000, maxBuffer: 256 * 1024 });
  return JSON.parse(stdout);
}
export const OmarCoordination = async () => ({
  "experimental.chat.system.transform": async (_input, output) => {
    const state = await snapshot("PreInvocation");
    if (state.context) output.system.push(state.context);
  },
  "experimental.session.compacting": async (_input, output) => {
    const state = await snapshot("PreCompact");
    if (state.context) output.context.push(state.context);
  },
  event: async ({ event }) => {
    // The host owns wake delivery and retries; do not start a recursive prompt.
    if (event.type === "session.idle") await snapshot("Stop");
  },
});
