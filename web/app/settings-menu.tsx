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
 * Settings for this browser, which today means the theme.
 *
 * Dark is the default and the server render, so the attribute is only ever
 * added for light. The choice outlives the page through local storage and is
 * applied before paint by a script in the layout, so a light page does not
 * flash dark while React loads.
 */
export function SettingsMenu() {
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
  const rootRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) return;
    const dismiss = (event: MouseEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", dismiss);
    return () => window.removeEventListener("mousedown", dismiss);
  }, [open]);

  function choose(next: Theme) {
    applyTheme(next);
    setTheme(next);
    setOpen(false);
  }

  return (
    <div className="settings-menu" ref={rootRef}>
      <button
        type="button"
        className="settings-trigger"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label="Settings"
        title="Settings"
        onClick={() => setOpen((was) => !was)}
      >
        <svg viewBox="0 0 20 20" width="16" height="16" aria-hidden="true">
          <circle cx="10" cy="10" r="3" fill="none" stroke="currentColor" strokeWidth="1.6" />
          <path
            d="M10 2.5v2.2M10 15.3v2.2M2.5 10h2.2M15.3 10h2.2M4.7 4.7l1.6 1.6M13.7 13.7l1.6 1.6M4.7 15.3l1.6-1.6M13.7 6.3l1.6-1.6"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.6"
            strokeLinecap="round"
          />
        </svg>
      </button>
      {open ? (
        <div className="settings-options" role="menu" aria-label="Settings">
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
        </div>
      ) : null}
    </div>
  );
}
