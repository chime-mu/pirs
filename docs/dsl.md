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

Only the loop's own `write` and `edit` tools trigger that automatic reload: a file written
by a `bash` redirect, by an `[[on]]` process or by hand from another window does not, since
the server watches the tool calls and not the filesystem. Send `loop.reload` for those, or
leave it — the next `loop.create` in that directory reads the file like any other.

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

`header` applies to every source: it is the first line of the block, above a `text`, above
the globbed files, above a shell string's stdout, and above what an executable `{ append }`s.
A `{ replace }` is the whole prompt and has no block, so no header.

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

Fire and forget: the loop does not wait for the reply, and there is no timeout. `on start`
is therefore also how a long-lived extension is started — see "Connected processes". A
non-zero exit is reported as a warning unless `quiet`.

`start` fires once when the loop is created, and again after a reload for the entries that
are new since the last load — an entry is the same entry when its file, slot, position and
`run` are unchanged, so an untouched watcher is not started twice. `reload` fires on every
reload, including the automatic one after the agent writes a policy file itself. A
`[[status]]` or `[[widget]]` listing an event is *called* instead, with the usual 5 s, and
its output is sent as `ui.status` (trimmed) or `ui.widget` (one entry per line).

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
`run` · `timeout` (seconds; default 60 s for a `run`, none for a `loop`) · `disabled`
(bool) · `wrap` (a program the real tool's call is routed through) ·
`loop = { model, prompt, wait }` instead of `run` · `params` alone (no `run`, no `loop`)
declares a tool a connected process serves

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

`params.<name>` is the field's JSON Schema, and takes any JSON-Schema keyword, not only
`type`, `description` and `default`: `enum`, `minimum`, `pattern`, `items`, `format` and the
rest are passed to the model as written. A field without a `default` is required.
`params = {}` declares a tool that takes no arguments.

Payload `{ args, id }`. Reply `{ content, details }` or `{ error }`, where `content` is a
string or a list of `text`/`image` blocks and `details` is metadata the model never sees.

**What answers the call** depends on `run`:

- `run = "./tools/fetch.py"` — an executable (see "`run` semantics"): it reads the payload on
  stdin and prints the reply as one JSON line. A non-zero exit is an error result the model
  reads, with stderr (trimmed) as the message, or `exited with status N` when stderr is
  empty. A reply that is not one JSON line in the reply shape is an error result too, plus a
  `ui.notify` warning naming the entry. `timeout` applies.
- `run = "curl -s $url"` — a shell string: its stdout is the result as text, and a non-zero
  exit is an error result with stderr as the message. `timeout` applies.
- **no `run`, no `loop`, but `params`** — a *declaration*: the tool is in the manifest with
  this description and schema, and a call is sent to the connected client that has
  `register`ed `tool.<name>` on the loop (`register { loop, slot = "tool.fetch", timeout }`),
  which answers with the same `{ content, details }` or `{ error }` and within its own
  registered timeout (D-23). No client registered → the model reads the error
  ``no handler registered for tool `<name>` ``. This is how a long-lived process started
  from `[[on]] event = "start"` serves a tool with a real schema: the file declares, the
  process answers. A registration for a name no file declares still works, with
  `{ "type": "object" }` as its schema and a description naming the registrant; a
  registration for a name a file *does* declare takes the file's description and schema. A
  declaration takes no `timeout` — it is rejected, because the registrant's timeout is what
  bounds the call. A registration for a name a file gives a `run` or a `loop`, or for a
  built-in, is refused: the file is the declared intent (D-22), so `wrap` or `disabled` is
  how a built-in changes hands.
- `wrap = "./tools/sandboxed-bash.sh"` on a built-in: the same rule decides whether the
  wrapper is an executable or a shell string, and it receives the built-in's `{ args, id }`.
- `loop = { model, prompt, wait = "idle" }` — a second agent answers, see below.

### `[[tool]] loop` — ask a second agent

```toml
[[tool]]
name = "review"
description = "Ask a second loop to review the diff"
loop = { model = "anthropic/claude-opus-4-1", prompt = "Review this diff critically:\n$diff", wait = "idle" }
params.diff = { type = "string", description = "The diff to review" }
```

The call starts a second loop in the *same directory*, prompts it, waits for it to stop,
and hands its final message back as the tool result. The three fields:

- **`model`** — what the second loop runs, spelled as `--model` spells it
  (`provider/id`, or an id the server's registry resolves). Optional: without it the second
  loop runs the *calling* loop's model. A `model` this server cannot resolve is a mistake in
  the file, not a fallback: the load warns (so `pirs check` reports it as a conflict) and a
  call fails with the error result ``tool `review`: unknown model `claude-opus` ``.
- **`prompt`** — what it is asked. Every argument of the call is a variable: `$diff` and
  `${diff}` are both replaced by the `diff` argument, an object or array argument is pasted
  as compact JSON, and a name the call did not carry is left as it stands (so `$HOME` in a
  prompt stays `$HOME`). The substitution is raw text — nothing is quoted or escaped,
  because nothing here reaches a shell.
- **`wait`** — `"idle"`, and only `"idle"`: the call returns when the second loop goes
  idle. Any other value is a parse error `pirs check` reports.

The second loop **is a loop like any other**: it is in `loop.list` with `parent` set to the
calling loop, named `<caller>/<tool>`, a UI draws it under its parent, and you can attach to
it and watch it work. `parent` and the close cascade below are the server's own: a client's
`loop.create` cannot claim a parent, so only a `[[tool]] loop` call makes one. It reads the
same policy files, so give it a `[settings] tools` or a `disabled` if it should not have the
same tools — and note that it is offered the `loop` tool as well, so a policy can recurse:
a call nested more than 8 deep is refused with the error result
``loop tool `review`: nesting deeper than 8`` and a warning, which is an implementation
limit and nothing more.

`wait = "idle"` is *idle*, not *finished*: a second loop that stops to ask a question is
idle, so the call returns then and the question itself is the answer the calling model
reads.

**Lifetime.** The second loop stays alive after it answers, so you can read what it did
(D-28); it is closed when its parent is closed, and `loop.abort` on the parent aborts it and
ends the call with an error result. A loop that ends in an error, or that is closed or
aborted, is an error result for the caller, with the second loop's id in the message.

**Timeout.** A `[[tool]] loop` uses the `timeout` the entry declares, and has **no timeout**
when it declares none: 60 seconds is right for a script and wrong for a review. Write
`timeout = 900` to bound it; on expiry the second loop is aborted and the caller reads an
error.

**In the log.** The tool result the calling loop records carries
`details = { "loop": "<second loop's id>", "conversation": "<its conversation id>" }`, so a
reader of the session log can open the conversation the answer came from.

## `[settings]`

One table per file, merged key by key like the slots: the last file to set a key wins.
`[settings]` replaces `settings.json`. It has four keys and no others — `model`, `thinking`,
`tools` and `tool_execution`; anything else is a conflict `pirs check` reports (D-41).

```toml
[settings]
model = "anthropic/claude-sonnet-4-5"   # what a loop runs without --model
thinking = "medium"                     # its thinking level without --thinking
tools = ["read", "bash", "edit"]        # the tools it starts with; a [[tool]] is added anyway
tool_execution = "parallel"             # or "sequential": one tool call at a time
```

## `run` semantics

Every `run` is a *called* process. pirs spawns it in the loop's cwd, writes the slot payload
as one JSON line on stdin, and reads the reply from stdout. `PIRS_SOCKET`, `PIRS_LOOP`,
`PIRS_SLOT` and `PIRS_SESSION_DIR` are in the environment.

**One rule decides the binding.** A `run` value is an **executable** when it is a single
token — no whitespace — that names an existing file with the executable bit set, resolved
against the loop's cwd when relative (`./tools/fetch.py`, `tools/fetch.py`) or taken as given
when absolute. Everything else is a **shell string**. So `./tools/fetch.py --all`,
`git log --oneline -5` and `cat` are shell strings, and a script without its `x` bit is
run by the shell as a command name (and fails as one) rather than as an executable. There
is no tilde expansion in the test, so `~/bin/hook.sh` is never an executable: it stays a
shell string, and `sh` is what expands the `~` when it runs it — so such an entry replies as
text, not as one JSON line. Whitespace is decisive the same way: `./x.sh $args` is a shell
string, `./x.sh` an executable. The same rule applies to `wrap`.

- **An executable** is spawned directly: no shell, no `$field` interpolation, no
  `PIRS_ARG_*` — it reads the JSON line. The line is exactly the slot's payload from the
  table above: `{ text }` for `input`, `{ system_prompt }` for `prompt`,
  `{ tool, args, result }` for `tool_result`, `{ args, id }` for `tool.<name>`, the event
  payload for `on.<event>`. It replies with **one JSON line on stdout** in the slot's reply
  shape — `{ text }` or `{ handled: true }`; `{ append }` or `{ replace }`; `{ result }`;
  `{ content, details? }` or `{ error }` — the same shapes a connected handler sends over the
  socket, so the same program works either way. `on.<event>` expects no reply; a
  `[[status]]` or `[[widget]]` executable prints its value as text. Testable from a shell:
  `echo '{"args":{"url":"https://x"},"id":"t1"}' | ./tools/fetch.py`.
- **A shell string** runs under `sh -c`. It additionally gets each top-level payload field
  as `PIRS_ARG_<field>` and as `$field` interpolation, so a one-liner never parses JSON. Its
  stdout is the reply, as text: the new input text, the prompt block, the rewritten
  result, the tool's output. A field larger than 64 KiB — a whole file a `write` tool call
  carries, say — is **on stdin only**: the kernel refuses an `exec` whose environment and
  arguments exceed 128 KiB, so such a field is left off `PIRS_ARG_*` and `$field`
  substitutes the empty string (with a warning in the server's log). Read those from the
  JSON line, `jq -r .args.text` and such.
- **Failure.** A non-zero exit is an error with stderr as the message; an executable whose
  stdout is not one JSON line in the reply shape is an error naming the parse problem. For
  `input`, `prompt`, `tool_result`, `status` and `widget` an error is "no opinion" — the loop
  continues with what it had — plus a `ui.notify` warning naming the entry. For a `[[tool]]`
  it is an error result the model reads. The exception is an `[[input]]` with
  `handled = true`: the input is consumed whatever the process does, because the file said
  so, and a failure is recorded as the command's output.
- `timeout` is a `[[tool]]` field only, in seconds, default 60, and it bounds a process the
  server spawns: an entry with neither `run` nor `wrap` may not set it. Every other slot uses
  the server's fixed 5 s, and `[[on]]` is fire and forget with no timeout at all. On expiry the
  handler counts as "no opinion" and the loop continues with a warning.
- An executable `[[prompt]] run` is a `prompt` handler: it runs once the base system prompt
  is assembled (text, files and shell-string entries included), receives it whole as
  `{ system_prompt }`, and its `{ append }` lands under a `# <file>: [[prompt]] #n` line, or
  its `{ replace }` replaces the prompt. A shell-string `[[prompt]] run` is a block of the
  policy section in file order and sees only the section so far.
- Every process the server spawns belongs to the loop and dies with it: on `loop.close` a
  called process still running is killed with its whole process group, and so is a called
  executable whose tool call is aborted with `loop.abort`.

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
