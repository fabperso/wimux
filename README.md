<div align="center">

<img src="docs/media/banner.svg" alt="wimux — the terminal multiplexer Windows never had" width="820">

# wimux

**The terminal multiplexer Windows never had.**

Persistent sessions, a native GUI, a driveable browser, and AI sub‑agent orchestration — built for PowerShell and Windows Terminal, in Rust.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Platform: Windows](https://img.shields.io/badge/platform-Windows%2010%2F11-0078D6)
![Built with Rust](https://img.shields.io/badge/built%20with-Rust-CE422B?logo=rust&logoColor=white)
![GUI: Tauri](https://img.shields.io/badge/GUI-Tauri%20v2-24C8DB?logo=tauri&logoColor=white)
<!-- TODO: add a CI badge once GitHub Actions is set up -->

</div>

---

## Why wimux?

`tmux` and `zellij` are Unix‑first. On Windows they run only inside WSL, detached from the native shell. **wimux is a multiplexer designed for Windows from the ground up** — it speaks ConPTY, drives PowerShell natively, and survives your terminal closing, just like tmux does on Linux.

Then it goes further than a tmux clone:

- 🪟 **A real native GUI** (not just a TUI) — panes, tabs, mouse, drag‑to‑resize, right on top of the same detached server.
- 🌐 **A browser you can embed *and* drive** — preview your dev server in a pane, or automate a real Chromium via CDP.
- 🤖 **AI sub‑agent orchestration built in** — spawn agents into their own panes, or fan a task out across isolated git worktrees, and collect the results.

## Highlights

- **Persistent, detached sessions.** One per‑user server owns your sessions and survives any client closing. `wimux new -s dev` → close the window → `wimux attach dev` and it's still there.
- **Three ways to drive it, one server:** a **TUI** client, a native **GUI**, and a fully **scriptable CLI** — all attach to the same source of truth.
- **tmux‑style muscle memory.** `Ctrl‑b` prefix, splits, windows, zoom, a vi‑style copy mode with search, and mouse support.
- **Scriptable by design.** `send-keys`, `split-window`, `capture-pane`, `list-panes` — automate a session without ever attaching to it.
- **Embedded browser panes.** A layout pane can be a browser (🌐); it's owned by the server and survives GUI restarts, URL and all.
- **Driveable headless browser (CDP).** `navigate`, `snapshot`, `click`, `type`, `eval`, and more — accessibility‑ref targeting, headless by default for reliable input.
- **AI‑agent orchestration.** `wimux agent` runs sub‑agents in journaled panes; `wimux batch` fans a task out across isolated git worktrees and reviews the results.
- **Written in Rust.** A detached daemon, a binary protocol over named pipes, server‑side VT emulation, ConPTY child processes — strip‑optimized release builds.

## Demo

<!-- TODO: record docs/media/wimux-demo.gif (hero GIF — see the shot list) -->
![wimux in action](docs/media/wimux-demo.gif)

<!-- TODO: replace with real screenshots of the running app -->
| GUI — panes & tabs | Agent orchestration | Browser pane |
| :---: | :---: | :---: |
| ![](docs/media/gui.png) | ![](docs/media/agents.png) | ![](docs/media/browser.png) |

## Quick start

### Install (Windows 10/11, x64)

Download the latest installer from the [**Releases**](../../releases) page and run it. wimux starts its background server automatically the first time you launch the app — nothing else to set up.

### Or build from source

```powershell
# CLI + server (Rust)
cargo build --release
target\release\wimux --help

# GUI (Tauri v2 — needs Node.js)
cd wimux-gui
npm install
npm run tauri build          # produces a release .exe + installer
```

### First steps

```powershell
wimux new -s dev             # create a session and attach to it
#   … work in the shell, then Ctrl-b d to detach …
wimux ls                     # list sessions
wimux attach dev             # re-attach — the session is still alive

# Scriptable, without attaching — the automation sweet spot:
wimux send-keys -t dev "npm test" Enter
wimux split-window -t dev -h
wimux capture-pane -t dev
```

## Three faces, one server

| | |
|---|---|
| **TUI** | A lightweight client (`crossterm`) you attach from any VT terminal. tmux‑style keys, copy mode, mouse. |
| **GUI** | A native desktop app (Tauri v2 · WebView2 · xterm.js): panes, tabs, a cwd/branch rail, context menus, OS notifications. |
| **CLI** | Every action is a command — the whole thing is scriptable and CI‑friendly. |

→ Full keybindings, copy mode and configuration: **[docs/usage.md](docs/usage.md)**

## Spotlight: AI‑agent orchestration

Running Claude (or any agent) *inside* a wimux pane? It can now spawn **other** agents into their own panes and read their output — coordination happens through the server.

```powershell
wimux agent spawn --dir v -- claude -p "write tests for the parser"
wimux agent list                 # per pane: running? exit code?
wimux agent logs -p 3 --tail 50  # read a sub-agent's transcript
```

Or fan a task out across **isolated git worktrees** and pick the winner:

```powershell
wimux batch create --repo . --template claude --prompt "fix the flaky test" --count 3
wimux batch review -g batch0     # per agent: files changed, +/-, commits
wimux batch pr -g batch0 -i 2 --title "…" --body "why this one"
```

A ready‑to‑use Claude **skill** ships in [`skills/wimux/`](skills/wimux/).

## Spotlight: a browser you can embed *and* drive

Beyond the embedded 🌐 preview pane, `wimux browser` drives a real Chromium (Edge/Chrome) over the **Chrome DevTools Protocol** — headless by default for reliable keyboard input:

```powershell
wimux browser navigate --url https://example.com/login
wimux browser snapshot                       # accessibility tree with [ref=eN] targets
wimux browser type --ref e3 --text "hello"
wimux browser eval "(() => document.title)()"
```

→ Embedded panes, all verbs, and the security model: **[docs/browser.md](docs/browser.md)**

## Architecture

A single **detached, per‑user server** is the source of truth. Thin clients attach over a named pipe using a compact binary protocol; the server owns the sessions, emulates the terminals, and drives the child processes.

```mermaid
flowchart TB
    subgraph clients [Clients]
        TUI["TUI<br/>(crossterm)"]
        GUI["GUI<br/>(Tauri · xterm.js)"]
        CLI["Scriptable CLI"]
    end
    clients -->|"named pipe · postcard protocol"| server
    subgraph server ["wimux-server — detached per-user daemon"]
        SESS["Sessions / Windows / Panes"]
        VT["Server-side VT emulation"]
        PTY["ConPTY children<br/>(portable-pty)"]
        BR["Browser engine<br/>(CDP · chromiumoxide)"]
    end
```

| Crate | Role |
|-------|------|
| [`wimux-protocol`](crates/wimux-protocol) | Wire protocol — `serde` + `postcard` framed over named pipes |
| [`wimux-vt`](crates/wimux-vt) | Terminal / VT emulation |
| [`wimux-server`](crates/wimux-server) | The daemon: sessions, panes, ConPTY, browser engine |
| [`wimux-client`](crates/wimux-client) | Shared attach/client logic |
| [`wimux-cli`](crates/wimux-cli) | The `wimux` command‑line entry point |
| [`wimux-gui`](wimux-gui) | Tauri v2 desktop GUI (TypeScript + xterm.js) |

Design decisions are recorded as ADRs in [`docs/adr/`](docs/adr/) — language choice, ConPTY lessons, VT emulation, overlapped‑IO IPC, config format.

## Configuration

wimux reads `%USERPROFILE%\.wimux.conf` at startup (tmux‑style syntax):

```text
set prefix C-a
set default-shell pwsh.exe
bind | split-window -h
bind - split-window -v
```

See the full reference in [docs/usage.md](docs/usage.md).

## Status

**Active development — a portfolio / open‑source project.** The core is functional: persistent sessions, splits and windows, copy mode, the GUI, embedded and driveable browser, and agent orchestration all work today. Expect rough edges; feedback and issues are welcome.

## Tech stack

**Rust** · ConPTY (`portable-pty`) · server‑side VT emulation · Named‑Pipe IPC (`postcard`) · **Tauri v2** + WebView2 + `xterm.js` · `crossterm` (TUI) · `chromiumoxide` (CDP).

## License

[MIT](LICENSE) © Fabrice

<!--
BEFORE MAKING THE REPO PUBLIC — remove this comment and complete:
  [ ] Add a LICENSE file (MIT) at the repo root
  [ ] Record docs/media/wimux-demo.gif (hero) + docs/media/{gui,agents,browser}.png
  [ ] Confirm the real repo URL and fix Cargo.toml `repository` (currently a placeholder)
  [ ] Set up GitHub Actions (Windows build + cargo test) and add the CI badge
  [ ] Fix the known first-keystroke focus papercut before any public launch
-->
