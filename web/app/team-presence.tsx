"use client";

import { useEffect, useRef, useState } from "react";
import type { ConversationSummary } from "./lib/protocol";
import { checkIn, fetchRoom, say, type Online, type RoomMessage } from "./lib/team-client";

/** Well inside the daemon's 30s, so one slow check-in does not drop anyone. */
const CHECK_IN_MS = 10_000;
const ROOM_SHOWN = 500;

export type TeamPresence = {
  /** Everyone here, the signed-in member too. */
  online: Online[];
  messages: RoomMessage[];
  /** Others' messages since the room was last open. */
  unread: number;
  markRead: () => void;
  /** A message this tab just sent, shown before the next check-in. */
  sent: (message: RoomMessage) => void;
};

function readKey(serveUrl: string) {
  return `omar-room-read:${serveUrl}`;
}

/**
 * Check in with the session every few seconds, naming the chat on screen,
 * and keep the team room current. Off outside a gateway: the operator on the
 * loopback port is not a member.
 */
export function useTeamPresence(serveUrl: string, chat: string | null, user: string | undefined): TeamPresence {
  const [online, setOnline] = useState<Online[]>([]);
  const [messages, setMessages] = useState<RoomMessage[]>([]);
  // Unknown until this browser first opens the room or loads it: then what
  // is already there counts as read, so a first visit starts at zero.
  const [readSeq, setReadSeq] = useState<number | null>(() => {
    try {
      const saved = localStorage.getItem(readKey(serveUrl));
      return saved ? Number(saved) : null;
    } catch {
      return null;
    }
  });
  const seq = useRef(0);

  function append(newer: RoomMessage[]) {
    if (newer.length === 0) return;
    seq.current = Math.max(seq.current, newer[newer.length - 1].seq);
    setMessages((current) => {
      const last = current.at(-1)?.seq ?? 0;
      return [...current, ...newer.filter((message) => message.seq > last)].slice(-ROOM_SHOWN);
    });
  }

  useEffect(() => {
    if (!user) return;
    let live = true;
    const tick = async () => {
      try {
        const presence = await checkIn(serveUrl, chat);
        if (!live) return;
        setOnline(presence.online);
        setReadSeq((current) => current ?? presence.room_seq);
        if (presence.room_seq > seq.current) {
          const newer = await fetchRoom(serveUrl, seq.current);
          if (live) append(newer);
        }
      } catch {
        // The studio already says when the runtime is down; try next tick.
      }
    };
    void tick();
    const timer = setInterval(() => void tick(), CHECK_IN_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [serveUrl, chat, user]);

  const markRead = () => {
    setReadSeq(seq.current);
    try { localStorage.setItem(readKey(serveUrl), String(seq.current)); } catch { /* Unread counts reset next visit. */ }
  };
  const unread = readSeq === null ? 0 : messages.filter((message) => message.user !== user && message.seq > readSeq).length;
  return { online, messages, unread, markRead, sent: (message) => append([message]) };
}

/** A stable colour per name, readable on both themes. */
function hue(user: string) {
  let hash = 0;
  for (const char of user) hash = (hash * 31 + char.charCodeAt(0)) % 360;
  return hash;
}

export function Avatar({ user, size = "normal", here = false }: { user: string; size?: "normal" | "small"; here?: boolean }) {
  return (
    <span className={`presence-avatar ${size}${here ? " here" : ""}`} style={{ "--avatar-hue": hue(user) } as React.CSSProperties} aria-hidden="true">
      {user.slice(0, 1).toUpperCase()}
    </span>
  );
}

function chatName(chat: ConversationSummary) {
  return chat.workflow_name || chat.title;
}

/**
 * Who else has this session open, as in a shared document. A member whose
 * chat is shared can be followed there; a private one shows only that they
 * are here.
 */
export function PresenceAvatars({ online, user, current, onOpen }: {
  online: Online[];
  user: string;
  current: string | null;
  onOpen: (chat: ConversationSummary) => void;
}) {
  const others = online.filter((entry) => entry.user !== user);
  if (others.length === 0) return null;
  return (
    <div className="presence" role="group" aria-label="Online now">
      {others.map(({ user: other, chat }) => {
        const where = chat ? `viewing “${chatName(chat)}”` : "online";
        return (
          <button key={other} type="button" className="presence-member" title={`${other} · ${where}`} aria-label={`${other}, ${where}`}
            disabled={!chat} onClick={() => { if (chat) onOpen(chat); }}>
            <Avatar user={other} here={chat?.id === current} />
          </button>
        );
      })}
    </div>
  );
}

/** Others' avatars on the workflows they have open, for the history list. */
export function viewersByChat(online: Online[], user: string | undefined): Record<string, string[]> {
  const viewers: Record<string, string[]> = {};
  for (const entry of online) {
    if (entry.user === user || !entry.chat) continue;
    (viewers[entry.chat.id] ??= []).push(entry.user);
  }
  return viewers;
}

/** The session's room: every member, one conversation, no agents. */
export function TeamRoom({ serveUrl, session, user, presence, onClose }: {
  serveUrl: string;
  session: string;
  user: string;
  presence: TeamPresence;
  onClose: () => void;
}) {
  const [draft, setDraft] = useState("");
  const [error, setError] = useState("");
  const [sending, setSending] = useState(false);
  const list = useRef<HTMLOListElement>(null);
  const count = presence.messages.length;
  useEffect(() => {
    list.current?.scrollTo({ top: list.current.scrollHeight });
  }, [count]);

  async function send() {
    const text = draft.trim();
    if (!text || sending) return;
    setSending(true);
    try {
      presence.sent(await say(serveUrl, text));
      setDraft("");
      setError("");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSending(false);
    }
  }

  const here = presence.online.map((entry) => entry.user);
  return (
    <aside className="team-room" aria-label="Team room">
      <header>
        <div>
          <h2>{session}</h2>
          <small>{here.length} online · everyone in this session</small>
        </div>
        <button type="button" onClick={onClose}>Close</button>
      </header>
      <ol ref={list} className="team-room-messages">
        {presence.messages.length === 0 ? <li className="team-room-empty">No messages yet. Say hello.</li> : null}
        {presence.messages.map((message) => (
          <li key={message.seq} className={message.user === user ? "mine" : undefined}>
            <Avatar user={message.user} size="small" />
            <div>
              <span className="team-room-meta"><strong>{message.user}</strong><time dateTime={new Date(message.at * 1000).toISOString()}>{new Date(message.at * 1000).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" })}</time></span>
              <p>{message.text}</p>
            </div>
          </li>
        ))}
      </ol>
      {error ? <p className="team-error" role="alert">{error}</p> : null}
      <form className="team-room-composer" onSubmit={(event) => { event.preventDefault(); void send(); }}>
        <textarea aria-label="Message the team" placeholder="Message the team" rows={2} value={draft} maxLength={2000}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void send(); } }} />
        <button type="submit" disabled={sending || !draft.trim()}>Send</button>
      </form>
    </aside>
  );
}
