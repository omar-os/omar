import type { DiagramAgent } from "./protocol.ts";

/** Web agents wait for an operator, so allow time for the manual handoff. */
export function defaultRunTimeout(agents: DiagramAgent[]): number {
  return agents.some((agent) => agent.backend.toLowerCase() === "web") ? 3600 : 300;
}

export function describeRunFailure(error: string, agents: DiagramAgent[]): {
  summary: string;
  guidance: string;
} {
  if (error.includes("failed to invoke cargo")) {
    return {
      summary: "OMAR couldn't find Cargo to build this workflow's Rust steps. No workflow agents started.",
      guidance: "OMAR checks the installed Rust toolchain automatically. Follow the investigation in chat or Inspect terminal. If Rust is missing, install it in the runtime environment, then run again.",
    };
  }
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
      guidance: "OMAR will investigate the failed step and report its next steps in chat. You can follow the investigation with Inspect terminal. A deadline declared in the workflow still takes precedence.",
    };
  }
  return {
    summary: "The workflow stopped before it could finish.",
    guidance: "OMAR will investigate and try to repair the problem automatically when the assistant is available. Follow its progress and next steps in chat or Inspect terminal.",
  };
}
