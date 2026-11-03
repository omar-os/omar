"use client";

import { useEffect, useRef, useState, useSyncExternalStore, type RefObject } from "react";
import { saveWorkflowName, useWorkflowNames } from "./lib/workflow-names";
import { WorkflowIcon } from "./workflow-icon";
import { WorkflowSortMenu, type WorkflowSort } from "./workflow-sort-menu";
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
  const [sort, setSort] = useState<WorkflowSort>("recent");
  const names = useWorkflowNames(serveUrl);
  const [editing, setEditing] = useState<string | null>(null);
  const [editedName, setEditedName] = useState("");
  const cancelRename = useRef(false);
  const displayName = (chat: ConversationSummary) => names[chat.id]?.name ?? chat.title;
  const updatedAt = (chat: ConversationSummary) => Math.max(chat.updated_at, names[chat.id]?.updatedAt ?? 0);
  function saveName(id: string) {
    if (cancelRename.current) return;
    try { saveWorkflowName(serveUrl, id, editedName); setEditing(null); setError(""); }
    catch (cause) { setError(cause instanceof Error ? cause.message : "Could not save the workflow name in this browser."); }
  }

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

  const visible = chats.filter((chat) => `${displayName(chat)} ${chat.run?.team ?? ""}`.toLowerCase().includes(query.toLowerCase())).sort((a, b) => {
    const byName = () => displayName(a).localeCompare(displayName(b), undefined, { sensitivity: "base", numeric: true });
    if (sort === "name") return byName() || updatedAt(b) - updatedAt(a);
    if (sort === "status") {
      const priority = workflowStatusPriority(a) - workflowStatusPriority(b);
      if (priority) return priority;
    }
    return updatedAt(b) - updatedAt(a) || byName();
  });
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
      <div className="brand-lockup"><OmarLogo /><strong>OMAR</strong></div>
      <button type="button" className="history-fold" onClick={onClose} aria-label="Fold chat history" title="Fold sidebar">
        <SidebarIcon direction="close" />
      </button>
    </header>
    <div className="sidebar-heading">
      <div><span className="eyebrow">WORKSPACE</span><h2 id="chat-history-title">Workflows</h2></div>
      <span className="workflow-count">{chats.length}</span>
    </div>
    <div className="history-actions">
      <button className="new-workflow" type="button" disabled={loading || switching} onClick={() => void select()}><span className="new-icon"><WorkflowIcon name="plus" size={15} /></span>New workflow</button>
      <label className="workflow-search"><WorkflowIcon name="search" size={15} /><input aria-label="Search workflows" placeholder="Search workflows…" value={query} onChange={(event) => setQuery(event.target.value)} /></label>
    </div>
    <div className="history-label"><span>{sort === "recent" ? "RECENT" : sort === "name" ? "NAME" : "STATUS"}</span><WorkflowSortMenu value={sort} onChange={setSort} /></div>
    {error ? <div className="history-error"><p role="alert">{error}</p><button type="button" onClick={() => setRetry((current) => current + 1)}>Retry</button></div> : null}
    <ul aria-label="Saved chats" aria-busy={loading || switching}>
      {visible.map((chat) => (
        <li key={chat.id} className="workflow-row">
          <button type="button" disabled={switching} aria-current={chat.id === activeId ? "true" : undefined} aria-keyshortcuts="F2" onKeyDown={(event) => { if (event.key === "F2") { event.preventDefault(); cancelRename.current = false; setEditedName(displayName(chat)); setEditing(chat.id); } }} onClick={() => void select(chat.id)}>
            <span className="workflow-name"><span className="workflow-title" title={`${displayName(chat)} · Double-click to rename (saved in this browser)`} onDoubleClick={(event) => { event.preventDefault(); event.stopPropagation(); cancelRename.current = false; setEditedName(displayName(chat)); setEditing(chat.id); }}>{displayName(chat)}</span>{chat.id === activeId ? <WorkflowIcon name="chevron" size={14} /> : null}</span>
            <span className="workflow-description">{chat.run?.team ?? (chat.message_count ? `${chat.message_count} messages in this workflow` : "Describe what you want to automate")}</span>
            <span className="workflow-meta"><small className={`workflow-status status-${workflowStatus(chat).toLowerCase()}`}><i />{workflowStatus(chat)}</small><time dateTime={new Date(updatedAt(chat)).toISOString()}>{new Date(updatedAt(chat)).toLocaleDateString(undefined, { month: "short", day: "numeric" })}</time>{chat.busy && chat.run ? <small>Thinking</small> : null}</span>
          </button>
          {editing === chat.id ? <input className="workflow-rename" aria-label="Workflow name" maxLength={120} value={editedName} autoFocus onFocus={(event) => event.target.select()} onChange={(event) => setEditedName(event.target.value)} onBlur={() => saveName(chat.id)} onKeyDown={(event) => {
            if (event.key === "Escape") { event.preventDefault(); cancelRename.current = true; setEditing(null); }
            if (event.key === "Enter") { event.preventDefault(); saveName(chat.id); }
          }} /> : null}
        </li>
      ))}
    </ul>
    {loading ? <p role="status">Loading workflows…</p> : visible.length === 0 && !error ? <p>No workflows found.</p> : null}
  </>;
  return mobile ? (
    <dialog id="chat-history" ref={dialogRef} className="chat-history history-drawer" aria-label="Chat history" onCancel={onClose} onClick={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <div className="history-content">{content}</div>
    </dialog>
  ) : (
    <aside id="chat-history" className="chat-history" aria-label="Chat history">{content}</aside>
  );
}

/** The sidebar presents the runtime's actual state, including completed runs. */
function workflowStatus(chat: ConversationSummary): string {
  if (chat.run) return chat.run.status.charAt(0).toUpperCase() + chat.run.status.slice(1);
  return chat.busy ? "Thinking" : "Draft";
}

function workflowStatusPriority(chat: ConversationSummary): number {
  switch (workflowStatus(chat).toLowerCase()) {
    // Active work stays at the top, including startup and assistant activity.
    case "running": case "starting": case "thinking": return 0;
    case "failed": return 1;
    case "draft": return 2;
    case "paused": return 3;
    default: return 4;
  }
}
