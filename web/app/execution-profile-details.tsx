"use client";

import { useEffect, useState } from "react";
import type { ExecutionProfile, ModelAccessAssessment, ProfileFactAssessment, ProfileFactStatus, ReviewedProfileSource } from "./lib/protocol-generated";

function effectiveStatus(status: ProfileFactStatus | undefined, source: ReviewedProfileSource | undefined, now: number): ProfileFactStatus {
  if (!source) return "unknown";
  if (status !== "current") return status ?? "unknown";
  if (source.reviewed_at > now) return "not_yet_valid";
  return source.expires_at <= now ? "expired" : "current";
}

function Source({ source }: { source: ReviewedProfileSource }) {
  return <small>{source.source} · {source.locator}<br />Reviewed by {source.reviewer} on {new Date(source.reviewed_at * 1000).toLocaleString()}. Expires {new Date(source.expires_at * 1000).toLocaleString()}.<br />{source.conditions}</small>;
}

export function ExecutionProfileDetails({ profile, facts, access }: { profile: ExecutionProfile; facts?: ProfileFactAssessment; access?: ModelAccessAssessment }) {
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));
  useEffect(() => {
    const timer = setInterval(() => setNow(Math.floor(Date.now() / 1000)), 1000);
    return () => clearInterval(timer);
  }, []);
  const limitStatus = effectiveStatus(facts?.advertised_limits, profile.advertised_limits?.source, now);
  const economicsStatus = effectiveStatus(facts?.economics, profile.economics?.source, now);
  const limits = limitStatus === "current" ? profile.advertised_limits : null;
  const economics = economicsStatus === "current" ? profile.economics : null;
  const accessStatus = access?.expires_at && access.expires_at <= now ? "expired" : access?.status ?? "unknown";
  return <><details><summary>Model access: {accessStatus.replaceAll("_", " ")}</summary>
    <p>{accessStatus === "expired" ? "The reviewed native request observation has expired. Refresh model choices before running." : access?.reason || "No reviewed native model request is recorded."}</p>
    <p>The configured access flag is an operator declaration. Installation and login status do not verify model access.</p>
    {profile.access_observation ? <small>Attempt: {profile.access_observation.attempt_id}. Reviewed by {profile.access_observation.reviewer}.<br />Report: {profile.access_observation.report}<br />Observed {new Date(profile.access_observation.observed_at * 1000).toLocaleString()}; expires {new Date(profile.access_observation.expires_at * 1000).toLocaleString()}.<br />Reported model: {profile.access_observation.observed_model ?? "Unknown"}. Reported effort: {profile.access_observation.observed_effort ?? "Unknown"}.<br />{profile.access_observation.limitations}</small> : null}
    <p>Access evidence does not qualify task quality, tools or billing. Missing model identity stays unknown.</p>
  </details><details><summary>Limits and pricing</summary>
    <p>Declared provider: {profile.identity?.declared_provider ?? "Unknown"}. Requested model reference: {profile.identity?.model_reference ?? "unknown"}. The execution receipt reports any provider-observed identity separately.</p>
    <p>Advertised limits: {limitStatus.replaceAll("_", " ")}.<br />Context window: {limits?.context_window_tokens?.toLocaleString() ?? "Unknown"} tokens.<br />Maximum output: {limits?.max_output_tokens?.toLocaleString() ?? "Unknown"} tokens.</p>
    {limits ? <Source source={limits.source} /> : null}
    <p>Native session history and tool overhead are outside the measured invocation payload. Advertised token limits are not enforced caps. Declared output checks run before a handoff; they do not enable a provider JSON mode.</p>
    <p>Pricing and quota evidence: {economicsStatus.replaceAll("_", " ")}.<br />Billing model: {economics?.billing_model.replaceAll("_", " ") ?? "Unknown"}{economics?.subscription_plan ? ` · ${economics.subscription_plan}` : ""}.</p>
    {economics?.prices.length ? <ul>{economics.prices.map((price, index) => <li key={index}>${price.price_usd} per {price.quantity.toLocaleString()} {price.unit.replaceAll("_", " ")}(s). {price.conditions}</li>)}</ul> : <p>No current unit prices are known.</p>}
    {economics?.quota ? <p>Quota observation: {economics.quota.remaining ?? "Unknown"} remaining / {economics.quota.limit ?? "unknown limit"} {economics.quota.unit}.{economics.quota.window_seconds ? ` Window: ${economics.quota.window_seconds} seconds.` : ""} This is not a reservation of capacity for the run.</p> : <p>Remaining quota is unknown.</p>}
    {economics ? <Source source={economics.source} /> : null}
    <p>Published prices and subscription access do not establish this run’s cost. Native spend caps are unsupported; missing measured cost remains unknown.</p>
  </details></>;
}
