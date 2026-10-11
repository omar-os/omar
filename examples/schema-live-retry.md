# External schema live test

Run from the OMAR checkout with the current binary and compiler:

```bash
bash examples/run-schema-example.sh examples/schema-live-retry.omar \
  review.request "Approve adding a short public README section explaining how to run the compiler tests."
```

The runner uses the installed Codex CLI and Node, and supplies their paths to
tmux. It skips the startup update check for this invocation only.
The schema is `examples/schemas/decision.json`. The case deliberately attempts
`approved with required revisions`, then corrects it to `approved` and records
the actual validation error in the unrestricted explanation port.

Verified on October 1, 2026 with the installed Codex CLI and gpt-6-astra:

- Invalid decision rejected with allowed values `approved`, `needs_revision`.
- Corrected decision and explanation writes accepted.
- Topology `SchemaLiveRetryCase` completed with runtime exit code 0.
- Persisted outputs checked against the captured tool-call transcript.

The CLI prints accepted outputs; OMAR also persists topology outputs locally.
This is one deliberately prompted recovery case, not a general reliability
benchmark or evidence that decisions are semantically correct. The JSON stored
in the explanation is an unrestricted string; it is not object-schema validation.
