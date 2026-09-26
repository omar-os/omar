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

## Browse and edit in Mission Control

Use **Files & versions** in the chat to select a team workspace, browse text and
raster images, or compare saved files with the current worktree. HTML, SVG and
Markdown preview as text; symlinks are not followed. Previews are limited to
512 KiB and directory listings to 2,000 entries. **Refresh** reads new agent or
editor changes. Historical versions are read-only; **Restore as new workspace**
creates an editable copy without changing the active topology.

**Open in Web VS Code** starts code-server on demand and opens a new tab on the
same `worktree/`. Install it separately using the [code-server instructions](https://coder.com/docs/code-server/install).
OMAR uses `code-server` from PATH; `OMAR_CODE_SERVER_BIN` can select another binary.
Editor settings/logs live in `~/.omar/editors/<workspace-id>/`, outside snapshots.
Extensions use code-server's Open VSX gallery; Microsoft's extension catalog is
not interchangeable.

OMAR gives each editor a private Unix socket and an authenticated loopback
HTTP/WebSocket gateway. The launch link is a credential; do not share it. No
manual port/password setup is required. Editor tabs keep the runtime alive.
**Stop editor** only stops the editor and its child processes. Idle editors are
reclaimed after 60 seconds without connections; normal runtime shutdown also
stops them. Closing Mission Control alone does not interrupt a connected editor.

Edits save directly to disk. Agents can concurrently overwrite them; stop the
topology before conflicting edits. Unsaved editor buffers are not snapshots.
The IDE's Git history is separate from OMAR's snapshot history. Terminals and
extensions still run with host-user permissions; container isolation is later.

## Agent tools and Codex startup

Topology agents may use their normal file and command tools to do invocation
work. Only topology communication is restricted: write the invocation's allowed
output ports with `omar_set_port`, then finish with `omar_complete`. Persistent
artifacts belong in `worktree/`; disposable files belong in `temp/`.

For unattended Codex agents, disable the startup update prompt by setting this
at the top level of `$CODEX_HOME/config.toml` (default `~/.codex/config.toml`):

```toml
check_for_update_on_startup = false
```

Manage Codex updates separately. An update dialog can prevent the TUI from
loading a thread before OMAR's delivery deadline. OMAR does not change this
user-wide setting or dismiss dialogs automatically. Passing `-c` to the remote
TUI is not equivalent: config overrides select OMAR's native exec path instead.
