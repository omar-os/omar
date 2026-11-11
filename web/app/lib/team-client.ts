import type { ConversationSummary, Role, Visibility } from "./protocol.ts";
import { normalizeRuntimeUrl } from "./runtime-client.ts";

/** A session this account belongs to, as `omar gateway` lists them. */
export type Membership = { name: string; role: Role; state: string };

export type Me = { user: string; must_change: boolean; sessions: Membership[] };

/**
 * Where this page came from: a gateway (signed in or not), or a session's
 * own daemon, which has no accounts. A daemon answers `/auth/me` with its
 * Mission Control shell, so only JSON means a gateway.
 */
export type Gate =
  | { kind: "daemon" }
  | { kind: "signed-out" }
  | { kind: "signed-in"; me: Me };

export async function fetchGate(origin: string): Promise<Gate> {
  const response = await fetch(`${normalizeRuntimeUrl(origin)}/auth/me`);
  if (!(response.headers.get("content-type") ?? "").includes("application/json")) {
    // Unread, the page would hold the request open.
    await response.body?.cancel();
    return { kind: "daemon" };
  }
  if (response.status === 401) return { kind: "signed-out" };
  if (!response.ok) throw new Error(await readError(response));
  return { kind: "signed-in", me: (await response.json()) as Me };
}

/** The session a gateway page is for: `/s/<name>/…`. */
export function sessionFromPath(pathname: string): string | null {
  const match = /^\/s\/([^/]+)/.exec(pathname);
  return match ? decodeURIComponent(match[1]) : null;
}

export function sessionUrl(origin: string, session: string): string {
  return `${normalizeRuntimeUrl(origin)}/s/${encodeURIComponent(session)}`;
}

export async function signIn(origin: string, user: string, password: string): Promise<void> {
  await post(`${normalizeRuntimeUrl(origin)}/auth/login`, { user, password });
}

export async function signOut(origin: string): Promise<void> {
  await post(`${normalizeRuntimeUrl(origin)}/auth/logout`, {});
}

export async function changePassword(origin: string, current: string, next: string): Promise<void> {
  await post(`${normalizeRuntimeUrl(origin)}/auth/password`, { current, new: next });
}

/** One member's activity, as a session admin sees it: counts, no content. */
export type MemberStats = { user: string; role: Role; chats: number; live_workflows: number };

export type Team = {
  user: string | null;
  role: Role | null;
  admin: boolean;
  members?: MemberStats[];
  accounts?: string[];
  /** Provider keys an admin set, masked. */
  keys?: Record<string, string>;
  key_vars?: string[];
};

export async function fetchTeam(serveUrl: string): Promise<Team> {
  const response = await fetch(`${normalizeRuntimeUrl(serveUrl)}/v1/team`);
  if (!response.ok) throw new Error(await readError(response));
  return (await response.json()) as Team;
}

export async function setMember(serveUrl: string, user: string, role: Role): Promise<void> {
  await post(`${normalizeRuntimeUrl(serveUrl)}/v1/team/members`, { user, role });
}

export async function removeMember(serveUrl: string, user: string): Promise<void> {
  await post(`${normalizeRuntimeUrl(serveUrl)}/v1/team/members/remove`, { user });
}

/** An empty value clears the key. */
export async function setTeamKey(serveUrl: string, variable: string, value: string): Promise<void> {
  await post(`${normalizeRuntimeUrl(serveUrl)}/v1/team/keys`, { var: variable, value });
}

/** `serveUrl` is scoped to the chat (`…/chats/<id>`); only its owner may. */
export async function setVisibility(serveUrl: string, visibility: Visibility): Promise<ConversationSummary> {
  return (await post(`${normalizeRuntimeUrl(serveUrl)}/v1/chat/visibility`, { visibility })) as ConversationSummary;
}

/** A member with Mission Control open, and the chat they have open if its
    owner shares it. */
export type Online = { user: string; chat: ConversationSummary | null };

export type Presence = { online: Online[]; room_seq: number };

/** Check in as here, on `chat`; answers who else is. Call well within 30s. */
export async function checkIn(serveUrl: string, chat: string | null): Promise<Presence> {
  return (await post(`${normalizeRuntimeUrl(serveUrl)}/v1/team/presence`, { chat })) as Presence;
}

export type RoomMessage = { seq: number; user: string; text: string; at: number };

export async function fetchRoom(serveUrl: string, after: number): Promise<RoomMessage[]> {
  const response = await fetch(`${normalizeRuntimeUrl(serveUrl)}/v1/team/room?after=${after}`);
  if (!response.ok) throw new Error(await readError(response));
  return ((await response.json()) as { messages: RoomMessage[] }).messages;
}

export async function say(serveUrl: string, text: string): Promise<RoomMessage> {
  return (await post(`${normalizeRuntimeUrl(serveUrl)}/v1/team/room`, { text })) as RoomMessage;
}

async function post(url: string, body: unknown): Promise<unknown> {
  const response = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!response.ok) throw new Error(await readError(response));
  return response.json();
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
