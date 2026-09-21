#!/usr/bin/env python3
"""A TUI `[[render]]` hook for the `ask` tool call: draw the question, offer the options.

Payload  {"tool": "ask", "args": {"question": ..., "options": [...]}, "id": "<call id>"}
Reply    {"lines": ["<question>"], "options": ["yes", "no"]}

`lines` replace the default drawing of the tool call.  A non-empty `options` makes the TUI
open a picker over those lines, and the chosen string is sent as the next prompt -- which
is exactly the convention `ask` relies on.  This runs in the *client*: the server knows
nothing about it (S12, D-37).

    echo '{"tool":"ask","args":{"question":"Deploy?","options":["yes","no"]},"id":"x"}' | ./tools/ask-render.py
"""

import json
import sys


def main():
    line = sys.stdin.readline()
    try:
        args = (json.loads(line) or {}).get("args") or {}
    except json.JSONDecodeError:
        print(json.dumps({"lines": ["ask: unreadable tool call"]}))
        return 0

    question = args.get("question")
    question = question.strip() if isinstance(question, str) else ""
    options = args.get("options")
    if not isinstance(options, list):
        options = []
    options = [o for o in options if isinstance(o, str) and o.strip()]

    lines = [question or "ask: a question with no text"]
    print(json.dumps({"lines": lines, "options": options}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
