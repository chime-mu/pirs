# Architecture — components, crates, edges, phases

Status: draft. Prose is the v2 proposal as of 2026-09-20; proposed changes to it are in
`90-decisions.md` and are not applied until accepted.

pirs is a set of components with standardised protocols, combinable in different ways. The
loop server is the one we must build; the others are optional, replaceable, or someone
else's.

| Component | Protocol | Status |
|---|---|---|
| **loop server** | the pirs protocol: loops, events, handlers, read-only `fs.*` | core; the subject of `30-protocol.md` |
| LLM abstraction (`pi-ai`) | Rust API, inside the loop server | core component of the server; providers, streaming, model registry, credentials. It stays on the server side of every boundary |
| extension executables | JSON-RPC on stdin/stdout, optionally the socket | core |
| pty server | a pty protocol: spawn, input, output, resize, scrollback | optional and separate; tmux (control mode) can stand in until someone writes one. Never part of the loop server |
| TUI | client of any number of the above | reference client; one composition among possible ones |

## Server

Owns loops. Each loop has a session log, cwd, model, active tool set, and the DSL files that
apply to it. The server has no terminal; it never renders anything. It survives client
disconnects; loops keep running.

Lifecycle: `pirs serve` starts it explicitly on `$XDG_RUNTIME_DIR/pirs.sock` (or
`~/.pirs/pirs.sock`). Every client auto-starts it if the socket is missing. It exits on
`pirs stop` or when no loop is running and no client has been attached for a configurable
idle time. Explicit start plus auto-start covers both "I want to reason about it" and "just
work".

## Clients

Everything else. A client connects to one or more servers, optionally subscribes to a loop's
events, and issues requests. Four kinds ship; anyone can write a fifth.

| Client | What it is |
|---|---|
| `pirs` | print mode: create/attach a loop, send a prompt, stream to stdout, exit |
| `pirs tui` | the interactive UI: a sidebar of agents across servers, and pages |
| extension executables | spawned by the server from a DSL `run =`, connected as a client that registers handlers |
| a loop | a `tool` that prompts another loop and waits for it to go idle |

Two bindings, one vocabulary (D-23). A *called* process is spawned by the server, gets one
slot payload on stdin, replies on stdout, and exits; every DSL `run` is one. A *connected*
process opens the socket, says hello, registers slots, lives, and can also issue any client
request; the TUI, print mode and long-lived extensions are these. Payloads have the same
shape in both, so a handler is promoted from called to connected without being rewritten.
The server is a hub: it owns state and timing, clients own intent, and it initiates nothing
except a request to a slot somebody registered (with that registrant's timeout) and a call
to a process it spawned itself. It waits for nothing else; observers are never waited on.
Every process the server spawns belongs to a loop and dies with it.

## Several servers, local and remote

A client may hold connections to several servers at once. A loop is identified by
`(server, loop)`; the TUI's sidebar is the union across servers and a page opens on
`office:3` as easily as `local:1`.

A remote server is reached through a *bridge*: a command that forwards protocol lines
between its stdio and the remote unix socket (D-05, D-36). `~/.pirs/servers.toml` names each
server and its bridge command, and nothing else: `ssh build pirs proxy` for a machine,
`docker exec -i jail pirs proxy` for a container, no command for the local socket. pirs
knows nothing about SSH or containers; SSH provides authentication and encryption, and the
container runtime provides the boundary. The loop server never listens on the network and
never speaks HTTP. A bridge that does open a port — a web bridge serving a browser UI (S23)
— is a separate component the user chooses to run, owns its own login, and is documented as
network-exposed. Nothing new to audit in a corporate environment beyond a login the user
already has.

Everything belonging to a loop — cwd, tools, extensions, DSL files, session log — lives on
the server that runs it. A loop on the build box edits the build box's files and runs the
build box's extensions. The client only renders and steers.

Consequences for the protocol (all in `30-protocol.md`): `hello` carries versions so two
machines on different pirs releases can negotiate or refuse cleanly; events carry a per-loop
`seq` and `subscribe` accepts `since`, so a client that lost its SSH link can catch up on the
messages it missed while it was gone — the session log *is* the event stream, `seq` indexes
it, and replay reads from it across runs, with streaming deltas unsequenced and not replayed
(D-06); large payloads travel by reference because a path on
the server is meaningless to a remote client.

Cross-server orchestration (a loop on one server driving a loop on another) is a client
concern in v0. Making the server itself a client of other servers is possible with the same
protocol but deferred.

## Containment

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
make it unremarkable: a `servers.toml` entry whose bridge command is `docker exec -i <jail>
pirs proxy` (D-05); `pirs serve` documented as the entrypoint of a container image; `pirs tui` attaching to a jailed loop with no visible difference from a local one.

What the boundary does not close by itself is egress: a jailed server still holds a model
API key and needs the network to reach the provider, so the provider endpoint is the
exfiltration channel. Two answers: the boundary operator allowlists the provider endpoint
(their problem, standard tooling), or an HTTP proxy at the boundary holds the credentials
and the jail talks only to it. Both keep `pi-ai` where it belongs, inside the loop server;
moving model traffic to the client side of the boundary was considered and rejected,
because it would split the one component that must stay whole.

## Files, paths and platforms

`fs.list` and `fs.read` are the one stated exception to principle 8 (D-07): reading a file
does not need the loop, and they exist because the alternative — a separate file-server
component — is heavier than two read-only requests. They read whatever the server's user can
read; no narrower claim is made.

Every path in any message is a label produced by the server that runs the loop and only
ever handed back to that server (D-31). No client parses, joins or normalises one, and no
client touches a remote filesystem directly. Platform promise for the first version: Linux
and macOS servers; Linux, macOS and Windows clients; Windows servers later, which requires
deciding the shell for `run` strings and the local transport (named pipe or `AF_UNIX`).

## The TUI (a client)

`pirs tui` is the reference client. It is *one* client; someone who wants a different UI
writes a different client against `30-protocol.md`. Nothing below is a protocol concern.

- **Shape.** A sidebar listing agents across every configured server, each with its state,
  and a main area showing a page. Select an agent, see its page.
- **Page kinds.** `agent` (the conversation, widgets, status keys, the files it
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
- Agents needing attention are flagged: stopped is the server's fact, unread is the UI's own;
  no heuristics over the model's text.
- A client-side extension point hands a recognised tool call or content block to user code
  for drawing and input (S6, D-37).
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

## Crates

Crate boundaries are the architecture: a crate can only use what its `Cargo.toml` lists, and
Cargo forbids cycles, so the rules worth having are the ones that become a missing edge.

| Crate | May depend on (internal) | Holds |
|---|---|---|
| `pi-ai` | — | providers, streaming, model registry |
| `pi-agent` | `pi-ai` | the loop |
| `pirs-protocol` | — | every wire type, with `JsonSchema` derives, including its own message type converted from `pi-ai`'s at the server edge (D-08); `serde` and `schemars` only, no `tokio`, no `reqwest` |
| `pirs-server` | `pi-ai`, `pi-agent`, `pirs-protocol` | session log, built-in tools, DSL loader/checker, process runner, socket server |
| `pirs-client` | `pirs-protocol` | the client library: connect, hello, auto-start, `servers.toml` and bridge commands, subscribe with replay (D-03) |
| `pirs-tui` | `pirs-protocol`, `pirs-client` | the reference UI |
| `pirs` (bin) | all of the above | argument parsing and dispatch; `pirs serve` runs the server in-process; print mode, `check`, `ext` and `proxy` are thin clients built on `pirs-client` |

`pi-ext` is removed and `pi-cli` is dissolved: its server-side code moves to `pirs-server`
in phase 1, its interactive mode to `pirs-tui` in phase 3, and the crate is deleted at the
end of phase 3 (D-04). The edge that matters most is the one that is absent: neither
`pirs-tui` nor `pirs-client` depends on `pi-ai` or `pi-agent`. If the TUI ever needs a type from either, the protocol
is missing something, and the fix goes in `pirs-protocol` and `30-protocol.md`, not in the
TUI's `Cargo.toml`.

## Mechanical checks

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
  `std::process::Command::new` and `tokio::process::Command::new`; the process-runner module
  alone carries `#[allow(clippy::disallowed_methods)]` (D-09).
  Every crate denies `unreachable_pub` so its public surface is deliberate.
- **Protocol snapshot.** A test in `pirs-protocol` generates the JSON schema from the types
  and compares it to `docs/protocol.schema.json`. Adding a method or field without updating
  the schema fails CI; "anything not in these three tables does not exist" is a test, not a
  sentence. `cargo public-api --diff` on `pirs-protocol` at release time is what makes the
  `hello` version negotiation trustworthy.

## Phases

Each phase leaves a working binary, and each starts with a brief (see `00-north-star.md`,
working rules) naming the scenarios from `10-functionality.md` it enables.

0. **Design freeze.** `docs/protocol.md` and `docs/dsl.md` written and reviewed. The crate
   carve-up lands here: `pirs-protocol` with the schema snapshot test, and the architecture
   test over `cargo metadata`. The freeze is in code, not only in prose.
1. **Server + print client.** Loop over the socket; `pirs "prompt"` works end to end; TUI
   still the old in-process one. pi-ext still present. `deny.toml` and the per-crate lints
   arrive with `pirs-server`. Scenarios S1, S2.
2. **DSL, shell `run` only.** `input`/`prompt`/`status`/`widget`/`on`; `pirs check`.
   Scenarios S3–S5.
3. **TUI as client.** Sidebar plus agent page; attention flag; the rendering extension point;
   `fs.read`/`fs.changed` and file pages. Scenarios S6, S11–S15.
4. **Executables.** `tool`/`command` with executable `run`; one-shot then persistent; port
   `fetch`. Delete `pi-ext`, `examples/extensions/`, pi-compat docs. Scenarios S7–S9, and the process shape of S5.
5. **Multi-loop.** `loop.wait`, `[[tool]] loop = ...`; the sidebar already spans loops.
   Scenarios S16, S17.
6. **Remote and jailed servers.** `pirs proxy`, `servers.toml` with bridge commands,
   `since` replay, large payloads by reference; the sidebar spans servers; a documented
   container image with `pirs serve` as entrypoint. This is where the security story becomes
   usable, so it should not slip behind 7 and 8. Scenarios S18–S21.
7. **Intent tooling.** `pirs ext new` / `regen`. Scenario S10.
8. **pty server**, if still wanted: a separate component with its own protocol, and a
   `terminal` page kind in the TUI. tmux until then. Never inside the loop server.
   Scenario S22, which S14 may pull earlier and S23 requires.

## Open questions in this layer

- **Auth on the socket.** Filesystem permissions on the socket path locally; whatever the
  bridge provides remotely (D-36).
- **Do built-in tools ever become executables?** Proposed no. Revisit if someone actually
  wants to replace `edit`.
