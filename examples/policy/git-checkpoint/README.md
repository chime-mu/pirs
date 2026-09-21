# `git-checkpoint` — a commit after every turn

One declaration, no scripts. After every turn pirs runs

    git add -A && git commit -qm 'pirs checkpoint' || true

`run` has whitespace in it, so it is a **shell string**: pirs runs it under `sh -c` in the
loop's cwd, and its stdout would be the reply as text. `[[on]]` is fire and forget — the
loop does not wait and there is no timeout — and `quiet = true` discards the output, so a
turn where nothing changed (and `git commit` therefore fails) is silent. The `|| true` is
belt and braces: without `quiet`, a non-zero exit would be reported as a warning.

## Install

    mkdir -p <project>/.pirs/ext
    cp git-checkpoint.pirs.toml <project>/.pirs/ext/

The project must be a git repository with something committed already, and `user.name` /
`user.email` set, or the commit fails every turn (silently, because of `quiet`).

## How to undo

Every checkpoint is an ordinary commit with the message `pirs checkpoint`, so:

    git log --oneline                       # find the state you want
    git diff HEAD~1                          # what the last turn did
    git reset --hard HEAD~1                  # throw the last turn away
    git reset --soft <sha before the run>    # keep the work, drop the checkpoints
    git reset --soft "$(git log --format=%H --grep='^pirs checkpoint' --invert-grep -1)"
                                             # back to your last real commit, changes staged

The last one is the usual move at the end of a session: squash every checkpoint into one
commit of your own. If you would rather the checkpoints never reached your history, run the
agent on a scratch branch and cherry-pick the result.

## Test it from a shell

There is no script; the shell test is the command itself, in the project:

    git add -A && git commit -qm 'pirs checkpoint' || true; echo "exit $?"
