# The pirs protocol

How to talk to a pirs loop server: as a client, as an observer, or as a handler.

`docs/protocol.schema.json` is generated from the `pirs-protocol` crate and is the
authority. It is checked by a test, so a method or field that is not in it does not exist
on the wire. This file is commentary on it: every name below is copied from the schema and
from the crate's types. Where the two ever disagree, the schema is right.

Fenced blocks are literal exchanges: request line, then response line, as many pairs as the
block shows. The JSON is exactly what the crate's serde produces.

## Framing

Newline-delimited JSON-RPC 2.0: one message is one line, compact JSON then `\n`. No length
prefix, no content header, no encoding negotiation — a different encoding would be a new
protocol major. The framing is identical in the two places it is used (D-10, D-23):

- **On the socket.** A client connects to the server's unix socket (`$PIRS_SOCKET`, else
  `$XDG_RUNTIME_DIR/pirs.sock`, else `~/.pirs/pirs.sock`) and exchanges lines in both
  directions. `socat - UNIX:$PIRS_SOCKET` is a working client.
- **On a called process's stdin/stdout.** A process the server spawns for a DSL `run =`
  reads one line on stdin and writes one line on stdout. `echo '{"text":"hi"}' | ./tool.py`
  is a working test.

The difference is only the envelope: on the socket a slot payload arrives wrapped in a
JSON-RPC request, on stdin it arrives bare. The payload itself is byte-identical, so a
handler moves between the two bindings without being rewritten.

Every line is one of three shapes:

| Shape | Members | Direction |
|---|---|---|
| request | `jsonrpc`, `id`, `method`, `params` | client → server, and server → registered handler |
| response | `jsonrpc`, `id`, and `result` **or** `error` | the reply to a request, never both members |
| notification | `jsonrpc`, `method`, `params` | server → observers; no `id`, no reply |

`jsonrpc` is always the string `"2.0"`. `id` is an integer or a string, chosen by the
sender and echoed back. Protocol message fields are `snake_case`; conversation messages and
their content blocks are `camelCase`, because that is the session-log format.

## Two roles

A connection may be either, both, or neither.

- **Observers** `subscribe` to a loop's events and receive notifications. The server never
  waits for an observer, so a slow or wedged UI cannot stall a loop; one that stops reading
  is dropped, not blocked on.
- **Handlers** `register` for a slot on a loop with a `timeout` in milliseconds. The server sends the
  slot as a *request* and waits up to that long for the reply. **A timeout means "no
  opinion"**: the slot's default outcome applies (the text unchanged, the prompt unchanged,
  the result unchanged; for `tool.<name>`, an error result), and the server emits a
  `ui.notify` warning. A handler that exits non-zero or replies with an error is treated the
  same way. A dead extension therefore cannot brick a loop.

Neither role asks the user a question. Only the model does, in text, and the answer arrives
as the next prompt. A program that needs something from the user returns a result saying so.

## `hello` and version refusal

`hello` is the first message on every connection. `PROTOCOL_VERSION` is `"0.1"`, a
`major.minor` string. **Two peers are compatible when their majors are equal**; a minor bump
only adds optional fields, methods or events, which the older side ignores.

```json
{"jsonrpc":"2.0","id":1,"method":"hello","params":{"client":"pirs-tui 0.1.0","protocol_version":"0.1"}}
{"jsonrpc":"2.0","id":1,"result":{"protocol_version":"0.1","server":"pirs 0.1.0"}}
```

A different major is refused with `-32000` and the connection is closed:

```json
{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"protocol major 1 != 0","data":{"server":"0.1"}}}
```

## Requests (client → server)

| Request | Params | Result |
|---|---|---|
| `hello` | `client`, `protocol_version` | `server`, `protocol_version` |
| `loop.create` | `cwd`, `model?`, `name?`, `session?` | `id`, `name?`, `cwd`, `model`, `state`, `since`, `conversation` |
| `loop.list` | `cwd?` | `loops`, `conversations` |
| `loop.attach` | `loop` | `loop`, `manifest`, `seq` |
| `loop.close` | `loop` | `{}` |
| `loop.prompt` | `loop`, `text`, `when` | `{}` |
| `loop.abort` | `loop` | `{}` |
| `loop.wait` | `loop` | `state` |
| `subscribe` | `loop`, `events?`, `since?` | `{}` |
| `unsubscribe` | `loop` | `{}` |
| `register` | `loop`, `slot`, `timeout` (ms) | `{}` |
| `unregister` | `loop`, `slot` | `{}` |
| `ui.status` | `loop`, `key`, `text` | `{}` |
| `ui.widget` | `loop`, `key`, `lines` | `{}` |
| `ui.notify` | `loop`, `level`, `text` | `{}` |
| `fs.list` | `loop`, `path` | `entries` |
| `fs.read` | `loop`, `path` | `content`, or `ref` + `bytes` |
| `loop.tools` | `loop`, `names` | `{}` |
| `loop.model` | `loop`, `spec` | `{}` |
| `loop.reload` | `loop` | `files` |
| `dsl.check` | `cwd` | `files`, `manifest`, `conflicts`, `system_prompt` |

That is the whole list. `model` is `{ model, thinking? }` with `thinking` one of `off`,
`minimal`, `low`, `medium`, `high`, `xhigh`, `max`. `state` is `working` or `idle` — there
is no third state. `when` is `now` (steer), `after_turn` or `next_input`.

**Lifecycle.** `loop.create` starts a loop; `session` continues a stored conversation by id.

```json
{"jsonrpc":"2.0","id":2,"method":"loop.create","params":{"cwd":"/home/me/proj","model":{"model":"anthropic/claude-sonnet-4-5","thinking":"medium"},"name":"review"}}
{"jsonrpc":"2.0","id":2,"result":{"conversation":"c-19f2","cwd":"/home/me/proj","id":"a7f3","model":{"model":"anthropic/claude-sonnet-4-5","thinking":"medium"},"name":"review","since":1758412800000,"state":"idle"}}
```

`loop.list` lists running loops, and, when `cwd` is given, the conversations stored for that
directory (most recent first); without `cwd`, `conversations` is empty.

```json
{"jsonrpc":"2.0","id":3,"method":"loop.list","params":{"cwd":"/home/me/proj"}}
{"jsonrpc":"2.0","id":3,"result":{"conversations":[{"cwd":"/home/me/proj","id":"c-19f2","name":"review","path":"/home/me/.pirs/sessions/home-me-proj/c-19f2.jsonl","updated":1758412801000}],"loops":[{"conversation":"c-19f2","cwd":"/home/me/proj","id":"a7f3","model":{"model":"anthropic/claude-sonnet-4-5","thinking":"medium"},"name":"review","since":1758412800000,"state":"idle"}]}}
```

`loop.attach` returns the loop, its merged manifest (`tools`, `commands`, `status_keys`,
`widget_keys`) and the latest `seq`, so a client can lay out a status line before the first
event and then `subscribe { since: seq }` without a gap.

```json
{"jsonrpc":"2.0","id":4,"method":"loop.attach","params":{"loop":"a7f3"}}
{"jsonrpc":"2.0","id":4,"result":{"loop":{"conversation":"c-19f2","cwd":"/home/me/proj","id":"a7f3","model":{"model":"anthropic/claude-sonnet-4-5","thinking":"medium"},"name":"review","since":1758412800000,"state":"idle"},"manifest":{"commands":[{"description":"Review the working tree.","name":"review"}],"status_keys":["branch"],"tools":[{"description":"Run a shell command.","name":"bash","parameters":{"properties":{"command":{"type":"string"}},"required":["command"],"type":"object"}}],"widget_keys":["tests"]},"seq":12}}
```

**Driving a loop.** `loop.prompt` returns as soon as the prompt is accepted; the model's
answer arrives as events, and the text passes through the `input` slot first. `loop.wait` is
what a script or a subagent needs: its response arrives when the loop is idle.

```json
{"jsonrpc":"2.0","id":5,"method":"loop.prompt","params":{"loop":"a7f3","text":"what changed?","when":"now"}}
{"jsonrpc":"2.0","id":5,"result":{}}
{"jsonrpc":"2.0","id":6,"method":"loop.abort","params":{"loop":"a7f3"}}
{"jsonrpc":"2.0","id":6,"result":{}}
{"jsonrpc":"2.0","id":7,"method":"loop.wait","params":{"loop":"a7f3"}}
{"jsonrpc":"2.0","id":7,"result":{"state":"idle"}}
{"jsonrpc":"2.0","id":8,"method":"loop.close","params":{"loop":"a7f3"}}
{"jsonrpc":"2.0","id":8,"result":{}}
```

**Observing.** `loop` is a loop id or `"*"` (every loop, including ones created later).
`events` filters by event name; absent means all.

```json
{"jsonrpc":"2.0","id":9,"method":"subscribe","params":{"events":["loop.message","loop.run_end"],"loop":"a7f3","since":12}}
{"jsonrpc":"2.0","id":9,"result":{}}
{"jsonrpc":"2.0","id":10,"method":"subscribe","params":{"loop":"*"}}
{"jsonrpc":"2.0","id":10,"result":{}}
{"jsonrpc":"2.0","id":11,"method":"unsubscribe","params":{"loop":"a7f3"}}
{"jsonrpc":"2.0","id":11,"result":{}}
```

**Handling.** `slot` is the slot's string form, which is also the `method` of the requests
the server will send.

```json
{"jsonrpc":"2.0","id":12,"method":"register","params":{"loop":"a7f3","slot":"tool.fetch","timeout":60000}}
{"jsonrpc":"2.0","id":12,"result":{}}
{"jsonrpc":"2.0","id":13,"method":"unregister","params":{"loop":"a7f3","slot":"tool.fetch"}}
{"jsonrpc":"2.0","id":13,"result":{}}
```

**Emitting UI.** These ask the server to emit the matching event to the loop's observers.

```json
{"jsonrpc":"2.0","id":14,"method":"ui.status","params":{"key":"branch","loop":"a7f3","text":"adaptable*"}}
{"jsonrpc":"2.0","id":14,"result":{}}
{"jsonrpc":"2.0","id":15,"method":"ui.widget","params":{"key":"tests","lines":["42 passed","0 failed"],"loop":"a7f3"}}
{"jsonrpc":"2.0","id":15,"result":{}}
{"jsonrpc":"2.0","id":16,"method":"ui.notify","params":{"level":"warning","loop":"a7f3","text":"handler for `input` timed out"}}
{"jsonrpc":"2.0","id":16,"result":{}}
```

**Reading files.** Read-only, on the server that runs the loop. There is no write: editing
is the agent's job, or the user's editor. A relative `path` is relative to the loop's cwd.
Each `fs.list` entry carries its own `path`, the server's full spelling: hand that back to
`fs.list` or `fs.read` unchanged rather than joining `name` onto anything (D-31).

```json
{"jsonrpc":"2.0","id":17,"method":"fs.list","params":{"loop":"a7f3","path":"src"}}
{"jsonrpc":"2.0","id":17,"result":{"entries":[{"bytes":4625,"kind":"file","name":"lib.rs","path":"/home/me/proj/src/lib.rs"},{"kind":"dir","name":"tools","path":"/home/me/proj/src/tools"}]}}
{"jsonrpc":"2.0","id":18,"method":"fs.read","params":{"loop":"a7f3","path":"src/lib.rs"}}
{"jsonrpc":"2.0","id":18,"result":{"content":"fn main() {}\n"}}
```

**Control.** `loop.tools` sets the active set by name from the manifest; anything not listed
is disabled for that loop. `loop.model` changes the model between turns.

```json
{"jsonrpc":"2.0","id":20,"method":"loop.tools","params":{"loop":"a7f3","names":["read","grep"]}}
{"jsonrpc":"2.0","id":20,"result":{}}
{"jsonrpc":"2.0","id":21,"method":"loop.model","params":{"loop":"a7f3","spec":{"model":"anthropic/claude-opus-4-1","thinking":"high"}}}
{"jsonrpc":"2.0","id":21,"result":{}}
```

**Policy.** `loop.reload` re-reads the loop's policy files (the server also does this by
itself after the loop's own tools write one). `dsl.check` runs the loader and checker where
the files live and is what `pirs check` calls; `conflicts` name both files, and
`system_prompt` is the fully assembled prompt with every rewrite applied.

```json
{"jsonrpc":"2.0","id":22,"method":"loop.reload","params":{"loop":"a7f3"}}
{"jsonrpc":"2.0","id":22,"result":{"files":["/home/me/.pirs/ext/git.pirs.toml","/home/me/proj/.pirs/ext/review.pirs.toml"]}}
{"jsonrpc":"2.0","id":23,"method":"dsl.check","params":{"cwd":"/home/me/proj"}}
{"jsonrpc":"2.0","id":23,"result":{"conflicts":[{"files":["/home/me/.pirs/ext/git.pirs.toml","/home/me/proj/.pirs/ext/review.pirs.toml"],"message":"duplicate command `review`"}],"files":["/home/me/proj/.pirs/ext/review.pirs.toml"],"manifest":{"commands":[{"description":"Review the working tree.","name":"review"}],"status_keys":["branch"],"tools":[{"description":"Run a shell command.","name":"bash","parameters":{"properties":{"command":{"type":"string"}},"required":["command"],"type":"object"}}],"widget_keys":["tests"]},"system_prompt":"You are pirs.\n"}}
```

## Events (server → observers)

Notifications, so no reply. Every event carries `loop`; every event but a streaming delta
also carries `seq`.

| Event | Params |
|---|---|
| `loop.status` | `loop`, `seq`, `state`, `since`, `detail?` |
| `loop.message` | `loop`, `seq?`, `role`, and `delta` **or** `message` |
| `loop.turn_end` | `loop`, `seq`, `messages` |
| `loop.run_end` | `loop`, `seq`, `messages` |
| `ui.status` | `loop`, `seq`, `key`, `text` |
| `ui.widget` | `loop`, `seq`, `key`, `lines` |
| `ui.notify` | `loop`, `seq`, `level`, `text` |
| `fs.changed` | `loop`, `seq`, `path`, `by` |

`since` is unix milliseconds; `detail` says why the state changed, when there is something
to say. `role` is `system`, `user`, `assistant` or `toolResult`; `level` is `info`,
`warning` or `error`. `by` is `tool` (a tool wrote the file during a call) or `turn`
(noticed at the end of the turn) — there is no watcher, so files changed by anything else
are not reported. An empty `text` clears a status key; empty `lines` removes a widget. A
turn ends at every model stop, so a `toolUse` stop is followed by tool results and another
turn; a run ends when the loop goes idle.

```json
{"jsonrpc":"2.0","method":"loop.status","params":{"loop":"a7f3","seq":13,"since":1758412801000,"state":"working"}}
{"jsonrpc":"2.0","method":"loop.message","params":{"delta":{"index":0,"text":"Two files ","type":"text"},"loop":"a7f3","role":"assistant"}}
{"jsonrpc":"2.0","method":"loop.message","params":{"loop":"a7f3","message":{"api":"anthropic-messages","content":[{"text":"Two files changed.","type":"text"}],"model":"claude-sonnet-4-5","provider":"anthropic","responseId":"msg_014","role":"assistant","stopReason":"stop","timestamp":1758412801000,"usage":{"cacheRead":0,"cacheWrite":0,"cost":{"cacheRead":0.0,"cacheWrite":0.0,"input":0.0036,"output":0.00027,"total":0.00387},"input":1200,"output":18,"totalTokens":1218}},"role":"assistant","seq":15}}
{"jsonrpc":"2.0","method":"ui.status","params":{"key":"branch","loop":"a7f3","seq":18,"text":"adaptable*"}}
{"jsonrpc":"2.0","method":"ui.widget","params":{"key":"tests","lines":["42 passed"],"loop":"a7f3","seq":19}}
{"jsonrpc":"2.0","method":"ui.notify","params":{"level":"warning","loop":"a7f3","seq":20,"text":"handler for `input` timed out"}}
{"jsonrpc":"2.0","method":"fs.changed","params":{"by":"tool","loop":"a7f3","path":"/home/me/proj/src/lib.rs","seq":21}}
```

`loop.turn_end` and `loop.run_end` carry the messages they appended:

```json
{"jsonrpc":"2.0","method":"loop.turn_end","params":{"loop":"a7f3","messages":[{"api":"anthropic-messages","content":[{"text":"Two files changed.","type":"text"}],"model":"claude-sonnet-4-5","provider":"anthropic","responseId":"msg_014","role":"assistant","stopReason":"stop","timestamp":1758412801000,"usage":{"cacheRead":0,"cacheWrite":0,"cost":{"cacheRead":0.0,"cacheWrite":0.0,"input":0.0036,"output":0.00027,"total":0.00387},"input":1200,"output":18,"totalTokens":1218}}],"seq":16}}
```

## Handler slots (server → handlers)

Five forms. The slot string is the `method`; the payload is the `params`, and is exactly
what a called process reads on stdin.

| Slot | Payload | Reply |
|---|---|---|
| `input` | `text` | `text`, or `handled: true` |
| `prompt` | `system_prompt` | `append`, or `replace` |
| `tool_result` | `tool`, `args`, `result` | `result` |
| `tool.<name>` | `args`, `id` | `content` + `details?`, or `error` |
| `on.<event>` | that event's payload | none — a notification, fire and forget, but sequenced |

`<event>` is one of `start`, `turn_end`, `run_end`, `tool_result`, `reload`. A `result` (in
`tool_result` and as a `tool.<name>` reply) is either `{ content, details? }` — where
`content` is a string or a list of `text`/`image` blocks, and `details` is metadata for UIs
that the model never sees — or `{ error }`.

There is no permission slot. Nothing in this protocol decides what the model *may* do;
`tool_result` shapes what the model *reads*, and both the original and the rewrite go to the
session log.

**`input`** runs on every prompt before the model sees it. It is also how a slash command
works: match the text, expand it, or consume it with `handled: true` so no turn starts.

```json
{"jsonrpc":"2.0","id":100,"method":"input","params":{"text":"/review src"}}
{"jsonrpc":"2.0","id":100,"result":{"text":"Review the working tree under src."}}
{"jsonrpc":"2.0","id":101,"method":"input","params":{"text":"/clear"}}
{"jsonrpc":"2.0","id":101,"result":{"handled":true}}
```

**`prompt`** runs when the system prompt is assembled.

```json
{"jsonrpc":"2.0","id":102,"method":"prompt","params":{"system_prompt":"You are pirs.\n"}}
{"jsonrpc":"2.0","id":102,"result":{"append":"\nThe project uses Rust 2021."}}
```

**`tool_result`** runs on every tool result before the model reads it.

```json
{"jsonrpc":"2.0","id":103,"method":"tool_result","params":{"args":{"command":"cargo test"},"result":{"content":"running 412 tests\n...","details":{"exit":0}},"tool":"bash"}}
{"jsonrpc":"2.0","id":103,"result":{"result":{"content":"412 passed"}}}
```

**`tool.<name>`** implements a tool. `id` is the tool-call id, for correlating with the log.

```json
{"jsonrpc":"2.0","id":104,"method":"tool.fetch","params":{"args":{"url":"https://example.com"},"id":"call_1"}}
{"jsonrpc":"2.0","id":104,"result":{"content":"<!doctype html>…","details":{"status":200}}}
{"jsonrpc":"2.0","id":105,"method":"tool.fetch","params":{"args":{"url":"https://example.com"},"id":"call_2"}}
{"jsonrpc":"2.0","id":105,"result":{"error":"connection refused"}}
```

**`on.<event>`** observes. It is a *notification*: no `id`, no reply, nothing waited for,
which is why a process started from `on.start` may stay alive, open the socket, and become a
connected client. The `params` are the same as they would be on a request, so a called
process reads the same line on stdin either way.

```json
{"jsonrpc":"2.0","method":"on.start","params":{"cwd":"/home/me/proj","loop":"a7f3"}}
{"jsonrpc":"2.0","method":"on.tool_result","params":{"args":{"command":"cargo test"},"loop":"a7f3","result":{"content":"412 passed"},"tool":"bash"}}
{"jsonrpc":"2.0","method":"on.reload","params":{"files":["/home/me/proj/.pirs/ext/review.pirs.toml"],"loop":"a7f3"}}
```

`on.turn_end` and `on.run_end` carry the same payload as the events of those names,
including `seq`.

As a called process, the same `input` handler reads and writes bare payloads:

```json
{"text":"/review src"}
{"text":"Review the working tree under src."}
```

## Payloads by reference

Any single content above **64 KB** travels by reference: the server writes it to the loop's
session directory and sends `{ "ref": <path>, "bytes": <n> }` instead. Every line stays
small, and a remote client fetches only what it will show. A `ref` is itself a server path,
and `fs.read` serves it (D-11) in full, whatever its size:

```json
{"jsonrpc":"2.0","id":19,"result":{"bytes":182400,"ref":"/home/me/.pirs/sessions/home-me-proj/blob/7c1a.txt"}}
```

## `seq` and replay

The session log *is* the event stream. Every sequenced event is one log entry, and `seq` is
its index — monotonic per loop, stable across runs of the server. `subscribe { since: n }`
replays the logged events with `seq > n` before any live ones, so a client that lost its
connection (an SSH link, say) catches up on exactly what it missed. `since: 0` replays
everything; omitting `since` replays nothing. The pattern is `loop.attach` → note its `seq`
→ `subscribe { since: seq }`.

**Streaming deltas carry no `seq`, are not logged, and are never replayed** (D-06). A
reconnecting client gets the complete `loop.message` for each finished message instead of
the fragments that built it. Do not count on deltas for state; count on the messages.

## Error codes

| Code | Name | Meaning |
|---|---|---|
| `-32700` | parse error | the line was not valid JSON, or not an object |
| `-32600` | invalid request | JSON, but not a valid JSON-RPC 2.0 request |
| `-32601` | method not found | not a method this protocol version defines |
| `-32602` | invalid params | `params` did not deserialise; `data` carries the error text |
| `-32603` | internal error | the server failed while handling a valid request |
| `-32000` | version refused | `hello` named a different protocol major; `data` is `{ "server": "<version>" }` and the connection closes |
| `-32001` | unknown loop | the `loop` names no loop on this server |
| `-32002` | unknown slot | not one of the five slot forms, or `unregister` names a slot this connection never registered |
| `-32003` | handler timeout | a handler did not reply in time; not sent as a reply, but used in the `ui.notify` warning and in the log |
| `-32004` | handler error | a handler replied with an error, or a called process exited non-zero or wrote something that was not the reply type |
| `-32005` | not found | `fs.list`/`fs.read` named an unreadable path, or an unknown conversation |
| `-32006` | busy | not possible in the loop's current state (`loop.model` mid-turn, `loop.close` with another client attached); retry after `loop.wait` |

```json
{"jsonrpc":"2.0","id":5,"error":{"code":-32001,"message":"no loop `a7f3`"}}
```

## Paths are opaque

Every path in every message — `cwd`, an `fs.*` `path`, a `ref`, the `files` of
`loop.reload` — is a label produced by the server that runs the loop and only ever handed
back to that same server (D-31). **Do not parse, join, normalise or open one.** It may name
a file on a machine or in a container the client cannot see, under a path syntax the client
does not use. Display it; hand it back; nothing else.

## The environment of a called process

The server spawns a called process in the loop's cwd, writes the slot payload as one JSON
line to its stdin, reads one JSON line from its stdout, and kills it when the loop closes.
Four variables are in its environment:

| Variable | Value |
|---|---|
| `PIRS_SOCKET` | the server's socket, so the process can connect as a client if it wants to |
| `PIRS_LOOP` | the loop's id |
| `PIRS_SLOT` | the slot that fired, in its string form (`input`, `tool.fetch`, `on.turn_end`, …) |
| `PIRS_SESSION_DIR` | the loop's session directory, where by-reference payloads live |

A process that only answers its slot ignores all four. One that wants to steer the loop —
emit `ui.status`, prompt another loop, register more slots — connects to `PIRS_SOCKET`, says
`hello`, and is an ordinary client from then on.

## Proposed, not yet accepted

D-38 would have `loop.list` take an optional `cwd` and return the conversations stored for
it alongside the running loops; the crate already carries the fields, but the decision is
recorded as proposed in `docs/design/90-decisions.md` and not yet accepted.
