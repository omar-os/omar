export function restartText(count: number) {
  return { headline: `${count} restart${count === 1 ? "" : "s"}`, badge: `restarted ×${count}` };
}

/** Attempts arrive oldest first; updates replace the same attempt in place. */
export function restartRows(attempts: Record<string, unknown>[], reaction: string) {
  const served = (item: Record<string, unknown>) =>
    (item.invocations as { reaction?: string; invocation_id?: number }[] | undefined)?.filter((invocation) => invocation.reaction === reaction) ?? [];
  const task = attempts.flatMap(served).at(-1)?.invocation_id;
  return attempts.filter((item) => served(item).some((invocation) => invocation.invocation_id === task)).map((item, index) => {
    const retry = Number(item.retry) || 0;
    const parts = [`Attempt ${index + 1}`];
    if (retry) {
      parts.push(`retry ${retry} of 2`);
      parts.push(item.conversation_mode === "resumed native conversation" ? "resumed its conversation"
        : item.lifetime ? "new conversation" : "new conversation (earlier context unavailable)");
    }
    parts.push(item.outcome === "open" || item.outcome === "started" ? "in progress" : String(item.outcome));
    const reminded = (item.reminders as { reaction?: string; invocation_id?: number; task_id?: number }[] | undefined)?.some((reminder) => reminder.reaction === reaction && (reminder.invocation_id ?? reminder.task_id) === task) ?? false;
    if (reminded) {
      parts.push("reminded to finish");
    }
    const reason = String(item.death_reason ?? "").replace(/^tmux pane exited or process tree gone.*$/, "worker process exited");
    const session = String(item.native_session_id ?? "");
    const timestamp = item.started_at ?? item.timestamp;
    const date = typeof timestamp === "number" ? new Date(timestamp * 1000) : typeof timestamp === "string" ? new Date(timestamp) : null;
    return {
      id: String(item.attempt_id),
      reminded,
      summary: parts.join(" · ") + (reason ? ` — ${reason}` : ""),
      session, time: date && Number.isFinite(date.valueOf()) ? date.toISOString() : null,
    };
  });
}
