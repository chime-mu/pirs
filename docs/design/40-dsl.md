# The DSL (server-side policy)

Status: draft. Prose is the v2 proposal as of 2026-09-20; proposed changes to it are in
`90-decisions.md` and are not applied until accepted.

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

## Slots

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

[[tool_result]]                        # rewrite what the model reads back from a tool
tool = "bash"
run = "./tools/trim-test-output.sh"    # payload on stdin, rewritten result on stdout

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

Eight slots — `input`, `prompt`, `tool_result`, `status`, `widget`, `on`, `command`, `tool` —
plus the `intent` header. Every protocol handler slot has a row here (D-15); `tool_result`
rewrites are logged next to the original (D-21). Every slot accepts `run = "..."` as the way
to say "I need code for this". There is no `guard`: nothing in a `.pirs.toml` decides what
the model may do.

Three of the eight are sugar, and the server has one mechanism under them (D-17, D-18):
`[[status]]` and `[[widget]]` are an `[[on]]` entry whose output is sent as `ui.status` or
`ui.widget`; `[[command]]` is an `[[input]]` entry matching `^/name\b(.*)` with
`handled = true`, plus a description for the manifest the UI receives at attach.

## `run` semantics

Every `run` is a *called* process (D-23): the server spawns it in the loop's cwd with
`PIRS_SOCKET`, `PIRS_LOOP`, `PIRS_SLOT` and `PIRS_SESSION_DIR` in the environment, writes the
slot payload as one JSON line on stdin, and reads the reply from stdout.

- A shell string (`sh -c`) additionally gets each top-level payload field as an environment
  variable (`PIRS_ARG_url`) and as `$url` interpolation, so a one-liner never parses JSON
  (D-24). Its stdout is the reply as text; a non-zero exit is reported as an error with
  stderr as the message.
- A path to an executable replies with one JSON line on stdout, the same framing as the
  socket, so a one-shot executable never has to know the socket exists and the same handler
  code works on either. If it does open the socket, it can call `ui.*`, `loop.*` like any
  client.
- There is no `persistent` flag (D-16). A long-lived extension is started from
  `[[on]] event = "start" run = "./watcher"`; `on` handlers are not waited for, so the process
  lives on, connects to the socket, registers the slots it wants, and is a *connected*
  client until the loop closes and the server kills every process it spawned for that loop.

Testable from a shell: `echo '{"url":"https://x"}' | ./tools/fetch.py`.

## Coverage

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
| several `tool_result`s for one tool | applied in file order |
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

## Open questions in this layer

- **Which DSL files apply to a loop?** Proposed: global + the loop's cwd project files, read
  at `loop.create`, on `loop.reload`, and whenever one of the loop's own tools writes a
  policy file (D-33). Not otherwise re-read per turn.
- **`settings.json`**: fold into `.pirs.toml` as a `[settings]` table so there is one loader
  and one checker. Proposed yes.
