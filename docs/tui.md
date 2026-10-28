# Terminal UI

The terminal dashboard is OMAR's original client: one screen, one session, all
of its EAs, live agent panes. If you used bare `omar` before sessions, this is
the same dashboard — the session is just named now. See [sessions.md](sessions.md)
for the session model and CLI reference.

## One dashboard, many EAs

The pre-session `omar` showed every EA under `~/.omar` and `[`/`]` cycled them.
The equivalent today is to keep your EAs in **one session** and attach one
dashboard to it:

```bash
$ omar up --name dev          # one session, EA 0 (the assistant) already in it
$ omar attach -s dev --tui    # the dashboard
```

- `[` / `]` cycle the session's EAs — same keys as before.
- `N` spawns another EA (prompts for a name). `D` is currently disabled on
  session-attached dashboards until daemon-owned EA deletion ships.
- Each EA can run a different backend:
  `omar -s dev ea create --name review --agent codex` then
  `omar -s dev ea start review`, and `]` to reach it.
- `omar -s dev --ea review attach --tui` opens the dashboard on that EA
  directly. A dashboard that is already running keeps the EA it was showing.

Dashboards are per session: `omar attach -s <other> --tui` is a second
dashboard for a second session. There is no cross-session view; `omar ls`
enumerates sessions.

## Driving it

The dashboard opens on EA 0's assistant window — the same EA you used to type
prompts into. Tell it what to build; it spawns agents, which appear as panes.
Select one and press `Enter` to attach your terminal to it; `Esc` backs out.

Press `?` inside the TUI for the full shortcut list:

| Keys | Action |
| --- | --- |
| `←`/`→` or `h`/`l` | Switch panel (sidebar ↔ main) |
| `↑`/`↓` or `j`/`k` | Move selection |
| `Enter` | Attach terminal to selected agent |
| `Tab` / `Shift+Tab` or `Esc` | Drill into / out of a team level |
| `[` / `]` | Previous / next EA |
| `N` | New EA |
| `D` | Disabled on attached dashboards until daemon-owned deletion ships |
| `n` / `d` | Spawn / kill a single agent |
| `e` | Scheduled events |
| `z` | Detach (everything keeps running) |
| `Q` | Stop the session (asks first) |
| `r` | Refresh agent list |

## Detach, stop, return

- `z` detaches the client; the dashboard and every workload stay up. Attaching
  again joins the dashboard still running inside the session, on the EA it was
  showing.
- `Q` stops the session's runtime — every EA, agent, and topology. Workflows
  pause at their next tag boundary and the session keeps its state; restart it
  with `omar up --name dev`.
- Detaching never stops anything; stopping never needs a detach first.

## On a remote server

The TUI is a tmux client, so a plain SSH session is enough:

```bash
$ ssh sparky
batman@sparky:~$ omar attach -s dev --tui
```

The web dashboard is different: Mission Control binds loopback only, so
`omar attach --web` over SSH tries to open a browser on the server. Tunnel the
port instead (`ssh -L <port>:127.0.0.1:<port> sparky`) or just use the TUI.
