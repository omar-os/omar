# Jev decision profiles

Jev should be a bounded decision sensor in OMAR, not a general-purpose agent
or a source of workflow authority. A useful profile has a fixed choice set, a
clear human fallback, an observable outcome, and no direct path to a topology
write, agent message, approval, or run-state change.

## Product sequence

1. **Review routing — now.** Given a selected review finding and an enrolled
   responsibility map, recommend the owner or return `needs_review`. This is
   the safest first profile because operators already perform the task, the
   result is easy to correct, and the output is copied rather than delivered.
2. **Scenario coverage — next.** At proposal review, evaluate a named,
   template-supplied scenario against the proposed program, declared inputs,
   and compiler diagnostics. The choices are `covered`, `ambiguous`,
   `missing_contract`, `missing_handoff`, and `insufficient_evidence`. A Noul
   asks whether the supplied material is sufficient. The card identifies the
   scenario and evidence that the operator should inspect; it cannot approve
   or reject a proposal.
3. **Run intervention — later.** When an admitted run has a timeout, failed
   reaction, or contradictory review handoff, recommend the next *human*
   investigation: `inspect_contract`, `inspect_runtime`, `request_review`,
   `collect_logs`, or `stop_and_decide`. Do not add retry, stop, or routing
   choices until operators have reviewed the recommendation in the product.

## Scenario-coverage profile

Scenario design is the highest-value next use after review routing. It helps
an operator find semantic gaps before a workflow starts, where a correction is
cheap, while preserving the existing compiler and approval flow as the only
sources of authority.

Each prepared template should opt in with a versioned scenario catalog. A
scenario contains a stable ID, plain-language target, required inputs, expected
handoff evidence, and the responsibility map. OMAR sends only the selected
scenario, the proposal's source and diagnostics, and the catalog's explicit
context. Free-form workflow text is never silently turned into a scenario.

The provider request should contain one state and a small map of typed
questions. Use a Choice for coverage, a Noul for evidence sufficiency, and,
only when useful, a Score for review priority. Render the original scenario,
the result, confidence, probability distribution, and fixed next-inspection
text. A low-confidence or insufficient-evidence result becomes `needs_review`.

## Shared implementation contract

All profiles use the existing private decision store, loopback API, bounded
worker queue, source provenance, idempotency key, and feedback route. A profile
definition must pin its model, question IDs, criteria, policy thresholds, and
version hash. The provider response is validated before it is persisted as a
decision result.

The first profile uses the fixed TypeSafe System One endpoint and Jev
`1.13.0`. It sends a `state` object and named `questions`; a Choice response
contains the selected option, its full probability distribution, and model
confidence, while a Noul response contains the probability of `true`. These
signals remain separate in OMAR records.

## Measurement before expansion

For review routing, record the operator's feedback, corrected owner when
provided, and time from finding capture to the operator's next handoff. For
scenario coverage, record whether the operator found a real gap and whether
the scenario was corrected before a run. Evaluate a frozen profile and policy
against a labeled held-out set before any profile changes workflow behavior.

None of these measures make Jev a correctness oracle. They show whether the
advice is useful enough to keep visible and whether the policy remains
conservative.
