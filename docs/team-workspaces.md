# Team-instance workspaces

Every topology deployment allocates a fresh workspace for each declared team
instance. Agents in the same instance share it. Nested instances receive their
own workspace; nesting does not grant a shared writable directory. Legacy
bytecode without instance names uses one root workspace.

```
~/.omar/workspaces/<workspace-id>/
  worktree/   persistent files: source, reports, images, datasets, binaries
  temp/       disposable files, never included in snapshots
```

Agents and Rust reaction bodies start in `worktree/`. `OMAR_WORKTREE` points to
it; `OMAR_TEMP` and `TMPDIR` point to `temp/`. Agents receive these conventions
in their topology instructions. Web agents can inspect the workspace via the
CLI; artifact browsing in Mission Control is a later feature.

New worktrees are seeded from the configured `agent.default_workdir`. Git sources
include current tracked-file edits and non-ignored untracked files, without
changing the source repository or its index. Deleted files stay deleted.
Non-Git sources copy ordinary files and symlinks, excluding OMAR's state tree.
Source writers should be idle during deployment. Git submodules/nested
repositories and special files require a separate source directory; they are
not silently omitted from file snapshots.

## File versions

Git is required. Each instance has an independent repository whose administration,
snapshot metadata, and locks live in `~/.omar/workspace-history/<workspace-id>/`.
`worktree/.git` is a linked-worktree pointer. No source repository history or
remotes are copied. Workspace Git commits/staging and OMAR snapshots are separate:
OMAR uses its own temporary index and immutable snapshot refs.

An initial file version is recorded at deployment, and a final version after a
successful completion or graceful stop. Workspaces survive termination and
redeployment. Deployment records retain the instance-to-workspace mapping.

```sh
omar workspace list
omar workspace show <workspace-id>
omar workspace snapshot <workspace-id> --label 'Before experiment'
omar workspace restore <workspace-id> <snapshot-id>
# Add --ea <id-or-name> to select another EA.
```

Commands return JSON with stable IDs. `show` and `restore` include the directory
paths. Manual snapshots require a terminal owning deployment with no remaining
live agent sessions; a dead runner alone does not establish cleanup. Restored
copies have their own workspace IDs and are not owned by the original run.
Quiesce any other writers before snapshotting. Atomic publication protects completed snapshots,
but reading a changing directory is not a transactional filesystem snapshot.

Snapshots include all files in `worktree/`, including ignored/untracked files and
binary content. Git attributes, clean/smudge filters, and line-ending conversion
do not change stored or restored bytes. Executable bits and symlink targets are
preserved; links are not followed. Empty directories, ownership, timestamps,
extended attributes, and hard-link identity are not versioned. `.git`
administration and `temp/` are excluded. No automatic snapshot pruning is done.

Restore creates a new workspace and Git branch, with an empty `temp/` and a
record of its source snapshot. It leaves the original workspace, later files,
and snapshot history intact. It does not change which workspace an active run
uses, start agents, or restore queues, clocks, or conversations. Those belong to
the later runtime-checkpoint/resume feature.

This is an ownership convention, not filesystem isolation: processes still run
as the host user and can access paths outside their worktree. Container mounts
and mediated Git access will enforce the boundary later. Restoring files does
not undo external actions such as sent messages or database writes.
