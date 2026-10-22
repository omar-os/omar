# Shared messaging core

`workflow.mjs` owns one configured workflow's local runtime client, authorization,
command matching, bounded in-memory deduplication, busy state, monitoring, output
selection, and failure policy. It does not connect to messaging providers.

Each gateway is bound to one `platform` and `accountId`. A connection/account ID
must distinguish installations when a provider can have multiple installations in
the same workspace. Normalize that identity in the adapter; never use a display
name. The current Slack MVP supports one app in one configured workspace and uses
that workspace's ID. Deduplication keys include platform, account and provider
event ID. Separate gateways targeting one OMAR team can still receive a runtime
busy conflict; there is no global queue or cross-process deduplication.

## Adapter contract

After authenticating its provider connection, an adapter submits:

```js
await gateway.handle({
  platform: "mock-chat",
  accountId: "connected-account",
  senderId: "provider-user",
  eventId: "stable-provider-event-id",
  command: "run research",
  destination: {
    platform: "mock-chat",
    accountId: "connected-account",
    conversationId: "provider-conversation",
    // threadId: "optional-provider-thread"
  }
});
```

The core rejects events outside the configured platform/account/user/conversation
policy, malformed event IDs, and mismatched destinations before executing or
replying. Provider authentication does not replace workflow authorization.
Mention syntax and provider payloads are normalized before entering the core.

The injected adapter implements:

```js
const messenger = {
  usageHint(workflowName) { return `Send run ${workflowName}`; }, // optional
  async sendMessage(destination, { text, runId }) {
    // Format/escape/limit text and send through this adapter's authenticated API.
    // runId is optional on messages that did not start a run.
  }
};
```

The core passes full selected output. The adapter owns message limits, escaping,
thread mapping, and delivery strategy. Failed sends never rerun the workflow.
Destinations come from the normalized authorized event, not agent-produced text.
Credentials, acknowledgements, reconnects and platform errors stay in adapters.
No plugin registry or universal platform capability model is required.

`loadConfig(path)` accepts neutral `platform`, `accountId`, `allowedUsers`, and
`allowedConversations`, plus the existing workflow source/files/inputs/output/runtime
settings. `loadWorkflowConfig(config, path)` loads an already normalized config.

Slack's `../slack/workflow.mjs` is a compatibility entry point: it maps existing
`workspaceId` and `allowedChannels` config fields into these neutral names. Existing
Slack local configs and launch commands need no migration.

## Validation

```sh
node --test integrations/messaging/*.test.mjs integrations/slack/*.test.mjs
python3 tests/ci/slack_workflow.py
```

Mock tests cover two transport identities, authorization/destination isolation,
threaded and threadless messages, duplicate filtering, usage formatting, full-output
handoff and failures. The runtime integration harness also executes a real no-model
workflow through a mock adapter without importing any Slack module. This proves
the shared execution seam, not Discord/WhatsApp connectivity or durable delivery.

Current MVP limits remain: one fixed workflow per connection; in-memory deduplication
for up to one hour/2,000 events; one invocation per gateway at a time; no durable
inbox/outbox, crash recovery, conversational memory, or multi-workflow registry.
