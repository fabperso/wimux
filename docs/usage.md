# Usage — keybindings, copy mode & configuration

Full reference for driving wimux. See the [README](../README.md) for the overview.

## Configuration

wimux reads `%USERPROFILE%\.wimux.conf` at startup (tmux‑style syntax). See the
[commented example](wimux.conf.example):

```text
set prefix C-a
set default-shell pwsh.exe
bind | split-window -h
bind - split-window -v
```

## Keybindings (prefix `Ctrl-b`)

| Key | Action |
|--------|--------|
| `d` | Detach (the session keeps running) |
| `%` | Split the pane (side by side) |
| `"` | Split the pane (stacked) |
| `h` `j` `k` `l` | Move to the left/down/up/right pane |
| `H` `J` `K` `L` | Resize the pane (left/down/up/right) |
| `z` | Zoom the active pane (full screen; `Z` in the status bar) |
| `o` | Next pane |
| `x` | Close the active pane |
| `c` | New window |
| `n` / `p` | Next / previous window |
| `0`–`9` | Go to window N |
| `[` | Enter **copy mode** (scroll back through history) |
| `]` | Paste the last copied text |
| `:` | Command prompt (`split-window -h`, `new-window`, …) |

## Mouse

Enabled by default (disable with `set mouse off` in the config):

- **Wheel** in a pane → enters copy mode and scrolls the history.
- **Left click** on a pane → makes it active.

## Copy mode (after `Ctrl-b [`)

vi‑style navigation through the scrollback:

| Key | Action |
|--------|--------|
| `j` / `k` | Down / up one line |
| `Ctrl-u` / `Ctrl-d` | Half page up / down |
| `g` / `G` | Start / end of scrollback |
| `h` / `l` / `0` / `$` | Move the cursor within the line |
| `w` / `b` | Next / previous word |
| `/` / `?` | Search forward / backward |
| `n` / `N` | Next / previous match |
| `Space` | Start selection |
| `y` or `Enter` | Copy (→ Windows clipboard) and exit |
| `q` or `Esc` | Exit without copying |
