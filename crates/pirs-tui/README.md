# pirs-tui

The reference UI: a client of the pirs loop server (`docs/design/20-architecture.md`, "The TUI").
It depends on `pirs-protocol` and `pirs-client` only. Config is `~/.pirs/tui.toml` (see `src/config.rs`
for every key and its default); a missing file means defaults and a broken one is a notice.

## Several servers

The sidebar spans every server `~/.pirs/servers.toml` names (D-05); `pirs tui --server <name>`
narrows it to one and `--socket` overrides the local one. An agent is `(server, loop)` — two
servers can hand out the same loop id — and is written `server:id` whenever there is more than
one server, so a single-server screen is unchanged. Every request about a loop goes to its own
server's connection: prompts, `fs.read` for its file pages, `loop.close`. `/new` asks for a
directory and then, across several servers, which one runs the agent.

Stored conversations are listed per server: `loop.list { cwd }` goes to each one with this
machine's directory as written, because a path belongs to the server that produced it and no
client rewrites one (D-31). On a server elsewhere that directory usually names nothing and its
conversation list comes back empty; its running agents are listed all the same.

A link that drops is a notice ("server `build` disconnected; reconnecting…"), an `! build offline`
marker in the sidebar, and a retry after 0.5 s, 1 s, 2 s, 4 s and then every 5 s, none of it
blocking the UI. When it comes back the subscriptions are re-issued from the last `seq` seen and
the missed events replay exactly once (D-06, S19). A server that refuses this client's protocol
version is a notice that stays and no further retries (S21).

The editor pane (`/edit` on a file page) runs `tmux split-window -h "<prefix> $EDITOR <path>"`,
where the prefix is the server's: an explicit `editor_prefix` in `servers.toml`, else the bridge
command without its `pirs proxy` (`ssh build pirs proxy` → `ssh build`). `PIRS_TUI_TMUX` names a
program to run instead of `tmux`, which is how the tests assert on that command line.

## Headless protocol (`pirs tui --headless WxH`, `run_headless`)

The same engine drawn on `ratatui`'s `TestBackend`. Stdin is a script of JSON lines, one command per line,
executed in order; every command is echoed on stdout as `{"ok":"<command>"}` or `{"error":"<why>"}`:

- `{"key":"<name>"}` — one key press; names are crossterm-style: `j`, `1`, `enter`, `esc`, `tab`, `shift-tab`,
  `up`, `down`, `pageup`, `backspace`, `ctrl-q`, `alt-enter`, `f5`.
- `{"text":"..."}` — types the text into the focused line.
- `{"settle":200}` — sleeps that many milliseconds, then lets the engine catch up.
- `{"wait":"<substring>","timeout":5000}` — polls until the screen contains the substring (default timeout 5000 ms).
- `{"dump":true}` — prints the screen: a line `=== screen ===`, one line per row, then `=== end ===` (after the `ok` echo).
- `{"quit":true}` — stops the UI; end of input does the same.

The exit code is 0 when every command succeeded and 1 when any command echoed an error.
