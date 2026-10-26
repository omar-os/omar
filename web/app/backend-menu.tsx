"use client";

import { useEffect, useRef, useState, type ReactNode } from "react";
import {
  fetchBackendStatus,
  fetchBackends,
  switchBackend,
  type BackendStatus,
} from "./lib/runtime-client";

/** How each backend is written for a human. */
const LABELS: Record<string, string> = {
  claude: "Claude Code",
  codex: "Codex",
  cursor: "Cursor",
  opencode: "opencode",
  agy: "agy",
  pi: "pi",
};

function label(backend: string): string {
  return LABELS[backend] ?? backend;
}

/** One line on how a backend is signed in, or what it still needs. */
function note(status: BackendStatus | undefined): ReactNode {
  switch (status?.status) {
    case "connected":
      return status.via ?? "signed in";
    case "signed_out":
      return status.login ? (
        <>
          not signed in · run <code>{status.login}</code>
        </>
      ) : (
        "not signed in"
      );
    case "not_installed":
      return "not installed";
    default:
      return null;
  }
}

/**
 * Which backend the assistant runs on, and a way to change it.
 *
 * It opens upward because it lives at the bottom of the composer, and choosing
 * a different backend restarts the assistant — a backend is picked when the
 * process starts — so the change is confirmed rather than applied on a click.
 */
export function BackendMenu({
  serveUrl,
  live,
}: {
  serveUrl: string;
  live: boolean;
}) {
  const [current, setCurrent] = useState<string | null>(null);
  const [available, setAvailable] = useState<string[]>([]);
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [statuses, setStatuses] = useState<Record<string, BackendStatus>>({});
  const rootRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!live) return;
    let cancelled = false;
    fetchBackends(serveUrl)
      .then((backends) => {
        if (cancelled) return;
        setCurrent(backends.backend);
        setAvailable(backends.available);
      })
      // A runtime that predates this endpoint simply offers no choice.
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [serveUrl, live]);

  // Asked each time the menu opens: a sign-in may have happened meanwhile.
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    fetchBackendStatus(serveUrl)
      .then((list) => {
        if (!cancelled) {
          setStatuses(Object.fromEntries(list.map((s) => [s.name, s])));
        }
      })
      // A runtime that predates this endpoint just shows no status.
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [serveUrl, open]);

  useEffect(() => {
    if (!open) return;
    const dismiss = (event: MouseEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) {
        setOpen(false);
        setPending(null);
      }
    };
    window.addEventListener("mousedown", dismiss);
    return () => window.removeEventListener("mousedown", dismiss);
  }, [open]);

  async function choose(backend: string) {
    setError("");
    try {
      await switchBackend(serveUrl, backend);
      setCurrent(backend);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
    setPending(null);
    setOpen(false);
  }

  if (!live || available.length === 0) {
    return <span className="composer-status">Draft mode</span>;
  }

  return (
    <div className="backend-menu" ref={rootRef}>
      <button
        type="button"
        className="backend-trigger"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((was) => !was)}
      >
        {current ? label(current) : "Assistant"}
        <span aria-hidden="true">{open ? "▾" : "▴"}</span>
      </button>

      {open ? (
        <div className="backend-options" role="menu">
          {pending ? (
            <div className="backend-confirm">
              {/* Worth saying plainly: the running assistant is discarded. */}
              <p>
                Restart the assistant on {label(pending)}? Its current session
                is lost.
              </p>
              <span>
                <button type="button" onClick={() => void choose(pending)}>
                  Restart
                </button>
                <button type="button" onClick={() => setPending(null)}>
                  Cancel
                </button>
              </span>
            </div>
          ) : (
            available.map((backend) => {
              const status = statuses[backend];
              // Launching one that cannot sign in would only hang.
              const unusable =
                status?.status === "signed_out" ||
                status?.status === "not_installed";
              return (
                <button
                  key={backend}
                  type="button"
                  role="menuitem"
                  data-status={status?.status}
                  className={backend === current ? "current" : undefined}
                  disabled={unusable && backend !== current}
                  onClick={() =>
                    backend === current ? setOpen(false) : setPending(backend)
                  }
                >
                  <span className="backend-name">
                    {label(backend)}
                    {status && note(status) ? (
                      <small className="backend-note">{note(status)}</small>
                    ) : null}
                  </span>
                  {backend === current ? (
                    <span aria-hidden="true">✓</span>
                  ) : null}
                </button>
              );
            })
          )}
        </div>
      ) : null}

      {error ? <span className="backend-error">{error}</span> : null}
    </div>
  );
}
