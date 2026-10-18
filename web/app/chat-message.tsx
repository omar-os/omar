"use client";

import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { ChatMessage as ChatMessageModel } from "./lib/protocol";

/**
 * Chat messages are markdown — participants write bold, inline code, fenced
 * diagrams and lists, and reads as noise unrendered.
 *
 * `react-markdown` does not render raw HTML, which matters here: this is model
 * output relayed from an agent, so it is not trusted enough to inject.
 */
export function ChatMessage({ message }: { message: ChatMessageModel }) {
  return (
    <article
      className={`message ${message.role}${message.progress ? " progress" : ""}`}
    >
      <span aria-hidden="true">{message.role === "assistant" ? "O" : "YOU"}</span>
      {/* One column for the label and one for everything else. Adding a third
          child put the text in the label's 28px column, which broke it onto a
          line per word. */}
      <div className="message-content">
        <div className="message-author">{message.role === "assistant" ? "OMAR" : "You"}</div>
        {message.selection.length > 0 ? (
          // Kept on the message so the thread still shows what "this one" meant.
          <p className="message-selection">[{message.selection.join(", ")}]</p>
        ) : null}
        <div className="message-body">
          <Markdown remarkPlugins={[remarkGfm]}>{message.text}</Markdown>
        </div>
      </div>
    </article>
  );
}
