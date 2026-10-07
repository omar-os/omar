import type { DiagramAgent } from "./protocol.ts";

/** Web agents wait for an operator, so allow time for the manual handoff. */
export function defaultRunTimeout(agents: DiagramAgent[]): number {
  return agents.some((agent) => agent.backend.toLowerCase() === "web") ? 3600 : 300;
}

export function describeRunFailure(error: string, agents: DiagramAgent[]): {
  summary: string;
  guidance: string;
} {
  const timeout = /'([^']+)' did not answer within (\d+)s/.exec(error);
  if (timeout) {
    const [, name, secondsText] = timeout;
    const seconds = Number(secondsText);
    const duration = seconds % 60 === 0
      ? `${seconds / 60} minute${seconds === 60 ? "" : "s"}`
      : `${seconds} seconds`;
    const manual = agents.some((agent) => agent.name === name && agent.backend.toLowerCase() === "web");
    return manual ? {
      summary: `No response was submitted for ${name} within ${duration}.`,
      guidance: "This is a manual Web step. Complete its task outside OMAR, then submit the result through the response panel. Run again with enough time for that handoff.",
    } : {
      summary: `${name} did not respond within ${duration}.`,
      guidance: "Check the agent's terminal for the cause. Resolve the problem or allow more time before running again. A deadline declared in the workflow still takes precedence.",
    };
  }
  return {
    summary: "The workflow stopped before it could finish.",
    guidance: "Review the error details, correct the problem, then run the workflow again.",
  };
}
