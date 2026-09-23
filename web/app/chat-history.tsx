"use client";

import { useEffect, useRef, useState, useSyncExternalStore, type RefObject } from "react";
import type { ConversationSummary } from "./lib/protocol";
import { fetchConversations, selectConversation } from "./lib/runtime-client";

const mobileQuery = "(max-width: 900px)";
function subscribeViewport(onChange: () => void) {
  const query = window.matchMedia(mobileQuery);
  query.addEventListener("change", onChange);
  return () => query.removeEventListener("change", onChange);
}
export function useHistoryDrawer() {
  return useSyncExternalStore(subscribeViewport, () => window.matchMedia(mobileQuery).matches, () => false);
}

export function OmarLogo() {
  // Static artwork needs no image loader or remote request.
  // eslint-disable-next-line @next/next/no-img-element
  return <img className="brand-mark" src="/omar-logo.png" alt="Omar" width={40} height={40} />;
}

export function SidebarIcon({ direction }: { direction: "open" | "close" }) {
  return (
    <svg className="sidebar-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true">
      <rect x="3" y="4" width="18" height="16" rx="2" />
      <path d="M9 4v16" />
      {direction === "open" ? <path d="m12 9 3 3-3 3" /> : <path d="m14 9-3 3 3 3" />}
    </svg>
  );
}

export function ChatHistory({ serveUrl, activeId, mobile, revision, collapsed, onClose, onOpen, onSelect, onSwitchingChange, railButtonRef }: {
  serveUrl: string;
  activeId: string;
  mobile: boolean;
  revision: string;
  collapsed: boolean;
  onClose: () => void;
  onOpen: () => void;
  onSelect: (conversation: ConversationSummary) => void;
  onSwitchingChange: (switching: boolean) => void;
  railButtonRef: RefObject<HTMLButtonElement | null>;
}) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const [chats, setChats] = useState<ConversationSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [switching, setSwitching] = useState(false);
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const [retry, setRetry] = useState(0);

  // Only the narrow-screen drawer is modal. Desktop history stays beside the
  // conversation and never captures focus from the composer or topology.
  useEffect(() => {
    if (!mobile) return;
    const dialog = dialogRef.current;
    const opener = document.activeElement as HTMLElement | null;
    dialog?.showModal();
    return () => {
      dialog?.close();
      opener?.focus();
    };
  }, [mobile]);

  // The sidebar stays mounted: update its title, count, ordering and selection
  // after chat-stream changes, including selections made in another tab.
  useEffect(() => {
    const abort = new AbortController();
    const refresh = () => {
      void fetchConversations(serveUrl, abort.signal).then((history) => {
        if (abort.signal.aborted) return;
        setChats(history.conversations);
        setError("");
        setLoading(false);
      }).catch((cause) => {
        if (abort.signal.aborted) return;
        setError(cause instanceof Error ? cause.message : String(cause));
        setLoading(false);
      });
    };
    const timer = setTimeout(refresh, 150);
    const poll = setInterval(refresh, 2000);
    return () => { clearTimeout(timer); clearInterval(poll); abort.abort(); };
  }, [serveUrl, revision, retry]);

  async function select(id?: string) {
    if (id === activeId) {
      if (mobile) onClose();
      return;
    }
    setSwitching(true);
    onSwitchingChange(true);
    setError("");
    try {
      const conversation = await selectConversation(serveUrl, id);
      setQuery("");
      setChats((current) => [conversation, ...current.filter((chat) => chat.id !== conversation.id)]);
      onSelect(conversation);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSwitching(false);
      onSwitchingChange(false);
    }
  }

  const visible = chats.filter((chat) => chat.title.toLowerCase().includes(query.toLowerCase()));
  if (collapsed) {
    return (
      <nav className="history-rail" aria-label="Chat navigation">
        <OmarLogo />
        <button
          ref={railButtonRef}
          type="button"
          onClick={onOpen}
          aria-label="Open chat history"
          title="Open sidebar"
        >
          <SidebarIcon direction="open" />
        </button>
      </nav>
    );
  }
  const content = <>
    <header>
      <OmarLogo />
      <button type="button" className="history-fold" onClick={onClose} aria-label="Fold chat history" title="Fold sidebar">
        <SidebarIcon direction="close" />
      </button>
    </header>
    <h2 id="chat-history-title">Recent chats</h2>
    <div className="history-actions">
      <button type="button" disabled={loading || switching} onClick={() => void select()}>+ New chat</button>
      <input aria-label="Search chats" placeholder="Search chats…" value={query} onChange={(event) => setQuery(event.target.value)} />
    </div>
    {error ? <div className="history-error"><p role="alert">{error}</p><button type="button" onClick={() => setRetry((current) => current + 1)}>Retry</button></div> : null}
    <ul aria-label="Saved chats" aria-busy={loading || switching}>
      {visible.map((chat) => (
        <li key={chat.id}>
          <button type="button" disabled={switching} aria-current={chat.id === activeId ? "true" : undefined} onClick={() => void select(chat.id)}>
            <span>{chat.title}</span>
            <small>{new Date(chat.updated_at).toLocaleDateString()}{chat.busy ? " · Thinking" : ""}{chat.run && ["starting", "running", "stopping"].includes(chat.run.status) ? <> · <strong className="chat-running">Running</strong></> : null}</small>
          </button>
        </li>
      ))}
    </ul>
    {loading ? <p role="status">Loading conversations…</p> : visible.length === 0 && !error ? <p>No chats found.</p> : null}
  </>;
  return mobile ? (
    <dialog id="chat-history" ref={dialogRef} className="chat-history history-drawer" aria-label="Chat history" onCancel={onClose} onClick={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <div className="history-content">{content}</div>
    </dialog>
  ) : (
    <aside id="chat-history" className="chat-history" aria-label="Chat history">{content}</aside>
  );
}
