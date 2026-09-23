"use client";

import { useEffect, useRef } from "react";

export function DeployConfirmation({ team, disabled, onCancel, onConfirm }: {
  team: string;
  disabled: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current;
    const previous = document.activeElement as HTMLElement | null;
    dialog?.showModal();
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
      <h2 id="deploy-title">Deploy {team}?</h2>
      <div id="deploy-details">
        <p>This starts real agents. While a topology is running, closing Mission Control keeps the topology, assistant, and runtime running.</p>
        <p>Reopen Mission Control to reconnect to the runtime and pick up your saved chat.</p>
      </div>
      <div className="deploy-confirmation-actions">
        <button type="button" className="secondary-button" onClick={onCancel} autoFocus>Cancel</button>
        <button type="button" className="primary-button" disabled={disabled} onClick={onConfirm}>Confirm deploy</button>
      </div>
    </dialog>
  );
}
