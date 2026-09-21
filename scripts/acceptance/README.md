# Acceptance scripts

One script per phase, `phase-N.sh`; each is that phase's acceptance test and what you run
to check the phase by hand.

Run one from the repository root: `scripts/acceptance/phase-0.sh`. Exit status 0 means the
phase passes; every check prints one line.

- `phase-0.sh` — the design freeze: the crate graph, the protocol table and the docs agree.
- `phase-1.sh` — the server and the print client (S1, S2): a scripted answer on stdout,
  two one-shots in one directory, `--continue`, a tool result by reference, `subscribe`
  with `since`, idle exit, and `pirs tui`.

`lib/raw.py` is the raw wire client the scripts use for the steps the `pirs` command does
not expose: it says `hello`, sends each request line from stdin, prints every line the
server sends back, and can stand in for a registered handler (`--reply tool.x=file`).

They need no network and no credentials: model calls use the faux provider.

From phase 1, `cargo deny check bans` is part of every phase's verification: it asserts the
edges the crate graph cannot, so a UI library or an HTTP client reaching a crate that has no
business with it fails the phase (`deny.toml`, `20-architecture.md` "Mechanical checks").
