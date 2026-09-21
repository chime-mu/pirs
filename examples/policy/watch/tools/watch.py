#!/usr/bin/env python3
"""A *connected* process: it registers for a slot and lives until the loop closes.

`[[on]] event = "start"` is fire and forget -- pirs does not wait for a reply -- so the
process it starts may open `PIRS_SOCKET`, say `hello`, `register` for the slots it wants,
and stay an ordinary client from then on (D-16, D-23).  That is the only long-lived form;
there is no `persistent` flag.

This one registers for `on.turn_end` and appends a line to `.pirs/watch.log` for every
notification it receives, forever.  It writes its pid to `.pirs/watch.pid` so you can see
that pirs killed it when the loop closed.

`on.turn_end` is a *notification*: no `id`, nothing to reply to.  A slot that is a request
(`tool.<name>`, `input`, `prompt`, `tool_result`) would have an `id`, and the reply goes
back as `{"jsonrpc": "2.0", "id": <that id>, "result": {...}}` within the `timeout` this
registration declares.
"""

import datetime
import json
import os
import socket
import sys

PROTOCOL_VERSION = "0.1"
LOG = os.path.join(".pirs", "watch.log")
PID = os.path.join(".pirs", "watch.pid")


def send(sock, message):
    sock.sendall((json.dumps(message) + "\n").encode())


def lines(sock):
    """Every JSON object the server sends, one at a time."""
    buffer = b""
    while True:
        while b"\n" in buffer:
            line, _, buffer = buffer.partition(b"\n")
            line = line.strip()
            if line:
                try:
                    yield json.loads(line)
                except json.JSONDecodeError:
                    continue
        chunk = sock.recv(65536)
        if not chunk:
            return
        buffer += chunk


def append(path, text):
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(text)
        handle.flush()


def main():
    socket_path = os.environ.get("PIRS_SOCKET")
    loop_id = os.environ.get("PIRS_LOOP")
    if not socket_path or not loop_id:
        print("watch.py: PIRS_SOCKET and PIRS_LOOP must be set", file=sys.stderr)
        return 1

    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(socket_path)

    send(sock, {"jsonrpc": "2.0", "id": 1, "method": "hello",
                "params": {"client": "watch.py 0.1", "protocol_version": PROTOCOL_VERSION}})
    send(sock, {"jsonrpc": "2.0", "id": 2, "method": "register",
                "params": {"loop": loop_id, "slot": "on.turn_end", "timeout": 1000}})

    # The pid file is written only after `register` has been sent, so "the pid
    # file exists" means "this process is registered" (the acceptance script
    # waits on it).
    os.makedirs(os.path.dirname(PID) or ".", exist_ok=True)
    with open(PID, "w", encoding="utf-8") as handle:
        handle.write(f"{os.getpid()}\n")

    for message in lines(sock):
        # A `hello` refused for a protocol major mismatch, or a `register` for
        # an unknown slot, comes back as an error and the useful thing to do is
        # say so: nothing else here will ever fire.
        if "error" in message and "method" not in message:
            print(f"watch.py: {json.dumps(message['error'])}", file=sys.stderr)
            return 1
        if message.get("method") == "on.turn_end":
            now = datetime.datetime.now(datetime.timezone.utc).isoformat()
            append(LOG, f"turn_end {now}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
