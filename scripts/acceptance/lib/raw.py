#!/usr/bin/env python3
"""A raw pirs client for the acceptance scripts.

The acceptance scripts drive the real `pirs` binary wherever they can; this is
for the steps a command-line client does not expose, such as subscribing with
`since` after a run, or standing in for a registered handler.  It speaks the
wire protocol and nothing else: one JSON object per line over a unix socket
(`docs/protocol.md`).

    raw.py --socket <path> [options] < requests.jsonl

It connects, says `hello`, then sends each line of stdin as a request and
waits for that request's response before sending the next.  Every line the
server sends -- responses, events, slot requests -- is printed to stdout
verbatim, in arrival order, so the caller can assert on it with `jq`.

Options:

  --timeout SECS    give up after this long with nothing arriving (default 10)
  --until METHOD    after the last request, keep reading until a notification
                    with this method arrives (repeatable; any one of them ends
                    the wait)
  --reply SLOT=FILE answer a slot request the server sends -- `tool.<name>`,
                    `input`, ... -- with `{"content": <the file's text>}`
  --client NAME     what to call ourselves in `hello`

A request line is `{"method": ..., "params": {...}}`.  The string `$LOOP`
anywhere in `params` is replaced with the id of the loop the first
`loop.create` in this run returned, so a script can create, register,
subscribe, prompt and wait in one go.
"""

import argparse
import json
import socket
import sys
import time

PROTOCOL_VERSION = "0.1"


class Raw:
    """One connection: line framing, request ids, and the reply bookkeeping."""

    def __init__(self, path, timeout, replies):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout)
        self.sock.connect(path)
        self.timeout = timeout
        self.replies = replies
        self.buffer = b""
        self.next_id = 0
        self.loop_id = None

    def send(self, message):
        self.sock.sendall((json.dumps(message) + "\n").encode())

    def read_line(self):
        """The next line, or None when the server closed or went quiet."""
        while b"\n" not in self.buffer:
            try:
                chunk = self.sock.recv(65536)
            except socket.timeout:
                return None
            if not chunk:
                return None
            self.buffer += chunk
        line, _, self.buffer = self.buffer.partition(b"\n")
        return line.decode()

    def pump(self, want_id=None, until=()):
        """Read and print until `want_id` is answered or `until` is seen.

        Slot requests are answered from `--reply` along the way. Returns the
        response to `want_id`, or None when the wait ended without one.
        """
        while True:
            line = self.read_line()
            if line is None:
                return None
            line = line.strip()
            if not line:
                continue
            print(line, flush=True)
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            method = message.get("method")
            if method is not None and "id" in message:
                # A request from the server: a slot firing.
                content = self.replies.get(method)
                if content is not None:
                    self.send({"jsonrpc": "2.0", "id": message["id"],
                               "result": {"content": content}})
                continue
            if want_id is not None and message.get("id") == want_id and "method" not in message:
                return message
            if method is not None and method in until:
                return None

    def request(self, method, params):
        """Send one request and return its response."""
        self.next_id += 1
        request_id = self.next_id
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        response = self.pump(want_id=request_id)
        if response is None:
            raise SystemExit(f"raw.py: no response to {method} within {self.timeout}s")
        if "error" in response:
            raise SystemExit(f"raw.py: {method} failed: {json.dumps(response['error'])}")
        return response.get("result", {})


def substitute(value, loop_id):
    """Replace `$LOOP` with the created loop's id, anywhere in `params`."""
    if isinstance(value, str):
        return value.replace("$LOOP", loop_id) if loop_id else value
    if isinstance(value, list):
        return [substitute(item, loop_id) for item in value]
    if isinstance(value, dict):
        return {key: substitute(item, loop_id) for key, item in value.items()}
    return value


def main():
    parser = argparse.ArgumentParser(description="A raw pirs protocol client.")
    parser.add_argument("--socket", required=True)
    parser.add_argument("--timeout", type=float, default=10.0)
    parser.add_argument("--until", action="append", default=[], metavar="METHOD")
    parser.add_argument("--reply", action="append", default=[], metavar="SLOT=FILE")
    parser.add_argument("--client", default="pirs-acceptance 0.1.0")
    args = parser.parse_args()

    replies = {}
    for spec in args.reply:
        slot, _, path = spec.partition("=")
        with open(path) as handle:
            replies[slot] = handle.read()

    deadline = time.time() + 30.0
    while True:
        try:
            raw = Raw(args.socket, args.timeout, replies)
            break
        except (FileNotFoundError, ConnectionRefusedError):
            if time.time() > deadline:
                raise SystemExit(f"raw.py: nothing listening on {args.socket}")
            time.sleep(0.05)

    raw.request("hello", {"client": args.client, "protocol_version": PROTOCOL_VERSION})

    for line in sys.stdin:
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        message = json.loads(line)
        params = substitute(message.get("params", {}), raw.loop_id)
        result = raw.request(message["method"], params)
        if message["method"] == "loop.create" and raw.loop_id is None:
            raw.loop_id = result.get("id")

    if args.until:
        raw.pump(until=set(args.until))
    return 0


if __name__ == "__main__":
    sys.exit(main())
