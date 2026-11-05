"use client";

import { useEffect, useState, type RefObject } from "react";

/** Text the operator highlighted in a message, and where to float the button. */
type Highlight = { text: string; top: number; left: number };

/** The message text a DOM node belongs to, if any. Labels and avatars are not. */
function messageContent(node: Node, thread: HTMLElement): Element | null {
  const element = node instanceof Element ? node : node.parentElement;
  const content = element?.closest(".message-content");
  return content && thread.contains(content) ? content : null;
}

/**
 * "Chat about this": highlighting text in a message floats a button that
 * quotes the text into the composer. Selections outside message content, a
 * collapsed selection, or scrolling the thread hide it.
 */
export function QuoteSelection({ threadRef, onQuote }: {
  threadRef: RefObject<HTMLDivElement | null>;
  onQuote: (text: string) => void;
}) {
  const [highlight, setHighlight] = useState<Highlight | null>(null);

  useEffect(() => {
    const thread = threadRef.current;
    if (!thread) return;
    // Measured once the gesture ends, so the button does not chase a drag.
    const measure = () => {
      const selection = window.getSelection();
      const text = selection?.toString().trim();
      if (!selection || selection.isCollapsed || !text || selection.rangeCount === 0) return setHighlight(null);
      const range = selection.getRangeAt(0);
      if (!messageContent(range.startContainer, thread) || !messageContent(range.endContainer, thread)) return setHighlight(null);
      const rect = range.getBoundingClientRect();
      setHighlight({ text, top: rect.top, left: rect.left + rect.width / 2 });
    };
    const clearIfCollapsed = () => {
      if (window.getSelection()?.isCollapsed ?? true) setHighlight(null);
    };
    const hide = () => setHighlight(null);
    document.addEventListener("mouseup", measure);
    document.addEventListener("keyup", measure);
    document.addEventListener("selectionchange", clearIfCollapsed);
    thread.addEventListener("scroll", hide);
    window.addEventListener("resize", hide);
    return () => {
      document.removeEventListener("mouseup", measure);
      document.removeEventListener("keyup", measure);
      document.removeEventListener("selectionchange", clearIfCollapsed);
      thread.removeEventListener("scroll", hide);
      window.removeEventListener("resize", hide);
    };
  }, [threadRef]);

  if (!highlight) return null;
  return (
    <button
      type="button"
      className="quote-selection"
      style={{ top: highlight.top, left: highlight.left }}
      // Keep the selection alive until the click lands.
      onMouseDown={(event) => event.preventDefault()}
      onClick={() => {
        onQuote(highlight.text);
        window.getSelection()?.removeAllRanges();
        setHighlight(null);
      }}
    >
      Chat about this
    </button>
  );
}

/** The quote as markdown, ahead of the operator's message. */
export function withQuote(quote: string, request: string): string {
  if (!quote) return request;
  return `${quote.split("\n").map((line) => `> ${line}`).join("\n")}\n\n${request}`;
}

/** Splits a leading markdown blockquote off a user message, for display. */
export function splitQuote(text: string): { quote: string; body: string } {
  const lines = text.split("\n");
  let count = 0;
  while (count < lines.length && lines[count].startsWith(">")) count++;
  if (count === 0) return { quote: "", body: text };
  const quote = lines.slice(0, count).map((line) => line.replace(/^> ?/, "")).join("\n");
  return { quote, body: lines.slice(count).join("\n").trim() };
}
