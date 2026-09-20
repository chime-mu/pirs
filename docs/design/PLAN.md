# Execution plan

Written 2026-09-21 for a fresh session to execute end to end. The orchestrator is a Fable
session. It reads this file, `HANDOFF.md`, and the six design files, then runs phases 0–7
in order without stopping for approval. Phase 8 (pty server) and a web UI are out of scope.

## Authority

1. The design files in `docs/design/` are the specification. `90-decisions.md` is the
   record of why. This plan is the schedule and the division of labour; where it conflicts
   with a design file, the design file wins and the conflict is a bug in this plan.
2. If implementation shows a design statement to be wrong or incomplete, the orchestrator
   appends a **proposed** entry to `90-decisions.md` (next free number, layer group, one
   paragraph, the evidence), applies its own best judgement so work continues, and lists
   the entry in `HANDOFF.md` under "Decisions taken without the user". It does not edit
   the design prose for a proposed entry. It never removes a protocol message, DSL slot or
   scenario on its own.
3. Every phase leaves a working binary and green tests. A phase that cannot be completed
   is left in a state where the previous phase's acceptance still passes, and the failure
   is recorded in `HANDOFF.md`. The run then continues with the next phase only if it does
   not depend on the failed one; otherwise it stops and writes the handoff.

## Division of labour

| Role | Model | Used for |
|---|---|---|
| orchestrator | Fable (this session) | reads the design, writes subagent briefs, runs verification, commits, keeps `HANDOFF.md` current |
| implementer | Opus subagent | everything not listed as advanced: moving code, CLI plumbing, DSL loader and checker, examples, docs, tests |
| advanced implementer | Fable subagent | the pieces marked **Fable** in the phase briefs: the protocol crate, the loop server's ownership and dispatch, handler timeouts, the session log as event stream, the TUI as a client |
| reviewer | Fable subagent | one review per phase, before the phase commit, against the brief and the review checklist |

Rules for every subagent brief the orchestrator writes:

- Name the phase, the scenarios it enables, and the exact files to read first: the relevant
  design files, this plan's section for the phase, and the code being changed.
- State what may not change: protocol messages, DSL slots and crate edges are fixed by the
  design; a subagent that needs one changed stops and reports instead of improvising.
- Require tests for every new behaviour and a green `cargo test --workspace` before it
  reports done. No stubs, no `todo!()`, no `#[ignore]` without a reason in the brief.
- Ask for a short written report: what was built, what was not, what it is unsure of.
- Run independent subagents in parallel only when they touch different crates.

The orchestrator does not implement in the main session except for fixups after review.
It runs verification itself and does not take a subagent's word for it.

## Verification, every phase

```bash
cargo build --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check bans            # from phase 1
scripts/acceptance/phase-N.sh    # the phase's scenario script, added by the phase
```

Acceptance scripts drive the real binary with the faux provider
(`--model faux/scripted`, `PIRS_FAUX_SCRIPT=<json>`, see `crates/pi-ai/src/faux.rs`) on a
temporary socket (`PIRS_SOCKET`), a temporary home (`HOME`, `XDG_RUNTIME_DIR`) and a
temporary project directory, and assert on stdout and on files. They need no network and no
credentials. Each script is the phase's acceptance test and is what the user runs to check
a phase by hand.

## Git

Work on branch `adaptable`, created from `main` at the start. One commit per phase after
review, message `Phase N: <title>`, body listing scenarios enabled and any proposed
decisions, ending with the attribution line the session's instructions require. Fixups
after review are squashed into the phase commit. Nothing is pushed.

## Defaults this plan fixes

These are implementation choices the design leaves open. They are not decisions about the
design; changing one later needs no log entry.

| Choice | Value |
|---|---|
| socket path | `$XDG_RUNTIME_DIR/pirs.sock`, else `~/.pirs/pirs.sock`; `PIRS_SOCKET` overrides |
| server idle exit | 10 minutes with no running loop and no attached client; `pirs serve --idle <secs>` |
| handler timeout | 5 s for `input`, `prompt`, `tool_result`, `on.*`; `timeout` field per `[[tool]]`, default 60 s |
| by-reference threshold | 64 KB |
| session directory | `~/.pirs/sessions/<encoded cwd>/`; JSONL format as in `docs/session-format.md` plus a per-entry `seq`; pi compatibility of the *directory* is dropped with the rest of pi compatibility, the *format* stays |
| loop identity | short random id; optional `name` at create; `--continue` alone picks the most recent conversation in the cwd, `--continue <name-or-id>` a specific one |
| policy file locations | `~/.pirs/ext/*.pirs.toml`, `<cwd>/.pirs/ext/*.pirs.toml` |
| UI config | `~/.pirs/tui.toml` |
| servers file | `~/.pirs/servers.toml` |
| protocol version | `0.1`; `hello` refuses a different major |
| model for `pirs ext new` | the loop's default model |

## Phase briefs

Scenario numbers refer to `10-functionality.md`. Message names refer to `30-protocol.md`,
slots to `40-dsl.md`, crates to `20-architecture.md`.

### Phase 0 — Design freeze in code

**Enables:** nothing user-visible. Makes "anything not in the tables does not exist" a test.

**Build:**

- `crates/pirs-protocol` (**Fable**). Every wire type in `30-protocol.md`: `Hello` and its
  reply, the five handler slot payloads and replies, every event, every request and
  response, the protocol's own message type (D-08) with a documented conversion from
  `pi_ai` types living in the server, not here. `serde` + `schemars` derives only; no
  `tokio`, no `reqwest`. A `Frame` helper that encodes one message as one JSON line and
  decodes lines (D-10). `PROTOCOL_VERSION`. A test that generates the JSON schema for the
  whole protocol and compares it to `docs/protocol.schema.json`, failing with a diff;
  `UPDATE_SNAPSHOT=1 cargo test -p pirs-protocol` rewrites it.
- `crates/pirs-protocol/tests/architecture.rs` (Opus). Reads `cargo metadata` and asserts
  each workspace member's internal dependencies against a table in the test, and that every
  member is in the table. The table starts with today's members (`pi-ai`, `pi-agent`,
  `pi-ext`, `pi-cli`, `pirs-protocol`) and is edited by each later phase. Moves to
  `crates/pirs/tests/` in phase 1.
- `docs/protocol.md` (Opus): commentary on the schema — roles, framing, the three tables
  rendered from the types, examples of one exchange per request. `docs/dsl.md` (Opus):
  the model-facing instruction set from `40-dsl.md` — slots, fields, checks, one example per
  slot, nothing else (D-22's tripwire stated).
- `unreachable_pub` denied in every crate.

**Acceptance:** `cargo test -p pirs-protocol` passes; deleting a field from any type fails
the snapshot test; `scripts/acceptance/phase-0.sh` runs those two checks.

### Phase 1 — Server and print client

**Enables:** S1, S2.

**Build:**

- `crates/pirs-server` (**Fable** for `server.rs`, `loop.rs`, `dispatch.rs`, `log.rs`;
  Opus for moved code). A tokio unix-socket server speaking JSON lines. `hello` with version
  refusal. Loops: `loop.create/list/attach/close`, `loop.prompt` with `when`, `loop.abort`,
  `loop.wait`, `loop.tools`, `loop.model`. Observers: `subscribe`/`unsubscribe` with `since`,
  never waited on; events `loop.status` (`working`/`idle`), `loop.message`,
  `loop.turn_end`, `loop.run_end`, `fs.changed` from the loop's own tool writes. Handlers:
  `register`/`unregister` with timeout; the dispatch path exists now (D-23: server initiates
  only toward registered slots and its own children) even though nothing registers until
  phase 4. The session log is the event stream (D-06): every sequenced event is a log entry
  with `seq`, replay reads the log, deltas are not logged. By-reference payloads above the
  threshold written to the session directory; `fs.list`/`fs.read`, with `ref` paths served
  by `fs.read` (D-11). Idle exit. `pi-cli`'s `session.rs`, `tools/`, `settings.rs`,
  `system_prompt.rs` move here (D-04); `agent_session.rs` is dissolved into the loop.
  Built-in tools registered through a manifest. Lints: `print_stdout`/`print_stderr` denied;
  `disallowed_methods` on `Command::new` with the process-runner module allowed (D-09).
- `crates/pirs-client` (Opus). Connect to a socket, auto-start `pirs serve` detached if
  the socket is missing, `hello`, request/response with ids, an event stream from
  `subscribe`. No `pi-ai`, no `pi-agent` (architecture test).
- `crates/pirs` bin (Opus). `pirs serve [--idle]`, `pirs stop`, `pirs "prompt"` (print mode:
  create or continue, prompt, stream text to stdout, wait for idle, close the loop, exit —
  D-28), `pirs --continue [name-or-id]`, `pirs --list` (conversations in this cwd),
  `pirs tui` runs the *old* in-process interactive mode by calling into `pi-cli`, which
  becomes a library crate for the duration of phases 1–3 (its `[[bin]]` removed). `--model`,
  `--thinking`, `--cwd` as today.
- `deny.toml` (Opus): `cargo-deny` bans with wrappers — `crossterm` and `ratatui` only via
  `pi-cli` (phase 1–2) then `pirs-tui`; `reqwest` only via `pi-ai`; `vt100` and
  `alacritty_terminal` banned outright. Install `cargo-deny` with `cargo install`.
- Architecture test table updated: `pirs-server` → `pi-ai`, `pi-agent`, `pirs-protocol`;
  `pirs-client` → `pirs-protocol`; `pirs` → all; `pi-cli` → `pi-ai`, `pi-agent`, `pi-ext`.

**Acceptance (`phase-1.sh`):** with a scripted faux response, `pirs "hi"` prints it and
exits, and `pirs stop` finds no running loop; two `pirs "x" &` in one directory run as two
agents (`pirs --list` shows two conversations); `pirs --continue` resumes the most recent;
a >64 KB scripted tool result arrives as a `ref` and `fs.read` returns it; a client that
subscribes with `since` after the run replays `loop.turn_end`; the server exits after
`--idle 1`; `pirs tui` still starts the old interactive mode.

### Phase 2 — Policy without code

**Enables:** S3, S4, S5 (declaration and script shapes).

**Build (Opus; Fable reviews):**

- `crates/pirs-server/src/dsl/`: parse `*.pirs.toml` from the two locations; slots `input`,
  `prompt`, `tool_result`, `status`, `widget`, `on`, `command`, `tool` (with shell-string
  `run` only in this phase; executable `run` is phase 4), `intent`, `priority`, and a
  `[settings]` table replacing `settings.json` (open question 6, taken as yes). Desugaring
  per D-17 and D-18. Composition per `40-dsl.md`'s table, with errors that name both files.
  Called processes for shell strings per D-24: JSON line on stdin, `PIRS_ARG_*` env and
  `$name` interpolation, cwd, `PIRS_SESSION_DIR`, `PIRS_LOOP`, `PIRS_SLOT`, `PIRS_SOCKET`.
- `dsl.check { cwd }` and `pirs check [--cwd]`: merged result, conflicts, the fully
  assembled system prompt (D-21).
- `loop.reload`, and automatic reload when one of the loop's own tools writes a file that
  matches a policy location (D-33). `loop.attach` returns the manifest (tools, commands,
  status and widget keys).
- Events and requests `ui.status`, `ui.widget`, `ui.notify`. `on` events: `start`,
  `turn_end`, `run_end`, `tool_result`, `reload`. Handler timeouts (5 s) with "no opinion"
  on expiry and a `ui.notify` warning.
- Print mode shows `ui.notify` on stderr and ignores widgets.
- `docs/dsl.md` is in the model's `<docs>` section (see `system_prompt.rs`); the model can
  write policy files and they take effect on the next turn.

**Acceptance (`phase-2.sh`):** fixtures under `scripts/acceptance/fixtures/phase-2/`: a
prompt file, an input rewrite, a status command, an `on turn_end` that touches a file, a
`tool_result` that rewrites `bash` output, a `[[command]]`, and a duplicate-key conflict.
`pirs check` prints the assembled prompt and the conflict with both file names; a faux run
shows the rewritten input reaching the model (the faux echo mode proves it), the touched
file after the turn, the rewritten tool result in the session log next to the original;
a scripted faux tool call that writes `.pirs/ext/new.pirs.toml` is followed by a turn that
sees its `[[prompt]]` text in the assembled prompt.

### Phase 3 — TUI as a client

**Enables:** S6, S10, S11, S12, S13 (inside tmux), S14.

**Build (**Fable** for the client and page model; Opus for config, tmux and tests):**

- `crates/pirs-tui`, depending on `pirs-protocol` and `pirs-client` only. Move the
  terminal driver and the hard-won parts of `pi-cli/src/modes/interactive.rs`
  (`TrackedBackend`, the cursor-query rule, `scrolling-regions`) and rebuild the rest on the
  protocol: sidebar of agents with state and an attention flag (`idle` from the server plus
  "not viewed since" kept by the UI), agent page (streamed conversation, tool calls and
  results, widgets, status keys, files touched this run as a jump list), file page
  (`fs.read`, refreshed on `fs.changed`, several open at once), start a new agent from the
  UI in any directory, close an agent, prompt with `when`. `~/.pirs/tui.toml`: key
  bindings, status format string over `ui.status` keys and `loop.status`, UI-side commands.
  One merged command list from the attach manifest and the UI's own; a name on both sides
  is shown as a conflict (D-32). The rendering extension point (S6, D-37): a `[[render]]`
  table in `tui.toml` mapping a tool name or fenced-block tag to a command that receives
  the JSON and returns lines to draw and, optionally, a list of options the TUI presents as
  a native picker whose choice is sent as the next prompt.
- Editor in a pane (D-29): from a file page, a key runs `tmux split-window "$EDITOR
  <path>"` when `$TMUX` is set, with a server-side bridge prefix when the agent's server is
  remote (phase 6 fills that in; phase 3 supports local only and says so if not in tmux).
- `pirs tui` now runs the new client. `pi-cli` is deleted (D-04). Architecture test and
  `deny.toml` updated: `crossterm`/`ratatui` only via `pirs-tui`.
- `pi-ext` remains, unused by the new TUI; it is deleted in phase 4.

**Acceptance (`phase-3.sh`, plus `ratatui` `TestBackend` unit tests):** with a headless
harness driving the TUI's event loop: two agents appear in the sidebar with states; the
flag appears when an agent goes idle and clears when its page is viewed; a scripted tool
write updates an open file page; a `[[render]]` command for a scripted `ask` tool call
produces a picker and the chosen option is sent as the next prompt; the old interactive
mode is gone (`pi-cli` absent from `cargo metadata`).

### Phase 4 — Executables

**Enables:** S5 (process shape), S7, S8, S9.

**Build (**Fable** for handler dispatch over the socket and process lifecycle; Opus for
the rest):**

- Called executables: `run = <path>` with JSON line on stdin and reply on stdout; `[[tool]]`
  with `params` schema, `timeout`, `disabled`, `wrap`; errors with stderr as message.
- Connected clients as handlers: `register { loop, slot, timeout }` for `input`, `prompt`,
  `tool_result`, `tool.<name>`, `on.<event>`; the server sends requests and waits per
  registration; timeout is "no opinion" plus a warning; `unregister`; disconnection
  unregisters.
- Process lifecycle (D-23): every process spawned for a loop is tracked and killed on
  `loop.close`; `on start` handlers are not waited for, so a started process may connect and
  register (D-16). `reload` re-runs `on start` for entries that are new since the last load.
- Examples under `examples/policy/`: `fetch` (Python, called), `todo` (a widget from a file
  plus an `on tool_result` refresh), `watch` (a connected process registering `on.turn_end`),
  `ask` (the structured-question tool from D-37, with a `[[render]]` snippet for the TUI),
  `git-checkpoint`, `input-shortcuts`. Each with an `intent`, tested by the acceptance
  script.
- Delete `crates/pi-ext`, `examples/extensions/`, `docs/extensions.md`,
  `docs/pi-extensions-reference.md`; remove the `<docs>` reference to `extensions.md`;
  update the architecture table and `deny.toml`.

**Acceptance (`phase-4.sh`):** a scripted faux tool call to `fetch` runs the Python script
against a file URL and the result reaches the model; a connected `watch` process receives
`on.turn_end` and is dead after `loop.close`; a registered `input` handler that sleeps past
its timeout is skipped with a warning and the turn completes; `ask`'s call is visible as
JSON in the session log; `pi-ext` is absent from `cargo metadata`.

### Phase 5 — Several agents

**Enables:** S15, S16.

**Build (Opus):** `[[tool]] loop = { model, prompt, wait = "idle" }`: the server creates a
child loop in the same cwd, prompts it with the interpolated prompt, waits for idle, returns
its final message as the tool result; the child appears in `loop.list` with a `parent`
field and in the sidebar under its parent. `pirs wait <loop>` for scripts, on `loop.wait`.

**Acceptance (`phase-5.sh`):** a scripted parent calls `review`; a second scripted faux
sequence answers as the child; the parent's next message contains the child's answer; the
session log of the parent records the child's id; `pirs wait` returns when the child is
idle.

### Phase 6 — Remote and contained servers

**Enables:** S17, S18, S19, S20, S21.

**Build (Opus; Fable reviews the reconnect path):**

- `pirs proxy [--socket <path>]`: a bridge forwarding JSON lines between stdio and a socket.
- `~/.pirs/servers.toml`: `[[server]] name = "..." command = "..."` (D-05); no command means
  the local socket. `pirs-client` holds several connections; every loop is `(server, loop)`.
- TUI: sidebar spans servers; agent and file pages on any server; the editor pane uses the
  server's bridge command as a prefix (`ssh build $EDITOR <path>` when the command is
  `ssh build pirs proxy`; a `[[server]] editor_prefix` overrides).
- Reconnect: on a dropped bridge the client reconnects, `hello`, `subscribe ... since` with
  the last `seq` it saw, and the UI catches up (D-06). Version refusal shown as a notice.
- Paths opaque (D-31): the protocol path type is a string newtype with no filesystem
  operations; `pirs-tui` and `pirs-client` contain no `std::path` joins on protocol paths
  (a clippy `disallowed_types` or a grep in the acceptance script).
- `docs/containment.md` and a `Dockerfile` with `pirs serve` as entrypoint, documented
  `servers.toml` entry `command = "docker exec -i <name> pirs proxy"`, and the egress note
  from `20-architecture.md`.

**Acceptance (`phase-6.sh`):** two servers on two temporary sockets, one reached through
`command = "target/release/pirs proxy --socket <tmp2>"`; `pirs --server two "hi"` runs
there; the TUI harness lists loops from both; killing and restarting the bridge replays
the missed `loop.turn_end`; a server built with a different `PROTOCOL_VERSION` major (test
feature flag) is refused with a readable message. The Docker path is documented, not run.

### Phase 7 — Intent tooling

**Enables:** S10 fully.

**Build (Opus):** `pirs ext new "<intent>"` starts a loop whose prompt is `docs/dsl.md` plus
the intent, asks for a single file, writes it under `.pirs/ext/`, runs `dsl.check`, prints
the result; `pirs ext regen <file>` does the same from the file's `intent`. Both work
against the faux provider with a scripted file body.

**Acceptance (`phase-7.sh`):** with a scripted faux body, `pirs ext new` writes a file whose
`intent` is the argument and whose `pirs check` is clean; `regen` on it reproduces it.

### Final tidy (Opus, then Fable review of the whole branch)

- `README.md` rewritten for the new pirs; `STATUS.md` replaced by the phase table with
  what passed; `docs/index.md` updated; `docs/session-format.md` updated for `seq` and the
  directory; `HANDOFF.md` per the section below.
- A last full run of every acceptance script from a clean checkout of the branch.

## Review checklist (Fable reviewer, every phase)

1. Every scenario the brief names is walked through in the acceptance script, not only in
   unit tests.
2. No protocol message, DSL slot or crate edge exists that the design does not name; the
   snapshot and architecture tests are green and were not weakened.
3. Nothing checks what the model may do (D-19). Any `confirm`-like behaviour is a script
   returning a result, never a gate.
4. The server never renders (lints), never listens on the network, never interprets a
   path from a client.
5. Injected context is visible: `pirs check` output and the session log show every rewrite.
6. Removed code is removed, not left behind a feature flag.
7. The brief's "what was not built" list is empty or each item is in `HANDOFF.md`.

## HANDOFF.md at the end of the run

Rewrite it to hold: which phases passed and the commit of each; every proposed decision
taken without the user, with its number; every acceptance script and how to run it; the
things that bite (carry forward the still-true ones from today's file: the cursor-query
rule, OAuth sources, `PIRS_TRACE`); and the suggested next work: phase 8 if wanted, the web
UI, remote policy sync (D-26), Windows servers (D-31).
