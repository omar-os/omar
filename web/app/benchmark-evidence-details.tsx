"use client";

import { useEffect, useState } from "react";
import type { CapabilityEvidence, CapabilityEvidenceAssessment } from "./lib/protocol-generated";

export function BenchmarkEvidenceDetails({ evidence, assessment }: {evidence: CapabilityEvidence; assessment?: CapabilityEvidenceAssessment}) {
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));
  useEffect(() => {
    const timer = setInterval(() => setNow(Math.floor(Date.now() / 1000)), 1000);
    return () => clearInterval(timer);
  }, []);
  const b = evidence.benchmark_result;
  if (!b) return null;
  const state = evidence.expires_at <= now ? "stale" : assessment?.status ?? "insufficient";
  const current = state === "candidate" && b.review?.accepted && evidence.reviewed_at <= now;
  const date = (value: number | null) => value ? new Date(value * 1000).toLocaleDateString() : "Unknown";
  return <details><summary>Published benchmark: {state.replaceAll("_", " ")}</summary>
    <p>{b.benchmark} · {b.benchmark_version ?? "Unknown version"}<br />Measured task: {b.task_slice}</p>
    <p>{current ? <>Published score: {b.score} {b.unit ?? "(unknown unit)"}. {b.direction.replaceAll("_", " ")}.<br />Sample size: {b.sample_size ?? "Unknown"}.{b.uncertainty ? <> Interval: {b.uncertainty.lower}–{b.uncertainty.upper} ({b.uncertainty.method}).</> : " Uncertainty: Unknown."}</> : "No current reviewed numerical result is available for this selection."}</p>
    <p>Published results help shortlist candidates. They do not qualify a default or establish performance on your workflow.</p>
    <p>Measured model: {b.measured_model}. Backend: {b.measured_backend ?? "Unknown"}. Harness: {b.measured_harness ?? "Unknown"}. Effort: {b.measured_effort ?? "Unknown"}.<br />Measured tools: {b.measured_tools === null ? "Unknown" : b.measured_tools.join(", ") || "None reported"}.</p>
    <p>{b.measurement_conditions}<br />{b.applicability}<br />Dataset overlap: {b.dataset_overlap}<br />Contamination: {b.contamination}</p>
    <small>{evidence.source} · {b.source_row.locator}<br />Published: {date(b.published_at)}. Retrieved: {date(b.retrieved_at)}. Expires: {date(evidence.expires_at)}.<br />Reviewed by {b.review?.reviewer ?? "No reviewer"}. {b.review?.rationale}<br />The source extract is operator-supplied; its digest binds the reviewed row, not independent authorship or retrieval.</small>
  </details>;
}
