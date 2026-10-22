import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { setTimeout as sleep } from "node:timers/promises";
import { loadConfig, OmarRuntime, WorkflowGateway } from "./workflow.mjs";

export function normalizeMention(payload, botId) {
  const event = payload?.event;
  if (payload?.type !== "event_callback" || typeof payload.event_id !== "string"
      || !payload.event_id || typeof payload.team_id !== "string"
      || event?.type !== "app_mention" || event.bot_id || event.subtype
      || typeof event.user !== "string" || event.user === botId
      || typeof event.channel !== "string" || typeof event.ts !== "string"
      || typeof event.text !== "string" || (event.thread_ts !== undefined && typeof event.thread_ts !== "string")) return null;
  const prefix = `<@${botId}>`;
  const text = event.text.trim();
  if (!text.startsWith(`${prefix} `)) return null;
  return { platform: "slack", accountId: payload.team_id, senderId: event.user, eventId: payload.event_id,
    command: text.slice(prefix.length).trim(),
    destination: { platform: "slack", accountId: payload.team_id, conversationId: event.channel, threadId: event.thread_ts ?? event.ts } };
}

// Slack owns escaping and size limits; the core passes the full selected output.
export function formatSlackReply({ text, runId }) {
  const escaped = text.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
  if (escaped.length <= 3500) return escaped;
  return `${escaped.slice(0, 3300)}\n[Truncated; full output in OMAR${runId ? ` run ${runId}` : ""}.]`;
}

export class SlackApi {
  constructor(botToken, appToken, { fetchImpl = fetch } = {}) {
    this.botToken = botToken;
    this.appToken = appToken;
    this.fetch = fetchImpl;
  }
  async call(method, body = {}, app = false) {
    const response = await this.fetch(`https://slack.com/api/${method}`, {
      method: "POST", headers: { authorization: `Bearer ${app ? this.appToken : this.botToken}`,
        "content-type": "application/json; charset=utf-8" }, body: JSON.stringify(body),
      signal: AbortSignal.timeout(15_000), redirect: "error",
    });
    const value = await response.json();
    // Never log tokens, provider bodies, or socket URLs (they contain secrets).
    if (!response.ok || !value.ok) throw new Error("Slack API request failed");
    return value;
  }
  usageHint(command) { return `Use @Omar run ${command}`; }
  async sendMessage(destination, reply) {
    await this.call("chat.postMessage", { channel: destination.conversationId,
      thread_ts: destination.threadId,
      text: formatSlackReply(reply),
      mrkdwn: false, parse: "none", unfurl_links: false, unfurl_media: false });
  }
}

export async function handleEnvelope(raw, socket, botId, gateway, log = console.error) {
  let envelope;
  try { envelope = JSON.parse(raw); } catch { return; }
  if (typeof envelope.envelope_id === "string") {
    // MVP: ack immediately, then dedup/process in memory. No durable inbox.
    socket.send(JSON.stringify({ envelope_id: envelope.envelope_id }));
  }
  if (envelope.type === "disconnect") { socket.close(); return; }
  if (envelope.type !== "events_api") return;
  const message = normalizeMention(envelope.payload, botId);
  if (message) {
    try { await gateway.handle(message); }
    catch { log(JSON.stringify({ kind: "message_processing_failed" })); }
  }
}

export async function socketSession(api, botId, gateway, { WebSocketImpl = WebSocket,
  signal, log = console.error, openTimeoutMs = 15_000 } = {}) {
  const { url } = await api.call("apps.connections.open", {}, true);
  const address = new URL(url);
  if (address.protocol !== "wss:") throw new Error("Invalid Slack socket URL");
  if (signal?.aborted) return;
  await new Promise((done) => {
    const socket = new WebSocketImpl(url);
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      signal?.removeEventListener("abort", close);
      done();
    };
    const close = () => { socket.close(); };
    const timer = setTimeout(close, openTimeoutMs);
    signal?.addEventListener("abort", close, { once: true });
    socket.addEventListener("open", () => { clearTimeout(timer); log("Slack Socket Mode connected."); });
    socket.addEventListener("message", (event) => {
      void handleEnvelope(event.data, socket, botId, gateway, log).catch(() => log("Socket message failed."));
    });
    socket.addEventListener("error", () => { close(); finish(); });
    socket.addEventListener("close", finish);
  });
}

async function main(path) {
  if (!path) throw new Error("Usage: node integrations/slack/slack.mjs <config.json>");
  const config = await loadConfig(path);
  const bot = process.env.SLACK_BOT_TOKEN;
  const app = process.env.SLACK_APP_TOKEN;
  if (!bot?.startsWith("xoxb-") || !app?.startsWith("xapp-")) throw new Error("Set SLACK_BOT_TOKEN and SLACK_APP_TOKEN");
  const api = new SlackApi(bot, app);
  const identity = await api.call("auth.test");
  if (identity.team_id !== config.accountId || typeof identity.user_id !== "string") throw new Error("Slack workspace does not match configuration");
  const runtime = new OmarRuntime(config.runtimeUrl);
  await runtime.call("/v1/chat"); // Fail early if the configured runtime/chat is unavailable.
  const gateway = new WorkflowGateway(config, { runtime, messenger: api });
  const stop = new AbortController();
  for (const name of ["SIGINT", "SIGTERM"]) process.once(name, () => stop.abort());
  let delay = 1000;
  while (!stop.signal.aborted) {
    const started = Date.now();
    try { await socketSession(api, identity.user_id, gateway, { signal: stop.signal }); }
    catch { console.error("Slack connection failed; reconnecting."); }
    if (stop.signal.aborted) break;
    if (Date.now() - started > 60_000) delay = 1000;
    await sleep(delay, undefined, { signal: stop.signal }).catch(() => {});
    delay = Math.min(delay * 2, 30_000);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main(process.argv[2]).catch((error) => {
    // Setup messages are controlled here; network errors never expose URLs/tokens.
    console.error(["SyntaxError", "TypeError"].includes(error.name) ? "Invalid configuration or response." : error.message);
    process.exitCode = 1;
  });
}
