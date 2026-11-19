"use client";

import { useEffect, useRef, useState } from "react";

export type Theme = "dark" | "light";

/** Where the choice is kept; `layout.tsx` reads the same key before paint. */
export const THEME_KEY = "omar.theme";

function applyTheme(theme: Theme) {
  if (theme === "light") document.documentElement.dataset.theme = "light";
  else delete document.documentElement.dataset.theme;
  try {
    localStorage.setItem(THEME_KEY, theme);
  } catch {
    // Private windows may refuse storage; the choice still holds for the page.
  }
}

/**
 * Browser theme and available runtime settings.
 *
 * Dark is the default and the server render, so the attribute is only ever
 * added for light. The choice outlives the page through local storage and is
 * applied before paint by a script in the layout, so a light page does not
 * flash dark while React loads.
 */
export function SettingsMenu({
  onOpenBackends,
  onOpenIntegrations,
}: {
  /** Opens the Backends page; absent where nothing can be connected. */
  onOpenBackends?: () => void;
  onOpenIntegrations?: () => void;
} = {}) {
  // Read off the document rather than storage: the layout script has already
  // applied the stored choice by the time this runs. Nothing visible depends
  // on it until the menu opens, so the server's "dark" never mismatches.
  const [theme, setTheme] = useState<Theme>(() =>
    typeof document !== "undefined" &&
    document.documentElement.dataset.theme === "light"
      ? "light"
      : "dark",
  );
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) return;
    rootRef.current?.querySelector<HTMLButtonElement>('[role="menu"] button')?.focus();
    const dismiss = (event: MouseEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", dismiss);
    return () => window.removeEventListener("mousedown", dismiss);
  }, [open]);

  function close() {
    setOpen(false);
    triggerRef.current?.focus();
  }

  function choose(next: Theme) {
    applyTheme(next);
    setTheme(next);
    close();
  }

  return (
    <div className="settings-menu" ref={rootRef} onBlur={(event) => {
      if (!event.currentTarget.contains(event.relatedTarget)) setOpen(false);
    }}>
      <button
        type="button"
        ref={triggerRef}
        className="settings-trigger"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label="Settings"
        title="Settings"
        onClick={() => setOpen((was) => !was)}
      >
        <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinejoin="round" aria-hidden="true">
          <path d="m9.5 4 .6-2h3.8l.6 2 2 .9 1.9-.5 1.9 3.2-1.5 1.6.2 2.8 1.4 1.6-1.9 3.3-2-.5-2 1-.6 2.6h-3.8l-.6-2.6-2-1-2 .5-1.9-3.3L5 12l.2-2.8-1.5-1.6 1.9-3.2 1.9.5Z" transform="translate(0 1)" />
          <circle cx="12" cy="12" r="3" />
        </svg>
        <span>Settings</span>
      </button>
      {open ? (
        <div className="settings-options" role="menu" aria-label="Settings" onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            close();
          } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
            event.preventDefault();
            const items = Array.from(event.currentTarget.querySelectorAll("button"));
            const index = items.indexOf(document.activeElement as HTMLButtonElement);
            items[(index + (event.key === "ArrowDown" ? 1 : -1) + items.length) % items.length]?.focus();
          }
        }}>
          {onOpenIntegrations ? (
            <button type="button" role="menuitem" onClick={() => { close(); onOpenIntegrations(); }}>
              Integrations
            </button>
          ) : null}
          <span className="settings-heading">Theme</span>
          {(["light", "dark"] as const).map((option) => (
            <button
              key={option}
              type="button"
              role="menuitemradio"
              aria-checked={theme === option}
              className={theme === option ? "current" : undefined}
              onClick={() => choose(option)}
            >
              {option === "light" ? "Light" : "Dark"}
              {theme === option ? <span aria-hidden="true">✓</span> : null}
            </button>
          ))}
          {onOpenBackends ? (
            <>
              <span className="settings-heading">Backends</span>
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  close();
                  onOpenBackends();
                }}
              >
                Connect backends…
              </button>
            </>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
