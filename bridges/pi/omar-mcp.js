import { spawn as nodeSpawn } from "node:child_process";
import { createInterface } from "node:readline";

const MCP_PROTOCOL_VERSION = "2024-11-05";
const DEFAULT_TIMEOUT_MS = 20_000;

/**
 * Keep the MCP server name in every Pi tool name. Besides making the source of
 * a tool obvious to the model, this avoids collisions with Pi tools and other
 * extensions.
 */
export function piToolName(mcpName) {
  const safeName = String(mcpName).replace(/[^A-Za-z0-9_-]/g, "_");
  return `omar_${safeName}`;
}

export function jsonSchemaForPi(inputSchema) {
  if (!inputSchema || typeof inputSchema !== "object") {
    return { type: "object", properties: {}, additionalProperties: false };
  }

  // MCP already describes tool parameters with JSON Schema. Pi's registerTool
  // contract accepts that same plain schema, so do not introduce a TypeBox
  // runtime dependency (the package changed names across Pi releases).
  return inputSchema;
}

function errorText(error) {
  return error instanceof Error ? error.message : String(error);
}

function contentText(content) {
  if (!Array.isArray(content)) return "";
  return content
    .map((item) => {
      if (item?.type === "text" && typeof item.text === "string") return item.text;
      return item == null ? "" : JSON.stringify(item);
    })
    .filter(Boolean)
    .join("\n");
}

export function piResultFromMcp(result, toolName) {
  if (result?.isError === true) {
    throw new Error(contentText(result.content) || `OMAR tool ${toolName} failed`);
  }

  const structuredContent = result?.structuredContent;
  let text = contentText(result?.content);
  if (!text && structuredContent !== undefined) {
    text = JSON.stringify(structuredContent, null, 2);
  }
  if (!text) text = "OMAR tool completed successfully.";

  return {
    content: [{ type: "text", text }],
    details: { omarTool: toolName, structuredContent },
  };
}

function mcpError(response) {
  const message = response?.error?.message;
  return new Error(message ? `OMAR MCP error: ${message}` : "Invalid OMAR MCP response");
}

/**
 * Small line-delimited JSON-RPC client for `omar mcp-server`.
 *
 * OMAR accepts both MCP Content-Length framing and JSONL. JSONL is the least
 * fragile contract for an extension because it works with Node's readline and
 * is also the framing used by the repository's Slack bridge.
 */
export class OmarMcpClient {
  constructor({
    binary = process.env.OMAR_BINARY || "omar",
    args,
    env = {},
    cwd,
    timeoutMs = Number(process.env.OMAR_MCP_TIMEOUT_MS) || DEFAULT_TIMEOUT_MS,
    spawnProcess = nodeSpawn,
  } = {}) {
    const contextFile = env.OMAR_MCP_CONTEXT_FILE ?? process.env.OMAR_MCP_CONTEXT_FILE;
    this.binary = binary;
    this.args = args || [
      "mcp-server",
      ...(contextFile
        ? ["--context-file", contextFile]
        : []),
    ];
    this.env = env;
    this.cwd = cwd;
    this.timeoutMs = timeoutMs;
    this.spawnProcess = spawnProcess;
    this.child = undefined;
    this.lines = undefined;
    this.nextId = 1;
    this.pending = new Map();
    this.startPromise = undefined;
  }

  async start() {
    if (this.child) return;
    if (this.startPromise) return this.startPromise;

    this.startPromise = (async () => {
      const env = { ...process.env, ...this.env };
      const child = this.spawnProcess(this.binary, this.args, {
        cwd: this.cwd,
        env,
        stdio: ["pipe", "pipe", "inherit"],
      });
      this.child = child;
      this.lines = createInterface({ input: child.stdout });
      this.lines.on("line", (line) => this.handleLine(line));
      child.on("error", (error) => this.failPending(error));
      child.on("close", (code, signal) => {
        const suffix = signal ? ` (${signal})` : code == null ? "" : ` (exit ${code})`;
        this.failPending(new Error(`OMAR MCP server exited${suffix}`));
        this.child = undefined;
        this.lines?.close();
        this.lines = undefined;
      });

      await this.request("initialize", {
        protocolVersion: MCP_PROTOCOL_VERSION,
        capabilities: {},
        clientInfo: { name: "omar-pi-extension", version: "0.1.0" },
      });
      await this.notify("notifications/initialized", {});
    })();

    try {
      await this.startPromise;
    } catch (error) {
      this.close();
      throw error;
    } finally {
      this.startPromise = undefined;
    }
  }

  async listTools() {
    await this.start();
    const result = await this.request("tools/list", {});
    return Array.isArray(result?.tools) ? result.tools : [];
  }

  async callTool(name, argumentsValue = {}, signal) {
    await this.start();
    const result = await this.request(
      "tools/call",
      { name, arguments: argumentsValue },
      signal,
    );
    return piResultFromMcp(result, name);
  }

  close() {
    this.lines?.close();
    this.lines = undefined;
    this.failPending(new Error("OMAR MCP client closed"));
    if (this.child && !this.child.killed) this.child.kill();
    this.child = undefined;
  }

  async notify(method, params) {
    if (!this.child?.stdin) return;
    this.child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method, params })}\n`);
  }

  request(method, params, signal) {
    if (!this.child?.stdin) return Promise.reject(new Error("OMAR MCP client is not started"));

    const id = this.nextId++;
    const request = `${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`;

    return new Promise((resolve, reject) => {
      let timer;
      const abort = () => {
        clearTimeout(timer);
        this.pending.delete(id);
        reject(new Error(`OMAR MCP request ${method} was aborted`));
      };
      if (signal?.aborted) return abort();

      timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`OMAR MCP request ${method} timed out`));
      }, this.timeoutMs);
      signal?.addEventListener("abort", abort, { once: true });
      this.pending.set(id, {
        resolve: (value) => {
          clearTimeout(timer);
          signal?.removeEventListener("abort", abort);
          resolve(value);
        },
        reject: (error) => {
          clearTimeout(timer);
          signal?.removeEventListener("abort", abort);
          reject(error);
        },
      });

      try {
        this.child.stdin.write(request);
      } catch (error) {
        this.pending.delete(id);
        clearTimeout(timer);
        signal?.removeEventListener("abort", abort);
        reject(error);
      }
    });
  }

  handleLine(line) {
    if (!line.trim()) return;
    let response;
    try {
      response = JSON.parse(line);
    } catch (error) {
      this.failPending(new Error(`Invalid JSON from OMAR MCP server: ${errorText(error)}`));
      return;
    }
    const id = response?.id;
    const pending = this.pending.get(id);
    if (!pending) return;
    this.pending.delete(id);
    if (response.error) pending.reject(mcpError(response));
    else if (!("result" in response)) pending.reject(mcpError(response));
    else pending.resolve(response.result);
  }

  failPending(error) {
    for (const { reject } of this.pending.values()) reject(error);
    this.pending.clear();
  }
}
