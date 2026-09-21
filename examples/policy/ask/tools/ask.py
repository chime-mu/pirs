#!/usr/bin/env python3
"""The `ask` tool: a question is a tool call, and the tool answers "shown, now stop".

Payload  {"args": {"question": ..., "options": [...]}, "id": "<tool call id>"}
Reply    {"content": "The question has been shown to the user. ..."}

Nothing here asks anybody anything.  Only the model asks (D-37): the *call* carries the
question as structured data, a UI that recognises the tool name draws it (see
`tui.toml.snippet`), and the user's choice comes back as an ordinary next prompt.  The
server does not know this tool is special and does not need to.

    echo '{"args":{"question":"Deploy?","options":["yes","no"]},"id":"x"}' | ./tools/ask.py
"""

import json
import sys

ANSWER = (
    "The question has been shown to the user. Stop now and wait for their answer; "
    "it will arrive as the next message."
)


def main():
    line = sys.stdin.readline()
    try:
        args = (json.loads(line) or {}).get("args") or {}
    except json.JSONDecodeError as error:
        print(json.dumps({"error": f"bad payload on stdin: {error}"}))
        return 0

    question = args.get("question")
    if not isinstance(question, str) or not question.strip():
        print(json.dumps({"error": "`question` is required and must be a non-empty string"}))
        return 0

    options = args.get("options")
    if not isinstance(options, list) or not all(isinstance(o, str) for o in options):
        print(json.dumps({"error": "`options` is required and must be a list of strings"}))
        return 0

    # `details` is metadata for UIs; the model never sees it.
    print(json.dumps({
        "content": ANSWER,
        "details": {"question": question, "options": options},
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
