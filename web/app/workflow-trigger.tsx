"use client";

import { useEffect, useState } from "react";
import { normalizeRuntimeUrl } from "./lib/runtime-client";
import { fetchWorkflow } from "./lib/workflow-state";
import type { DiagramSnapshot } from "./lib/protocol";

type Source = {
  auth: "hmac-sha256" | "token";
  auth_header: string;
  event_header: string;
  event: string;
  id_header: string;
};
type Trigger = Source & {
  id: string;
  revision: number;
  enabled: boolean;
  secret_name: string;
  mapping: Record<string, string>;
};

// Any sender works. These are GitHub's header names, offered as a shortcut.
const GITHUB: Source = {
  auth: "hmac-sha256",
  auth_header: "x-hub-signature-256",
  event_header: "x-github-event",
  event: "push",
  id_header: "x-github-delivery",
};
const CUSTOM: Source = {
  auth: "hmac-sha256",
  auth_header: "x-signature",
  event_header: "",
  event: "",
  id_header: "",
};
type Receipt = {
  delivery_id: string;
  run_id: string;
  revision: number;
  received_at: number;
  status: string;
  error: string | null;
};
type Inbox = { trigger: Trigger | null; deliveries: Receipt[] };

export function WorkflowTrigger({
  base,
  source,
  snapshot,
  onClose,
}: {
  base: string;
  source: string;
  snapshot: DiagramSnapshot;
  onClose: () => void;
}) {
  const url = normalizeRuntimeUrl(base) + "/v1/webhook-trigger";
  const [inbox, setInbox] = useState<Inbox | null>(null);
  const [src, setSrc] = useState<Source>(CUSTOM);
  const [secret, setSecret] = useState("webhook");
  const [mapping, setMapping] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [revision, setRevision] = useState<number | null>(null);
  useEffect(() => {
    const ctl = new AbortController();
    Promise.all([
      fetch(url, { signal: ctl.signal }).then(async (response) => {
        if (!response.ok)
          throw Error("This runtime could not load webhook settings.");
        return (await response.json()) as Inbox;
      }),
      fetchWorkflow(base),
    ])
      .then(([saved, workflow]) => {
        if (ctl.signal.aborted) return;
        setInbox(saved);
        if (workflow.draft?.design.program === source)
          setRevision(workflow.revision);
        if (saved.trigger) {
          const t = saved.trigger;
          setSrc({
            auth: t.auth,
            auth_header: t.auth_header,
            event_header: t.event_header,
            event: t.event,
            id_header: t.id_header,
          });
          setSecret(saved.trigger.secret_name);
          setMapping(saved.trigger.mapping);
        }
      })
      .catch((reason) => {
        if (!ctl.signal.aborted) setError(String(reason));
      });
    return () => ctl.abort();
  }, [base, source, url]);
  async function update(body?: object) {
    setBusy(true);
    setError("");
    try {
      const response = await fetch(
        url,
        body
          ? {
              method: "POST",
              headers: { "content-type": "application/json" },
              body: JSON.stringify(body),
            }
          : undefined,
      );
      const result = await response.json();
      if (!response.ok)
        throw Error(result.error ?? "Could not update webhook.");
      setInbox(result as Inbox);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(false);
    }
  }
  const inputs = snapshot.ports.filter(
    (p) =>
      p.kind === "input" &&
      !snapshot.edges.some((e) => e.kind === "connection" && e.target === p.id),
  );
  return (
    <aside className="step-inspector" aria-label="Webhook trigger">
      <header>
        <h3>Webhook trigger</h3>
        <button
          type="button"
          aria-label="Close webhook settings"
          onClick={onClose}
        >
          ×
        </button>
      </header>
      <p>
        Run this workflow when any service calls its webhook URL. Map JSON
        payload fields to inputs. With a delivery ID header, a resent request
        does not run it twice; requests that arrive while it runs wait their
        turn.
      </p>
      {inbox?.trigger ? (
        <div className="inspector-field">
          <strong>
            {inbox.trigger.enabled ? "Active" : "Paused"} · revision{" "}
            {inbox.trigger.revision}
          </strong>
          <label>
            Delivery URL
            <input
              readOnly
              onFocus={(e) => e.target.select()}
              value={
                normalizeRuntimeUrl(base) +
                "/v1/webhook-deliveries/" +
                inbox.trigger.id
              }
            />
          </label>
        </div>
      ) : null}
      <fieldset disabled={busy || !inbox}>
        <label className="inspector-field">
          Preset
          <select
            aria-label="Webhook preset"
            value=""
            onChange={(e) => {
              if (e.target.value === "github") setSrc(GITHUB);
              if (e.target.value === "custom") setSrc(CUSTOM);
            }}
          >
            <option value="">Fill in from…</option>
            <option value="github">GitHub</option>
            <option value="custom">Other service</option>
          </select>
        </label>
        <label className="inspector-field">
          Authentication
          <select
            aria-label="Authentication"
            value={src.auth}
            onChange={(e) =>
              setSrc({ ...src, auth: e.target.value as Source["auth"] })
            }
          >
            <option value="hmac-sha256">HMAC-SHA256 signature of body</option>
            <option value="token">Shared secret token</option>
          </select>
        </label>
        <label className="inspector-field">
          {src.auth === "token" ? "Token header" : "Signature header"}
          <input
            value={src.auth_header}
            onChange={(e) => setSrc({ ...src, auth_header: e.target.value })}
          />
        </label>
        <label className="inspector-field">
          Event header (optional)
          <input
            value={src.event_header}
            onChange={(e) => setSrc({ ...src, event_header: e.target.value })}
          />
        </label>
        {src.event_header ? (
          <label className="inspector-field">
            Event value to match
            <input
              value={src.event}
              onChange={(e) => setSrc({ ...src, event: e.target.value })}
            />
          </label>
        ) : null}
        <label className="inspector-field">
          Delivery ID header (optional)
          <input
            value={src.id_header}
            onChange={(e) => setSrc({ ...src, id_header: e.target.value })}
          />
        </label>
        <label className="inspector-field">
          Local secret name
          <input value={secret} onChange={(e) => setSecret(e.target.value)} />
        </label>
        <p>
          Store the signing secret or token in a private (0600) file under the
          runtime home’s webhook-secrets directory. Enter its file name above.
          Signatures may be hex or base64, with or without a sha256= prefix.
        </p>
        {inputs.map((input) => (
          <label className="inspector-field" key={input.id}>
            {input.name} · {input.type}
            <input
              disabled={input.type === "path"}
              placeholder="Use saved input, or enter /field/path"
              value={mapping[input.name] ?? ""}
              onChange={(e) =>
                setMapping({ ...mapping, [input.name]: e.target.value })
              }
            />
          </label>
        ))}
        {revision === null && inbox ? (
          <p>
            Save the current draft and reopen these settings to activate it.
          </p>
        ) : null}
        <button
          className="step-save"
          type="button"
          disabled={revision === null}
          onClick={() =>
            void update({
              revision,
              ...src,
              secret_name: secret,
              mapping: Object.fromEntries(
                Object.entries(mapping).filter(([, value]) => value.length),
              ),
            })
          }
        >
          Activate saved revision
        </button>
        {inbox?.trigger?.enabled ? (
          <button
            className="step-ask"
            type="button"
            onClick={() => void update({ enabled: false })}
          >
            Pause new deliveries
          </button>
        ) : null}
      </fieldset>
      {error ? <p role="alert">{error}</p> : null}
      <h4>Recent deliveries</h4>
      <button type="button" disabled={busy} onClick={() => void update()}>
        Refresh deliveries
      </button>
      {inbox?.deliveries.length ? (
        <ul>
          {inbox.deliveries.map((r) => (
            <li key={r.delivery_id}>
              <strong>{r.status}</strong> · {r.delivery_id}
              <br />
              Revision {r.revision} ·{" "}
              {new Date(r.received_at * 1000).toLocaleString()}
              {r.status === "started" ? (
                <>
                  {" "}
                  ·{" "}
                  <a
                    href={normalizeRuntimeUrl(base) + "/v1/runs/" + r.run_id}
                    target="_blank"
                    rel="noreferrer"
                  >
                    Run details
                  </a>
                </>
              ) : null}
              {r.error ? <p>{r.error}</p> : null}
            </li>
          ))}
        </ul>
      ) : (
        <p>No accepted deliveries yet.</p>
      )}
    </aside>
  );
}
