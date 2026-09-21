# Handoff

Written 2026-09-21 at the end of the `docs/design/PLAN.md` run. Read this first, then
`STATUS.md` for the phase table, `README.md` for the user's view, `docs/index.md` for the
docs map, and `docs/design/README.md` if you are discussing the design.

## What happened

The plan was executed end to end on branch `adaptable` (from `main` at 87fb1ce), one commit
per phase after a Fable review and fixups. Nothing is pushed. The old pi port is gone:
`pi-cli` (phase 3) and `pi-ext` (phase 4) are deleted, `examples/extensions/` and the pi
extension docs with them. The workspace is `pi-ai`, `pi-agent`, `pirs-protocol`,
`pirs-server`, `pirs-client`, `pirs-tui`, `pirs`.

| Phase | Commit | Tests | Acceptance |
|---|---|---|---|
| 0 Design freeze in code | f424c04 | 113 | `phase-0.sh` |
| 1 Server and print client | 2f7f5e2 | 254 | `phase-1.sh` (14 checks) |
| 2 Policy without code | 5c08088 | 346 | `phase-2.sh` (16 checks) |
| 3 TUI as a client | b243abb | 303 | `phase-3.sh` (10 checks) |
| 4 Executables | 2583315 | 312 | `phase-4.sh` (21 checks) |
| 5 Several agents | cec282d | 324 | `phase-5.sh` (12 checks) |
| 6 Remote and contained servers | dfec48b | 354 | `phase-6.sh` (13 checks) |
| 7 Intent tooling | 9b0c92d | 380 | `phase-7.sh` (27 checks) |
| Final tidy and review fixups | the last two commits on the branch | 381 | all eight from a clean checkout of the tidy commit; the fixup commit re-verified in place |

Phase 3's total is lower than phase 2's because `pi-cli`'s 71 tests left with it.

## Decisions taken without the user

Entries at the end of `docs/design/90-decisions.md`, with the evidence. D-38 to D-41 are still
**proposed** and the layer files were not edited for them (one exception below); D-42 and
D-43 are accepted and folded. Accept, reject, or
supersede each; the code follows the entry as written.

- **D-38** `loop.list { cwd? }` returns stored conversations. (phase 0)
- **D-39** `fs.read` serves the requested path in full; by-reference payloads appear in
  events and tool results. (phase 1)
- **D-40** `dsl.check` returns `rendered`, the merged policy as text, so `pirs check` can
  print it. (phase 2)
- **D-41** `[settings]` has four keys: `model`, `thinking`, `tools`, `tool_execution`. (phase 2)
- **D-42** A `[[tool]]` with `params` and neither `run` nor `loop` declares a tool served by
  whichever client registered `tool.<name>`. (phase 4) **Accepted by the user 2026-09-21 and
  folded into `40-dsl.md`.**
- **D-43** An executable `[[prompt]] run` is a prompt handler over the assembled prompt,
  the one exception to "prompts concatenate". (phase 4) **Accepted by the user 2026-09-21
  and folded into `40-dsl.md`.**

Design prose edited for an *accepted* entry: the "Editing" bullet in
`docs/design/20-architecture.md` said the TUI suspends into `$EDITOR`; it now matches D-29
(a tmux pane, never suspends). Two plan defaults were added to `PLAN.md`'s table for loop
tools (no timeout unless written, `model` absent = the caller's model, an unresolvable model
is an error result, nesting deeper than 8 is an error result).

Implementation choices outside the tables, recorded here rather than as decisions:
`on.<event>` slot requests are JSON-RPC notifications; `register.timeout` is milliseconds;
`FsEntry.path` is a server-produced label (D-31); `--continue=<name-or-id>` uses an equals
sign so a bare `--continue "prompt"` stays a prompt; the version-refusal acceptance uses a
Python fake server instead of the plan's test feature flag; `pirs proxy` auto-starts the
server on its side (the design says every client auto-starts a missing server, and over
SSH the proxy is the client's stand-in there).

## Acceptance scripts

`scripts/acceptance/phase-N.sh`, run from the repo root with `target/release/pirs` built.
Each starts fresh servers on temporary sockets with a temporary `HOME`/`PIRS_HOME`,
`XDG_RUNTIME_DIR` and project directory, drives the real binary with the faux provider
(`--model faux/scripted`, `PIRS_FAUX_SCRIPT=<json>` in the **server's** environment, cursor
per server process), prints one `PASS`/`FAIL` line per check and `phase-N: OK`, and needs no
network or credentials. `scripts/acceptance/lib/raw.py` is a raw protocol client for
scripts; `scripts/acceptance/README.md` lists every script.

```bash
cargo build --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check bans
for n in 0 1 2 3 4 5 6 7; do scripts/acceptance/phase-$n.sh; done
```

## Where things live

- `~/.pirs/` (`PIRS_HOME` overrides): `pirs.sock` (or `$XDG_RUNTIME_DIR/pirs.sock`),
  `pirs.sock.pid`, `sessions/<encoded cwd>/*.jsonl` (+ `refs/`), `ext/*.pirs.toml`,
  `tui.toml`, `servers.toml`, `models.json`, `auth.json`. `settings.json` is no longer read.
- `<project>/.pirs/ext/*.pirs.toml`: project policy. `pirs check` prints the merged result.
- `docs/dsl.md` is the model-facing policy instruction set (also embedded in the binary for
  `pirs ext new`); `docs/protocol.md` + `docs/protocol.schema.json` the wire;
  `docs/session-format.md` the log; `docs/containment.md` the jailed setup;
  `examples/policy/` six working examples; `crates/pirs-tui/README.md` the headless protocol.

## Things that bite

- **Never query the terminal cursor in the TUI** while crossterm's `EventStream` exists; it
  blocks two seconds and corrupts the screen. `crates/pirs-tui/src/terminal.rs` carries the
  rule and a `TrackedBackend`; ratatui has `scrolling-regions`.
- **OAuth**: `crates/pi-ai/src/oauth.rs`. Sources in order: pirs `auth.json`, Claude Code's
  credentials file, the keychain; expired file tokens are refreshed and written back;
  keychain tokens are never refreshed. A stale `~/.claude/.credentials.json` on this machine
  is skipped. Live Anthropic requests were last verified on 2026-09-18, before this branch;
  the provider code moved unchanged but the new server has not been run against a live
  provider. Do that first.
- **`PIRS_LOG=debug`** turns on `tracing` to stderr (the old `PIRS_TRACE` is gone).
- **The faux script cursor is per server process.** A scenario that needs a particular
  reply order starts its own server; parent and child loops in one server consume one
  ordered script.
- **Only `write`/`edit` tool results trigger the automatic policy reload** (D-33); a `bash`
  redirect into `.pirs/ext/` needs `loop.reload` or the next `loop.create`.
- **The executable rule**: a `run` that is one token naming an existing executable file is
  spawned directly (JSON in, one JSON line out, no `$name`, no `PIRS_ARG_*`); anything with
  whitespace, or a `~/…` token, is `sh -c` (text out). Built-in tools truncate at 50 KB, so
  only handler tools produce by-reference results (64 KB).
- **Process lifecycle**: every process spawned for a loop is in a group; `loop.close` waits
  1 s, sends TERM to the groups, then KILL after 1 s more; an aborted call kills its group
  on drop; `ETXTBSY` right after writing a script is retried.
- **Reconnect**: per-loop subscriptions re-subscribe first with `since`, `*` last;
  `subscribe "*"` refuses `since`, so a `*` subscriber cannot catch up on its own (the TUI
  re-lists loops instead). The server fans out once per matching subscription, so a
  connection on both `*` and a loop sees that loop's status twice; `SeqTracker` dedups.
- **Registration precedence**: `register tool.<name>` is refused for a name the policy
  defines with `run`/`loop` or for an undeclared built-in; a declaration naming a built-in is
  a conflict (use `wrap`/`disabled`).
- **Child loops** (`[[tool]] loop`): a child loads the same policy and sees the same tool;
  depth is capped at 8 as an implementation limit; idle children accumulate until the parent
  closes; the child's outcome is read from the status `detail` string.
- `pirs ext new`/`regen` refuse a remote server (policy lives with the server, D-26; run
  them there); the generation loop runs under the project's own policy, so an `[[input]]`
  entry can consume its prompt (the error message says so).
- `cargo fmt --check` has never been clean on this repo; `pirs-protocol` and `pirs-tui`
  are, the older crates are not.
- **Environment hooks**: `PIRS_SERVER_COMMAND` (what a client runs to auto-start a server),
  `PIRS_TUI_TMUX` (a stand-in for `tmux` in tests), `PIRS_DOCS_DIR` (where the docs the
  system prompt points at live; the server embeds `docs/dsl.md` and `docs/protocol.md` and
  writes them under `<PIRS_HOME>/docs/` when no docs directory is found), `PIRS_FAUX_SCRIPT`
  (server side), `PIRS_LOG`, `PIRS_SOCKET`, `PIRS_HOME`.

## Where to look first when real use breaks

Three places the final review ranked highest, none exercised beyond the faux provider and
in-process fakes:

1. **Live provider**: `crates/pirs-server/src/agent_loop.rs` (`resolve_startup_model`,
   `get_api_key`, `stream_options`) and `crates/pi-ai/src/oauth.rs`. A credential error
   becomes a `ui.notify` and an empty key.
2. **Real terminal and editor**: `crates/pirs-tui/src/terminal.rs` and the editor pane in
   `crates/pirs-tui/src/process.rs` (S14 is tested through a fake `tmux` only; the quoting
   of a remote path passes through ssh's shell).
3. **Real ssh**: `crates/pirs-client/src/{reconnect,spawn}.rs`; bridges were tested only
   with `pirs proxy --socket` on one machine, never with latency or a half-open link. S20's
   container was documented, never built.

## Known gaps and candidate decisions (not taken)

- `Id` has no null variant, so a JSON-RPC parse-error reply (`id: null`) cannot be typed.
- `LoopWaitResult.state` is always `idle`; it cannot say "closed".
- `Manifest.tools` has no active flag; `loop.attach` is the only way to get a manifest;
  `loop.status { detail: "created" }` carries no cwd or name, so UIs re-run `loop.list`.
- `DslCheckResult` has no error list; parse errors ride as one-file conflicts.
- The TUI: no per-result expand cursor, no input-line cursor movement or history, block
  render hooks only on complete messages, `[[render]]` commands run in the TUI's cwd.
- `pirs-tui`'s test fake broadcasts each event once per connection, not once per matching
  subscription like the real server.
- Print mode and `pirs ext` can leak a loop if the connection fails between `loop.create`
  and the first subscribe/prompt (a `?` before the Ctrl-C select); same fix for both.
- The plan's phase briefs cited scenario numbers one off from `10-functionality.md` for
  phases 3, 5 and 6; the commits and `PLAN.md` now use the functionality file's numbers.
- `dsl.check { cwd }` runs `[[prompt]] run` entries from whatever policy sits in a
  client-named directory (the same trust as `loop.create` running `on start`).

## Suggested next work

1. Run the server against a live provider (`pirs "say hi" --model anthropic/…`) and fix
   what the faux provider could not show.
2. Decide the six proposed entries above; then let the layer files say what the code does.
3. An Elixir conformance server, as an experiment the user asked to record (2026-09-21):
   the loop server is replaceable by construction (JSON-lines protocol with a schema
   snapshot; `pirs-tui` and `pirs-client` depend on nothing server-side; the acceptance
   scripts drive the binary through the socket). Build a phase 1 server in Elixir/OTP,
   where connections, loops, timeouts, observers and process lifecycle are native, judged
   by `scripts/acceptance/phase-1.sh` passing unchanged with `pirs serve` exec'ing the
   release. A full replacement would also port providers, streaming and OAuth (`pi-ai`),
   the agent loop (`pi-agent`), the seven tools and the session log, about 7k lines against
   a 2k-line hub. Extensions stay separate processes in any language (D-22), never code
   loaded into the BEAM.
4. Phase 8 if still wanted: a pty server and a `terminal` page (S22); the web UI (S23),
   which makes it non-optional.
5. Remote policy sync (D-26): copying `~/.pirs/ext` to a remote server.
6. Windows servers (D-31): the shell for `run` strings and the local transport.
7. Smaller: `Id::Null`; a typed run outcome instead of the `detail` string; `active` in the
   manifest; `since` for `*` subscriptions; compaction, `/tree` and `/fork` in the new TUI
   (the session manager still supports them).
