"use client";

import type { ReactNode } from "react";
import type { CheckpointDetail, CheckpointSummary, TimelineStep } from "./lib/runtime-client";
import { formatDuration } from "./lib/protocol";

/**
 * The logical timeline of a program: every tag it passes through, in order.
 *
 * Before anything is deployed these are projected — worked out from the program
 * rather than observed, which is possible because what decides a tag is the
 * program and not what an agent says. Stepping through them is the determinism
 * claim made checkable: this is what will happen, and here it is before it has.
 *
 * Once a run is live the same strip follows it. Same steps, same order; what
 * changes is that a position is now a fact rather than a prediction, and the
 * strip moves on its own. That the two are the same list is the point — a live
 * run that departed from its projection would be visible here as a mismatch,
 * rather than being invisible because projection and observation were drawn by
 * different things.
 *
 * An input arriving mid-run makes the tail of this wrong, so the projection is
 * recomputed and the strip redrawn from where the run actually is.
 *
 * Checkpoints live on the same strip, because a checkpoint is a tag: the state
 * after that tag completed, with every instance's files as they were. A green
 * tick stands on the track where one was taken; it grows under the pointer,
 * and clicking it previews what it holds, from where a paused run can be
 * rolled back to it. Rolling back is choosing a tag to continue from, which
 * is why it belongs on the timeline and not in a list — the same determinism
 * that lets the strip predict what will happen is what makes "continue from
 * here" a precise instruction. Pause, Stop and Resume sit here for the same
 * reason: they act on where the run is on this strip.
 */
export function Timeline({
  steps,
  index,
  live,
  truncated,
  checkpoints = [],
  resumePoint = null,
  selected = null,
  detail = null,
  canRollBack = false,
  rollBackHint,
  controls,
  onSelectCheckpoint,
  onRollBack,
  onScrub,
  onClose,
}: {
  steps: TimelineStep[];
  /** Which step is showing; the run's own position when live. */
  index: number;
  /** Following a run rather than being scrubbed by hand. */
  live: boolean;
  /** The projection stopped early. The program has not. */
  truncated: boolean;
  /** Every checkpoint the run has published, oldest first. */
  checkpoints?: CheckpointSummary[];
  /** The checkpoint a resume continues from. */
  resumePoint?: string | null;
  /** The checkpoint whose preview is open. */
  selected?: string | null;
  /** What the selected checkpoint holds, once fetched. */
  detail?: CheckpointDetail | null;
  /** Whether the run is in a state that can roll back (paused). */
  canRollBack?: boolean;
  /** Why it cannot, when it cannot. */
  rollBackHint?: string;
  /** Pause, Stop, Resume: the run's own controls, rendered beside the strip. */
  controls?: ReactNode;
  onSelectCheckpoint?: (id: string | null) => void;
  onRollBack?: (id: string) => void;
  onScrub: (index: number) => void;
  onClose: () => void;
}) {
  const step = steps[index];
  const last = steps.length - 1;

  // Where each checkpoint sits on the strip: the step whose tag it completed.
  // One taken before any tag ran sits at the start. A tag the projection does
  // not list (it stopped early, or there is none) is placed by its timestamp
  // against the furthest tag known, so the mark still lands in order.
  const furthest = Math.max(
    1,
    ...steps.map((s) => s.timestamp),
    ...checkpoints.map((c) => c.completed_tag?.[0] ?? 0),
  );
  const placed = checkpoints.map((checkpoint, order) => {
    const tag = checkpoint.completed_tag;
    const at = tag
      ? steps.findIndex((s) => s.timestamp === tag[0] && s.microstep === tag[1])
      : -1;
    const fraction =
      at >= 0 && last > 0
        ? at / last
        : tag
          ? Math.min(1, tag[0] / furthest)
          : 0;
    return { checkpoint, at, fraction };
  });
  const tagLabel = (tag: [number, number] | null) =>
    tag ? `${formatDuration(tag[0])}:${tag[1]}` : "start";

  return (
    <div className="timeline" aria-label="Logical timeline">
      <div className="timeline-controls">
        <button
          type="button"
          aria-label="Previous tag"
          disabled={index <= 0}
          onClick={() => onScrub(Math.max(0, index - 1))}
        >
          ‹
        </button>
        <div className="timeline-rail">
          <input
            type="range"
            min={0}
            max={Math.max(0, last)}
            value={index}
            aria-label="Logical tag"
            disabled={steps.length === 0}
            onChange={(event) => onScrub(Number(event.target.value))}
          />
          {/* Checkpoint ticks, standing on the track at the tag each one
              completed. The slider thumb is 16px wide, so the track's usable
              span is the rail minus one thumb, offset by half of it. */}
          <div className="timeline-ticks" aria-label="Checkpoints">
            {placed.map(({ checkpoint, at, fraction }) => {
              const isResume = checkpoint.id === resumePoint;
              const isSelected = checkpoint.id === selected;
              return (
                <button
                  key={checkpoint.id}
                  type="button"
                  className={
                    "timeline-tick" +
                    (isResume ? " resume" : "") +
                    (isSelected ? " selected" : "")
                  }
                  style={{ left: `calc(${fraction} * (100% - 16px) + 8px)` }}
                  aria-label={`Checkpoint #${checkpoint.sequence}`}
                  aria-pressed={isSelected}
                  title={`Checkpoint #${checkpoint.sequence} · ${checkpoint.trigger} · after ${tagLabel(checkpoint.completed_tag)}${isResume ? " · resume point" : ""}`}
                  onClick={() => {
                    if (at >= 0) onScrub(at);
                    onSelectCheckpoint?.(isSelected ? null : checkpoint.id);
                  }}
                />
              );
            })}
          </div>
        </div>
        <button
          type="button"
          aria-label="Next tag"
          disabled={index >= last}
          onClick={() => onScrub(Math.min(last, index + 1))}
        >
          ›
        </button>
        <button type="button" className="timeline-close" onClick={onClose}>
          Hide
        </button>
        {controls}
      </div>

      <div className="timeline-readout">
        {steps.length === 0 ? (
          <span className="timeline-idle">
            Nothing to project: no input is set and no timer fires, so the
            program does not move.
          </span>
        ) : (
          <>
            <span className="timeline-tag">
              {/* The tag itself, which is what "when" means here — there is no
                  wall clock in it. */}
              {step.timestamp}:{step.microstep}
            </span>
            <span className="timeline-count">
              {index + 1} of {steps.length}
              {truncated ? "+" : ""}
            </span>
            <span className={live ? "timeline-mode live" : "timeline-mode"}>
              {live ? "live" : "projected"}
            </span>
            <span className="timeline-detail">
              {step.reactions.length > 0
                ? `${step.reactions.length} reaction${step.reactions.length > 1 ? "s" : ""} fire`
                : "no reaction fires"}
              {step.events.length > 0 ? ` · ${step.events.join(", ")}` : ""}
            </span>
          </>
        )}
        {checkpoints.length > 0 ? (
          <span className="timeline-checkpoints">
            {checkpoints.length} checkpoint{checkpoints.length > 1 ? "s" : ""} on the track
            {resumePoint ? " · ringed: resume point" : ""}
          </span>
        ) : null}
      </div>

      {selected ? (
        <section className="timeline-preview" aria-label="Checkpoint preview">
          {detail && detail.id === selected ? (
            <>
              <div className="timeline-preview-head">
                <strong>
                  Checkpoint #{detail.sequence} · {detail.trigger}
                </strong>
                <span>
                  after tag {tagLabel(detail.completed_tag)} · next{" "}
                  {detail.queued_tags.length > 0
                    ? tagLabel(detail.queued_tags[0])
                    : "nothing queued"}{" "}
                  · {detail.queue_len} tag{detail.queue_len === 1 ? "" : "s"} queued ·{" "}
                  {new Date(detail.created_at * 1000).toLocaleString()}
                  {detail.is_resume_point ? " · resume point" : ""}
                </span>
              </div>
              <dl className="timeline-preview-body">
                <dt>Files</dt>
                <dd>
                  {Object.entries(detail.workspaces).length === 0
                    ? "no instance files"
                    : Object.entries(detail.workspaces)
                        .map(
                          ([instance, ws]) =>
                            `${instance || "root"} → workspace ${ws.workspace_id.slice(0, 8)}, version ${ws.snapshot_id.slice(0, 8)}`,
                        )
                        .join(" · ")}
                </dd>
                <dt>State</dt>
                <dd>
                  {Object.entries(detail.state_vars).length === 0
                    ? "none"
                    : Object.entries(detail.state_vars)
                        .map(([name, value]) => `${name} = ${JSON.stringify(value)}`)
                        .join(" · ")}
                </dd>
                <dt>Outputs</dt>
                <dd>
                  {Object.entries(detail.outputs).length === 0
                    ? "none yet"
                    : Object.entries(detail.outputs)
                        .map(([name, value]) => `${name} = ${JSON.stringify(value)}`)
                        .join(" · ")}
                </dd>
                <dt>Agents</dt>
                <dd>
                  {Object.entries(detail.agents).length === 0
                    ? "none"
                    : Object.entries(detail.agents)
                        .map(([name, agent]) => `${name} (${agent.backend}): ${agent.restoration.replaceAll("_", " ")}`)
                        .join(" · ")}
                </dd>
              </dl>
              <div className="timeline-preview-actions">
                {detail.is_resume_point ? (
                  <span className="timeline-preview-note">
                    A resume continues from here.
                  </span>
                ) : (
                  <>
                    <button
                      type="button"
                      className="primary-button"
                      disabled={!canRollBack}
                      title={
                        canRollBack
                          ? "Moves the resume point here; later checkpoints stay on disk and external effects are not undone"
                          : rollBackHint
                      }
                      onClick={() => onRollBack?.(detail.id)}
                    >
                      Roll back to this checkpoint
                    </button>
                    {!canRollBack && rollBackHint ? (
                      <span className="timeline-preview-note">{rollBackHint}</span>
                    ) : null}
                  </>
                )}
              </div>
            </>
          ) : (
            <span className="timeline-preview-note">Loading checkpoint…</span>
          )}
        </section>
      ) : null}

      {truncated ? (
        <p className="timeline-truncated">
          Stopped after {steps.length} tags. A periodic timer has no end, so a
          preview has to.
        </p>
      ) : null}
    </div>
  );
}
