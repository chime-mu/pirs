# Policy examples

Six working policies, one directory each. Every one is a `*.pirs.toml` file with an
`intent` at the top saying what it is for, plus whatever scripts its `run` values point at.
Read them in this order — the first three add code, the last three are declarations alone.

| Directory | What it does |
|---|---|
| [`fetch/`](fetch/) | a `[[tool]]` in Python: the model gets a `fetch` tool for http(s) and file URLs |
| [`todo/`](todo/) | a `[[widget]]` showing `.pirs/todo.md` beside the conversation, refreshed after every tool result, plus the `[[prompt]]` that tells the model to keep it |
| [`watch/`](watch/) | a **connected** process: `[[on]] event = "start"` spawns a script that registers `on.turn_end` and logs every turn until the loop closes |
| [`ask/`](ask/) | the structured-question convention: an `ask` tool whose call carries the question and options, with a TUI `[[render]]` snippet that draws a picker — and nothing new on the server |
| [`git-checkpoint/`](git-checkpoint/) | `[[on]] event = "turn_end"` committing the working tree after every turn |
| [`input-shortcuts/`](input-shortcuts/) | `[[input]]`: `?` for a brief answer, `!` to run a shell command the model never sees |

## The install rule

The same for all of them, and for anything you write yourself:

1. The `.pirs.toml` goes in `<project>/.pirs/ext/` for one project, or `~/.pirs/ext/` for
   every project. Every file in both directories is loaded and the slots are unioned.
2. The scripts go **where the `run` paths point**. `run` is resolved against the loop's
   cwd, so `run = "./tools/fetch.py"` means `<project>/tools/fetch.py`. Put them elsewhere
   and edit the path, or use an absolute one.
3. `chmod +x` every script. The executable bit is what makes pirs spawn it directly with
   JSON on stdin instead of handing the string to `sh -c`.
4. `pirs check` in the project prints the merged policy, the assembled system prompt and
   any conflict. Run it after every edit. A loop re-reads its policy on `loop.reload`, and
   automatically when one of its own tools writes a file in either directory.

## Two kinds of `run`

A `run` value is an **executable** when it is a single token with no whitespace naming an
existing file with the executable bit set. pirs spawns it directly, writes the slot payload
as one JSON line on its stdin, and reads one JSON line of reply from its stdout. There is
no `$name` interpolation and no `PIRS_ARG_*` for an executable — it has the whole payload.
`PIRS_SOCKET`, `PIRS_LOOP`, `PIRS_SLOT` and `PIRS_SESSION_DIR` are in its environment. A
non-zero exit is an error whose message is its stderr.

Anything with whitespace in it is a **shell string**, run under `sh -c`, replying as text.
That is `git add -A && git commit …` in `git-checkpoint/` and `$1` in `input-shortcuts/`.

Because the payload on stdin is the same JSON the socket carries, every script here is
testable with one line of shell before pirs is involved at all:

    echo '{"args":{"url":"file:///etc/hostname"},"id":"x"}' | fetch/tools/fetch.py

Each README ends with the commands for its own scripts. `docs/dsl.md` is the full
vocabulary and `docs/protocol.md` the wire these payloads travel on.
