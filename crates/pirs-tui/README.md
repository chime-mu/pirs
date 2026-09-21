# pirs-tui

The reference UI: a client of the pirs loop server (`docs/design/20-architecture.md`, "The TUI").
It depends on `pirs-protocol` and `pirs-client` only. Config is `~/.pirs/tui.toml` (see `src/config.rs`
for every key and its default); a missing file means defaults and a broken one is a notice.

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
