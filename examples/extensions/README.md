# Example extensions

Unmodified copies of example extensions from [pi](https://github.com/earendil-works/pi)
(`packages/coding-agent/examples/extensions`, MIT license), kept here so they can be loaded
with `pirs -e examples/extensions/<name>.ts` without a pi checkout.

pirs-specific examples:

- `fetch.ts` — a `fetch` tool modelled on Claude Code's WebFetch: downloads a URL, converts
  HTML to Markdown-style text (JSON is pretty-printed), truncates to `max_length`, and with an
  optional `prompt` asks the current model to answer from the page so only the answer enters the
  conversation. Timeout, http/https-only, and abort via Esc are handled.
