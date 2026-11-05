"use client";

import { useCallback, useSyncExternalStore } from "react";

const CHANGE = "omar-workflow-name";
type SavedName = { name: string; updatedAt: number };
const key = (base: string) => `omar:workflow-names:${base.replace(/\/$/, "")}`;

function subscribe(listener: () => void) {
  window.addEventListener("storage", listener);
  window.addEventListener(CHANGE, listener);
  return () => { window.removeEventListener("storage", listener); window.removeEventListener(CHANGE, listener); };
}

/** Display names stay in this browser; they never rewrite a compiled team name. */
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

export function saveWorkflowName(base: string, id: string, name: string) {
  const trimmed = name.trim();
  if (!trimmed) throw new Error("Enter a workflow name.");
  let saved: Record<string, SavedName> = {};
  try { saved = JSON.parse(localStorage.getItem(key(base)) ?? "{}"); } catch { /* Start with an empty local naming store. */ }
  localStorage.setItem(key(base), JSON.stringify({ ...saved, [id]: { name: trimmed.slice(0, 120), updatedAt: Date.now() } }));
  window.dispatchEvent(new Event(CHANGE));
}
