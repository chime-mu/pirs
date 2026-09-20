# The protocol

Status: draft. Prose is the v2 proposal as of 2026-09-20; proposed changes to it are in
`90-decisions.md` and are not applied until accepted. Once phase 0 lands, the tables here
are generated from `pirs-protocol` and this file becomes commentary on them.

JSON-RPC 2.0 over a unix socket. Chosen because request ids are exactly what handlers
need, and the format is boring. All messages are documented in `docs/protocol.md`;
the accompanying `docs/protocol.schema.json` is generated from the `pirs-protocol` crate and
checked by a test (see "Mechanical checks" in `20-architecture.md`). Together they are the
formal interface.

## Why JSON, and how to not regret it

Nothing on this wire is fast enough for the encoding to matter: a model round trip is
200 ms–30 s, spawning an extension is ~1 ms, a socket round trip is ~20 µs, and serde_json on
a 4 KB message is ~5–10 µs. The busiest stream (assistant deltas, ~100/s) costs well under a
millisecond per second. What JSON buys is the thing the design depends on: any process, any
language, no toolchain. `echo '{...}' | ./tool.py` tests an extension; `socat - UNIX:...`
debugs the protocol; a model writing a shell-script tool gets JSON right.

Two decisions so this never has to change:

- **Framing is newline-delimited JSON**, one message per line, the same on the socket and on
  an executable's stdin/stdout (D-10). There is no encoding negotiation; a binary encoding,
  if ever wanted, is a new protocol major. Raw bytes never travel on this wire: terminal
  data belongs to a pty server's own protocol, not this one.
- **Large payloads travel by reference.** A tool result or attachment above a threshold
  (proposed 64 KB) is written to the session directory and the message carries
  `{ "ref": path, "bytes": n }` instead of the content; the path is read back with `fs.read`.
  Images already work this way. This keeps every line small.

## Two roles, kept distinct

And one thing neither role does: ask the user a question. Only the model asks, in text, and
the answer is a prompt. A program that needs something from the user returns a result that
says so, and the model asks (D-37).

- **Observers** `subscribe` to events. The server never waits for them. A slow TUI cannot
  stall a loop.
- **Handlers** `register` for a slot on a loop with a timeout. The server sends the event as a
  *request* and waits for the reply (or the timeout, which counts as "no opinion").

## Events (server → observers, notifications)

Every event carries `loop` and a monotonic per-loop `seq`.

| Event | Payload |
|---|---|
| `loop.status` | `{ loop, state: working \| idle, since, detail }` |
| `loop.message` | `{ loop, role, delta \| message }` (streaming assistant text, tool calls, results) |
| `loop.turn_end` / `loop.run_end` | `{ loop, messages }` |
| `ui.status` | `{ loop, key, text }` |
| `ui.widget` | `{ loop, key, lines }` |
| `ui.notify` | `{ loop, level, text }` |
| `fs.changed` | `{ loop, path, by: tool \| turn }` — emitted when one of the loop's own tools wrote the file; no watcher |

`idle` means the loop has stopped and will do nothing until prompted. There is no third
state: nothing but the model ever asks the user anything, and it does so in text (D-37).

## Handler slots (server → handlers, requests with reply)

| Slot | Payload | Reply |
|---|---|---|
| `input` | `{ text }` | `{ text }` or `{ handled: true }` |
| `prompt` | `{ system_prompt }` | `{ append }` or `{ replace }` |
| `tool_result` | `{ tool, args, result }` | `{ result }` |
| `tool.<name>` | `{ args, id }` | `{ content, details }` or `{ error }` |
| `on.<event>` | event payload | — (fire and forget, but sequenced) |

Five slots. A slash command is an `input` match with a name and a description (D-13); the UI learns the command list from `loop.attach`. `message_*`, `context`, provider hooks, compaction, tree, fork: gone until a
real need appears, and then only with a slot here and a DSL entry in `40-dsl.md`.
`tool_call` — the permission hook — is deliberately absent; see "No checks inside the loop"
in `00-north-star.md`. `tool_result` remains because it shapes what the model reads, not
what it may do; it is recorded in the session log alongside the original.

## Requests (client → server)

| Request | Meaning |
|---|---|
| `hello { client, protocol_version }` → `{ server, protocol_version }` | first message on every connection; the server refuses incompatible majors |
| `loop.create { cwd, model, session? }` / `loop.list` / `loop.attach` / `loop.close` | lifecycle; `loop.attach` returns the loop's merged manifest: tools, commands, status and widget keys |
| `loop.prompt { loop, text, when }` | `when`: `now` (steer), `after_turn`, `next_input` |
| `loop.abort { loop }` | Esc |
| `loop.wait { loop }` | blocks until the loop is idle — what subagents and scripts need |
| `subscribe { loop \| "*", events, since? }` / `unsubscribe` | observer; `since` replays events after that `seq` from the session log, across runs; deltas are not replayed (D-06) |
| `register { loop, slot, timeout }` / `unregister` | handler |
| `ui.status` / `ui.widget` / `ui.notify` | an extension asking the server to emit |
| `fs.list { loop, path }` / `fs.read { loop, path }` | read-only file access on the server that runs the loop; `fs.read` returns content or `{ ref, bytes }` above the threshold, and a `ref` is itself a server path that `fs.read` serves (D-11). No write; editing is the agent's job or the user's editor |
| `loop.tools { loop, names }` / `loop.model { loop, spec }` | control |
| `loop.reload { loop }` | re-read the loop's policy files (D-33 makes this automatic after the loop's own writes) |
| `dsl.check { cwd }` | run the policy loader and checker where the files live; what `pirs check` calls |

Anything not in these three tables does not exist.

## Open questions in this layer

- **Handler timeout default.** Proposed 5 s for `input`/`prompt`/`tool_result`,
  tool-specific for `tool.*`; a timed-out handler counts as "no opinion" with a warning, so
  a dead extension cannot brick the loop. Nothing security-relevant hangs on this now that
  there is no permission hook.
