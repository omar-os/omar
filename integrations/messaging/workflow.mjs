import { readFile } from "node:fs/promises";
import { basename, dirname, resolve } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

const active = new Set(["starting", "running", "stopping", "pausing"]);
const ended = new Set(["completed", "failed", "stopped", "paused"]);

export async function loadConfig(path) {
  return loadWorkflowConfig(JSON.parse(await readFile(path, "utf8")), path);
}

export async function loadWorkflowConfig(config, path) {
  const url = new URL(config.runtimeUrl);
  if (url.protocol !== "http:" || !["127.0.0.1", "localhost", "[::1]"].includes(url.hostname)
      || url.username || url.password || url.search || url.hash
      || !/^\/chats\/[A-Za-z0-9-]+\/?$/.test(url.pathname)) {
    throw new Error("runtimeUrl must be a loopback HTTP URL scoped to /chats/<id>");
  }
  for (const key of ["platform", "accountId", "command", "outputPort"]) {
    if (typeof config[key] !== "string" || !config[key]) throw new Error(`Missing ${key}`);
  }
  if (!/^[a-z0-9-]+$/.test(config.command)) throw new Error("Invalid command name");
  for (const key of ["allowedUsers", "allowedConversations"]) {
    if (!Array.isArray(config[key]) || !config[key].length
        || config[key].some((value) => typeof value !== "string" || !value)) {
      throw new Error(`${key} must be a nonempty list of IDs`);
    }
  }
  const timeout = config.timeoutSeconds ?? 60;
  if (!Number.isInteger(timeout) || timeout < 1 || timeout > 3600) throw new Error("Invalid timeoutSeconds");
  if (config.rawInputs !== undefined && (!Array.isArray(config.rawInputs)
      || config.rawInputs.some((value) => typeof value !== "string" || !value.includes("=")))) {
    throw new Error("rawInputs must contain NAME=VALUE strings");
  }
  const folder = dirname(resolve(path));
  const source = resolve(folder, config.programPath);
  const files = {};
  // All paths and input values come from this operator-owned configuration.
  for (const [relative, diskPath] of Object.entries(config.files ?? {})) {
    files[relative] = await readFile(resolve(folder, diskPath), "utf8");
  }
  return { ...config, runtimeUrl: url.href.replace(/\/$/, ""), timeoutSeconds: timeout,
    request: { program: await readFile(source, "utf8"), filename: basename(source), files,
      inputs: config.inputs ?? {}, raw_inputs: config.rawInputs ?? [], timeout_seconds: timeout, replace: false } };
}

export class RuntimeError extends Error {
  constructor(status, runId) {
    super(`Runtime returned HTTP ${status}`);
    this.status = status;
    this.runId = runId;
  }
}

export class OmarRuntime {
  constructor(base, { fetchImpl = fetch, pollMs = 500 } = {}) {
    this.base = base;
    this.fetch = fetchImpl;
    this.pollMs = pollMs;
  }
  async call(path, body) {
    // A start can compile and wait 30s for readiness; never retry this POST.
    const response = await this.fetch(`${this.base}${path}`, {
      method: body === undefined ? "GET" : "POST",
      headers: { "content-type": "application/json" },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      signal: AbortSignal.timeout(body === undefined ? 10_000 : 60_000),
      redirect: "error",
    });
    const value = await response.json();
    if (!response.ok) throw new RuntimeError(response.status,
      typeof value.run_id === "string" ? value.run_id : undefined);
    return value;
  }
  async start(request) {
    const record = await this.call("/v1/runs", request);
    if (typeof record.run_id !== "string" || !record.run_id) throw new Error("Missing run ID");
    return record;
  }
  async wait(record, timeoutSeconds) {
    const deadline = Date.now() + (timeoutSeconds + 15) * 1000;
    while (active.has(record.status)) {
      if (Date.now() >= deadline) throw new Error("Monitoring deadline reached");
      await sleep(this.pollMs);
      record = await this.call(`/v1/runs/${encodeURIComponent(record.run_id)}`);
    }
    if (!ended.has(record.status)) throw new Error("Unknown run status");
    return record;
  }
}

/**
 * Adapter contract (provider authentication happens before handle):
 * message = { platform, accountId, senderId, eventId, command, destination }
 * destination = { platform, accountId, conversationId, threadId? }
 * messenger.sendMessage(destination, { text, runId? }) -> Promise<void>
 * messenger.usageHint(workflowName) -> string (optional)
 *
 * IDs are opaque strings scoped to a configured platform/account. Threads are
 * optional. Formatting, message limits, connections and ACKs belong to adapters.
 */
export class WorkflowGateway {
  constructor(config, { runtime, messenger, log = console.error, now = Date.now,
    dedupLimit = 2000, dedupMs = 60 * 60 * 1000 }) {
    this.config = config;
    this.runtime = runtime;
    this.messenger = messenger;
    this.log = log;
    this.now = now;
    this.dedupLimit = dedupLimit;
    this.dedupMs = dedupMs;
    this.seen = new Map();
    this.busy = false;
    // Unknown start/monitor outcomes require operator reconciliation, not rerun.
    this.uncertain = false;
  }
  async reply(destination, text, runId) {
    try {
      await this.messenger.sendMessage(destination, { text, ...(runId ? { runId } : {}) });
    } catch {
      this.log(JSON.stringify({ kind: "delivery_failed", destination, runId: runId ?? null }));
    }
  }
  async handle(message) {
    const c = this.config;
    if (!message || message.platform !== c.platform || message.accountId !== c.accountId
        || typeof message.eventId !== "string" || !message.eventId
        || typeof message.command !== "string" || !c.allowedUsers.includes(message.senderId)
        || message.destination?.platform !== c.platform
        || message.destination.accountId !== c.accountId
        || !c.allowedConversations.includes(message.destination.conversationId)
        || (message.destination.threadId !== undefined && typeof message.destination.threadId !== "string")) return;
    for (const [key, expires] of this.seen) if (expires <= this.now()) this.seen.delete(key);
    const key = JSON.stringify([message.platform, message.accountId, message.eventId]);
    if (this.seen.has(key)) return;
    // Never evict an unexpired entry and silently admit its redelivery.
    if (this.seen.size >= this.dedupLimit) {
      this.log(JSON.stringify({ kind: "dedup_capacity" }));
      return;
    }
    this.seen.set(key, this.now() + this.dedupMs);
    const destination = message.destination;
    if (message.command !== `run ${c.command}`) {
      await this.reply(destination, this.messenger.usageHint?.(c.command) ?? `Use run ${c.command}`);
      return;
    }
    if (this.busy || this.uncertain) {
      await this.reply(destination, this.uncertain
        ? "A previous run needs operator checking in OMAR. No new run started."
        : "This workflow is busy. Try again after it finishes.");
      return;
    }
    this.busy = true;
    let record;
    try {
      try {
        record = await this.runtime.start(c.request);
      } catch (error) {
        const rejected = error instanceof RuntimeError && ([400, 413, 503].includes(error.status)
          && !error.runId || error.status === 409);
        this.uncertain = !rejected;
        await this.reply(destination, error instanceof RuntimeError && error.status === 409
          ? "This workflow is already active or paused in OMAR. No new run started."
          : rejected ? "OMAR rejected the request. Ask the operator to check the configuration."
          : "Start outcome is uncertain. Check OMAR before trying again; no automatic retry.", error.runId);
        if (!rejected) this.log(JSON.stringify({ kind: "start_uncertain", runId: error.runId ?? null, destination }));
        return;
      }
      await this.reply(destination, `Started OMAR run ${record.run_id}.`, record.run_id);
      try {
        record = await this.runtime.wait(record, c.timeoutSeconds);
      } catch {
        this.uncertain = true;
        this.log(JSON.stringify({ kind: "monitoring_failed", runId: record.run_id, destination }));
        await this.reply(destination, `Could not retrieve run ${record.run_id}. Check OMAR; it was not restarted.`, record.run_id);
        return;
      }
      let text = `OMAR run ${record.run_id}: ${record.status}.`;
      if (record.status === "completed") {
        const value = record.outputs?.[c.outputPort];
        text += value === undefined ? `\nConfigured output ${c.outputPort} is missing; check OMAR.`
          : `\n${typeof value === "string" ? value : JSON.stringify(value)}`;
      } else {
        text += " Check OMAR for details.";
      }
      await this.reply(destination, text, record.run_id);
    } finally {
      this.busy = false;
    }
  }
}
