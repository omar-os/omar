export type ConnectionAuth =
  | { type: "none" }
  | { type: "bearer"; variable: string }
  | { type: "api-key"; variable: string; header: string };

export type TaskConnection = { id: string; name: string } & (
  | { type: "http" | "webhook"; url: string; method: string; auth: ConnectionAuth }
  | { type: "mcp"; server: string }
);

export const connectionTypes = [
  { type: "http", title: "HTTP API", description: "Read or send data to an API endpoint.", icon: "api" },
  { type: "webhook", title: "Webhook", description: "Send a notification or payload to a URL.", icon: "bolt" },
  { type: "mcp", title: "MCP server", description: "Use a tool server installed in your agent.", icon: "layers" },
] as const;

export function connectionDetail(connection: TaskConnection): string {
  return connection.type === "mcp" ? connection.server : `${connection.method} ${connection.url}`;
}

/** Only references to credentials belong in portable workflow source. */
export function validateConnection(connection: TaskConnection, others: TaskConnection[] = []): string | null {
  if (!connection.name.trim()) return "Give this connection a name.";
  if (connection.name.length > 80) return "Use a name of 80 characters or fewer.";
  if (others.some((item) => item.id !== connection.id && item.name.trim().toLowerCase() === connection.name.trim().toLowerCase())) return "A connection with this name already exists.";
  if (JSON.stringify(connection).includes("$(")) return "Connection settings cannot contain workflow expressions.";
  if (connection.type === "mcp") {
    if (typeof connection.server !== "string" || !/^[A-Za-z0-9_-]+$/.test(connection.server)) return "Enter the installed MCP server name using letters, numbers, dashes, or underscores.";
    return null;
  }
  try {
    const url = new URL(connection.url);
    if (!["http:", "https:"].includes(url.protocol) || !url.hostname) return "Enter a complete HTTP or HTTPS endpoint URL.";
    if (url.username || url.password) return "Use an environment variable for authentication instead of credentials in the URL.";
    if (url.hash) return "Remove the fragment (#) from the endpoint URL.";
  } catch { return "Enter a complete HTTP or HTTPS endpoint URL."; }
  if (!["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"].includes(connection.method) || (connection.type === "webhook" && connection.method !== "POST")) return "Choose a supported HTTP method.";
  if (!connection.auth || !["none", "bearer", "api-key"].includes(connection.auth.type)) return "Choose a supported authentication method.";
  if (connection.auth.type !== "none") {
    if (typeof connection.auth.variable !== "string" || !/^[A-Za-z_][A-Za-z_0-9]*$/.test(connection.auth.variable)) return "Enter an environment variable name, such as SERVICE_API_KEY, instead of the secret itself.";
    if (connection.auth.type === "api-key" && (typeof connection.auth.header !== "string" || !/^[A-Za-z0-9!#$%&'*+.^_`|~-]+$/.test(connection.auth.header))) return "Enter a valid API key header, such as X-API-Key.";
  }
  return null;
}

const START = '\n\n<omar-connections version="1">\n';
const END = "\n</omar-connections>";
const GUIDANCE = "These connections are available for this task. Use them only as needed for the task prompt. HTTP and webhook requests are made by the execution agent using its network tools; they are not workflow triggers. Resolve authentication from the named environment variable in the agent's environment, never print its value, and report a missing credential or failed request instead of inventing a result. For bearer authentication use the Authorization header with the Bearer scheme; for api-key authentication use the configured header. An MCP connection refers to a server already installed in the execution agent; use its tools and report if unavailable. Saving these settings does not test connectivity or install an MCP server.";

/** The connection block travels with the real execution prompt, so exporting,
 * compiling and running the workflow all use the same connection settings. */
export function writeConnectionPrompt(prompt: string, connections: TaskConnection[]): string {
  if (!connections.length) return prompt;
  for (const connection of connections) {
    const error = validateConnection(connection, connections);
    if (error) throw new Error(error);
  }
  return `${prompt}${START}${JSON.stringify({ connections, instructions: GUIDANCE }, null, 2)}${END}`;
}

export function readConnectionPrompt(prompt: string): { prompt: string; connections: TaskConnection[] } {
  const start = prompt.lastIndexOf(START);
  if (start < 0 || !prompt.endsWith(END)) return { prompt, connections: [] };
  try {
    const value = JSON.parse(prompt.slice(start + START.length, -END.length));
    if (!Array.isArray(value.connections) || value.connections.some((item: TaskConnection) =>
      !item || typeof item.id !== "string" || typeof item.name !== "string" ||
      !["http", "webhook", "mcp"].includes(item.type) || validateConnection(item, value.connections))) throw new Error("Invalid connection");
    if (new Set(value.connections.map((item: TaskConnection) => item.id)).size !== value.connections.length) throw new Error("Duplicate connection");
    return { prompt: prompt.slice(0, start), connections: value.connections };
  } catch {
    // Preserve manually edited or future metadata verbatim in the source editor.
    throw new Error("Connection settings could not be read. Edit this task in the source editor.");
  }
}
