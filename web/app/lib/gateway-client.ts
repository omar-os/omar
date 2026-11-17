import { normalizeRuntimeUrl } from "./runtime-client.ts";

/** A session `omar gateway` can reach, as `omar ls` lists it. */
export type GatewaySession = { name: string; state: string };

/**
 * Where this page came from: `omar gateway` (signed in or not), or a
 * session's own daemon, which has no sign-in. A daemon answers `/auth/me`
 * with its Mission Control shell, so only JSON means a gateway.
 */
export type Gate =
  | { kind: "daemon" }
  | { kind: "signed-out" }
  | { kind: "signed-in"; sessions: GatewaySession[] };

export async function fetchGate(origin: string): Promise<Gate> {
  const response = await fetch(`${normalizeRuntimeUrl(origin)}/auth/me`);
  if (!(response.headers.get("content-type") ?? "").includes("application/json")) {
    // Unread, the page would hold the request open.
    await response.body?.cancel();
    return { kind: "daemon" };
  }
  if (response.status === 401) return { kind: "signed-out" };
  if (!response.ok) throw new Error(await readError(response));
  const { sessions } = (await response.json()) as { sessions: GatewaySession[] };
  return { kind: "signed-in", sessions };
}

/** The session a gateway page is for: `/s/<name>/…`. */
export function sessionFromPath(pathname: string): string | null {
  const match = /^\/s\/([^/]+)/.exec(pathname);
  return match ? decodeURIComponent(match[1]) : null;
}

export function sessionUrl(origin: string, session: string): string {
  return `${normalizeRuntimeUrl(origin)}/s/${encodeURIComponent(session)}`;
}

export async function signIn(origin: string, password: string): Promise<void> {
  await post(`${normalizeRuntimeUrl(origin)}/auth/login`, { password });
}

export async function signOut(origin: string): Promise<void> {
  await post(`${normalizeRuntimeUrl(origin)}/auth/logout`, {});
}

async function post(url: string, body: unknown): Promise<void> {
  const response = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!response.ok) throw new Error(await readError(response));
  await response.body?.cancel();
}

async function readError(response: Response): Promise<string> {
  try {
    const body = (await response.json()) as { error?: unknown };
    if (typeof body.error === "string") return body.error;
  } catch {
    // Not JSON: fall through to the status.
  }
  return `HTTP ${response.status}`;
}
