# Acceptance scripts

One script per phase, `phase-N.sh`; each is that phase's acceptance test and what you run
to check the phase by hand.

Run one from the repository root: `scripts/acceptance/phase-0.sh`. Exit status 0 means the
phase passes; every check prints one line.

- `phase-0.sh` — the design freeze: the crate graph, the protocol table and the docs agree.
- `phase-1.sh` — the server and the print client (S1, S2): a scripted answer on stdout,
  two one-shots in one directory, `--continue`, a tool result by reference, `subscribe`
  with `since`, idle exit, and `pirs tui`.
- `phase-2.sh` — policy without code (S3, S4, S5): `pirs check` with the assembled prompt
  and a conflict naming both files, a rewritten input reaching the model, an `on turn_end`
  hook, a rewritten tool result next to the original, a `[[command]]`, a policy the model
  writes taking effect on the next turn, and a broken file as a warning.
- `phase-3.sh` — the TUI as a client (S6, S10–S14): `pirs tui --headless WxH` drives the
  real UI against the real server from a script of JSON lines on stdin
  (`crates/pirs-tui/README.md`) and the checks assert on the screens it dumps — two agents
  with their states, the attention flag appearing on idle and clearing when the page is
  viewed, a scripted tool write refreshing an open file page, a `[[render]]` picker whose
  choice becomes the next prompt, and the old interactive mode gone from `cargo metadata`.
- `phase-4.sh` — executables and connected handlers (S5, S7, S8, S9): the six examples
  under `examples/policy/` installed the way their READMEs say, then driven — `fetch`'s
  Python script answering a scripted call and its text reaching the model, `watch`
  connecting back to the socket, logging `on.turn_end` and dying with the loop, a
  registered `input` handler that never replies being skipped with a `ui.notify` warning
  while the turn completes, `ask`'s call as ordinary JSON in the session log,
  `git-checkpoint` committing after a turn that writes, `input-shortcuts` rewriting `?` and
  consuming `!`, `todo`'s widget in the manifest and on the wire, and `pi-ext` gone from
  `cargo metadata`.

- `phase-5.sh` — several agents (S16, S17): a `[[tool]]` with
  `loop = { model, prompt, wait = "idle" }` drives a second agent from one ordered faux
  script — the parent's final message on stdout, the review toolResult in its session log
  with the child's verdict and `details.loop`, `loop.list` showing the child with `parent`
  set and named `parent/review`, the child's answer reaching the model itself (the faux
  echo quotes it), `pirs wait <parent>` started before the prompt blocking for the whole
  of a run the child spends two seconds on and then printing `idle` (and exiting 1 for an
  agent that does not exist), and `loop.close` on the parent taking the child with it.

- `phase-6.sh` — remote and contained servers (S18–S21): two real servers on two temporary
  sockets, `two` reached through the bridge `pirs proxy --socket <sock2>` that a
  `servers.toml` `command =` spawns, each server with its own `PIRS_HOME` so that where a
  conversation lands says which server ran it — `pirs --server two "hi"` answering from
  two and storing there, the TUI harness listing loops from both as `one:<id>` and
  `two:<id>`, the bridge killed under an attached UI and the turn it missed replayed onto
  the page when the UI reconnects (the reconnect re-spawns the bridge, so that *is*
  restarting it), a fake server from another release (`fixtures/phase-6/fake-server.py`)
  refusing `hello` with a message naming both protocol versions, the `disallowed-methods`
  bans and a clean `cargo clippy -p pirs-client -p pirs-tui` for paths staying opaque
  (D-31), and the Docker path documented rather than run.

`lib/raw.py` is the raw wire client the scripts use for the steps the `pirs` command does
not expose: it says `hello`, sends each request line from stdin, prints every line the
server sends back, and can stand in for a registered handler (`--reply tool.x=file`).

They need no network and no credentials: model calls use the faux provider.

From phase 1, `cargo deny check bans` is part of every phase's verification: it asserts the
edges the crate graph cannot, so a UI library or an HTTP client reaching a crate that has no
business with it fails the phase (`deny.toml`, `20-architecture.md` "Mechanical checks").
