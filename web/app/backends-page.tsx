"use client";

import { useCallback, useEffect, useState } from "react";
import { AgentTerminal } from "./agent-terminal";
import {
  checkSignIn,
  connectTerminalUrl,
  fetchBackendStatus,
  startConnect,
  startDisconnect,
  type BackendStatus,
} from "./lib/runtime-client";
import { backendLabel } from "./backend-menu";

/** While a sign-in is open, how often its backend is asked whether it took. */
const POLL_MS = 3000;

const NOTE =
  "This is the backend's own install and sign-in; it closes once the backend answers a test prompt.";

/** What a terminal opened here does to a backend's sign-in. */
type Action = "connect" | "replace" | "disconnect" | "sandbox";

const NOTES: Record<Action, string> = {
  connect: NOTE,
  replace:
    "This is the backend's own sign-in, run again; it closes once the new one answers a test prompt.",
  disconnect: "This is the backend's own sign-out; it closes once it finishes.",
  sandbox:
    "This reuses the backend's API key, or asks Docker Sandboxes for one; it closes once the backend answers a test prompt inside a sandbox.",
};

/** How the backend stands for sandboxed teams, or null without sbx. */
function describeSandbox(status: BackendStatus): string | null {
  const sandbox = status.sandbox;
  if (!sandbox || sandbox.status === "not_installed") return null;
  switch (sandbox.status) {
    case "connected":
      return sandbox.via
        ? `Sandboxes · ${sandbox.via}`
        : "Sandboxes · connected";
    case "signed_out":
      return "Sandboxes · not connected";
    default:
      return sandbox.reason
        ? `Sandboxes · ${sandbox.reason}`
        : "Sandboxes · unknown";
  }
}

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
 * terminal closes only when that works. A working sign-in can be replaced
 * (`omar connect <backend> --replace`) or, once confirmed, signed out
 * (`omar disconnect <backend>`); both are done when their terminal ends.
 * Where Docker Sandboxes is installed, each backend also shows its sandbox
 * sign-in, with a Sandbox button that runs `omar connect <backend> --sandbox`,
 * done when its terminal ends too.
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
  const [action, setAction] = useState<Action>("connect");
  // The backend whose sign-out is waiting on a second click.
  const [confirming, setConfirming] = useState<string | null>(null);
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

  // `closing`: the terminal has ended, so a rejection closes it too and the
  // row says why; there is nothing left in it to sign in again with.
  const check = useCallback(
    async (backend: string, closing = false) => {
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
        if (closing) {
          setConnecting(null);
          setPhase("waiting");
        } else {
          setPhase("rejected");
        }
      }
    },
    [serveUrl],
  );

  // A sign-in is done when its backend says so and then answers a prompt.
  // A replaced one already reads connected, so it waits for its terminal.
  useEffect(() => {
    if (!connecting || action !== "connect" || phase !== "waiting") return;
    const timer = setInterval(() => {
      void refresh().then((list) => {
        const now = list?.find((status) => status.name === connecting);
        if (now?.status === "connected") void check(connecting);
      });
    }, POLL_MS);
    return () => clearInterval(timer);
  }, [connecting, action, phase, refresh, check]);

  async function connect(backend: string, next: Action = "connect") {
    setError("");
    setConfirming(null);
    try {
      if (next === "disconnect") {
        await startDisconnect(serveUrl, backend);
      } else {
        await startConnect(serveUrl, backend, {
          replace: next === "replace",
          sandbox: next === "sandbox",
        });
      }
      setPhase("waiting");
      setAction(next);
      setConnecting(backend);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }

  // A replace, sign-out or sandbox sign-in is done when its terminal ends.
  async function ended() {
    if (!connecting || action === "connect") return;
    const list = await refresh();
    const now = list?.find((status) => status.name === connecting);
    if (action === "replace" && now?.status === "connected") {
      void check(connecting, true);
    } else {
      setConnecting(null);
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
          {(statuses ?? []).map((status) => {
            const sandbox = describeSandbox(status);
            return (
              <li key={status.name} data-status={status.status}>
                <span className="backend-name">
                  {backendLabel(status.name)}
                  <small title={rejected[status.name]}>
                    {describe(status, rejected[status.name])}
                  </small>
                  {sandbox ? (
                    <small className="backend-sandbox">
                      {sandbox}
                      {status.sandbox?.status !== "connected" ? (
                        <>
                          {" · "}
                          <button
                            type="button"
                            className="link-button"
                            aria-label={`Set up ${backendLabel(status.name)} for sandboxes`}
                            onClick={() => void connect(status.name, "sandbox")}
                          >
                            Set up
                          </button>
                        </>
                      ) : null}
                    </small>
                  ) : null}
                </span>
                <span className="backend-actions">
                  {confirming === status.name ? (
                    <>
                      <button
                        type="button"
                        className="secondary-button"
                        onClick={() => setConfirming(null)}
                      >
                        Cancel
                      </button>
                      <button
                        type="button"
                        aria-label={`Sign ${backendLabel(status.name)} out`}
                        onClick={() => void connect(status.name, "disconnect")}
                      >
                        Sign out
                      </button>
                    </>
                  ) : (
                    <>
                      {status.status === "connected" ? (
                        <button
                          type="button"
                          className="secondary-button"
                          aria-label={`Disconnect ${backendLabel(status.name)}`}
                          onClick={() => setConfirming(status.name)}
                        >
                          Disconnect
                        </button>
                      ) : null}
                      {working(status) ? (
                        <button
                          type="button"
                          className="secondary-button"
                          aria-label={`Replace ${backendLabel(status.name)}'s sign-in`}
                          onClick={() => void connect(status.name, "replace")}
                        >
                          Replace
                        </button>
                      ) : (
                        <button
                          type="button"
                          aria-label={`Connect ${backendLabel(status.name)}`}
                          onClick={() => void connect(status.name)}
                        >
                          Connect
                        </button>
                      )}
                    </>
                  )}
                </span>
              </li>
            );
          })}
        </ul>
      </section>
      {connecting ? (
        <AgentTerminal
          serveUrl={serveUrl}
          agent={connecting}
          url={connectTerminalUrl(serveUrl, connecting)}
          title={
            action === "replace"
              ? `Replace ${backendLabel(connecting)}'s sign-in`
              : action === "disconnect"
                ? `Disconnect ${backendLabel(connecting)}`
                : action === "sandbox"
                  ? `Set up ${backendLabel(connecting)} for sandboxes`
                  : `Connect ${backendLabel(connecting)}`
          }
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
              NOTES[action]
            )
          }
          onEnded={() => void ended()}
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
