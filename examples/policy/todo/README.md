# `todo` — a file as a widget, plus the instruction that keeps it true

Two declarations and no code at all.

`[[widget]]` puts the contents of `.pirs/todo.md` in a panel beside the conversation. It has
a `file`, not a `run`, so pirs reads the file itself. `on = ["start", "tool_result"]` says
when to re-read it: once when the loop starts, and again after **every tool result** — so
the moment the agent edits the file, the panel shows the new list. (A widget listing events
is an `on` entry underneath, whose output the server sends as `ui.widget`; D-17.)

`[[prompt]]` is the other half. The widget only displays a file; what makes the file worth
displaying is the model being told the list is its own, and to keep it current with the
`write` and `edit` tools. Those are the loop's own tools, so their writes also trigger the
policy reload rule — nothing here needs a process.

## Install

    mkdir -p <project>/.pirs/ext
    cp todo.pirs.toml <project>/.pirs/ext/
    : > <project>/.pirs/todo.md      # optional; an empty panel until the agent writes one

Nothing to make executable: this example has no scripts.

## Test it

There is no script to run from a shell. `pirs check` in the project prints the merged
policy, including the assembled system prompt with the paragraph above in it. Then start an
agent, ask it to plan something, and watch the panel fill in after its first `write`.
