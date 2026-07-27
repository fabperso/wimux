# wimux

**The terminal multiplexer Windows never had.**

`tmux` and `zellij` are Unix-first — on Windows they only really live inside
WSL, detached from the native shell. wimux is built for Windows from the
ground up: it speaks **ConPTY**, drives PowerShell natively, and keeps your
sessions alive after every client is gone.

```powershell
cargo install wimux wimux-server
```

Both binaries must sit side by side — `wimux` starts `wimux-server` for you on
first use, and `cargo install` puts them in the same directory.

```powershell
wimux new -s dev          # create a session and attach to it
#   … work in the shell, then Ctrl-b d to detach …
wimux ls                  # list sessions
wimux attach dev          # re-attach — everything is still running

# Scriptable, without attaching:
wimux send-keys -t dev "cargo test" Enter
wimux capture-pane -t dev
```

## What you get

- **Persistent, detached sessions** — one per-user server owns them and
  survives any client closing.
- **tmux-style muscle memory** — `Ctrl-b` prefix, splits, windows, zoom, a
  vi-style copy mode with search, mouse support.
- **A scriptable CLI** — `send-keys`, `capture-pane`, `list-panes`: automate a
  session without ever attaching to it.
- **Driveable browser panes** — `wimux browser` controls a real Chromium over
  CDP (navigate, snapshot the accessibility tree, click, type, eval).
- **AI sub-agent orchestration** — `wimux agent` spawns agents into their own
  journaled panes; `wimux batch` fans a task out across isolated git worktrees.

A native GUI (Tauri v2 + xterm.js) ships separately — see the installer on the
[releases page](https://github.com/fabperso/wimux/releases).

## Documentation

Full keybindings, copy mode, configuration and the browser reference live in
the [repository](https://github.com/fabperso/wimux).

Windows 10/11, x64. MIT licensed.
