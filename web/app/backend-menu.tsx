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

export function backendLabel(backend: string): string {
  return LABELS[backend] ?? backend;
}

/** One line on how a backend is signed in, or what it still needs. */
function note(status: BackendStatus | undefined): ReactNode {
  switch (status?.status) {
    case "connected":
      return status.via ?? "signed in";
    case "signed_out":
    case "not_installed": {
      const state =
        status.status === "signed_out" ? "not signed in" : "not installed";
      return status.login ? (
        <>
          {state} · run <code>{status.login}</code>
        </>
      ) : (
        state
      );
    }
    default:
      return null;
  }
}

/** Launching one that cannot sign in would only hang. */
function unusable(status: BackendStatus | undefined): boolean {
  return status?.status === "signed_out" || status?.status === "not_installed";
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
  onConnectMore,
}: {
  serveUrl: string;
  live: boolean;
  /** Opens the Backends page, offered while some backend is not connected. */
  onConnectMore?: () => void;
}) {
  const [current, setCurrent] = useState<string | null>(null);
  const [available, setAvailable] = useState<string[]>([]);
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [statuses, setStatuses] = useState<Record<string, BackendStatus>>({});
  // Nothing switches until status has answered: "unavailable" is a runtime
  // that predates the status endpoint, which still gets to switch; "failed"
  // is any other failure, which blocks switching until a retry answers.
  const [statusState, setStatusState] = useState<
    "loading" | "ready" | "unavailable" | "failed"
  >("loading");
  const [asked, setAsked] = useState(0);
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
        if (cancelled) return;
        // A runtime that predates this endpoint just shows no status.
        setStatuses(Object.fromEntries((list ?? []).map((s) => [s.name, s])));
        setStatusState(list ? "ready" : "unavailable");
      })
      .catch(() => {
        if (!cancelled) setStatusState("failed");
      });
    return () => {
      cancelled = true;
    };
  }, [serveUrl, open, asked]);

  /** Whether `backend` cannot be switched to as status stands. */
  function blocked(backend: string): boolean {
    return (
      statusState === "loading" ||
      statusState === "failed" ||
      unusable(statuses[backend])
    );
  }

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
    // Status may have changed since Restart was offered.
    if (blocked(backend)) {
      setPending(null);
      return;
    }
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
        onClick={() => {
          // Status is asked afresh on each open; nothing switches until then.
          if (!open) setStatusState("loading");
          setOpen(!open);
        }}
      >
        {current ? backendLabel(current) : "Assistant"}
        <span aria-hidden="true">{open ? "▾" : "▴"}</span>
      </button>

      {open ? (
        <div className="backend-options" role="menu">
          {pending ? (
            <div className="backend-confirm">
              {/* Worth saying plainly: the running assistant is discarded. */}
              <p>
                Restart the assistant on {backendLabel(pending)}? Its current
                session is lost.
              </p>
              <span>
                <button
                  type="button"
                  disabled={blocked(pending)}
                  onClick={() => void choose(pending)}
                >
                  Restart
                </button>
                <button type="button" onClick={() => setPending(null)}>
                  Cancel
                </button>
              </span>
            </div>
          ) : (
            <>
              {statusState === "failed" ? (
                <div className="backend-status-failed" role="alert">
                  Couldn&apos;t check backends.
                  <button
                    type="button"
                    onClick={() => {
                      setStatusState("loading");
                      setAsked((count) => count + 1);
                    }}
                  >
                    Retry
                  </button>
                </div>
              ) : null}
              {/* Only what can run is offered; the rest are connected on the
                  Backends page. Without status, every backend is listed. */}
              {available
                .filter(
                  (backend) =>
                    backend === current ||
                    statusState !== "ready" ||
                    statuses[backend]?.status === "connected",
                )
                .map((backend) => {
                  const status = statuses[backend];
                  return (
                    <button
                      key={backend}
                      type="button"
                      role="menuitem"
                      data-status={status?.status}
                      className={backend === current ? "current" : undefined}
                      disabled={blocked(backend) && backend !== current}
                      onClick={() =>
                        backend === current
                          ? setOpen(false)
                          : setPending(backend)
                      }
                    >
                      <span className="backend-name">
                        {backendLabel(backend)}
                        {status && note(status) ? (
                          <small className="backend-note">{note(status)}</small>
                        ) : null}
                      </span>
                      {backend === current ? (
                        <span aria-hidden="true">✓</span>
                      ) : null}
                    </button>
                  );
                })}
              {onConnectMore &&
              statusState === "ready" &&
              available.some(
                (backend) => statuses[backend]?.status !== "connected",
              ) ? (
                <button
                  type="button"
                  role="menuitem"
                  className="backend-connect-more"
                  onClick={() => {
                    setOpen(false);
                    onConnectMore();
                  }}
                >
                  Connect more backends…
                </button>
              ) : null}
            </>
          )}
        </div>
      ) : null}

      {error ? <span className="backend-error">{error}</span> : null}
    </div>
  );
}
