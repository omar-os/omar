# Runtime checkpoints, pause, resume, rollback

A checkpoint is a topology's execution state at a completed tag plus the
file version of every team instance at that moment. It lives under the
deployment directory, outside agent mounts:

```
~/.omar/ea/<id>/topologies/<Team>/checkpoints/<seq>-<id>/
  manifest.json   identity, tags, policy, per-instance workspace+snapshot+commit, agents
  state.json      queued tags with payloads, outputs, state variables, elapsed logical time
  program.json    the verified bytecode
```

Publication stages, validates, fsyncs, renames. A crash leaves the previous
complete checkpoint or the new one; staging dirs are never advertised.
Timers need no entry: a timer's next firing is a queued tag.

## When

Physical time, default every 1h, per run. Never per tag.

Every command below targets a session (`-s NAME`, or the inherited one);
the daemon owns the run, the CLI asks it.

- `omar run prog.omar --checkpoint-period 30m` overrides at launch.
- `omar checkpoint configure Team --period 2h` changes a running topology;
  next capture is one new period after the change.
- `omar checkpoint create Team [--wait]` captures now, run continues.
- `omar pause Team [--wait]` always captures first.

A deadline cannot interrupt a tag. Capture happens at the next boundary, or
at the current one while the run waits for a future tag, with that tag still
queued. Nothing is captured for time spent paused.

If capture fails, the run holds at the boundary: previous checkpoint stands,
`checkpoint_error` is in `omar status`, three internal retries, then
`omar checkpoint retry Team`. Other topologies continue.

## Pause / resume / rollback

- `pause`: finish the tag, checkpoint, tear agents down, record `PAUSED`.
- `resume Team`: same run id; restore each instance's captured version into
  a **new** workspace, respawn agents, continue the queue. The logical clock
  continues from the captured elapsed time; a tag due later waits exactly the
  remainder. Agents start fresh conversations and are told which checkpoint
  they continue from (`restoration: fresh_conversation` in the manifest; no
  backend offers an immutable fork of a conversation). A resumed run uses the
  default timeout and real-time pace.
- `rollback Team --checkpoint ID`: paused run only. Moves the resume point;
  deletes nothing; the abandoned branch stays listed. External effects are
  not undone.
- `checkpoint list|show|verify`: inspect; `verify` checks program, state and
  every referenced file version without launching anything.

`omar run --wait` returns on a pause too; a session the run created for
itself stays up so `omar resume` can continue it.

Daemon: `POST /v1/runs/<id>/pause`, `POST /v1/runs/<id>/resume` (the run
keeps its id and comes back `running` with a new diagram address),
`GET /v1/runs/<id>/checkpoints`, `GET /v1/runs/<id>/checkpoints/<cp>`
(preview: tag, queue, outputs, state, per-instance file versions), and
`POST /v1/runs/<id>/rollback {"checkpoint": ID}` for a paused run.

Mission Control: Pause, Stop and Resume live on the timeline bar at the
bottom, folded or open, because they act on where the run is on its logical
timeline. Each checkpoint is a green tick on the timeline track at the tag it
completed (the diagram stream announces `run_checkpointed` as they land); it
grows under the pointer, and clicking it scrubs to that tag and opens a
preview of what it holds; a paused run commits with "Roll back to this
checkpoint" from that preview.
`GET /v1/runs/<id>/timeline` projects a run's strip from its staged bytecode
and admitted inputs, so it exists without a draft behind it.
The live diagram reports `paused`.

Not in this PR: checkpoint import into another session, input journalling
while paused, format migration, pruning, manual capture or period changes
from Mission Control.

Validation: `python3 tests/ci/team_checkpoints.py` (CI) runs a real-time
timer topology with a Rust body and a stub agent in one session: automatic
and manual captures, live period change, pause, resume under the same run id
with restored files and no repeated tag, rollback to an older checkpoint, stop.
