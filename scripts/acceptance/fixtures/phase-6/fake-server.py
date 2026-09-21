#!/usr/bin/env python3
"""A pirs server from another release: it speaks protocol 1.0 and refuses us.

Phase 6 has to show what a client does when the two ends do not agree on the
protocol (S21).  The plan suggested building the real server with a different
`PROTOCOL_VERSION` behind a test feature flag; this tests the same client path
without one -- and without a flag the shipped binary would carry for ever.
What is under test is the client's side of `hello`: the refusal it receives,
and the message it prints.

    fake-server.py <socket>

It listens on the unix socket, answers `hello` with
`error { code: -32000, data: { server: "1.0" } }` (`code::VERSION_REFUSED`,
`crates/pirs-protocol/src/envelope.rs`), and closes the connection.  Anything
that is not `hello` gets the same refusal, because a client that has not
agreed on a version has no business asking for anything else.
"""

import json
import os
import socket
import sys

# The major this fake claims. Different from the real PROTOCOL_VERSION ("0.1"),
# which is what makes it a refusal rather than a negotiation.
SERVER_VERSION = "1.0"
VERSION_REFUSED = -32000


def refusal(request_id):
    return {
        "jsonrpc": "2.0",
        "id": request_id,
        "error": {
            "code": VERSION_REFUSED,
            "message": f"this server speaks protocol {SERVER_VERSION}",
            "data": {"server": SERVER_VERSION},
        },
    }


def serve(path):
    if os.path.exists(path):
        os.unlink(path)
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(path)
    listener.listen(8)
    while True:
        conn, _ = listener.accept()
        with conn:
            # One request, one refusal, then the connection closes -- which
            # is what a server that cannot talk to this client should do.
            buffer = b""
            while b"\n" not in buffer:
                chunk = conn.recv(65536)
                if not chunk:
                    break
                buffer += chunk
            line, _, _ = buffer.partition(b"\n")
            try:
                message = json.loads(line)
            except ValueError:
                continue
            if "id" not in message:
                continue
            conn.sendall((json.dumps(refusal(message["id"])) + "\n").encode())


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("usage: fake-server.py <socket>", file=sys.stderr)
        sys.exit(2)
    try:
        serve(sys.argv[1])
    except KeyboardInterrupt:
        pass
