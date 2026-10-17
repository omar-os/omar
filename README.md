<div align="center">

<img src="img/full-black-banner.jpg" alt="OMAR" width="250">

**LLM agents are unpredictable. Their coordination doesn't have to be.**

**`omar` delivers deterministic, formally specified orchestration for multi-agent systems.**

<p align="center">
  <a href="https://omar.rs">omar.rs</a>&nbsp; • &nbsp;
  <a href="https://omar.rs/zh/">中文</a>&nbsp; • &nbsp;
  <a href="https://opensource.org/licenses/BSD-3-Clause"><img src="https://img.shields.io/badge/License-BSD_3--Clause-blue.svg" alt="License" valign="middle"/></a>&nbsp; • &nbsp;
  <a href="https://github.com/lsk567/omar/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/lsk567/omar/ci.yml?label=CI&logo=github" alt="CI Status" valign="middle"/></a>&nbsp; • &nbsp;
  <a href="https://discord.gg/X76PSzmfWr"><img src="https://img.shields.io/discord/1467663881588572182?label=Discord&logo=discord&logoColor=white&color=5865F2&cacheSeconds=60" alt="Discord" valign="middle"/></a>
</p>

<p align="center">
<img src="./img/web.gif" alt="Web UI" valign="middle"/>
Web UI
</p>

</div>

## Features

- **Deep hierarchies**: Agents managing agents, just like a company.
- **Heterogeneity**: Let `claude`, `codex`, and other agents collaborate as a team.
- **Full control**: Talk to and control any subagent you want.
- **Life span**: Long-running or ephemeral agents, your choice.
- **Customization**: Support all `tmux` commands you love.
- **Team workspaces**: Each topology team instance gets a versioned `worktree/` and disposable `temp/`; inspect and restore file versions from Mission Control or `omar workspace`.

Other features include computer use and more.

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
$ omar up --name dev
$ omar attach -s dev --web
```

Starts an independent background runtime, then opens its web UI. Closing the
browser leaves the runtime and its topologies running. Stop it with `omar down -s dev`.
`omar --help` lists the session commands.

#### Step 2: Describe a workflow

Type what the team should do. The assistant drafts an OMAR program and shows you
the topology it compiles to.
Nothing runs until you press **Confirm deploy**,
then the diagram goes live.

### Session commands

| Command | What it does |
|---------|--------------|
| `omar up --name dev` | Start a background session, or restart the stopped session named `dev`. |
| `omar attach -s dev --web` | Open Mission Control for that session. Use `--tui` for the terminal UI. |
| `omar down -s dev` | Stop the session and keep its state. |
| `omar rm -s dev` | Remove the session and its state, stopping it first if running. |

Closing the browser or detaching leaves the session running. Use `omar --help`
for more commands and `omar <command> --help` for options.

### Terminal UI

Note: The terminal UI does not yet implement the deterministic model in the mission control.

#### Step 1: Start a session and attach the terminal UI

```bash
$ omar up --tui
```

Or attach to a running session with `omar attach -s <session> --tui`. `z`
detaches and the runtime keeps every workload running; `Q` stops the session.

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
| [Claude Code](https://docs.anthropic.com/en/docs/agents-and-tools/claude-code/overview) | `omar -a claude up` (default) |
| [Codex CLI](https://developers.openai.com/codex/cli) | `omar -a codex up` |
| [Cursor CLI](https://cursor.com/cli) | `omar -a cursor up` |
| [Opencode](https://github.com/anomalyco/opencode) | `omar -a opencode up` |
| [Google Antigravity CLI](https://antigravity.google/product/antigravity-cli) | `omar -a agy up` |
| [Pi](https://pi.dev) | `omar -a pi up` |

Each `omar -a <backend> up` starts a new independent session. Add `--name dev`
to name it, and `--web` or `--tui` to open a client immediately.

## License

BSD 3-Clause

## Contributors

Thanks to all of our amazing contributors!

<a href="https://github.com/omar-os/omar/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=omar-os/omar" />
</a>

---

OMAR is made with ❤️ in Berkeley, CA.
