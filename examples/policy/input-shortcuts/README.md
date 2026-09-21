# `input-shortcuts` — `?` for brief, `!` for the shell

Two declarations, no scripts. `[[input]]` runs on every prompt before the model sees it.

`^\?(.*)` with `replace = "Explain briefly: $1"` rewrites the text: typing `?what is D-37`
reaches the model as `Explain briefly: what is D-37`. `$1` is the first capture group of
the regex — this is a rewrite, not a program, so no process is spawned at all.

`^!(.*)` with `handled = true` consumes the input: no turn starts and the model never sees
it. `run = "$1"` is a shell string, so pirs runs the capture group under `sh -c` in the
loop's cwd and shows the output. Because the entry says `handled`, the input is consumed
whatever the command does — a failing command's output is recorded as the command's output
rather than becoming an error that starts a turn.

Order matters: several `[[input]]` entries run in file order and the first `handled` one
stops the rest. These two cannot both match, so the order here is free.

## Install

    mkdir -p <project>/.pirs/ext
    cp input-shortcuts.pirs.toml <project>/.pirs/ext/

Or into `~/.pirs/ext/` to have them everywhere — this one is a good candidate for the home
directory, since it is about how *you* type rather than about the project.

## Test it from a shell

No scripts to pipe JSON into. The rewrite is pure declaration; the `!` half is just `sh`:

    sh -c 'git status --short'; echo "exit $?"

and `pirs check` in the project prints both entries in the merged policy. Then type
`?why is the build slow` and `!ls -la` at an agent and watch what each does.
