"use client";

import { useEffect, useId, useRef, useState } from "react";
import { WorkflowIcon } from "./workflow-icon";

const options = [
  { value: "recent", label: "Recent activity" },
  { value: "name", label: "Name A–Z" },
  { value: "status", label: "Status" },
] as const;

export type WorkflowSort = typeof options[number]["value"];

export function WorkflowSortMenu({ value, onChange }: { value: WorkflowSort; onChange: (value: WorkflowSort) => void }) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const optionRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const menuId = useId();

  useEffect(() => {
    if (!open) return;
    optionRefs.current[options.findIndex((option) => option.value === value)]?.focus();
    const dismiss = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", dismiss);
    return () => document.removeEventListener("pointerdown", dismiss);
  }, [open, value]);

  function close() {
    setOpen(false);
    triggerRef.current?.focus();
  }

  return (
    <div className="workflow-sort" ref={rootRef} onBlur={(event) => {
      if (!event.currentTarget.contains(event.relatedTarget)) setOpen(false);
    }}>
      <button
        ref={triggerRef}
        type="button"
        className="workflow-sort-trigger"
        aria-label="Sort workflows"
        title="Sort workflows"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        onClick={() => setOpen((current) => !current)}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown" || event.key === "ArrowUp") {
            event.preventDefault();
            setOpen(true);
          }
        }}
      >
        <WorkflowIcon name="layers" size={17} />
      </button>
      {open ? <div className="workflow-sort-menu" id={menuId} role="menu" aria-label="Sort workflows" onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          close();
        } else if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
          event.preventDefault();
          const current = optionRefs.current.findIndex((option) => option === document.activeElement);
          const next = event.key === "Home" ? 0 : event.key === "End" ? options.length - 1 : (current + (event.key === "ArrowDown" ? 1 : -1) + options.length) % options.length;
          optionRefs.current[next]?.focus();
        }
      }}>
        <div className="workflow-sort-heading">SORT WORKFLOWS</div>
        {options.map((option, index) => <button
          key={option.value}
          ref={(element) => { optionRefs.current[index] = element; }}
          type="button"
          role="menuitemradio"
          aria-checked={value === option.value}
          tabIndex={-1}
          onClick={() => { onChange(option.value); close(); }}
        >
          <span>{option.label}</span>
          {value === option.value ? <svg className="workflow-sort-check" aria-hidden="true" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round"><path d="m5 12 4 4L19 6" /></svg> : null}
        </button>)}
      </div> : null}
    </div>
  );
}
