"use client";

import { useCallback, useSyncExternalStore } from "react";
import { updateWorkflow } from "./workflow-state";

const CHANGE = "omar-workflow-name";
type SavedName = { name: string; updatedAt: number };
const key = (base: string) => `omar:workflow-names:${base.replace(/\/$/, "")}`;

function subscribe(listener: () => void) {
  window.addEventListener("storage", listener);
  window.addEventListener(CHANGE, listener);
  return () => { window.removeEventListener("storage", listener); window.removeEventListener(CHANGE, listener); };
}

/** Browser cache for display names. The server persists them independently of compiled team names. */
export function useWorkflowNames(base: string): Record<string, SavedName> {
  const read = useCallback(() => {
    try { return localStorage.getItem(key(base)); } catch { return null; }
  }, [base]);
  const value = useSyncExternalStore(subscribe, read, () => null);
  try {
    const saved = value ? JSON.parse(value) : {};
    return Object.fromEntries(Object.entries(saved).filter(([, value]) => typeof (value as SavedName)?.name === "string" && typeof (value as SavedName)?.updatedAt === "number")) as Record<string, SavedName>;
  } catch { return {}; }
}

export async function saveWorkflowName(base: string, id: string, name: string) {
  const trimmed = name.trim();
  if (!trimmed) throw new Error("Enter a workflow name.");
  await updateWorkflow(`${base.replace(/\/$/, "")}/chats/${encodeURIComponent(id)}`, { revision: 0, name: trimmed.slice(0, 120) });
  let saved: Record<string, SavedName> = {};
  try { saved = JSON.parse(localStorage.getItem(key(base)) ?? "{}"); } catch { /* Start with an empty local naming store. */ }
  localStorage.setItem(key(base), JSON.stringify({ ...saved, [id]: { name: trimmed.slice(0, 120), updatedAt: Date.now() } }));
  window.dispatchEvent(new Event(CHANGE));
}

export function forgetWorkflowCache(base: string, id: string) {
  try {
    const saved = JSON.parse(localStorage.getItem(key(base)) ?? "{}");
    delete saved[id];
    localStorage.setItem(key(base), JSON.stringify(saved));
    localStorage.removeItem(`omar:workflow-draft:${base.replace(/\/$/, "")}/chats/${encodeURIComponent(id)}`);
  } catch { /* A disabled browser cache must not undo a successful server deletion. */ }
  window.dispatchEvent(new Event(CHANGE));
}
