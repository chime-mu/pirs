# `watch` — a process that lives with the loop

`[[on]] event = "start"` is fire and forget: pirs spawns `./tools/watch.py` when the loop is
created and does not wait for it. So the script can do what a called process cannot —
connect to `PIRS_SOCKET`, say `hello`, `register` for a slot, and stay. This is the only
long-lived shape in pirs; there is no `persistent` flag (D-16).

It registers `{"loop": $PIRS_LOOP, "slot": "on.turn_end", "timeout": 1000}` and appends one
line `turn_end <ISO time>` to `.pirs/watch.log` for every notification. `on.*` slots are
notifications — no `id`, no reply, nothing waited on — so the `timeout` matters only for
the request-shaped slots. It writes its pid to `.pirs/watch.pid`.

**It dies with the loop.** Every process pirs spawns belongs to the loop that spawned it
and is killed on `loop.close` (D-23): the process gets a moment to finish, then its process
group is stopped. Nothing needs to clean up, and `.pirs/watch.pid` names a pid that is gone.

## Install

    mkdir -p <project>/.pirs/ext <project>/tools
    cp watch.pirs.toml  <project>/.pirs/ext/
    cp tools/watch.py   <project>/tools/
    chmod +x <project>/tools/watch.py

`[[on]]` entries fire `start` again after a reload only for entries that are new since the
last load, so an untouched watcher is not started twice.

## Test it from a shell

The script needs a live server, so the shell test is the environment, not a pipe:

    PIRS_SOCKET=/nonexistent PIRS_LOOP=x ./tools/watch.py; echo "exit $?"
    PIRS_LOOP= ./tools/watch.py; echo "exit $?"

The first fails to connect (a traceback, non-zero exit), the second prints
`watch.py: PIRS_SOCKET and PIRS_LOOP must be set` and exits 1. Against a real server:

    pirs "hi" &                      # anything that runs a turn in this project
    cat .pirs/watch.log              # one line per turn that ended
    kill -0 "$(cat .pirs/watch.pid)" # succeeds while the loop lives, fails after it closes
