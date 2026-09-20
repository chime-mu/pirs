# pirs as adaptable software — design proposal (v2)

Status: draft for discussion. Nothing here is implemented. Supersedes v1, which assumed a
fixed TUI inside the same process; this version separates a headless server from clients.
Revised 2026-09-20: relation to pi, crate carve-up, mechanical checks.

## Goal

pirs stops being a pi port and becomes an experiment in moldable software: a small stable
core, one formal interface, and everything else outside it — policy as declarations, code as
separate executables, UI as separate clients. pi extension compatibility is dropped.

Two bets:

1. Most of what people want from an agent tool is *policy* (what to add to the prompt, what
   to run after a turn, how to react to input). Policy is better expressed as a short
   declaration than as a program. The evidence for this is not pi's example extensions — we
   do not know whether anyone runs them — but tools where the declarative shape demonstrably
   gets used: Claude Code's permission rules, hooks and CLAUDE.md, Cursor rules, Codex's
   config. pi's examples serve only as a corpus to check the DSL can express.
2. The right boundary is not "an agent tool inside a terminal multiplexer" but "one or more
   agent loops behind a protocol, with a UI that is just another client". Owning the loop is
   what makes status trustworthy and UIs replaceable. The thing this design refuses is
   *guessing*, not tmux or herdr: a multiplexer that does not own the loop has to infer state
   from the screen (herdr's own docs: hooks when the agent reports, screen manifests when it
   doesn't) and we emit it as a fact. In whichever multiplexer pirs sits, that stays true.

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
8. **Components behind protocols.** The loop server carries only what needs the loop.
   Anything else — terminals, editors, multiplexing, windows — is a separate component with
   its own protocol or the client's own business, so the core shrinks while the buildable
   space grows. The test for any feature is: does it need the loop?

## Relation to pi

pirs started as a port of pi, and the argument for this design is mostly pi's own, as stated
in Mario Zechner's November 2025 write-up — written before pi grew its in-process TypeScript
extension system. That post argues for a minimal core; a headless agent behind RPC and JSON
modes with the TUI as one client among possible others; extensions as external CLI tools
rather than plugins or MCP; sub-agents spawned explicitly so they stay observable; and state
kept in files rather than in the model. This proposal keeps all of that. Where it departs
from *current* pi is the extension model: it sides with the post's "CLI tools, no plugin
system" over the in-process API that `pi-ext` ports, and accepts losing pi extension
compatibility to do so. That is a deliberate trade of milestone 1's main result, and the
justification is the two bets above, not the post.

Three places where this design and the post genuinely disagree, stated so they are argued
rather than assumed:

**No checks inside the loop.** pi runs unrestricted by default on the grounds that once an
agent can write and run code, sandboxing inside the tool is theatre; if you need containment,
use a container. This design agrees, and goes one step further than v1 of this proposal did:
pirs performs no security checks inside the loop and ships no feature that could be mistaken
for one. An earlier draft had a `[[guard]]` slot for the foot-gun class (`rm -rf`, edits under
`.git`). It is gone. A pattern match living in the process the model controls cannot hold
against a model that has been steered into routing around it — the sandbox escapes reported
against OpenAI's and Anthropic's own agents in 2026 are the demonstration — and a feature
that *looks* like enforcement makes people run on untrusted input without the boundary that
would actually protect them. That is worse than having nothing. Containment is a boundary
around the *server*, and the design's job is to make running the server inside one boring;
see "Containment" below. If an accident-class convenience ever proves necessary, it returns
under a name that cannot be read as enforcement (`confirm`, never `guard`, never `block`).

**A daemon instead of tmux.** pi has no long-running server; its answer to detach,
multiplexing and long jobs is tmux, and it rejects built-in background processes because
lifecycle, buffering and cleanup are complexity. This design takes on exactly that
complexity: a server that owns loops, auto-starts, idles out, replays events after a dropped
link, negotiates versions with clients. The reason is principle 7. A multiplexer that does
not own the loop can only guess whether a pane is working, blocked or idle; a server that
owns it knows. The cost is real and every item of it appears in the protocol tables and open
questions below; if that list grows past what the status guarantee is worth, tmux is the
fallback.

**Injected context must stay visible.** The strongest claim in pi's favour is that you see
exactly what context the model received, and its sharpest criticism of other tools is
context injected behind the user's back. `[[prompt]] run = ...` and `tool_result` rewrites
are that, unless they are inspectable. So: `pirs check` prints the fully assembled system
prompt, not just the slot list; the session log records the post-rewrite tool result
together with the handler that rewrote it; and the TUI can show either on request. A DSL that
could not meet this would be worse than the plugin system it replaces.

One argument not to borrow: the post recommends CLI tools for *token* economy — the model
reads a README only when it needs the tool. This design chooses processes for *toolchain*
independence — any language, no build step, testable with `echo | ./tool`. Both are good
reasons; they are different reasons.

## Architecture

pirs is a set of components with standardised protocols, combinable in different ways. The
loop server is the one we must build; the others are optional, replaceable, or someone
else's.

| Component | Protocol | Status |
|---|---|---|
| **loop server** | the pirs protocol: loops, events, handlers, read-only `fs.*` | core; the subject of this document |
| LLM abstraction (`pi-ai`) | Rust API, inside the loop server | core component of the server; providers, streaming, model registry, credentials. It stays on the server side of every boundary |
| extension executables | JSON-RPC on stdin/stdout, optionally the socket | core |
| pty server | a pty protocol: spawn, input, output, resize, scrollback | optional and separate; tmux (control mode) can stand in until someone writes one. Never part of the loop server |
| TUI | client of any number of the above | reference client; one composition among possible ones |

```
                 ┌───────────────────────────────────────────────┐
                 │ pirs loop server (headless, one per user)     │
                 │                                               │
                 │   loop A ── session, cwd, model, tools, DSL   │
                 │   loop B ── session, cwd, model, tools, DSL   │
                 │   pi-ai ── providers, streaming, credentials  │
                 │                                               │
                 │   emits   events, status, dialogs, fs.changed │
                 │   accepts prompt, reply, control, fs.read     │
                 └──────────────┬────────────────────────────────┘
                                │ JSON-RPC 2.0 over unix socket
                                │ (remote: ssh <host> pirs proxy)
       ┌────────────┬───────────┼────────────┬─────────────────┐
       │            │           │            │                 │
    pirs tui   pirs (print)  extension    another loop     someone else's
                one-shot     executable   (subagent)        client
       │
       └── may also be a client of a pty server (separate component)
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
| `pirs tui` | the interactive UI: a sidebar of agents across servers, and pages |
| extension executables | spawned by the server from a DSL `run =`, connected as a client that registers handlers |
| a loop | a `tool` that prompts another loop and waits for it to go idle |

### Several servers, local and remote

A client may hold connections to several servers at once. A loop is identified by
`(server, loop)`; the TUI's sidebar is the union across servers and a page opens on
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

### Containment

The server/client split is also the security model, and the only one pirs has. Everything a
loop does — built-in tools, `run =` shell strings, extension executables, file access — runs
where the server runs. Put the server inside a boundary you trust (a container, a VM, a
separate user account, a throwaway machine) and attach from the host over the socket or
`ssh <jail> pirs proxy`. Then:

- the TUI stays on the host with the user's terminal, clipboard and fonts;
- every tool and extension executes inside the boundary, because that is where the server is;
- the boundary is enforced by the kernel, hypervisor or SSH, not by pirs;
- pirs checks nothing and therefore claims nothing.

This is the recommended setup for any repository or input the user does not trust, and it
is one pi cannot offer without giving up the TUI-on-host split. The design's obligation is to
make it unremarkable: a `servers.toml` entry of kind `container` (image, mounts, the
project directory) next to `ssh`; `pirs serve` documented as the entrypoint of a container
image; `pirs tui` attaching to a jailed loop with no visible difference from a local one.

What the boundary does not close by itself is egress: a jailed server still holds a model
API key and needs the network to reach the provider, so the provider endpoint is the
exfiltration channel. See open question 8.

## The protocol

JSON-RPC 2.0 over a unix socket. Chosen because request ids are exactly what dialogs and
handlers need, and the format is boring. All messages are documented in `docs/protocol.md`;
the accompanying `docs/protocol.schema.json` is generated from the `pirs-protocol` crate and
checked by a test (see Mechanical checks). Together they are the formal interface.

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
  stays the truth). Raw-bytes frames are not reserved for anything: terminal data belongs to
  a pty server's own protocol, not this one.
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
| `fs.changed` | `{ loop, path, by: tool \| turn }` — emitted when one of the loop's own tools wrote the file; no watcher |

`blocked` means exactly "a `ui.dialog` is open and unanswered". Nothing else.

### Handler slots (server → handlers, requests with reply)

| Slot | Payload | Reply |
|---|---|---|
| `input` | `{ text }` | `{ text }` or `{ handled: true }` |
| `prompt` | `{ system_prompt }` | `{ append }` or `{ replace }` |
| `tool_result` | `{ tool, args, result }` | `{ result }` |
| `tool.<name>` | `{ args, id }` | `{ content, details }` or `{ error }` |
| `command.<name>` | `{ args }` | — |
| `on.<event>` | event payload | — (fire and forget, but sequenced) |

Seven slots. `message_*`, `context`, provider hooks, compaction, tree, fork: gone until a
real need appears, and then only with a slot here and a DSL entry below. `tool_call` — the
permission hook — is deliberately absent; see "No checks inside the loop" above.
`tool_result` remains because it shapes what the model reads, not what it may do; it is
recorded in the session log alongside the original.

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
| `fs.list { loop, path }` / `fs.read { loop, path }` | read-only file access on the server that runs the loop, scoped to what the loop itself could read; `fs.read` returns content or `{ ref, bytes }` above the threshold. No write; editing is the agent's job or the user's editor |
| `loop.tools { loop, names }` / `loop.model { loop, spec }` | control |

Anything not in these three tables does not exist.

## The DSL (server-side policy)

A declarative file, `*.pirs.toml`, from `~/.pirs/ext/` (global) and `.pirs/ext/` (project),
loaded per loop from its cwd. TOML for now: free parser, comments, models write it reliably.
The vocabulary is the hard part; the spelling can change once the vocabulary settles.

Every file starts with its intent:

```toml
intent = """
Show the current git branch in the status line and refresh it after every turn.
Let me type ?question for a brief answer and !cmd to run a shell command.
"""
```

### Slots

```toml
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

Eight slots: `intent`, `input`, `prompt`, `status`, `widget`, `on`, `command`, `tool`.
Every slot accepts `run = "..."` as the way to say "I need code for this". There is no
`guard`: nothing in a `.pirs.toml` decides what the model may do.

### `run` semantics

- A shell string (`sh -c`, in the loop's cwd): stdout is the result as text; a non-zero
  exit is reported as an error with stderr as the message.
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

This is a check of what the DSL can express, not a claim about demand; pi's example
extensions are a demo corpus and we have no data on which of them anyone runs. Against those
~70: most of the "trivial" and "moderate" tiers need no executable. `todo` needs one (state),
`fetch` needs one (real work), `subagent` becomes a `[[tool]]` with `loop = ...`.
Deliberately *not* expressible: `permission-gate`, `protected-paths` and every other
tool-call interceptor — see "No checks inside the loop". Also unsupported, by design: custom
providers with their own streaming, custom renderers, editors, overlays.

## Composition

Loading a loop's DSL is: parse every applicable file, union the slots, check.

| Check | Outcome |
|---|---|
| duplicate `tool` name | error unless one is `wrap`/`disabled` of the other |
| duplicate `status`/`widget` key, duplicate `command` name | error |
| several `input`s | file order; first `handled` stops |
| several `prompt`s | concatenated in file order |
| `on` entries | all run, file order |

File order is path-sorted, global before project; `priority = N` moves a file earlier.
`pirs check [--cwd]` prints the merged result, the fully assembled system prompt, and every
conflict without starting a loop.

## Intent, regeneration, sharing

- The shareable unit is the `intent` string. The file is a cache of it.
- `pirs ext new "<intent>"` asks the current model to write a `.pirs.toml` from the intent and
  `docs/dsl.md`, runs `pirs check`, shows the result. `pirs ext regen <file>` repeats it from
  the file's own intent.
- `docs/dsl.md` replaces `docs/extensions.md` and is written as the instruction set for that
  model call: slots, fields, checks, one example per slot, nothing else.

## The TUI (a client)

`pirs tui` is the reference client. It is *one* client; someone who wants a different UI
writes a different client against `docs/protocol.md`. Nothing below is a protocol concern.

- **Shape.** A sidebar listing agents across every configured server, each with its state,
  and a main area showing a page. Select an agent, see its page.
- **Page kinds.** `agent` (the conversation, dialogs, widgets, status keys, the files it
  touched this turn as a jump list); `file` (read-only, fetched with `fs.read` from the
  server that runs the loop, refreshed on `fs.changed` while the agent works, so you can
  read what an agent is editing without leaving the sidebar); later `terminal`, if and only
  if a pty server is present. Several file pages may be open at once.
- **Arrangement is the client's business.** The reference TUI starts with sidebar plus one
  page. Splits, tabs, windows, saved layouts — herdr's window features — are all buildable
  inside the client with no protocol change, and this document neither specifies nor limits
  them.
- **Editing.** Not in the TUI. Ask the agent, or shell out to `$EDITOR` (the TUI suspends
  and redraws on return; `ssh <server> $EDITOR` for a remote path). pirs does not build a
  worse version of a tool the user already has.
- `blocked` agents are visibly flagged because the server said so; no heuristics.
- Config in `~/.pirs/tui.toml`: key bindings and a status format string over `ui.status`
  keys and `loop.status`. No layout schema.

## What is core (and size)

| Component | Lines | Fate |
|---|---|---|
| `pi-ai` providers, streaming | 2.6k | core, a component inside the loop server |
| `pi-agent` loop | 1.3k | core |
| session log | 2.1k | core |
| built-in tools | 3.0k | core, registered through the manifest, overridable |
| settings, prompt assembly | 0.4k | core |
| terminal driver, interactive mode | 1.0k | moves to `pirs-tui` |
| `pi-ext` (QuickJS, TS strip, runtime.js) | 3.0k | deleted |
| server, protocol, DSL loader/checker, process runner, `fs.*` | ~1.6k new | core |

Built-in tools stay compiled in for latency and fidelity but are ordinary manifest entries so
a DSL file can disable, replace, or wrap them. Small *surface*, not small binary.

### Crates

Crate boundaries are the architecture: a crate can only use what its `Cargo.toml` lists, and
Cargo forbids cycles, so the rules worth having are the ones that become a missing edge.

| Crate | May depend on (internal) | Holds |
|---|---|---|
| `pi-ai` | — | providers, streaming, model registry |
| `pi-agent` | `pi-ai` | the loop |
| `pirs-protocol` | — | every wire type, with `JsonSchema` derives; `serde` and `schemars` only, no `tokio`, no `reqwest` |
| `pirs-server` | `pi-ai`, `pi-agent`, `pirs-protocol` | session log, built-in tools, DSL loader/checker, process runner, socket server |
| `pirs-tui` | `pirs-protocol` | the reference UI |
| `pirs-client` | `pirs-protocol` | print mode, `check`, `ext`, `proxy`: everything a thin client does |
| `pirs` (bin) | all of the above | argument parsing and dispatch; `pirs serve` runs the server in-process, everything else is a client |

`pi-ext` is removed. The edge that matters most is the one that is absent: `pirs-tui` does
not depend on `pi-ai` or `pi-agent`. If the TUI ever needs a type from either, the protocol
is missing something, and the fix goes in `pirs-protocol` and `docs/protocol.md`, not in the
TUI's `Cargo.toml`.

### Mechanical checks

The layering is enforced, not documented:

- **Edge set.** `crates/pirs/tests/architecture.rs` reads `cargo metadata` and asserts each
  workspace member's internal dependencies against the table above — and that every member
  is in the table, so a new crate cannot arrive unclassified.
- **Transitive bans.** `deny.toml` uses `cargo-deny`'s `wrappers`: `crossterm` and `ratatui`
  reachable only through `pirs-tui`, `reqwest` only through `pi-ai`, and any terminal
  emulator crate (`vt100`, `alacritty_terminal`) banned outright — a pty server is a
  separate component, and this ban is what keeps it one.
- **Per-crate lints.** `pirs-server` denies `clippy::print_stdout` / `print_stderr` (the
  server never renders) and, via its own `clippy.toml`, `disallowed_methods` on
  `std::process::Command::new` and `tokio::process::Command::new` outside the process runner.
  Every crate denies `unreachable_pub` so its public surface is deliberate.
- **Protocol snapshot.** A test in `pirs-protocol` generates the JSON schema from the types
  and compares it to `docs/protocol.schema.json`. Adding a method or field without updating
  the schema fails CI; "anything not in these three tables does not exist" is a test, not a
  sentence. `cargo public-api --diff` on `pirs-protocol` at release time is what makes the
  `hello` version negotiation trustworthy.

## Phases

0. **Design freeze.** `docs/protocol.md` and `docs/dsl.md` written and reviewed. The crate
   carve-up lands here: `pirs-protocol` with the schema snapshot test, and the architecture
   test over `cargo metadata`. The freeze is in code, not only in prose.
1. **Server + print client.** Loop over the socket; `pirs "prompt"` works end to end; TUI
   still the old in-process one. pi-ext still present. `deny.toml` and the per-crate lints
   arrive with `pirs-server`.
2. **DSL, shell `run` only.** `input`/`prompt`/`status`/`widget`/`on`; `pirs check`.
3. **TUI as client.** Sidebar plus agent page; dialogs over the protocol; `blocked` flag;
   `fs.read`/`fs.changed` and file pages.
4. **Executables.** `tool`/`command` with executable `run`; one-shot then persistent; port
   `fetch`. Delete `pi-ext`, `examples/extensions/`, pi-compat docs.
5. **Multi-loop.** `loop.wait`, `[[tool]] loop = ...`; the sidebar already spans loops.
6. **Remote and jailed servers.** `pirs proxy`, `servers.toml` with `ssh` and `container`
   kinds, `since` replay, `blob.get`; the sidebar spans servers; a documented container image
   with `pirs serve` as entrypoint. This is where the security story becomes usable, so it
   should not slip behind 7 and 8.
7. **Intent tooling.** `pirs ext new` / `regen`.
8. **pty server**, if still wanted: a separate component with its own protocol, and a
   `terminal` page kind in the TUI. tmux until then. Never inside the loop server.

Each phase leaves a working binary.

## Open questions

1. **Which DSL files apply to a loop?** Proposed: global + the loop's cwd project files, read
   at `loop.create` and on `loop.reload`. Not re-read per turn.
2. **Who answers a dialog when three clients are attached?** Proposed: any; first reply
   wins; the rest get `ui.dialog_closed`.
3. **Handler timeout default.** Proposed 5 s for `input`/`prompt`/`tool_result`,
   tool-specific for `tool.*`; a timed-out handler counts as "no opinion" with a warning, so
   a dead extension cannot brick the loop. Nothing security-relevant hangs on this now that
   there is no permission hook.
4. **Do built-in tools ever become executables?** Proposed no. Revisit if someone actually
   wants to replace `edit`.
5. **Auth on the socket.** Filesystem permissions on the socket path locally; SSH remotely.
   No TCP listener, ever, unless someone makes a case that SSH can't cover.
6. **`settings.json`**: fold into `.pirs.toml` as a `[settings]` table so there is one loader
   and one checker. Proposed yes.
7. **Event retention for `since`.** Replaying the current run is cheap and covers the
   dropped-SSH case. Replaying across runs means retaining the event stream, which the
   session log almost is; decide whether the log *becomes* the event stream.
8. **Egress from a jailed server.** The jail needs the provider endpoint reachable and holds
   the API key, so a steered model can still exfiltrate through it. Two answers: the boundary
   operator allowlists the provider endpoint (their problem, standard tooling), or an HTTP
   proxy at the boundary holds the credentials and the jail talks only to it. Both keep the
   LLM abstraction (`pi-ai`) where it belongs, inside the loop server; moving model traffic
   to the client side of the boundary was considered and rejected, because it would split
   the one component that must stay whole.
