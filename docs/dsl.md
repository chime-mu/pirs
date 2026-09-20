# The pirs DSL

How to change what pirs does. You write one TOML file; pirs loads it and the change is live
on the next turn. Nothing else is needed — no plugin, no build step, no restart.

## Where files live

| Path | Scope |
|---|---|
| `~/.pirs/ext/*.pirs.toml` | every project |
| `<project>/.pirs/ext/*.pirs.toml` | that project only |

A loop reads both sets from its cwd at creation. They are re-read on `loop.reload`, and
automatically when one of the loop's own tools writes a file in either location — so an
agent asked to change pirs writes the file and the change is live on the next turn.

## `intent`

Required, first in the file. One or two sentences saying what the file is for, in the words
a person would use. It is the shareable unit: the file is a cache of the intent, and
`pirs ext regen <file>` rebuilds the file from it.

```toml
intent = """
Show the current git branch in the status line and refresh it after every turn.
Let me type ?question for a brief answer and !cmd to run a shell command.
"""
```

## Slots

Eight, all arrays of tables. Every slot accepts `run = "..."` — that is how you say "this
needs code".

### `[[input]]` — transform or consume what the user typed

`match` (regex, required) · `replace` (rewrite, `$1`… are the capture groups) ·
`handled` (bool; stop here, never reach the model) · `run`

```toml
[[input]]
match = '^\?(.*)'
replace = "Explain briefly: $1"

[[input]]
match = '^!(.*)'
handled = true
run = "$1"
```

Payload `{ text }`. Reply `{ text }` or `{ handled: true }`.

### `[[prompt]]` — add to the system prompt

`text` · `files` (glob, contents appended) · `run` · `header` (printed above the output)

```toml
[[prompt]]
text = "Prefer small commits. Never rewrite history."

[[prompt]]
files = ".claude/rules/*.md"

[[prompt]]
run = "git log --oneline -5"
header = "Recent commits:"
```

Payload `{ system_prompt }`. Reply `{ append }` or `{ replace }`.

### `[[tool_result]]` — rewrite what the model reads back from a tool

`tool` (tool name, required) · `run` (required)

```toml
[[tool_result]]
tool = "bash"
run = "./tools/trim-test-output.sh"    # payload on stdin, rewritten result on stdout
```

Payload `{ tool, args, result }`. Reply `{ result }`. Both the original and the rewrite are
recorded in the session log.

### `[[status]]` — one key in the status line

`key` (required) · `run` (required) · `on` (events that refresh it)

```toml
[[status]]
key = "branch"
run = "git branch --show-current"
on = ["start", "turn_end"]
```

### `[[widget]]` — lines the UI shows beside the conversation

`key` (required) · `file` or `run` · `on`

```toml
[[widget]]
key = "todo"
file = ".pirs/todo.md"
on = ["start", "tool_result"]
```

### `[[on]]` — run something at an event

`event` (required: `start`, `turn_end`, `run_end`, `tool_result`, `reload`) · `run`
(required) · `quiet` (discard output)

```toml
[[on]]
event = "turn_end"
run = "git add -A && git commit -qm 'pirs checkpoint'"
quiet = true
```

Fire and forget: the loop does not wait for the reply. `on start` is therefore also how a
long-lived extension is started — see "Connected processes".

### `[[command]]` — a `/name` the user can type

`name` (required) · `description` (shown in the UI's command list) · `run`

```toml
[[command]]
name = "handoff"
description = "Start a fresh session with a summary"
run = "./scripts/handoff.sh $args"
```

`$args` is everything after the command name.

### `[[tool]]` — give the model an executable

`name` (required) · `description` · `params.<field> = { type, description, default }` ·
`run` · `timeout` (default 60 s) · `disabled` (bool) · `wrap` (a program the real tool's
call is routed through) · `loop = { model, prompt, wait }` instead of `run`

```toml
[[tool]]
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

Payload `{ args, id }`. Reply `{ content, details }` or `{ error }`.

## `[settings]`

One table per file, merged like the slots. `[settings]` replaces `settings.json`; its keys
are defined in phase 2.

## `run` semantics

Every `run` is a *called* process. pirs spawns it in the loop's cwd, writes the slot payload
as one JSON line on stdin, and reads the reply from stdout. `PIRS_SOCKET`, `PIRS_LOOP`,
`PIRS_SLOT` and `PIRS_SESSION_DIR` are in the environment.

- **A shell string** (anything that is not a path to an executable) runs under `sh -c`. It
  additionally gets each top-level payload field as `PIRS_ARG_<field>` and as `$field`
  interpolation, so a one-liner never parses JSON. Its stdout is the reply, as text.
- **A path to an executable** replies with one JSON line on stdout — the same shape as the
  socket, so the same handler works either way. Testable from a shell:
  `echo '{"url":"https://x"}' | ./tools/fetch.py`.
- A non-zero exit is an error; stderr is the message.
- `timeout` is a `[[tool]]` field only, in seconds, default 60. Every other slot uses the
  server's fixed 5 s, and `[[on]]` is fire and forget with no timeout at all. On expiry the
  handler counts as "no opinion" and the loop continues with a warning.

### Connected processes

A process started by `[[on]] event = "start" run = "./watcher"` is not waited for. It may
open `PIRS_SOCKET`, say hello, register for slots, and stay alive as an ordinary client
until the loop closes, at which point pirs kills it. There is no `persistent` flag; this is
the only long-lived form.

## Composition

Every applicable file is parsed, the slots are unioned, and the result is checked.

| Check | Outcome |
|---|---|
| duplicate `tool` name | error unless one is `wrap`/`disabled` of the other |
| duplicate `status`/`widget` key, duplicate `command` name | error |
| several `tool_result`s for one tool | applied in file order |
| several `input`s | file order; the first `handled` stops the rest |
| several `prompt`s | concatenated in file order |
| `on` entries | all run, file order |

File order is path-sorted, global before project. `priority = N` at the top of a file moves
it earlier.

`pirs check [--cwd <dir>]` prints the merged result, the fully assembled system prompt and
every conflict, without starting a loop. Run it after every edit.

## What the DSL cannot say

- **No conditionals, no loops, no expressions.** The day a slot seems to need one, the
  answer is `run = "..."` — a program in the language of your choice — never a new field.
  The DSL stays a list of declarations.
- **No guard, no permission slot, no interceptor.** Nothing in a `.pirs.toml` decides what
  the model may do, and no such field will be added. Containment is a boundary around the
  server (a container), not a pattern match inside the process the model controls.
- **Programs never ask the user anything.** A program that needs a decision returns a result
  that says so; the model reads it, asks in text, stops, and calls the program again with
  the answer. Only the model asks.
