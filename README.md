<div align="center">

<img src="img/full-black-banner.jpg" alt="OMAR" width="250">

**Turn your coding agents into a programmable team.**

Claude Code, Codex, and other agents. Explicit handoffs. Live control. Running on your machine.

<p align="center">
  <a href="https://omar.rs">omar.rs</a>&nbsp; • &nbsp;
  <a href="https://omar.rs/zh/">中文</a>&nbsp; • &nbsp;
  <a href="https://opensource.org/licenses/BSD-3-Clause"><img src="https://img.shields.io/badge/License-BSD_3--Clause-blue.svg" alt="License" valign="middle"/></a>&nbsp; • &nbsp;
  <a href="https://github.com/lsk567/omar/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/lsk567/omar/ci.yml?label=CI&logo=github" alt="CI Status" valign="middle"/></a>&nbsp; • &nbsp;
  <a href="https://discord.gg/X76PSzmfWr"><img src="https://img.shields.io/discord/1467663881588572182?label=Discord&logo=discord&logoColor=white&color=5865F2&cacheSeconds=60" alt="Discord" valign="middle"/></a>
</p>

</div>

**OMAR (Open Multi-Agent Runtime) is an open-source runtime for coordinating the coding agents you already use.** Describe a workflow in Mission Control, inspect the generated team diagram, and confirm deployment. Watch the work unfold, open individual agents' terminals, and keep the files the team produces.

The coordination itself is a program. OMAR compiles a `.omar` workflow into explicit inputs, outputs, and handoffs, then uses a deterministic runtime to schedule the work. You can define who receives a result, which steps can run in parallel, and where a human decision enters the workflow. That determinism applies to orchestration; model answers and external tool actions can still vary. See the [language specification](lang/spec.md) for the execution model.

<div align="center">

<p align="center">
<img src="./img/web.gif" alt="Web UI" valign="middle"/>
Mission Control
</p>

<p align="center">
<img src="./img/demo.gif" alt="Terminal UI" valign="middle"/>
Terminal UI (legacy)
</p>

</div>

## Features

- **Explicit coordination contracts**: Declare typed inputs and outputs, triggers, and permitted output writes. The compiler and runtime validate the plan before execution, and the runtime enforces output contracts as agents work.
- **Mixed agent teams**: Put Claude Code, Codex, Cursor, OpenCode, Antigravity, and Pi in the same workflow. Choose a backend for each role using the [supported agent backends](#supported-agent-backends).
- **Teams within teams**: Compose reusable, parameterized teams into deeper hierarchies. Build pipelines, parallel branches, and feedback loops with explicit connections between them.
- **Live visibility and direct access**: Follow execution status and values in Mission Control's live diagram. Open an individual agent's terminal to inspect its session and interact directly.
- **Human steps in the workflow**: Use the `Web` backend to supply an answer or review decision through Mission Control. Human responses follow the same output contracts as agent responses.
- **Files with history**: Each team instance gets a persistent `worktree/` and disposable `temp/`. Keep outputs after a run, inspect file snapshots, and restore a version into a new workspace. [Workspace details](docs/team-workspaces.md).
- **Flexible sessions and familiar tools**: Run long-lived or task-specific agents, with terminal access and the `tmux` controls you already use.

Optional bridges connect agents to [Slack](bridges/slack/README.md) and [computer-use tools](bridges/computer/README.md). Explore the [example programs](tests/topology/src) to see how workflows are expressed in `.omar` source.

## Installation

### Prerequisites

- tmux 3.0+
- Git (for team-instance workspaces and file snapshots)
- Rust 1.89+
- GNU Make
- Node.js 22.13+ (to build Mission Control, which `make build` embeds)
- [elan](https://github.com/leanprover/elan) (to build `omarc`, which `make install` installs alongside `omar`)
- One or more coding agents [listed here](#supported-agent-backends).

### One-liner (recommended)

```bash
curl -fsSL https://omar.rs/install.sh | sh
```

Installs all binaries to `/usr/local/bin`.

### Homebrew

```bash
brew install omar-os/omar/omar
```

### Build from source

Requires Rust 1.89+, GNU Make, and elan.

```bash
git clone https://github.com/omar-os/omar.git
cd omar && make install
```

## Quick Start

#### Step 1: Launch Mission Control

```bash
$ omar serve --ui
```

Serves the web UI from the daemon's own address and opens it in your browser.

#### Step 2: Describe a workflow

Type what the team should do. For example:

> Create a team that drafts and reviews a technical design. Use Claude Code to
> write a proposal from my brief, then pass it to Codex to review the tradeoffs
> and missing requirements. Send both the proposal and review to me for a final
> decision in Mission Control.

The assistant drafts an OMAR program and shows you the team diagram it compiles
to. Inspect the roles and handoffs, then press **Confirm deploy** to start the
workflow. The diagram goes live as agents work and pass results between steps.
This example requires both Claude Code and Codex to be installed and configured.

### Terminal UI (Legacy)

Note: The legacy terminal UI does not yet implement the deterministic model in the mission control.

#### Step 1: Launch `omar`

```bash
$ omar
```

Go [here](#supported-agent-backends) to see how to launch with specific agent backends.

#### Step 2: Tell the Executive Assistant (EA) to run a test prompt.

Copy the following into the EA window:
```
Run https://github.com/omar-os/omar/blob/main/prompts/tests/project-factory.md
```

You should see agents being spawned by the EA.

Tip: Use `↑↓←→` to cycle through agents at the current level. Use `Tab` to drill into a deeper level. Use `Shift+Tab` to back out.

#### Step 3: Shutdown the project.

Go back to the EA and type in:
```
Shutdown the test project and its agents.
```

## Supported Agent Backends

| Backend | How to launch |
|---------|---------------|
| [Claude Code](https://docs.anthropic.com/en/docs/agents-and-tools/claude-code/overview) | `omar -a claude` (default) |
| [Codex CLI](https://developers.openai.com/codex/cli) | `omar -a codex` |
| [Cursor CLI](https://cursor.com/cli) | `omar -a cursor` |
| [Opencode](https://github.com/anomalyco/opencode) | `omar -a opencode` |
| [Google Antigravity CLI](https://antigravity.google/product/antigravity-cli) | `omar -a agy` |
| [Pi](https://pi.dev) | `omar -a pi` |

Each `omar -a <backend>` launch creates a new EA, named with its new EA number.
Use `omar -a codex --ea Research` to give the new EA a semantic name. An existing
name is rejected rather than replacing its manager. Run `omar --ea <id-or-name>`
without `-a` to open an existing EA.

Codex launch commands no longer disable the alternate screen. OMAR does not
add `--no-alt-screen`; an explicit flag in a custom command is still respected.
Remove that flag from an existing `default_command` to try the alternate screen,
then launch a new session (running sessions keep their original arguments).

The web terminal forwards Escape to the agent. Close with the **Close** button,
**Ctrl+Shift+Escape**, or a click on the backdrop. New sessions use the launching
terminal's dimensions, or 120×40 when headless. Web attachments start at the
panel's measured size. After the last viewer in a daemon closes, OMAR restores
the original size and sizing policy if no external client remains and the
policy has not been changed. Concurrent clients still share tmux's window and
its sizing policy; a manually sized window stays manual.

## License

BSD 3-Clause

## Contributors

Thanks to all of our amazing contributors!

<a href="https://github.com/omar-os/omar/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=omar-os/omar" />
</a>

---

OMAR is made with ❤️ in Berkeley, CA.
