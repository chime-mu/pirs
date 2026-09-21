# Decisions

Append-only. One entry per decision, one paragraph, dated, with a status: **proposed**,
**accepted**, **rejected**, **superseded by D-nn**. A proposed entry changes nothing until it
is accepted; then the prose in the layer file is edited to match and the entry stays here as
the record. Entries are grouped by layer so a discussion can stay on one layer at a time.

Entries D-02 to D-18 were seeded on 2026-09-20 from a review of the v2 proposal and are
proposed, undiscussed. D-19 to D-22 are the arguments that were in the north star before it
was cut to one page; they were already settled and are recorded as accepted. D-23 and D-24
came out of the north-star discussion on the extension model and belong to lower layers.

## Process

**D-01 · accepted · 2026-09-20 · Layered design documents and a decision log.**
The single proposal is split into north star, functionality, architecture, protocol and DSL
files, plus this log. Detail is written for the next phase only. Each phase starts with a
brief naming the scenarios it enables, the edges and messages it adds, and the test that
proves it; the brief is approved before code and the test is the acceptance. Changes to any
layer enter here as proposed first.

## North star

**D-19 · accepted · 2026-09-20 · No checks inside the loop.**
pi runs unrestricted by default on the grounds that once an agent can write and run code,
sandboxing inside the tool is theatre; if you need containment, use a container. This design
agrees, and goes one step further than v1 of the proposal did: pirs performs no security
checks inside the loop and ships no feature that could be mistaken for one. An earlier draft
had a `[[guard]]` slot for the foot-gun class (`rm -rf`, edits under `.git`). It is gone. A
pattern match living in the process the model controls cannot hold against a model that has
been steered into routing around it — the sandbox escapes reported against OpenAI's and
Anthropic's own agents in 2026 are the demonstration — and a feature that *looks* like
enforcement makes people run on untrusted input without the boundary that would actually
protect them. That is worse than having nothing. Containment is a boundary around the
*server*, and the design's job is to make running the server inside one boring; see
"Containment" in `20-architecture.md`. If an accident-class convenience ever proves
necessary, it returns under a name that cannot be read as enforcement (`confirm`, never
`guard`, never `block`). The `tool_call` permission hook is absent from the protocol for
the same reason.

**D-20 · accepted · 2026-09-20 · A daemon instead of tmux.**
pi has no long-running server; its answer to detach, multiplexing and long jobs is tmux, and
it rejects built-in background processes because lifecycle, buffering and cleanup are
complexity. This design takes on exactly that complexity: a server that owns loops,
auto-starts, idles out, replays events after a dropped link, negotiates versions with
clients. The reason is principle 7. A multiplexer that does not own the loop can only guess
whether a pane is working, blocked or idle; a server that owns it knows. The cost is real
and every item of it appears in the protocol tables and open questions; if that list grows
past what the status guarantee is worth, tmux is the fallback.

**D-21 · accepted · 2026-09-20 · Injected context must stay visible.**
The strongest claim in pi's favour is that you see exactly what context the model received,
and its sharpest criticism of other tools is context injected behind the user's back.
`[[prompt]] run = ...` and `tool_result` rewrites are that, unless they are inspectable. So:
`pirs check` prints the fully assembled system prompt, not just the slot list; the session
log records the post-rewrite tool result together with the handler that rewrote it; and the
TUI can show either on request. A DSL that could not meet this would be worse than the
plugin system it replaces.

**D-22 · accepted · 2026-09-20 · Customisations are declarations or separate programs, never code inside the tool.**
pirs started as a port of pi, and the argument for this design is mostly pi's own, as stated
in Mario Zechner's November 2025 write-up — written before pi grew its in-process TypeScript
extension system. That post argues for a minimal core; a headless agent behind RPC and JSON
modes with the TUI as one client among possible others; tools as external CLI programs
rather than plugins or MCP; sub-agents spawned explicitly so they stay observable; and
state kept in files rather than in the model. This design keeps all of that.

Stated precisely, since the post carries less weight here than v2 of the proposal let it:
(1) tools the model calls are executables, which agrees with the post; (2) hooks into the
loop — prompt, input, status, reactions to events — are declarations first (bet 1) and
separate processes second (principle 6), both speaking the same vocabulary as every other
peer (principle 2), and the post has no opinion on this because it predates the need;
(3) UI hooks do not exist, because a UI is a peer. This departs from *current* pi's
in-process API, which `pi-ext` ports, and accepts losing pi extension compatibility — a
deliberate trade of milestone 1's main result, justified by the two bets, not the post.

Why it is right: a failing extension is a dead process, not a wedged loop (the in-process
host already carries a structural deadlock hazard, see HANDOFF.md); there is one surface to
keep, the protocol, which the schema snapshot tests, instead of pi's `pi`/`ctx` API; an
extension runs where the server runs, so containment covers it for free; and a script that
reads JSON on stdin can be written and tested by the model without pirs.

The costs, stated so they are not waved at: two programming models (a one-shot executable
is a function; anything stateful is a small client); no typed API, so payload errors appear
at runtime, mitigated by docs and the schema; dependencies are the user's problem, though
shell and stdlib Python cover the common case; the pi ecosystem and 71 loading examples are
lost. Latency is not a cost: a spawn per hook is a millisecond against a model round trip.

One argument not to borrow: the post recommends CLI tools for *token* economy — the model
reads a README only when it needs the tool. This design chooses processes for *toolchain*
independence — any language, no build step, testable with `echo | ./tool`. Both are good
reasons; they are different reasons.

The tripwire for bet 1: the day a DSL slot needs a conditional or a loop, the answer is
`run =`, never a new field. Otherwise the DSL becomes a plugin system written in TOML, the
worst of both.

## Functionality

**D-02 · superseded by D-37 · 2026-09-20 · Print mode and dialogs.**
A one-shot `pirs "prompt"` run with no UI attached can still hit a `ui.dialog` opened by an
extension or the loop. Decide the behaviour: answer from the controlling tty when there is
one, otherwise cancel the dialog with an error the model sees. Without a decision, S1 can
block forever on a question nobody can see.

**D-25 · accepted · 2026-09-20 · Several agents per directory; a new one by default.**
An agent is a session and a directory can hold any number. `pirs "prompt"` starts a new
agent unless asked to continue one, by name or from a list; the UI lists all of them. The
first draft of S2 said print mode picks up "the same session", which was wrong.

**D-26 · accepted · 2026-09-20 · Policy files live with the server.**
A declaration file sits next to the project it applies to and personal ones in the home
directory of the machine the server runs on. For a remote server the personal policy must be
there too; copying it from the client is a later convenience, not part of the first version.

**D-27 · accepted · 2026-09-20 · Editing needs a terminal that reaches the server.**
The read-only file page works from anywhere because the server reads the file. Editing does
not: the user's editor runs in a terminal on the machine the server is on. Locally the UI
suspends into `$EDITOR`; remotely or in a jail that is a pane beside the UI — tmux today, the
terminal page later. This is the first scenario that makes the pty server more than a
nicety, and phase 8 may move earlier because of it. The first draft's "`ssh <server>
$EDITOR`" was that pane, unnamed.

**D-28 · accepted · 2026-09-20 · A one-shot closes its agent on exit.**
A running agent and a conversation on disk are different things. `pirs "prompt"` starts an
agent, runs, and closes it when the command exits; the conversation stays and can be
continued, which starts a fresh agent on it. The sidebar shows running agents and offers past
conversations separately. Agents started from the UI live until closed. Many one-shots
leave nothing running.

**D-29 · accepted · 2026-09-20 · The editor opens in a pane beside the UI; the UI never suspends.**
Refines D-27. Suspending the UI into `$EDITOR` is the old in-process habit and a bad
experience, so the local case uses a pane too. Inside tmux the UI asks tmux for a pane and
runs the editor there, with ssh in front for a remote or jailed server; later the terminal
page does the same without tmux. Local and remote work the same way. Client-side only.

**D-30 · accepted · 2026-09-20 · Two kinds of customisation, placed by one rule.**
About what the agent does — instructions, tools, reactions, commands that run in the
project — lives with the agent, on the server, as policy. About how things look and what
keys do — theme, bindings, status format, page placement — lives with the UI, on the user's
machine, as UI config. A customisation needing both is two files. The UI's own
extensibility is the UI's business and never a server or protocol concern.

**D-32 · accepted · 2026-09-20 · The placement test, and commands on both sides.**
Clarifies D-30, whose agent-versus-screen wording could not place a slash command, which
is typed on the screen and runs on the server. The test: if the user switched UI or drove
the agent from a script, should it still happen? Yes means policy on the server; only on
this screen means UI config. Commands exist on both sides, placed by that test: `/handoff`
(runs in the project) is policy, `/theme` is UI config, `!cmd` is policy because it runs in
the project and its output enters the conversation. The UI merges the server's commands,
learned at attach, with its own into one list; a name on both sides is a reported conflict.

**D-33 · accepted · 2026-09-20 · Ask, write, live is the acceptance test; policy reloads on the agent's own write.**
The north star now names three wants — many agents, one view of who needs attention, more
than one machine — and that shaping the tool must stay as easy as asking. pi's loop (ask,
the agent writes a file, the tool loads it, try) is preserved by two facts: policy and tools
live in the project where the agent already writes files, and the server reloads policy
when one of the loop's own tools writes a policy file, so the change is live next turn. The
same loop covers scripts (testable from the shell before pirs is involved) and connected
processes (startable from the shell as ordinary clients). The agent must have the policy
vocabulary and the wire messages in its prompt. This retires "not re-read per turn" in the
DSL layer as the whole answer. `pirs ext new` becomes a convenience for when no agent runs.

**D-34 · accepted · 2026-09-20 · A local server always exists; shaping the UI is a local agent's job.**
The one place the split costs moldability against pi is the UI: an agent on a build box
cannot reach the UI's config on the laptop, and the UI is config rather than code. Answer: a
machine that runs a UI can run an agent, so there is always a local server, and UI shaping is
done by a local agent editing the UI's config file, which the UI reloads. What is lost
against pi — an agent writing a new kind of page — is the trade for a replaceable UI, and
stays buildable as the UI's own extension mechanism later.

**D-35 · accepted · 2026-09-20 · A web UI must be buildable with no change to the loop server.**
Scenario S23. Browser page as the client over a websocket, a bridge that serves the page and
forwards messages, loop server unchanged. Needing a server change means the protocol is
missing something, the same rule as for the TUI. The web UI is where the pty server stops
being optional.

## Architecture

**D-03 · accepted · 2026-09-21 · `pirs-client` is a library; `pirs-tui` depends on it.**
Connection, `hello`, auto-start, `servers.toml` resolution, the bridge transport, and
subscribe-with-replay are client plumbing every client needs. The crate table currently has
`pirs-tui` depending on `pirs-protocol` only, which forces the TUI to reimplement all of it.
Make `pirs-client` that library, let `pirs-tui` depend on it, and move the subcommands
(print, `check`, `ext`, `proxy`) into the `pirs` bin. The rule that neither `pirs-client` nor
`pirs-tui` depends on `pi-ai` or `pi-agent` is unchanged and remains the edge worth naming.

**D-04 · accepted · 2026-09-21 · `pi-cli` gets a phase in which it is split.**
The crate table names `pi-cli`'s fate nowhere. It is the largest crate today and becomes
`pirs-server`, `pirs-tui` and the bin. Say in the phases list when the split happens
(proposed: server-side code moves in phase 1, the interactive mode in phase 3, the crate
is deleted at the end of phase 3).

**D-05 · accepted · 2026-09-21 · One `command` transport in `servers.toml`.**
Replace the `ssh` and `container` kinds with a single `command = "..."` that names a stdio
bridge. SSH is `ssh host pirs proxy`; a container is `docker exec -i jail pirs proxy`; a
local socket is the default. pirs then knows nothing about containers or SSH, which is
principle 6 applied to the client side. Container images, mounts and lifecycle stay
buildable by whoever runs the container.

**D-06 · accepted · 2026-09-21 · The session log is the event stream.**
Closes the retention question. `seq` is the index into the loop's session log, and
`subscribe … since` replays from it, across runs. Streaming deltas are not sequenced and not
replayed; complete messages, status changes and dialogs are. This removes the "bounded by
the current run" rule and a second retention mechanism.

**D-07 · accepted · 2026-09-21 · `fs.read` is the stated exception to principle 8.**
Reading a file does not need the loop. `fs.list` and `fs.read` exist because the alternative
is a separate file-server component, which is heavier than two read-only requests. Say so
in the architecture file, so the principle remains usable as a test. Drop the clause "scoped
to what the loop itself could read", which claims nothing because the loop can read anything
the server user can.

**D-08 · accepted · 2026-09-21 · `pirs-protocol` carries its own message types.**
`loop.turn_end { messages }` needs a message type on the wire, and `pi-ai` already has one.
The protocol crate defines its own, deliberately, and `pirs-server` converts at the edge.
Without this being written down, the first implementer will reach for a `pi-ai` dependency
in `pirs-protocol`, which the architecture test would then have to refuse.

**D-09 · accepted · 2026-09-21 · The `Command::new` ban is a module-level allow.**
`clippy.toml` `disallowed_methods` cannot exempt one module. The process runner carries
`#[allow(clippy::disallowed_methods)]` and everything else in `pirs-server` is denied. Same
effect; the text should claim what the tool can do.

**D-23 · accepted · 2026-09-21 · Two bindings, one vocabulary; the server is a hub.**
"One protocol" is true of payloads, not of programming models, and the architecture should
say so. A `tool.fetch` request has the same JSON shape on stdin and on the socket, and that
sameness is the promise: a handler can be promoted from one binding to the other without
being rewritten. The two bindings are *called* — the server spawns the process, writes one
slot payload, reads one reply, the process exits; a shell string is the degenerate form —
and *connected* — the process opens the socket, says hello, registers slots, lives, and can
also issue any client request, so it can steer the loop as well as serve it. Direction of
control: the server owns state and timing, clients own intent, and the server initiates
nothing except a request to a slot somebody registered (with that registrant's timeout) and
a call to a process it spawned itself. It waits for nothing else; observers are never waited
on. Lifecycle: every process the server spawns belongs to a loop and dies with it. Pairs
with D-16: an `on start` handler is fire and forget, so the process it starts can connect,
register, and become a connected client until the loop closes.

**D-31 · accepted · 2026-09-21 · Paths are opaque and owned by their server; platform promise.**
Everything about an agent belongs to the server that runs it. A path in any message is a
label produced by that server and only ever handed back to it; no client parses, joins or
normalises one, and no client touches a remote filesystem directly (the server reads files
for it). Version agreement happens at connect. Platform promise for the first version:
Linux and macOS servers; Linux, macOS and Windows clients; Windows servers later, which
requires deciding the shell for `run` strings and the local transport (named pipe or
`AF_UNIX`). Enforced by the protocol's path type carrying no filesystem operations and by
the TUI depending on nothing that interprets paths.

**D-36 · accepted · 2026-09-21 · Transport bridges are separate components; whoever opens a port owns login.**
The ssh proxy and a web bridge are the same kind of thing: a component that forwards the
protocol between one transport and the server's socket, with no knowledge of loops. A
bridge that listens on the network is a separate thing the user chooses to run, owns its own
authentication, and is documented as network-exposed. The loop server never listens on the
network and never speaks HTTP; the no-TCP rule in the architecture's open questions becomes
this decision.

## Protocol

**D-10 · accepted · 2026-09-20 · Newline-delimited JSON on the socket and on stdio.**
Drop the length-prefixed framing, the encoding byte, and `encodings` in `hello`. The
protocol file's own numbers say the encoding never matters for speed, so the hedge buys
nothing and costs a second framing. One framing means `socat` and `echo |` behave the same on
both. A binary encoding, if ever needed, is a protocol major bump regardless. Accepted 2026-09-20; the prefix's only benefits, raw bytes and skipping without parsing, are needed nowhere in the functionality file, and terminal data belongs to the pty server's protocol.

**D-11 · accepted · 2026-09-20 · Drop `blob.get`; a ref is a server path served by `fs.read`.**
A by-reference payload is a path in the session directory on the server, and `fs.read`
already reads server paths and already returns a ref above the threshold. Define a ref as
such a path and let `fs.read { loop, path }` serve it. One request instead of two.

**D-12 · accepted · 2026-09-20 · Drop `state.set` / `state.get`.**
Nothing in the DSL or the examples uses it; the todo example uses a file. Executables get
`PIRS_SESSION_DIR` in their environment and use the filesystem. Returns if a real need
appears, which is the standard applied to every other removed slot.

**D-13 · accepted · 2026-09-20 · Drop the `command.<name>` slot.**
A slash command is text arriving on the `input` slot with a name and a description attached.
The DSL keeps `[[command]]` as sugar (see D-18); the protocol loses a slot. So a UI can list
and complete commands, `loop.attach` returns the loop's merged manifest: tools, commands,
status and widget keys.

**D-14 · superseded by D-37 · 2026-09-20 · Add `dialog.open`, `loop.reload` and `dsl.check`.**
Three requests the prose relies on but the table omits. `dialog.open { loop, kind, title,
text, options }` returns the answer and is the only way an extension can make a loop
`blocked`. `loop.reload` re-reads the loop's DSL files. `dsl.check { cwd }` runs the loader
and checker on the server where the files live, so `pirs check` stays a thin client and works
against a jailed server.

**D-37 · accepted · 2026-09-21 · Only the model asks; no dialog primitive; `loop.reload` and `dsl.check` added.**
Replaces D-14 and retires D-02. The proposal for `dialog.open` rested on the claim that a
program the agent runs must be able to ask the user something the model does not know
about. That claim is false: a program that needs something returns a result saying so, and
the model asks in text, stops, and calls it again with the answer, which is how every tool
already works and how Claude Code's own questions work today (its permission prompts are
the checks pirs has removed on purpose). So the model is the only thing that asks a person
anything, in text; the answer is a prompt. Consequences: `ui.dialog`, `ui.dialog_closed`
and `dialog.reply` are gone; status is `working` or `idle`, with no `blocked`; "needs
attention" is stopped (server fact) plus not looked at since (UI fact), never a reading of
the model's prose; the "who answers a dialog" question is moot. A confirm-before-deploy
convenience is a script returning "not without a yes", which the model relays, at the same
discretion it has everywhere; D-19's "confirm, never guard" stays with that meaning.
Structured questions are built without any new message: declare an `ask` tool whose script
returns "asked, now stop"; the model's call carries question and options as JSON that every
client already receives in the message stream, and a UI that recognises the tool draws a
picker and sends the choice as a prompt. That needs one client-side extension point in the
reference TUI (S6), never a protocol element. `loop.reload` and `dsl.check` are added: S5
and S4 rely on them.

**D-15 · accepted · 2026-09-21 · `tool_result` gets a DSL entry or goes.**
The rule is "a slot here and a DSL entry there". `tool_result` has no DSL row, so today it is
a slot only an executable can use, which contradicts bet 1. Either add
`[[tool_result]] tool = "…" run = "…"` to the DSL, or remove the slot until a real need
appears. Recommendation: add the entry, because the visibility argument in the north star
already depends on it.

## DSL

**D-16 · accepted · 2026-09-21 · Drop `persistent = true`.**
An executable that opens the socket and calls `register` is already a persistent extension,
and the run semantics say it may. A long-lived extension is started from
`[[on]] event = "start" run = "./watcher"`, registers the handlers it wants, and is
disconnected when the loop closes. One lifecycle instead of two.

**D-17 · accepted · 2026-09-21 · `[[status]]` and `[[widget]]` desugar to `on` plus `ui.*`.**
They stay in the DSL because they read well, but the server has one execution path for a
scheduled `run`: an `on` entry whose output is sent as `ui.status` or `ui.widget`. State
this so the checker and the runner do not grow three variants of the same thing.

**D-18 · accepted · 2026-09-21 · `[[command]]` desugars to an `input` entry.**
`[[command]] name = "handoff" run = …` is `[[input]] match = '^/handoff\b(.*)' handled = true
run = …` plus a description for the manifest. Pairs with D-13.

**D-24 · accepted · 2026-09-21 · How a shell-string `run` receives its payload.**
The DSL says a shell string gets regex groups as `$1` and `$args`, and an executable gets the
slot payload as JSON on stdin; what a shell-string `[[tool]]` with `params` receives is
unspecified. Proposed: every called process gets the JSON payload on stdin, and a shell
string additionally gets each top-level field as an environment variable (`PIRS_ARG_url`)
and `$name` interpolation, so a one-liner never has to parse JSON.

**D-38 · proposed · 2026-09-21 · `loop.list` takes an optional `cwd` and returns stored conversations.**
Recorded by the orchestrator during phase 0. S2 says the UI "has a way to open past
conversations" and D-28 says the sidebar "offers past conversations separately"; the plan's
`pirs --list` prints the conversations in the cwd and `--continue <name-or-id>` picks one.
Nothing in the request table can list conversations on disk: `loop.list` lists running loops
and `fs.list` needs a server path the client must not construct (D-31). Rather than add a
request, `loop.list { cwd? }` returns `{ loops, conversations }`, where `conversations` is
the server's list of stored conversations for `cwd` (id, name, cwd, path, updated) and is
empty when `cwd` is absent. `loop.create { session }` continues one by id. Evidence: the
phase 1 acceptance script (`pirs --list` shows two conversations) cannot be written otherwise.

**D-39 · proposed · 2026-09-21 · `fs.read` serves the requested path in full; refs appear in events.**
Recorded by the orchestrator during phase 1. `30-protocol.md` says `fs.read` "returns content or
`{ ref, bytes }` above the threshold, and a `ref` is itself a server path that `fs.read` serves"
(D-11). Read literally, `fs.read` on any file above 64 KB returns a ref naming that same file,
which `fs.read` would again answer with a ref: a file page (S13) could never show a file over
the threshold. The threshold exists to keep unsolicited lines small (events, tool results); an
explicit read is the client asking for the bytes. So: by-reference payloads are produced by
the server for tool results and messages above the threshold, written under the loop's
session directory, and carried in events as `{ ref, bytes }`; `fs.read { loop, path }`
returns the content of `path` whether it is a ref or an ordinary file, refusing only files
above a hard cap (16 MB, `INVALID_PARAMS`). The `{ ref, bytes }` form stays in the schema as
the shape of a by-reference payload. Evidence: the phase 1 acceptance ("a >64 KB scripted tool
result arrives as a `ref` and `fs.read` returns it") is satisfiable only this way.

**D-40 · proposed · 2026-09-21 · `dsl.check` returns the rendered merged policy.**
Recorded by the orchestrator during phase 2. S4 and `40-dsl.md` say `pirs check` prints the
merged policy, every conflict, and the assembled system prompt. `DslCheckResult` carries
`files`, `manifest`, `conflicts` and `system_prompt`; the manifest shows tools, commands and
keys but not the `input`, `tool_result` and `on` rules, the `[settings]` table or the intents,
so the client cannot print the merged result without re-implementing the loader. Add one
field, `rendered: String`, the server's human-readable rendering of the composed policy
(files in order with priority, each slot's entries with their origin, settings and their
sources, intents). It is display text, not a second schema. Evidence: the phase 2 review found
`dsl::render` had no production caller.

**D-41 · proposed · 2026-09-21 · `[settings]` carries `tool_execution`.**
Recorded by the orchestrator during phase 2. The plan folds `settings.json` into a
`[settings]` table (open question 6 taken as yes) and names `model`, `thinking`, `tools`.
`settings.json` also had `toolExecution` (`parallel` | `sequential`), which the loop server
honours; dropping it would remove a capability rather than relocate it. The table therefore
has four keys: `model`, `thinking`, `tools`, `tool_execution`. `deny_unknown_fields` keeps
it closed. `settings.json` is no longer read; `models.json` (a provider catalogue, not a
setting) still is.
