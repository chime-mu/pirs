# pirs status

2026-09-21. Branch `adaptable`: phases 0–7 of [`docs/design/PLAN.md`](docs/design/PLAN.md),
one commit each.

pirs is now a loop server behind a unix socket that owns every agent, and a set of clients
at the same door: the `pirs` command in print mode (`pirs "prompt"`, `--continue`,
`--list`, `wait`), the terminal UI (`pirs tui`), and anything else that speaks
[`docs/protocol.md`](docs/protocol.md). Behaviour is changed by `*.pirs.toml` policy files
in `~/.pirs/ext/` and `<project>/.pirs/ext/` — eight slots plus a `[settings]` table,
readable with `pirs check` — and by programs those files name: a `[[tool]] run = <path>`
executable in any language, a process started by `[[on]] event = "start"` that connects back
and registers as a handler, or a `[[tool]] loop = { … }` that runs a child agent and returns
its answer. Servers reach across machines and containers through `pirs proxy` and
`~/.pirs/servers.toml` (ssh, `docker exec`), with reconnect and replay from the last `seq`.
`pirs ext new "<sentence>"` writes a policy file from a sentence and keeps it as the file's
`intent`. pi's TypeScript extension compatibility is gone, with `pi-cli` and `pi-ext`:
a customisation is a declaration or a separate program (D-22 in
[`docs/design/90-decisions.md`](docs/design/90-decisions.md)). The session *format* stays
pi-readable; the session *directory* does not.

## Phases

| Phase | Title | Commit | Scenarios | Acceptance | Passed |
|---|---|---|---|---|---|
| 0 | Design freeze in code | `f424c04` | — | `phase-0.sh` | 113 tests, 4 checks |
| 1 | Server and print client | `2f7f5e2` | S1, S2 | `phase-1.sh` | 254 tests, 14 checks |
| 2 | Policy without code | `5c08088` | S3, S4, S5 (declarations, shell) | `phase-2.sh` | 346 tests, 16 checks |
| 3 | TUI as a client | `b243abb` | S6, S11, S12, S13, S14 | `phase-3.sh` | 303 tests, 10 checks |
| 4 | Executables | `2583315` | S5 (processes), S7, S8, S9 | `phase-4.sh` | 312 tests, 21 checks |
| 5 | Several agents | `cec282d` | S16, S17 | `phase-5.sh` | 324 tests, 12 checks |
| 6 | Remote and contained servers | `dfec48b` | S18, S19, S20, S21 | `phase-6.sh` | 354 tests, 13 checks |
| 7 | Intent tooling | 9b0c92d | S10 | `phase-7.sh` | 380 tests, 27 checks |

Test counts are the whole workspace at that phase. Phase 3 is lower than phase 2 because
`pi-cli` and its 71 tests were deleted with the old interactive mode (D-04).

Now: `cargo test --workspace` is **381 tests, 0 failures**, and all eight acceptance
scripts pass on this tree (117 checks in total).

Scenario titles, from [`docs/design/10-functionality.md`](docs/design/10-functionality.md):

| | | | |
|---|---|---|---|
| S1 Ask and get an answer | S2 Several agents in one directory | S3 Customise without code | S4 Check before running |
| S5 Ask the agent to extend pirs | S6 Customise the UI | S7 Give the model a tool in any language | S8 Replace or wrap a built-in |
| S9 A long-lived extension | S10 Write the policy from a sentence | S11 A sidebar of agents | S12 Needs attention is a fact |
| S13 Read what the agent is editing | S14 Edit with my own editor | S15 Arrange the UI my way | S16 An agent that asks another agent |
| S17 Wait for an agent from a script | S18 Agents on another machine | S19 A dropped link | S20 Run the agent in a jail |
| S21 Mixed machines | | | |

`PLAN.md`'s **Enables** line for phase 3 and the header of `phase-3.sh` both read
"S6, S10–S14"; S10 is phase 7's scenario in `10-functionality.md` and in `phase-7.sh`, so
the table above reads that range as S11–S14. S15 is client-side config only and needs no
server work.

## Verified

```bash
cargo build --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check bans                        # from phase 1; needs `cargo install cargo-deny`
scripts/acceptance/phase-0.sh                # … through phase-7.sh
```

All of it runs with no network and no credentials: model calls in the scripts and in the
tests use the faux provider (`--model faux/scripted`, `PIRS_FAUX_SCRIPT=<json>`) on a
temporary socket, home and project directory. `scripts/acceptance/README.md` says what each
script checks.

Live Anthropic requests were last verified on **2026-09-18**, before this branch, through
the Claude Code login. The provider code moved unchanged from `pi-cli` into `pirs-server`,
but the new server has **not** been exercised against a live provider on this branch: no
live request of any provider has gone through `pirs-server`.

## Not built

- **S22 · A terminal page.** No pty server, no terminal pages. Phase 8, not run.
- **S23 · A web UI.** Not planned; the protocol is meant to carry it unchanged.
- **Windows servers.** Linux and macOS servers only (D-31); the `run` shell and the local
  transport are undecided. Windows clients are in the promise, untested here.
- **Remote policy sync (D-26).** Policy files must already be on the machine the server
  runs on; copying them from the client is not implemented.
- **The Docker path is documented, not run.** `phase-6.sh` asserts that the `Dockerfile`
  runs `pirs serve` and that `docs/containment.md` documents the `docker exec -i` bridge;
  it never builds or starts the container.
- **Stored conversations on a remote server.** `loop.list { cwd }` sends this machine's
  directory as written (D-31), so a remote server usually has nothing under that path and
  returns an empty conversation list; its *running* agents are still listed
  (`crates/pirs-tui/README.md`).
- **Editing without tmux.** `ctrl-e` needs `$TMUX`; outside tmux the UI says so and does
  nothing (D-27, and S22 is the answer).

## Crates

| Crate | What |
|---|---|
| `pi-ai` | Providers and models: Anthropic and OpenAI streaming, OAuth and API-key credentials, the model registry and `models.json`, the faux scripted provider. |
| `pi-agent` | The agent loop itself: the `AgentTool` trait, the message union, tool execution and hooks, the event protocol. |
| `pirs-protocol` | Every wire type, the JSON-line `Frame`, `PROTOCOL_VERSION`, and the schema snapshot test against `docs/protocol.schema.json`. `serde` and `schemars` only. |
| `pirs-server` | The loop server: the socket, loops and their sessions, built-in tools, the policy DSL and `dsl.check`, handler dispatch, the session log as event stream, `fs.read`/`fs.list`, idle exit. |
| `pirs-client` | Connecting to one or many servers: auto-start, `hello`, requests by id, the subscribed event stream, reconnect with `since`. Protocol types only. |
| `pirs-tui` | The reference UI: sidebar across servers, agent and file pages, widgets and status keys, `tui.toml`, `[[render]]` hooks, the tmux editor pane, and the `--headless` script mode the tests drive. |
| `pirs` | The binary: `pirs "prompt"`, `serve`, `stop`, `proxy`, `check`, `wait`, `ext new`/`ext regen`, `tui`. |
