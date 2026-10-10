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
`omar checkpoint retry Team` (the request that asked for the capture is
consumed by the hold, its trigger kept). Other topologies continue.

Publication renames the checkpoint before it moves the head pointer. A crash
between the two is recovered on read: a complete checkpoint whose parent is
the head, published after it, is the resume point.

Instance files are snapshotted after the boundary's invocations complete;
a background process an agent left writing can still change a worktree
between two instances' snapshots. A writer barrier is a follow-up (#277).

## Pause / resume / rollback

- `pause`: finish the tag, checkpoint, tear agents down, record `PAUSED`.
- `resume Team`: same run id; restore each instance's captured version into
  a **new** workspace, respawn agents, continue the queue. The logical clock
  continues from the captured elapsed time; a tag due later waits exactly the
  remainder. Agents start fresh conversations and are told which checkpoint
  they continue from (`restoration: fresh_conversation` in the manifest; no
  backend offers an immutable fork of a conversation). A resumed run uses the
  default timeout and real-time pace.
- A paused run holds its team: a fresh run of the team is refused until the
  paused one is resumed or stopped. `stop` on a paused run gives it up in
  place (nothing to tear down); its checkpoints stay on disk.
- `stop` outranks a pending pause (the run stops at the boundary instead),
  which is also the way out of a capture that is held. A session `down`
  waits for a pausing run to park.
- `rollback Team --checkpoint ID`: paused run only. Moves the resume point;
  deletes nothing; the abandoned branch stays listed. External effects are
  not undone.
- A fresh run starts its own lineage: the previous run's `checkpoints/` is
  moved to `checkpoints-<unix>/` beside it, so a rollback can only pick from
  the current run. Nothing is deleted.
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
`GET /v1/runs/<id>/snapshot` is the diagram a paused run left behind (its
own server dies with the loop), so a reload shows the parked run.
A run's record is kept beside its staged program from admission on, so a
daemon that starts over still offers a paused run under its own id, to the
chat and to the CLI; the deployment record settles a run that parked just
before its record said so. A run's checkpoint routes answer only while its
lineage is the team's live one; after a later run starts, they say so.
The live diagram reports `paused`.

Not in this PR: checkpoint import into another session, input journalling
while paused, format migration, pruning, manual capture or period changes
from Mission Control.

Validation: `python3 tests/ci/team_checkpoints.py` (CI) runs a real-time
timer topology with a Rust body and a stub agent in one session: automatic
and manual captures, live period change, pause, resume under the same run id
with restored files and no repeated tag, rollback to an older checkpoint, stop.
