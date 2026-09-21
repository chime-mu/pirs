# `ask` — a question with choices, and nothing new on the server

Only the model asks a person anything, in text, and the answer is the next prompt (D-37).
There is no dialog primitive, no `blocked` state, and nothing in the protocol that lets a
program stop the loop to interrogate you. What this example adds is a *convention*: when
the model wants a choice, it calls a tool whose arguments are the question and the options,
so the question is structured data a UI can draw instead of a paragraph a UI must guess at.

Three pieces:

- `ask.pirs.toml` declares the `[[tool]]` with `params.question` and `params.options`, plus
  a `[[prompt]]` telling the model when to use it and to stop afterwards.
- `tools/ask.py` is the tool. It does not ask anybody anything — it replies "the question
  has been shown to the user, stop now and wait", and echoes the question and options in
  `details` (metadata for UIs; the model never sees it). A tool that returns "I need
  something from you" is the whole mechanism.
- `tui.toml.snippet` plus `tools/ask-render.py` are the *client* half: a `[[render]]` hook
  that turns the call into a picker in the TUI, whose chosen option is sent as the next
  prompt.

**Nothing on the server changes for this** (S12, D-37). The server sees an ordinary tool
with an ordinary declaration; the call is in the session log as JSON like every other. A
client that has never heard of `ask` shows the call and the reply, and you answer by
typing. That is the test of the convention: it degrades to text.

## Install

    mkdir -p <project>/.pirs/ext <project>/tools
    cp ask.pirs.toml         <project>/.pirs/ext/
    cp tools/ask.py          <project>/tools/
    cp tools/ask-render.py   <project>/tools/
    chmod +x <project>/tools/ask.py <project>/tools/ask-render.py

For the picker, append `tui.toml.snippet` to `~/.pirs/tui.toml` (the TUI re-reads that file
when it changes) and make `run` point at wherever you put `ask-render.py` — it is resolved
by the *client's* cwd, not the loop's.

## Test the scripts from a shell

    echo '{"args":{"question":"Deploy to production?","options":["yes","no"]},"id":"x"}' | ./tools/ask.py
    echo '{"tool":"ask","args":{"question":"Deploy to production?","options":["yes","no"]},"id":"x"}' | ./tools/ask-render.py

The first prints the "stop and wait" `content`, the second
`{"lines": ["Deploy to production?"], "options": ["yes", "no"]}`.
