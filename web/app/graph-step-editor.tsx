"use client";

import { useState } from "react";
import {
  graphChange,
  splitConnection,
  type GraphChange,
} from "./lib/graph-edits";
import type {
  DiagramEdge,
  DiagramReaction,
  DiagramSnapshot,
} from "./lib/protocol";

type Props = {
  source: string;
  snapshot: DiagramSnapshot;
  reaction?: DiagramReaction;
  edge?: DiagramEdge;
  onApply: (source: string) => Promise<void>;
  onClose: () => void;
};
export function GraphStepEditor({
  source,
  snapshot,
  reaction,
  edge,
  onApply,
  onClose,
}: Props) {
  const [change, setChange] = useState<GraphChange | null>(null);
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [base, setBase] = useState(source);
  const wire = edge?.kind === "connection";
  if (!reaction && !wire) return null;
  const roles = wire
    ? ["Web", "Codex", "ClaudeCode", "Cursor", "OpenCode", "Pi", "Agy"]
    : snapshot.agents
        .filter(
          (a) => a.instance === reaction?.instance && a.name.split(".").at(-1),
        )
        .map((a) => a.name.split(".").at(-1)!);
  if (!wire && !roles.length) roles.push("reviewer");
  const endpoints = (position: GraphChange["position"]) =>
    position === "before"
      ? (reaction?.triggers ?? [])
      : (reaction?.effects ?? []);
  function start(position: GraphChange["position"], port?: string) {
    setMessage("");
    setBase(source);
    setChange({
      position,
      port: port ?? endpoints(position)[0] ?? "",
      role: roles[0] ?? "",
      prompt: "",
      name: "",
      repairs: {},
    });
  }
  const forEdge = edge?.kind === "trigger" ? "before" : "after";
  if (!change)
    return (
      <div className="step-editor">
        <h4>Graph</h4>
        {edge ? (
          <button
            className="step-ask"
            onClick={() =>
              start(
                forEdge,
                edge.kind === "trigger" ? edge.source : edge.target,
              )
            }
          >
            Insert on connection
          </button>
        ) : (
          <>
            <button className="step-ask" onClick={() => start("before")}>
              Add step before
            </button>
            <button className="step-ask" onClick={() => start("after")}>
              Add step after
            </button>
            <button className="step-ask" onClick={() => start("remove")}>
              Remove step
            </button>
          </>
        )}
      </div>
    );
  const current = change;
  const patch = (field: Partial<GraphChange>) => {
    setMessage("");
    setChange({ ...current, ...field });
  };
  const revise = () =>
    wire && edge
      ? splitConnection(base, snapshot, edge, current)
      : graphChange(base, snapshot, reaction!, current);
  let next: string | undefined;
  let problem = "";
  try {
    next = revise();
  } catch (error) {
    problem = error instanceof Error ? error.message : String(error);
  }
  if (source !== base)
    problem = "The draft changed. Cancel and reopen this operation.";
  async function submit() {
    if (!next || problem || busy) return;
    setBusy(true);
    try {
      await onApply(next);
      onClose();
    } catch (error) {
      setMessage(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="step-editor">
      <h4>{current.position === "remove" ? "Remove step" : "Insert step"}</h4>
      <fieldset disabled={busy}>
        {current.position === "remove" ? (
          <>
            {reaction!.effects.map((id) => {
              const output = snapshot.ports.find((p) => p.id === id)!;
              const consumers = snapshot.edges
                .filter((e) => e.source === id)
                .map((e) => e.target.replace(/^[^:]+::/, ""));
              return (
                <label className="inspector-field" key={id}>
                  <span>Replacement for {output.name}</span>
                  <small>
                    {consumers.length
                      ? "Feeds " + consumers.join(", ")
                      : "Workflow port"}{" "}
                    · {output.type}
                  </small>
                  <select
                    value={current.repairs[id] ?? ""}
                    onChange={(e) =>
                      patch({
                        repairs: { ...current.repairs, [id]: e.target.value },
                      })
                    }
                  >
                    <option value="">Choose a compatible port</option>
                    {snapshot.ports
                      .filter(
                        (p) =>
                          p.instance === reaction!.instance &&
                          p.type === output.type &&
                          !reaction!.effects.includes(p.id),
                      )
                      .map((p) => (
                        <option key={p.id} value={p.id}>
                          {p.name}
                        </option>
                      ))}
                  </select>
                </label>
              );
            })}
          </>
        ) : (
          <>
            {!wire && !edge ? (
              <label className="inspector-field">
                <span>Connection</span>
                <select
                  value={current.port}
                  onChange={(e) => patch({ port: e.target.value })}
                >
                  {endpoints(current.position).map((id) => (
                    <option key={id} value={id}>
                      {id.replace(/^[^:]+::/, "")}
                    </option>
                  ))}
                </select>
              </label>
            ) : null}
            <label className="inspector-field">
              <span>{wire ? "Step name" : "Intermediate port name"}</span>
              <input
                value={current.name}
                onChange={(e) => patch({ name: e.target.value })}
              />
            </label>
            <label className="inspector-field">
              <span>Execution role</span>
              <select
                value={current.role}
                onChange={(e) => patch({ role: e.target.value })}
              >
                {roles.map((r) => (
                  <option key={r}>{r}</option>
                ))}
              </select>
            </label>
            {!wire &&
            !snapshot.agents.some((a) => a.instance === reaction?.instance) ? (
              <small>Adds a human Web reviewer to this team.</small>
            ) : null}
            <label className="inspector-field">
              <span>New step instructions</span>
              <textarea
                rows={3}
                value={current.prompt}
                onChange={(e) => patch({ prompt: e.target.value })}
              />
            </label>
          </>
        )}
        {reaction ? (
          <small>Changes the team definition for all its instances.</small>
        ) : null}
        {next && !problem ? (
          <details>
            <summary>Preview resulting source</summary>
            <pre className="step-contract">{next}</pre>
          </details>
        ) : null}
        {problem ? <p>{problem}</p> : null}
        {message ? <p role="alert">{message}</p> : null}
        <button
          type="button"
          className="step-save"
          disabled={!next || Boolean(problem)}
          onClick={() => void submit()}
        >
          {busy ? "Validating…" : "Validate and save"}
        </button>
        <button className="step-ask" onClick={() => setChange(null)}>
          Cancel
        </button>
      </fieldset>
    </div>
  );
}
