// Compatibility entry point: existing Slack configs and imports keep working.
import { readFile } from "node:fs/promises";
import { loadWorkflowConfig } from "../messaging/workflow.mjs";
export { OmarRuntime, RuntimeError, WorkflowGateway } from "../messaging/workflow.mjs";

export async function loadConfig(path) {
  const { workspaceId, allowedChannels, ...config } = JSON.parse(await readFile(path, "utf8"));
  return loadWorkflowConfig({ ...config, platform: "slack", accountId: workspaceId,
    allowedConversations: allowedChannels }, path);
}
