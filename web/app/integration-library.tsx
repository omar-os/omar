"use client";

import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { catalogRequest, previousIntegrations, type IntegrationChange } from "./lib/integration-catalog";
import type { TaskConnection } from "./lib/task-connections";

type Library = {
  entries: TaskConnection[];
  loading: boolean;
  error: string;
  refresh: () => void;
  save: (changes: IntegrationChange[], importOnly?: boolean) => Promise<void>;
};

const Context = createContext<Library | null>(null);
export function useIntegrationLibrary(): Library {
  const catalog = useContext(Context);
  if (!catalog) throw new Error("Integration library is unavailable.");
  return catalog;
}

export function IntegrationLibrary({ runtime, enabled, children }: { runtime: string; enabled: boolean; children: ReactNode }) {
  const [entries, setEntries] = useState<TaskConnection[]>([]);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const queue = useRef(Promise.resolve());
  const enqueue = useCallback((operation: () => Promise<void>) => {
    const result = queue.current.then(operation);
    queue.current = result.catch(() => {});
    return result;
  }, []);
  const refresh = useCallback(() => {
    if (!enabled) return;
    void enqueue(async () => {
      setLoading(true);
      try {
        let list = await catalogRequest(runtime);
        let legacy: TaskConnection[] = [];
        try { legacy = previousIntegrations(localStorage, runtime); } catch { /* Browser storage is optional. */ }
        const known = new Set(list.map(entry => entry.id));
        const missing = legacy.filter(entry => !known.has(entry.id));
        for (let i = 0; i < missing.length; i += 32) {
          list = await catalogRequest(runtime, missing.slice(i, i + 32).map(connection => ({ connection })), true);
        }
        setEntries(list);
        setError("");
      } catch (cause) {
        setError(cause instanceof Error ? cause.message : "Could not load integrations.");
      } finally { setLoading(false); }
    });
  }, [runtime, enabled, enqueue]);

  useEffect(() => { refresh(); }, [refresh]);
  const save = useCallback(async (changes: IntegrationChange[], importOnly = false) => {
    if (!changes.length) return;
    if (!enabled) throw new Error("Connect to the operator runtime to save integrations.");
    await enqueue(async () => {
      setEntries(await catalogRequest(runtime, changes, importOnly));
      setError("");
    });
  }, [runtime, enabled, enqueue]);
  return <Context.Provider value={{ entries, loading, error, refresh, save }}>{children}</Context.Provider>;
}
