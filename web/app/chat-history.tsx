"use client";

import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { forgetWorkflowCache, saveWorkflowName, useWorkflowNames } from "./lib/workflow-names";
import { WorkflowIcon } from "./workflow-icon";
import { WorkflowSortMenu, type WorkflowSort } from "./workflow-sort-menu";
import type { ConversationSummary } from "./lib/protocol";
import { fetchConversations, deleteConversation, fetchArchivedConversations, setConversationArchived, selectConversation } from "./lib/runtime-client";

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

export function ChatHistory({ serveUrl, activeId, mobile, revision, collapsed, onClose, onSelect, onSwitchingChange }: {
  serveUrl: string;
  activeId: string;
  mobile: boolean;
  revision: string;
  collapsed: boolean;
  onClose: () => void;
  onSelect: (conversation: ConversationSummary) => void;
  onSwitchingChange: (switching: boolean) => void;
}) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const [chats, setChats] = useState<ConversationSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [switching, setSwitching] = useState(false);
  const [error, setError] = useState("");
  const [actionError, setActionError] = useState("");
  const [query, setQuery] = useState("");
  const [showArchive, setShowArchive] = useState(false);
  const [archivedChats, setArchivedChats] = useState<ConversationSummary[]>([]);
  const [lastArchived, setLastArchived] = useState<ConversationSummary | null>(null);
  useEffect(() => {
    if (!lastArchived) return;
    const timeout = setTimeout(() => setLastArchived(null), 3000);
    return () => clearTimeout(timeout);
  }, [lastArchived]);
  const [deleteTarget, setDeleteTarget] = useState<ConversationSummary | null>(null);
  const [deleteError, setDeleteError] = useState("");
  const mutation = useRef(0);
  const selectionRef = useRef({activeId, onSelect});
  useEffect(() => { selectionRef.current = {activeId, onSelect}; }, [activeId, onSelect]);
  const [retry, setRetry] = useState(0);
  const [sort, setSort] = useState<WorkflowSort>("recent");
  const names = useWorkflowNames(serveUrl);
  const namesRef = useRef(names);
  useEffect(() => { namesRef.current = names; }, [names]);
  const [editing, setEditing] = useState<string | null>(null);
  const [editedName, setEditedName] = useState("");
  const cancelRename = useRef(false);
  const migratedNames = useRef(new Set<string>());
  const displayName = (chat: ConversationSummary) => chat.workflow_name ?? names[chat.id]?.name ?? chat.title;
  const updatedAt = (chat: ConversationSummary) => Math.max(chat.updated_at, names[chat.id]?.updatedAt ?? 0);
  async function saveName(id: string) {
    if (cancelRename.current) return;
    try { await saveWorkflowName(serveUrl, id, editedName); setEditing(null); setError(""); }
    catch (cause) { setError(cause instanceof Error ? cause.message : "Could not save the workflow name to this workspace."); }
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
      const version = mutation.current;
      void Promise.all([fetchConversations(serveUrl, abort.signal), showArchive ? fetchArchivedConversations(serveUrl, abort.signal) : Promise.resolve(null)]).then(([history, removed]) => {
        if (abort.signal.aborted || version !== mutation.current) return;
        if (removed) setArchivedChats(removed);
        const selection = selectionRef.current;
        if (history.removed_ids?.includes(selection.activeId)) {
          const next = history.conversations.find(chat => chat.id === history.active_id);
          if (next) selection.onSelect(next);
        }
        setChats(history.conversations);
        for (const chat of history.conversations) {
          const local = namesRef.current[chat.id];
          if (local && !chat.workflow_name && !migratedNames.current.has(chat.id)) {
            migratedNames.current.add(chat.id);
            void saveWorkflowName(serveUrl, chat.id, local.name).catch(() => migratedNames.current.delete(chat.id));
          }
        }
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
  }, [serveUrl, revision, retry, showArchive]);

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

  async function archiveOrRestore(chat: ConversationSummary, removed: boolean) {
    mutation.current++;
    setSwitching(true);
    setActionError("");
    try {
      const history = await setConversationArchived(serveUrl, chat.id, removed);
      setChats(history.conversations);
      setArchivedChats(current => removed ? [chat, ...current.filter(value => value.id !== chat.id)] : current.filter(value => value.id !== chat.id));
      setLastArchived(removed ? chat : null);
      if (removed && chat.id === activeId) {
        const next = history.conversations.find(value => value.id === history.active_id)!;
        onSelect(next);
      }
      setRetry(current => current + 1);
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : String(cause));
    } finally { mutation.current++; setSwitching(false); }
  }

  async function permanentlyDelete(chat: ConversationSummary) {
    mutation.current++;
    setSwitching(true);
    setDeleteError("");
    try {
      await deleteConversation(serveUrl, chat.id);
      forgetWorkflowCache(serveUrl, chat.id);
      setArchivedChats(current => current.filter(value => value.id !== chat.id));
      setLastArchived(current => current?.id === chat.id ? null : current);
      setDeleteTarget(null);
      setRetry(current => current + 1);
    } catch (cause) {
      setDeleteError(cause instanceof Error ? cause.message : String(cause));
    } finally { mutation.current++; setSwitching(false); }
  }

  const visible = (showArchive ? archivedChats : chats).filter((chat) => `${displayName(chat)} ${chat.run?.team ?? ""}`.toLowerCase().includes(query.toLowerCase())).sort((a, b) => {
    const byName = () => displayName(a).localeCompare(displayName(b), undefined, { sensitivity: "base", numeric: true });
    if (sort === "name") return byName() || updatedAt(b) - updatedAt(a);
    if (sort === "status") {
      const priority = workflowStatusPriority(a) - workflowStatusPriority(b);
      if (priority) return priority;
    }
    return updatedAt(b) - updatedAt(a) || byName();
  });
  // Folded: the divider's handle beside the conversation is the way back.
  if (collapsed) return null;
  const content = <>
    {deleteTarget ? <DeleteWorkflowDialog name={displayName(deleteTarget)} busy={switching} error={deleteError} onCancel={() => setDeleteTarget(null)} onConfirm={() => void permanentlyDelete(deleteTarget)} /> : null}
    <header>
      <div className="brand-lockup"><OmarLogo /><strong>OMAR</strong></div>
    </header>
    <div className="sidebar-heading">
      <div><span className="eyebrow">WORKSPACE</span><h2 id="chat-history-title">Workflows</h2></div>
      <span className="workflow-count">{showArchive ? archivedChats.length : chats.length}</span>
    </div>
    <div className="history-actions">
      <button className="new-workflow" type="button" disabled={loading || switching} onClick={() => void select()}><span className="new-icon"><WorkflowIcon name="plus" size={15} /></span>New workflow</button>
      <label className="workflow-search"><WorkflowIcon name="search" size={15} /><input aria-label="Search workflows" placeholder="Search workflows…" value={query} onChange={(event) => setQuery(event.target.value)} /></label>
    </div>
    <div className="history-label"><span>{showArchive ? "ARCHIVE" : sort === "recent" ? "RECENT" : sort === "name" ? "NAME" : "STATUS"}</span><div className="history-view-actions">
      <button type="button" className="history-archive-toggle" aria-label={showArchive ? "Back to workflows" : "Archive"} title={showArchive ? "Back to workflows" : "Archive"} onClick={() => { setShowArchive(value => !value); setQuery(""); setError(""); }}><WorkflowIcon name={showArchive ? "back" : "archive"} size={16} /></button>
      {!showArchive ? <WorkflowSortMenu value={sort} onChange={setSort} /> : null}
    </div></div>
    {lastArchived ? <div className="history-undo" role="status"><span>Workflow archived.</span><button type="button" disabled={switching} onClick={() => void archiveOrRestore(lastArchived, false)}>Undo</button></div> : null}
    {error || actionError ? <div className="history-error"><p role="alert">{error || actionError}</p><button type="button" onClick={() => { setActionError(""); setRetry((current) => current + 1); }}>Retry</button></div> : null}
    <ul aria-label={showArchive ? "Archive" : "Saved chats"} aria-busy={loading || switching}>
      {visible.map((chat) => (
        <li key={chat.id} className="workflow-row">
          {showArchive ? <div className="archived-workflow"><span className="workflow-title">{displayName(chat)}</span><small>{chat.message_count} messages · Saved workflow</small><div className="archived-workflow-actions"><button type="button" disabled={switching} onClick={() => void archiveOrRestore(chat, false)}>Restore workflow</button><button type="button" className="workflow-delete" aria-label="Delete workflow permanently" title={`Delete ${displayName(chat)} permanently`} disabled={switching} onClick={() => { setDeleteError(""); setDeleteTarget(chat); }}><WorkflowIcon name="trash" size={16} /></button></div></div> : <>
          <button className="workflow-select" type="button" disabled={switching} aria-current={chat.id === activeId ? "true" : undefined} aria-keyshortcuts="F2" onKeyDown={(event) => { if (event.key === "F2") { event.preventDefault(); cancelRename.current = false; setEditedName(displayName(chat)); setEditing(chat.id); } }} onClick={() => void select(chat.id)}>
            <span className="workflow-name"><span className="workflow-title" title={`${displayName(chat)} · Double-click to rename (saved automatically)`} onDoubleClick={(event) => { event.preventDefault(); event.stopPropagation(); cancelRename.current = false; setEditedName(displayName(chat)); setEditing(chat.id); }}>{displayName(chat)}</span>{chat.id === activeId ? <WorkflowIcon name="chevron" size={14} /> : null}</span>
            <span className="workflow-description">{chat.run?.team ?? (chat.message_count ? `${chat.message_count} messages in this workflow` : "Describe what you want to automate")}</span>
            <span className="workflow-meta"><small className={`workflow-status status-${workflowStatus(chat).toLowerCase()}`}><i />{workflowStatus(chat)}</small><time dateTime={new Date(updatedAt(chat)).toISOString()}>{new Date(updatedAt(chat)).toLocaleDateString(undefined, { month: "short", day: "numeric" })}</time>{chat.busy && chat.run ? <small>Thinking</small> : null}</span>
          </button>
          <button type="button" className="workflow-archive" aria-label="Archive workflow" title={archiveReason(chat) || `Archive ${displayName(chat)}`} disabled={switching || !!archiveReason(chat)} onClick={() => void archiveOrRestore(chat, true)}><WorkflowIcon name="archive" size={16} /></button>
          {editing === chat.id ? <input className="workflow-rename" aria-label="Workflow name" maxLength={120} value={editedName} autoFocus onFocus={(event) => event.target.select()} onChange={(event) => setEditedName(event.target.value)} onBlur={() => void saveName(chat.id)} onKeyDown={(event) => {
            if (event.key === "Escape") { event.preventDefault(); cancelRename.current = true; setEditing(null); }
            if (event.key === "Enter") { event.preventDefault(); void saveName(chat.id); }
          }} /> : null}
          </>}
        </li>
      ))}
    </ul>
    {loading ? <p role="status">Loading workflows…</p> : visible.length === 0 && !error ? <p>{showArchive ? "No archived workflows." : "No workflows found."}</p> : null}
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

function archiveReason(chat: ConversationSummary): string | null {
  if (chat.busy) return "Wait for the assistant to finish before archiving this workflow.";
  if (chat.run && ["starting", "running", "stopping", "pausing", "paused"].includes(chat.run.status)) return "Stop this workflow before archiving it.";
  return null;
}

function DeleteWorkflowDialog({ name, busy, error, onCancel, onConfirm }: {
  name: string; busy: boolean; error: string; onCancel: () => void; onConfirm: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const element = dialog.current;
    const opener = document.activeElement as HTMLElement | null;
    element?.showModal();
    return () => { element?.close(); opener?.focus(); };
  }, []);
  return <dialog ref={dialog} className="delete-workflow-dialog" aria-labelledby="delete-workflow-title" aria-describedby="delete-workflow-description" onCancel={event => { event.preventDefault(); event.stopPropagation(); if (!busy) onCancel(); }}>
    <h2 id="delete-workflow-title">Delete workflow?</h2>
    <p id="delete-workflow-description">Permanently delete <strong>{name}</strong> and its saved conversation and workflow configuration? This cannot be undone.</p>
    <p className="delete-workflow-note">Generated files, run records, and existing backups are kept.</p>
    {error ? <p role="alert">{error}</p> : null}
    <div className="delete-workflow-actions"><button type="button" autoFocus disabled={busy} onClick={onCancel}>Cancel</button><button type="button" className="confirm-workflow-delete" disabled={busy} onClick={onConfirm}>{busy ? "Deleting…" : "Delete permanently"}</button></div>
  </dialog>;
}
