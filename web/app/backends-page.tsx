"use client";

import { useCallback, useEffect, useState } from "react";
import { AgentTerminal } from "./agent-terminal";
import {
  checkSignIn,
  connectTerminalUrl,
  fetchBackendStatus,
  startConnect,
  type BackendStatus,
} from "./lib/runtime-client";
import { backendLabel } from "./backend-menu";

/** While a sign-in is open, how often its backend is asked whether it took. */
const POLL_MS = 3000;

const NOTE =
  "This is the backend's own install and sign-in; it closes once the backend answers a test prompt.";

function describe(status: BackendStatus, rejected?: string): string {
  if (rejected) return "Signed in, but its test prompt failed";
  switch (status.status) {
    case "connected":
      return status.via ? `Connected · ${status.via}` : "Connected";
    case "signed_out":
      return "Not signed in";
    case "not_installed":
      return "Not installed";
    default:
      return status.reason ? `Unknown · ${status.reason}` : "Unknown";
  }
}

/**
 * Where the operator gets backends ready: each one's status, and a Connect
 * that runs `omar connect <backend>` in a terminal here, the same install and
 * sign-in as on the command line. Once the backend reports connected, its CLI
 * answers a test prompt, since a stored key can still be rejected; the
 * terminal closes only when that works.
 */
export function BackendsPage({
  serveUrl,
  onClose,
}: {
  serveUrl: string;
  onClose: () => void;
}) {
  const [statuses, setStatuses] = useState<BackendStatus[] | null>(null);
  const [error, setError] = useState("");
  const [connecting, setConnecting] = useState<string | null>(null);
  // "waiting" for the sign-in, "checking" its test prompt, or "rejected".
  const [phase, setPhase] = useState<"waiting" | "checking" | "rejected">(
    "waiting",
  );
  // Backends whose test prompt failed, and the CLI's reason.
  const [rejected, setRejected] = useState<Record<string, string>>({});

  const refresh = useCallback(async () => {
    try {
      const list = await fetchBackendStatus(serveUrl);
      setStatuses(list ?? []);
      setError(list ? "" : "This runtime predates backend status.");
      return list ?? [];
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      return null;
    }
  }, [serveUrl]);

  useEffect(() => {
    let cancelled = false;
    fetchBackendStatus(serveUrl)
      .then((list) => {
        if (cancelled) return;
        setStatuses(list ?? []);
        if (!list) setError("This runtime predates backend status.");
      })
      .catch((cause) => {
        if (!cancelled) setError(String(cause?.message ?? cause));
      });
    return () => {
      cancelled = true;
    };
  }, [serveUrl]);

  const check = useCallback(
    async (backend: string) => {
      setPhase("checking");
      try {
        await checkSignIn(serveUrl, backend);
        setRejected((all) => {
          const rest = { ...all };
          delete rest[backend];
          return rest;
        });
        setConnecting(null);
        setPhase("waiting");
      } catch (cause) {
        const reason = cause instanceof Error ? cause.message : String(cause);
        setRejected((all) => ({ ...all, [backend]: reason }));
        setPhase("rejected");
      }
    },
    [serveUrl],
  );

  // A sign-in is done when its backend says so and then answers a prompt.
  useEffect(() => {
    if (!connecting || phase !== "waiting") return;
    const timer = setInterval(() => {
      void refresh().then((list) => {
        const now = list?.find((status) => status.name === connecting);
        if (now?.status === "connected") void check(connecting);
      });
    }, POLL_MS);
    return () => clearInterval(timer);
  }, [connecting, phase, refresh, check]);

  async function connect(backend: string) {
    setError("");
    try {
      await startConnect(serveUrl, backend);
      setPhase("waiting");
      setConnecting(backend);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }

  const working = (status: BackendStatus) =>
    status.status === "connected" && !rejected[status.name];
  const connected = statuses?.filter(working).length ?? 0;

  return (
    <div
      className="backends-page"
      role="dialog"
      aria-label="Backends"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <section className="backends-panel">
        <header>
          <h2>Backends</h2>
          <button type="button" onClick={onClose}>
            Close
          </button>
        </header>
        <p className="backends-intro">
          {statuses === null
            ? "Checking backends…"
            : connected === 0
              ? "No backend is connected yet. Connect one to start the assistant."
              : `${connected} of ${statuses.length} connected.`}
        </p>
        {error ? (
          <p className="backends-error" role="alert">
            {error}
          </p>
        ) : null}
        <ul>
          {(statuses ?? []).map((status) => (
            <li key={status.name} data-status={status.status}>
              <span className="backend-name">
                {backendLabel(status.name)}
                <small title={rejected[status.name]}>
                  {describe(status, rejected[status.name])}
                </small>
              </span>
              {working(status) ? (
                <span aria-hidden="true">✓</span>
              ) : (
                <button
                  type="button"
                  aria-label={`Connect ${backendLabel(status.name)}`}
                  onClick={() => void connect(status.name)}
                >
                  Connect
                </button>
              )}
            </li>
          ))}
        </ul>
      </section>
      {connecting ? (
        <AgentTerminal
          serveUrl={serveUrl}
          agent={connecting}
          url={connectTerminalUrl(serveUrl, connecting)}
          title={`Connect ${backendLabel(connecting)}`}
          note={
            phase === "checking" ? (
              <span role="status">
                Signed in. Checking it with a test prompt…
              </span>
            ) : phase === "rejected" ? (
              <span className="backends-rejected" role="alert">
                {rejected[connecting]}. Sign in again here, then{" "}
                <button type="button" onClick={() => void check(connecting)}>
                  Check again
                </button>
              </span>
            ) : (
              NOTE
            )
          }
          onClose={() => {
            setConnecting(null);
            setPhase("waiting");
            void refresh();
          }}
        />
      ) : null}
    </div>
  );
}
