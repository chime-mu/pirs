#!/usr/bin/env python3
"""The `fetch` tool: one JSON line in, one JSON line out.

Payload  {"args": {"url": ..., "max_length": ...}, "id": "<tool call id>"}
Reply    {"content": "<the text>"}   or   {"error": "<what went wrong>"}

Failure is a reply, not a crash: this script prints `{"error": ...}` and exits 0 so the
model reads the message and can try something else.  (The other convention -- exit 1 with
the message on stderr -- means the same thing to pirs; pick one and stay with it.)

Test it from a shell, no pirs involved:

    echo '{"args":{"url":"file:///etc/hostname"},"id":"x"}' | ./tools/fetch.py
"""

import html
import json
import re
import sys
import urllib.request

USER_AGENT = "pirs-fetch/0.1"
DEFAULT_MAX_LENGTH = 50000

TAG = re.compile(r"<[^>]+>")
DROP = re.compile(r"<(script|style)\b.*?</\1>", re.IGNORECASE | re.DOTALL)
BLANK = re.compile(r"\n{3,}")


def strip_html(text):
    """Crude but dependency-free: drop script and style, then every tag."""
    text = DROP.sub(" ", text)
    text = re.sub(r"<br\s*/?>|</p>|</div>|</li>|</h[1-6]>", "\n", text, flags=re.IGNORECASE)
    text = TAG.sub("", text)
    return BLANK.sub("\n\n", html.unescape(text)).strip()


def fetch(url):
    """Return (text, looks_like_html) for one URL, or raise."""
    if url.startswith("file://"):
        with urllib.request.urlopen(url, timeout=20) as response:
            body = response.read()
        return body.decode("utf-8", "replace"), url.endswith((".html", ".htm"))
    if not url.startswith(("http://", "https://")):
        raise ValueError(f"unsupported URL scheme: {url!r} (http, https and file only)")
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=20) as response:
        body = response.read()
        content_type = response.headers.get("Content-Type", "")
    charset = "utf-8"
    if "charset=" in content_type:
        charset = content_type.split("charset=", 1)[1].split(";")[0].strip() or "utf-8"
    return body.decode(charset, "replace"), "html" in content_type.lower()


def main():
    line = sys.stdin.readline()
    try:
        args = (json.loads(line) or {}).get("args") or {}
    except json.JSONDecodeError as error:
        print(json.dumps({"error": f"bad payload on stdin: {error}"}))
        return 0

    url = args.get("url")
    if not isinstance(url, str) or not url:
        print(json.dumps({"error": "`url` is required and must be a string"}))
        return 0
    try:
        max_length = int(args.get("max_length") or DEFAULT_MAX_LENGTH)
    except (TypeError, ValueError):
        max_length = DEFAULT_MAX_LENGTH

    try:
        text, is_html = fetch(url)
    except Exception as error:  # every failure is a reply, not a crash
        print(json.dumps({"error": f"{url}: {error}"}))
        return 0

    if is_html:
        text = strip_html(text)
    if len(text) > max_length:
        text = text[:max_length] + f"\n\n[truncated at {max_length} characters]"
    print(json.dumps({"content": text}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
