# pirs as adaptable software — design proposal (v2)

Status: draft for discussion. Nothing here is implemented. Supersedes v1, which assumed a
fixed TUI inside the same process; this version separates a headless server from clients.

## Goal

pirs stops being a pi port and becomes an experiment in moldable software: a small stable
core, one formal interface, and everything else outside it — policy as declarations, code as
separate executables, UI as separate clients. pi extension compatibility is dropped.

Two bets:

1. Most of what people want from an agent tool is *policy* (what to block, what to add to the
   prompt, what to run after a turn). Policy is better expressed as a short declaration than as
   a program.
2. The right boundary is not "an agent tool inside a terminal multiplexer" but "one or more
   agent loops behind a protocol, with a UI that is just another client". Owning the loop is
   what makes status trustworthy and UIs replaceable; multiplexers that don't own the loop
   (herdr, tmux + detectors) have to screen-scrape to get what we emit for free.

## Principles

1. **The core is what would hurt to change.** Only the parts whose interface we promise.
2. **One protocol.** Extensions, UIs, print mode, and other loops all talk to the server the
   same way. There is no in-process plugin API.
3. **Policy is declarative.** A small DSL covers ~80% of extensions without code.
4. **Intent travels with the artifact.** Every DSL file carries its own prose intent and can be
   regenerated from it.
5. **Composition is a set operation** with fixed conflict rules, checkable before running.
6. **The escape hatch is a process.** Anything the DSL cannot say is an executable in any
   language, connected as a client.
7. **Status is a fact, not a guess.** `working`, `blocked`, `idle` come from loop state.

## Architecture

```
                 ┌───────────────────────────────────────────────┐
                 │ pirs server (headless, one per user)          │
                 │                                               │
                 │   loop A ── session, cwd, model, tools, DSL   │
                 │   loop B ── session, cwd, model, tools, DSL   │
                 │   pty  C ── (later) shell, screen state       │
                 │                                               │
                 │   emits   events, status, dialog requests     │
                 │   accepts prompt, reply, control, register    │
                 └──────────────┬────────────────────────────────┘
                                │ JSON-RPC 2.0 over unix socket
                                │ (remote: ssh <host> pirs proxy)
       ┌────────────┬───────────┼────────────┬─────────────────┐
       │            │           │            │                 │
    pirs tui   pirs (print)  extension    another loop     someone else's
  (layout cfg)  one-shot     executable   (subagent)        client
```

### Server

Owns loops. Each loop has a session log, cwd, model, active tool set, and the DSL files that
apply to it. The server has no terminal; it never renders anything. It survives client
disconnects; loops keep running.

Lifecycle: `pirs serve` starts it explicitly on `$XDG_RUNTIME_DIR/pirs.sock` (or
`~/.pirs/pirs.sock`). Every client auto-starts it if the socket is missing. It exits on
`pirs stop` or when no loop is running and no client has been attached for a configurable
idle time. Explicit start plus auto-start covers both "I want to reason about it" and "just
work".

### Clients

Everything else. A client connects to one or more servers, optionally subscribes to a loop's
events, and issues requests. Four kinds ship; anyone can write a fifth.

| Client | What it is |
|---|---|
| `pirs` | print mode: create/attach a loop, send a prompt, stream to stdout, exit |
| `pirs tui` | the interactive UI; layout from config; can show N loops across N servers |
| extension executables | spawned by the server from a DSL `run =`, connected as a client that registers handlers |
| a loop | a `tool` that prompts another loop and waits for it to go idle |

### Several servers, local and remote

A client may hold connections to several servers at once. A loop is identified by
`(server, loop)`; the TUI's loop list is the union across servers and a pane binds to
`office:3` as easily as `local:1`.

Remote transport is `ssh <host> pirs proxy`: a stdio bridge to the remote unix socket, same
frames, same messages. SSH provides authentication and encryption; pirs never listens on TCP.
Nothing new to audit in a corporate environment beyond an SSH login the user already has.
Servers are named in `~/.pirs/servers.toml` (`name`, `ssh` target, optional socket path).

Everything belonging to a loop — cwd, tools, extensions, DSL files, session log — lives on
the server that runs it. A loop on the build box edits the build box's files and runs the
build box's extensions. The client only renders and steers.

Consequences for the protocol (all in the tables below): `hello` carries versions so two
machines on different pirs releases can negotiate or refuse cleanly; events carry a per-loop
`seq` and `subscribe` accepts `since`, so a client that lost its SSH link can catch up on the
dialog that opened while it was gone; large payloads by reference need `blob.get` because a
path on the server is meaningless to a remote client.

Cross-server orchestration (a loop on one server driving a loop on another) is a client
concern in v0. Making the server itself a client of other servers is possible with the same
protocol but deferred.

## The protocol

JSON-RPC 2.0 over a unix socket. Chosen because request ids are exactly what dialogs and
guards need, and the format is boring. All messages are documented in `docs/protocol.md`
with their JSON schema; that file is the formal interface.

### Why JSON, and how to not regret it

Nothing on this wire is fast enough for the encoding to matter: a model round trip is
200 ms–30 s, spawning an extension is ~1 ms, a socket round trip is ~20 µs, and serde_json on
a 4 KB message is ~5–10 µs. The busiest stream (assistant deltas, ~100/s) costs well under a
millisecond per second. What JSON buys is the thing the design depends on: any process, any
language, no toolchain. `echo '{...}' | ./tool.py` tests an extension; `socat - UNIX:...`
debugs the protocol; a model writing a shell-script tool gets JSON right.

Two hedges so this never has to change:

- **Framing is length-prefixed, not newline-delimited**: `u32 length, u8 encoding, payload`.
  Encoding `0` is JSON and the only one in v0. A client may request another encoding at
  `hello` (MessagePack or CBOR are JSON-isomorphic, so every schema in `docs/protocol.md`
  stays the truth); raw-bytes frames are reserved for pty screen data if phase 7 needs them.
- **Large payloads travel by reference.** A tool result or attachment above a threshold
  (proposed 64 KB) is written to the session directory and the message carries
  `{ "ref": path, "bytes": n }` instead of the content. Images already work this way. This
  removes the bulk regardless of encoding.

### Two roles, kept distinct

- **Observers** `subscribe` to events. The server never waits for them. A slow TUI cannot
  stall a loop.
- **Handlers** `register` for a slot on a loop with a timeout. The server sends the event as a
  *request* and waits for the reply (or the timeout, which counts as "no opinion").

### Events (server → observers, notifications)

Every event carries `loop` and a monotonic per-loop `seq`.

| Event | Payload |
|---|---|
| `loop.status` | `{ loop, state: working \| blocked \| idle, since, detail }` |
| `loop.message` | `{ loop, role, delta \| message }` (streaming assistant text, tool calls, results) |
| `loop.turn_end` / `loop.run_end` | `{ loop, messages }` |
| `ui.status` | `{ loop, key, text }` |
| `ui.widget` | `{ loop, key, lines }` |
| `ui.notify` | `{ loop, level, text }` |
| `ui.dialog` | `{ loop, id, kind: confirm \| select \| input, title, text, options }` |
| `ui.dialog_closed` | `{ loop, id }` |

`blocked` means exactly "a `ui.dialog` is open and unanswered". Nothing else.

### Handler slots (server → handlers, requests with reply)

| Slot | Payload | Reply |
|---|---|---|
| `input` | `{ text }` | `{ text }` or `{ handled: true }` |
| `prompt` | `{ system_prompt }` | `{ append }` or `{ replace }` |
| `tool_call` | `{ tool, args, id }` | `{ block, reason }` or `{ args }` |
| `tool_result` | `{ tool, args, result }` | `{ result }` |
| `tool.<name>` | `{ args, id }` | `{ content, details }` or `{ error }` |
| `command.<name>` | `{ args }` | — |
| `on.<event>` | event payload | — (fire and forget, but sequenced) |

Eight slots. `message_*`, `context`, provider hooks, compaction, tree, fork: gone until a
real need appears, and then only with a slot here and a DSL entry below.

### Requests (client → server)

| Request | Meaning |
|---|---|
| `hello { client, protocol_version }` → `{ server, protocol_version, encodings }` | first message on every connection; the server refuses incompatible majors |
| `loop.create { cwd, model, session? }` / `loop.list` / `loop.attach` / `loop.close` | lifecycle |
| `loop.prompt { loop, text, when }` | `when`: `now` (steer), `after_turn`, `next_input` |
| `loop.abort { loop }` | Esc |
| `loop.wait { loop, until: idle \| blocked }` | blocks until the state is reached — what subagents and multiplexers need |
| `dialog.reply { loop, id, value }` | answer a dialog; first reply wins |
| `subscribe { loop \| "*", events, since? }` / `unsubscribe` | observer; `since` replays events after that `seq` (bounded by what the server retains, proposed: the current run) |
| `register { loop, slot, timeout }` / `unregister` | handler |
| `ui.status` / `ui.widget` / `ui.notify` | an extension asking the server to emit |
| `state.set { loop, key, value }` / `state.get` | persisted in the session log, not sent to the model |
| `blob.get { ref }` | fetch a by-reference payload; the only way a remote client reads one |
| `loop.tools { loop, names }` / `loop.model { loop, spec }` | control |

Anything not in these three tables does not exist.

## The DSL (server-side policy)

A declarative file, `*.pirs.toml`, from `~/.pirs/ext/` (global) and `.pirs/ext/` (project),
loaded per loop from its cwd. TOML for now: free parser, comments, models write it reliably.
The vocabulary is the hard part; the spelling can change once the vocabulary settles.

Every file starts with its intent:

```toml
intent = """
Ask before any rm -rf. Never let the model edit .git or node_modules.
Show the current git branch in the status line and refresh it after every turn.
"""
```

### Slots

```toml
[[guard]]                              # tool_call -> block / confirm / rewrite / run
tool = "bash"
match.command = '\brm\s+-rf\b'
action = "confirm"
message = "Destructive command"

[[guard]]
tool = ["edit", "write"]
match.path = '^(\.git|node_modules)/'
action = "block"
reason = "protected path"

[[guard]]
tool = "bash"
match.command = '^git push'
action = "rewrite"
set.command = "$command --dry-run"

[[input]]                              # transform or consume user input
match = '^\?(.*)'
replace = "Explain briefly: $1"

[[input]]
match = '^!(.*)'
handled = true
run = "$1"

[[prompt]]                             # add to the system prompt
text = "Prefer small commits. Never rewrite history."

[[prompt]]
files = ".claude/rules/*.md"

[[prompt]]
run = "git log --oneline -5"
header = "Recent commits:"

[[status]]                             # emit ui.status
key = "branch"
run = "git branch --show-current"
on = ["start", "turn_end"]

[[widget]]                             # emit ui.widget
key = "todo"
file = ".pirs/todo.md"
on = ["start", "tool_result"]

[[on]]                                 # run something at an event
event = "turn_end"
run = "git add -A && git commit -qm 'pirs checkpoint'"
quiet = true

[[command]]                            # /name
name = "handoff"
description = "Start a fresh session with a summary"
run = "./scripts/handoff.sh $args"

[[tool]]                               # give the model an executable
name = "fetch"
description = "Fetch a URL and return its text"
params.url = { type = "string", description = "Absolute http(s) URL" }
params.max_length = { type = "integer", default = 50000 }
run = "./tools/fetch.py"
timeout = 30

[[tool]]
name = "review"
description = "Ask a second loop to review the diff"
loop = { model = "claude-opus", prompt = "Review this diff critically:\n$diff", wait = "idle" }

[[tool]]
name = "bash"
disabled = true                        # or wrap = "./tools/sandboxed-bash.sh"
```

Nine slots: `intent`, `guard`, `input`, `prompt`, `status`, `widget`, `on`, `command`,
`tool`. Every slot accepts `run = "..."` as the way to say "I need code for this".

### `run` semantics

- A shell string (`sh -c`, in the loop's cwd): stdout is the result as text. For `guard`
  the exit code decides (0 allow, non-zero block with stderr as reason).
- A path to an executable: the server spawns it, connects it to the socket with
  `PIRS_SOCKET`, `PIRS_LOOP`, `PIRS_SLOT` in the environment, and sends the slot payload as
  a JSON-RPC request on stdin; the reply comes back on stdout. On stdin/stdout the frames
  are plain newline-delimited JSON, not the length-prefixed socket framing, so a one-shot
  executable never has to know the socket exists. If it does open the socket, it can call
  `dialog`, `state.*`, `ui.*`, `loop.*` like any client.
- `persistent = true` on a slot keeps the process alive and streams events to it; for
  file watchers and stateful tools.

Testable from a shell: `echo '{"url":"https://x"}' | ./tools/fetch.py`.

### Coverage

Against pi's ~70 example extensions: the entire "trivial" tier and most of "moderate" —
about 50 — need no executable. `todo` needs one (state), `fetch` needs one (real work),
`subagent` becomes a `[[tool]]` with `loop = ...`. Custom providers with their own streaming,
custom renderers, editors, overlays: not supported, by design.

## Composition

Loading a loop's DSL is: parse every applicable file, union the slots, check.

| Check | Outcome |
|---|---|
| duplicate `tool` name | error unless one is `wrap`/`disabled` of the other |
| duplicate `status`/`widget` key, duplicate `command` name | error |
| several `guard`s match one call | all evaluated; `block` > `confirm` > `rewrite`; rewrites apply in file order |
| several `input`s | file order; first `handled` stops |
| several `prompt`s | concatenated in file order |
| `on` entries | all run, file order |

File order is path-sorted, global before project; `priority = N` moves a file earlier.
`pirs check [--cwd]` prints the merged result and every conflict without starting a loop.

## Intent, regeneration, sharing

- The shareable unit is the `intent` string. The file is a cache of it.
- `pirs ext new "<intent>"` asks the current model to write a `.pirs.toml` from the intent and
  `docs/dsl.md`, runs `pirs check`, shows the result. `pirs ext regen <file>` repeats it from
  the file's own intent.
- `docs/dsl.md` replaces `docs/extensions.md` and is written as the instruction set for that
  model call: slots, fields, checks, one example per slot, nothing else.

## The TUI (a client)

`pirs tui` is the reference client. It is configurable, not fixed, but it is *one* client;
someone who wants a different UI writes a different client against `docs/protocol.md`.

- Layout config in `~/.pirs/tui.toml`: panes (each bound to a `server:loop`, or to the merged
  loop list), a status line format string over `ui.status` keys and `loop.status`, key
  bindings, where dialogs appear.
- Multiplexing is a layout with N loop panes, on any mix of servers. There is no separate
  multiplexer program.
- `blocked` loops are visibly flagged because the server said so; no heuristics.
- Later: a `pty` pane kind. The server would own the pty (so it survives detach) and stream
  screen updates as events; the TUI draws it. No agent detection, ever — if it's a pirs loop
  we know its state, if it's a shell we don't pretend to.

## What is core (and size)

| Component | Lines | Fate |
|---|---|---|
| `pi-ai` providers, streaming | 2.6k | core |
| `pi-agent` loop | 1.3k | core |
| session log | 2.1k | core |
| built-in tools | 3.0k | core, registered through the manifest, overridable |
| settings, prompt assembly | 0.4k | core |
| terminal driver, interactive mode | 1.0k | moves to `pirs-tui` |
| `pi-ext` (QuickJS, TS strip, runtime.js) | 3.0k | deleted |
| server, protocol, DSL loader/checker, process runner | ~1.5k new | core |

Crates: `pi-ai`, `pi-agent`, `pirs-server` (session, tools, DSL, protocol), `pirs-tui`,
`pirs` (CLI: print mode, `serve`, `check`, `ext`, all thin clients). `pi-ext` removed.

Built-in tools stay compiled in for latency and fidelity but are ordinary manifest entries so
a DSL file can disable, replace, or wrap them. Small *surface*, not small binary.

## Phases

0. **Design freeze.** `docs/protocol.md` and `docs/dsl.md` written and reviewed.
1. **Server + print client.** Loop over the socket; `pirs "prompt"` works end to end; TUI
   still the old in-process one. pi-ext still present.
2. **DSL, shell `run` only.** `guard`/`input`/`prompt`/`status`/`widget`/`on`; `pirs check`.
3. **TUI as client.** Single loop pane; dialogs over the protocol; `blocked` flag.
4. **Executables.** `tool`/`command` with executable `run`; one-shot then persistent; port
   `fetch`. Delete `pi-ext`, `examples/extensions/`, pi-compat docs.
5. **Multi-loop.** `loop.wait`, `[[tool]] loop = ...`, TUI layouts with N panes.
6. **Remote servers.** `pirs proxy`, `servers.toml`, `since` replay, `blob.get`; TUI panes
   across servers.
7. **Intent tooling.** `pirs ext new` / `regen`.
8. **pty panes**, if still wanted.

Each phase leaves a working binary.

## Open questions

1. **Which DSL files apply to a loop?** Proposed: global + the loop's cwd project files, read
   at `loop.create` and on `loop.reload`. Not re-read per turn.
2. **Who answers a dialog when three clients are attached?** Proposed: any; first reply
   wins; the rest get `ui.dialog_closed`.
3. **Handler timeout default.** Proposed 5 s for `guard`/`input`/`prompt`, tool-specific for
   `tool.*`, and a timed-out guard counts as *allow* with a warning — a dead extension should
   not brick the loop. Arguable; the opposite default is safer and more annoying.
4. **Do built-in tools ever become executables?** Proposed no. Revisit if someone actually
   wants to replace `edit`.
5. **Auth on the socket.** Filesystem permissions on the socket path locally; SSH remotely.
   No TCP listener, ever, unless someone makes a case that SSH can't cover.
6. **`settings.json`**: fold into `.pirs.toml` as a `[settings]` table so there is one loader
   and one checker. Proposed yes.
7. **Event retention for `since`.** Replaying the current run is cheap and covers the
   dropped-SSH case. Replaying across runs means retaining the event stream, which the
   session log almost is; decide whether the log *becomes* the event stream.
