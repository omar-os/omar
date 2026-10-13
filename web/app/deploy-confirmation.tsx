"use client";

import { useEffect, useRef, useState } from "react";

/** What the operator chose for the run, beyond the program itself. */
export type DeployOptions = {
  keepWorkspaces: boolean;
  /** A cadence spec the daemon parses: `off`, `tag`, `1h`, `logical:2h30m`. */
  checkpointPeriod: string;
};

type Cadence = "hour" | "tag" | "custom";
type Clock = "physical" | "logical";
type Parts = { days: number; hours: number; minutes: number; seconds: number };

/** The spec for a custom period, or null when it adds up to nothing. */
export function customSpec(clock: Clock, parts: Parts): string | null {
  const units: [number, string][] = [
    [parts.days, "d"],
    [parts.hours, "h"],
    [parts.minutes, "m"],
    [parts.seconds, "s"],
  ];
  const text = units
    .filter(([n]) => Number.isFinite(n) && n > 0)
    .map(([n, unit]) => `${Math.floor(n)}${unit}`)
    .join("");
  if (!text) return null;
  return clock === "logical" ? `logical:${text}` : text;
}

export function DeployConfirmation({ team, disabled, onCancel, onConfirm }: {
  team: string;
  disabled: boolean;
  onCancel: () => void;
  onConfirm: (options: DeployOptions) => void;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  const [keepWorkspaces, setKeepWorkspaces] = useState(false);
  // Off unless asked for: a checkpoint is a snapshot of every instance's
  // files, and not every run wants that kept.
  const [checkpointing, setCheckpointing] = useState(false);
  const [cadence, setCadence] = useState<Cadence>("hour");
  const [clock, setClock] = useState<Clock>("physical");
  const [parts, setParts] = useState<Parts>({ days: 0, hours: 0, minutes: 0, seconds: 0 });
  useEffect(() => {
    const dialog = ref.current;
    const previous = document.activeElement as HTMLElement | null;
    dialog?.showModal();
    // Opening lands on Cancel, not the first option: Escape and Enter back
    // out until the operator reaches for something.
    cancel.current?.focus();
    return () => {
      dialog?.close();
      previous?.focus();
    };
  }, []);

  const custom = customSpec(clock, parts);
  const checkpointPeriod = !checkpointing
    ? "off"
    : cadence === "tag"
      ? "tag"
      : cadence === "hour"
        ? "1h"
        : custom;
  const invalidCustom = checkpointing && cadence === "custom" && custom === null;
  const setPart = (key: keyof Parts) => (event: React.ChangeEvent<HTMLInputElement>) =>
    setParts((current) => ({ ...current, [key]: Math.max(0, Number(event.target.value) || 0) }));

  return (
    <dialog
      ref={ref}
      className="deploy-confirmation"
      aria-labelledby="deploy-title"
      aria-describedby="deploy-details"
      onCancel={onCancel}
      onClick={(event) => { if (event.target === event.currentTarget) onCancel(); }}
    >
      <span className="eyebrow">DEPLOY TOPOLOGY</span>
      <h2 id="deploy-title">Deploy {team}?</h2>
      <div id="deploy-details">
        <p>This starts real agents. While a topology is running, closing Mission Control keeps the topology, assistant, and runtime running.</p>
        <p>Reopen Mission Control to reconnect to the runtime and pick up your saved chat.</p>
      </div>
      <fieldset className="deploy-options">
        <legend>Workspaces</legend>
        <label className="deploy-check">
          <input type="checkbox" checked={keepWorkspaces} onChange={(event) => setKeepWorkspaces(event.target.checked)} />
          Keep workspace files after the run ends
        </label>
        <p className="deploy-hint">Otherwise each instance&apos;s files go with the run. A paused run keeps them either way.</p>
      </fieldset>
      <fieldset className="deploy-options">
        <legend>Checkpoints</legend>
        <label className="deploy-check">
          <input type="checkbox" checked={checkpointing} onChange={(event) => setCheckpointing(event.target.checked)} />
          Enable checkpoints
        </label>
        {checkpointing ? (
          <label className="deploy-field">
            Cadence
            <select aria-label="Checkpoint cadence" value={cadence} onChange={(event) => setCadence(event.target.value as Cadence)}>
              <option value="tag">Every logical tick</option>
              <option value="hour">Every 1 physical hour</option>
              <option value="custom">Custom</option>
            </select>
          </label>
        ) : null}
        {checkpointing && cadence === "custom" ? (
          <div className="deploy-custom" role="group" aria-label="Custom checkpoint period">
            <label className="deploy-field">
              Clock
              <select aria-label="Checkpoint clock" value={clock} onChange={(event) => setClock(event.target.value as Clock)}>
                <option value="physical">Physical time</option>
                <option value="logical">Logical time</option>
              </select>
            </label>
            <div className="deploy-period">
              {(["days", "hours", "minutes", "seconds"] as const).map((unit) => (
                <label key={unit} className="deploy-field">
                  {unit[0].toUpperCase() + unit.slice(1)}
                  <input type="number" min={0} step={1} inputMode="numeric" aria-label={`Period ${unit}`} value={parts[unit]} onChange={setPart(unit)} />
                </label>
              ))}
            </div>
            {invalidCustom ? (
              <p className="deploy-hint deploy-invalid" role="alert">A custom period needs at least one of days, hours, minutes or seconds above zero.</p>
            ) : (
              <p className="deploy-hint">Checkpoints every {custom?.replace("logical:", "")} of {clock} time.</p>
            )}
          </div>
        ) : null}
        {!checkpointing ? <p className="deploy-hint">Only Pause and a manual checkpoint will capture the run; enable to capture on a cadence.</p> : null}
      </fieldset>
      <div className="deploy-confirmation-actions">
        <button ref={cancel} type="button" className="secondary-button" onClick={onCancel}>Cancel</button>
        <button
          type="button"
          className="primary-button"
          disabled={disabled || invalidCustom}
          onClick={() => { if (checkpointPeriod) onConfirm({ keepWorkspaces, checkpointPeriod }); }}
        >
          Confirm deploy
        </button>
      </div>
    </dialog>
  );
}
