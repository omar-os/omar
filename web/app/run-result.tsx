"use client";

import { useEffect, useRef, useState } from "react";
import { fetchRunResult } from "./lib/runtime-client";

export function RunResult({ serveUrl, runId, onClose }: {
  serveUrl: string;
  runId: string;
  onClose: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [outputs, setOutputs] = useState<Record<string, string> | null>(null);
  const [selected, setSelected] = useState("");
  const [error, setError] = useState("");
  useEffect(() => {
    const node = dialog.current;
    const previous = document.activeElement as HTMLElement | null;
    node?.showModal();
    let active = true;
    void fetchRunResult(serveUrl, runId).then((values) => {
      if (!active) return;
      const text = Object.fromEntries(Object.entries(values).map(([key, value]) => [key, typeof value === "string" ? value : JSON.stringify(value, null, 2)]));
      setOutputs(text);
      setSelected(Object.keys(text)[0] ?? "");
    }).catch((cause) => { if (active) setError(cause instanceof Error ? cause.message : String(cause)); });
    return () => { active = false; node?.close(); previous?.focus(); };
  }, [serveUrl, runId]);

  function download() {
    if (!outputs || !selected) return;
    const blob = new Blob([outputs[selected]], { type: "text/plain;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = `${selected.replace(/[^a-zA-Z0-9._-]/g, "-")}.txt`;
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 0);
  }

  return <dialog ref={dialog} className="run-result" aria-labelledby="run-result-title" onCancel={onClose}>
    <header><div><span className="eyebrow">RUN OUTPUT</span><h2 id="run-result-title">Result</h2></div><button type="button" className="secondary-button" onClick={onClose}>Close</button></header>
    {error ? <p role="alert" className="connection-error">{error}</p> : null}
    {!outputs && !error ? <p>Loading result…</p> : null}
    {outputs ? <>
      {Object.keys(outputs).length === 0 ? <p>This run produced no output ports.</p> : <>
        <div className="run-result-tabs">{Object.keys(outputs).map((key) => <button type="button" key={key} aria-pressed={selected === key} onClick={() => setSelected(key)}>{key}</button>)}</div>
        <label htmlFor="run-result-text">Output text</label>
        <textarea id="run-result-text" value={outputs[selected] ?? ""} onChange={(event) => setOutputs({ ...outputs, [selected]: event.target.value })} />
        <p>Edits here are local to this browser view. Download a copy to keep them; the saved run output stays unchanged.</p>
        <button type="button" className="primary-button" onClick={download}>Download text</button>
      </>}
    </> : null}
  </dialog>;
}
