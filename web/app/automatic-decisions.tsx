import type { DiagramReaction } from "./lib/protocol";

const stages: Record<string, string> = {
  checking: "Checking with Jev",
  reasoning: "Reasoning review",
  decided: "Jev decided",
  reasoned: "Reasoning decided",
  unavailable: "Cannot continue",
};

const requirementOutcomes: Record<string, string> = {
  appears_satisfied: "Requirement met",
  partially_satisfied: "Partly met",
  not_satisfied: "Requirement not met",
};

export function AutomaticDecisions({ reactions }: { reactions: DiagramReaction[] }) {
  const gates = reactions.filter((reaction) => reaction.decision_gate);
  if (!gates.length) return null;
  return <div className="automatic-decisions" aria-label="Automatic workflow decisions">
    {gates.map((reaction) => {
      const gate = reaction.decision_gate!;
      const decision = reaction.decision;
      return <details key={reaction.id}>
        <summary>
          <span>{decision ? stages[decision.stage] ?? "Decision pending" : "Jev check · reasoning if needed"}</span>
          <span>{gate.criterion}</span>
          {decision?.route ? <strong>→ {decision.route}</strong> : null}
        </summary>
        <p>{decision?.reason ?? "Uses Jev when confidence and evidence sufficiency reach 0.95; otherwise uses the configured reasoning agent."}</p>
        <p>{gate.profile === "review-owner-v1" ? "Owner routing" : "Text requirement"} · {reaction.name}</p>
        <ul>{gate.routes.map((route) => <li key={route.outcome}>{gate.profile === "artifact-requirement-v1" ? requirementOutcomes[route.outcome] ?? route.outcome : route.outcome} → {route.port}{route.description ? `: ${route.description}` : ""}</li>)}</ul>
        {decision?.confidence != null ? <p>Confidence {decision.confidence.toFixed(3)} · Selected probability {decision.selected_probability?.toFixed(3)} · Evidence sufficiency {decision.sufficient_context?.toFixed(3)}</p> : null}
      </details>;
    })}
  </div>;
}
