# Automatic Jev decisions

An explicit `jev(...)` annotation turns a small reasoning prompt into a decision
gate. Jev selects a declared branch; uncertain answers use that prompt's existing
reasoning agent. The next reaction waits for the decision. Ordinary prompts,
code reactions, and the advisory off/shadow/suggest modes keep their behavior.

## Enable

Build with `cargo build --features decision-support` (add `ui` for Mission Control),
and build the matching compiler with `cd lang && lake build omarc`. In the config
passed to the daemon/CLI:

```toml
[decision_support]
enabled = true
automatic_enabled = true
default_mode = "off"
```

Supply `TYPESAFE_API_KEY` to the runtime process. Never put it in a workflow.
Automatic calls default off. The builder receives the enabled/disabled state and
adds annotations only to eligible decision prompts when enabled. The proposed
gate, criterion, and routes are visible before the operator runs the workflow;
there is no per-decision confirmation. Existing workflows are not silently rewritten.
Without the feature, config opt-in, or a working provider, annotated gates use
their reasoning fallback. Config changes apply to new daemon processes/runs;
use Stop to cancel an active workflow.

## Where it fits

Only two profiles are enabled for automatic execution:

- `artifact-requirement-v1`: one explicit semantic text requirement. Satisfied
  forwards to the next step; partial/unsatisfied forwards to repair.
- `review-owner-v1`: one finding routed using 2–12 declared responsibilities.
  Multiple/uncertain escalates. Declare a coordinator when cross-team work is expected.

Each gate reads one complete string port and forwards the original value to
exactly one declared string effect. Run deterministic validation upstream.
Use ordinary code for schema validity, file presence, counts, dates, and tests.
Use ordinary reasoning for generation, broad editorial judgment, tool use, missing
retrieval, or decisions whose relevant context cannot be supplied. Only add a gate
when it replaces a reasoning decision that would otherwise be necessary.
Passing a text criterion does not approve an entire artifact or bypass permissions.

```omar
team Check[reviewer : Codex]
{
    input draft : string
    output ready : string
    output revise : string

    prompt reviewer(draft) -> (ready | revise) within(60s)
        jev("artifact-requirement-v1", "The opening identifies the founder and the event.",
            appears_satisfied: ready, partially_satisfied: revise, not_satisfied: revise)
        "Review only the stated requirement using the supplied draft."
}

main RequirementCheck { check = Check() }
```

Connect `ready` to the next stage and `revise` to an existing revision stage.
See [owner routing](../examples/assistance/automatic_owner.omar) for responsibility
descriptions. `within(...)`, when present, precedes `jev(...)`. The annotation is
part of compiled bytecode, including qualified routes for nested team instances.

## Execution policy

- Jev 1.13.0 only; exact profile questions, finite labels, bounded response, strict
  model/distribution validation reuse the existing provider implementation.
- Choice confidence, selected probability, and sufficient-context probability must
  each be at least **0.95**. Keep them separate; confidence is not measured accuracy.
- Requirement decisions also need the supplied evidence reference. Uncertain or
  unsupported outcomes, insufficient evidence, timeout, and malformed provider
  responses escalate once. Evidence over 16 KiB skips Jev without truncation.
- The reasoning agent receives the criterion, complete evidence, allowed outcomes,
  validated Jev result, and escalation reason. It returns a declared outcome plus
  a short explanation. Invalid/blocked/failed fallback stops with a concrete error.
- Missing or empty evidence calls neither model. The workflow must gather it first.
- The gate's deadline includes provider and fallback time. Maximum 100 gate
  invocations per run, one provider attempt and at most one fallback each; no
  automatic retries. Gates are serialized per run to bound concurrency.
- Decisions bind invocation, evidence digest, model, profile, routes, and policy.
  Private records live in the deployment's `automatic-decisions/<deployment-id>`.
  Repeated invocation IDs reuse a resolved decision; changed evidence is rejected.
  An invocation left pending by a crash is refused, not sent again. A new run gets
  a new deployment and invocation ID; OMAR does not resume external side effects.
- Stop discards in-flight gate results before any downstream layer is scheduled.
  Workflows containing gates stop at layer boundaries; ordinary runs retain their
  existing tag-boundary behavior.

The workflow view shows checking, reasoning escalation, chosen branch, explanation,
and the three scores. Snapshots retain the latest update across stream reconnects.
Records keep latency, provider input tokens, scores and fallback reason for evaluation;
they do not duplicate evidence text. Provider and reasoning quality still require
reviewed real-world examples before broad rollout. Synthetic tests establish control
flow, not a 95% real-world accuracy claim.

## Verify

```sh
cargo test --bin omar --features decision-support decisions::
cargo test --bin omar --features decision-support topology::
cargo test --bin omar protocol
cd lang && lake test
```

CI runs the same runtime, compiler, and UI protocol regressions without provider
credentials. Live Jev calls are separate from these deterministic tests.
