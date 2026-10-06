"use client";

import { useEffect, useRef, useState } from "react";

export function DeployConfirmation({ team, disabled, retry = false, manual = false, initialTimeoutSeconds = 300, onCancel, onConfirm }: {
  team: string;
  disabled: boolean;
  retry?: boolean;
  manual?: boolean;
  initialTimeoutSeconds?: number;
  onCancel: () => void;
  onConfirm: (timeoutSeconds: number) => void;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const [timeoutSeconds, setTimeoutSeconds] = useState(initialTimeoutSeconds);
  useEffect(() => {
    const dialog = ref.current;
    const previous = document.activeElement as HTMLElement | null;
    dialog?.showModal();
    dialog?.querySelector<HTMLButtonElement>("button")?.focus();
    return () => {
      dialog?.close();
      previous?.focus();
    };
  }, []);

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
      <h2 id="deploy-title">{retry ? `Run ${team} again?` : `Deploy ${team}?`}</h2>
      <div id="deploy-details">
        {retry ? <p>This starts a new run from the beginning using the current source and inputs. Previously completed steps may run again.</p> : null}
        <p>This starts real agents. While a topology is running, closing Mission Control keeps the topology, assistant, and runtime running.</p>
        <p>Reopen Mission Control to reconnect to the runtime and pick up your saved chat.</p>
      </div>
      <label className="deploy-timeout">
        Default step timeout
        <select value={timeoutSeconds} onChange={(event) => setTimeoutSeconds(Number(event.target.value))}>
          <option value={300}>5 minutes</option>
          <option value={1800}>30 minutes</option>
          <option value={3600}>1 hour</option>
          <option value={7200}>2 hours</option>
        </select>
      </label>
      <p>Applies to steps without a deadline in the workflow.</p>
      {manual ? <p>This workflow includes manual Web steps. They wait for you to submit a result in the response panel; OMAR does not contact ChatGPT automatically.</p> : null}
      <div className="deploy-confirmation-actions">
        <button type="button" className="secondary-button" onClick={onCancel} autoFocus>Cancel</button>
        <button type="button" className="primary-button" disabled={disabled} onClick={() => onConfirm(timeoutSeconds)}>Confirm deploy</button>
      </div>
    </dialog>
  );
}
