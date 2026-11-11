import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "../app/globals.css";
import "../app/workflow-design.css";
import { TeamGate } from "../app/team-gate";

/**
 * Mode is chosen at launch in the Worker build, because only the server knows
 * the daemon's address there. Served by `omar serve` there is nothing to
 * choose: the page came from the daemon, so the daemon is where it came from.
 * Same origin, which is also why none of this needs CORS. From `omar
 * gateway` the same page signs a member in first; see `TeamGate`.
 */
createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <TeamGate origin={window.location.origin} pathname={window.location.pathname} />
  </StrictMode>,
);
