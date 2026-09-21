# `fetch` — a tool in Python

Gives the model a `fetch` tool: one URL in, its text out. `http(s)://` and `file://` URLs
work; HTML is reduced to its text with a crude tag strip, and the result is truncated to
`max_length` characters (50000 unless the model says otherwise).

`run = "./tools/fetch.py"` is a single token naming an executable file, so pirs spawns the
script directly and speaks JSON to it: the payload `{"args": {...}, "id": "..."}` as one
line on stdin, one line of `{"content": ...}` or `{"error": ...}` back on stdout. No shell,
no `$name` interpolation, no `PIRS_ARG_*`.

Failure is a reply here, not a crash: the script prints `{"error": "..."}` and exits 0, so
the model reads the message. Exiting non-zero with the message on stderr would mean the
same thing to pirs — pick one convention per script.

## Install

    mkdir -p <project>/.pirs/ext <project>/tools
    cp fetch.pirs.toml   <project>/.pirs/ext/
    cp tools/fetch.py    <project>/tools/
    chmod +x <project>/tools/fetch.py

`run` is resolved against the loop's cwd, so `./tools/fetch.py` means
`<project>/tools/fetch.py`. `pirs check` in the project prints the merged policy.

## Test the script from a shell

    echo '{"args":{"url":"file:///etc/hostname"},"id":"x"}' | ./tools/fetch.py
    echo '{"args":{"url":"https://example.com","max_length":200},"id":"x"}' | ./tools/fetch.py
    echo '{"args":{"url":"ftp://nope"},"id":"x"}' | ./tools/fetch.py

The first prints `{"content": "<hostname>\n"}`, the last an `{"error": ...}`. That is the
whole contract; if those work, pirs will work.
