# pirs

pirs is a tool for working with coding agents. Three things shape it:

- **Run many agents at once**, on more than one machine when one is not enough.
- **See them all in one place** and know at a glance which one is waiting for you.
- **Change how it behaves by asking an agent to change it** — no plugin, no build, no restart.

A server owns the agents. Everything else is a client at the same door: the `pirs` command,
the terminal UI, a shell script, your own tools, another agent. Because the server owns the
loop it knows whether an agent is working, waiting or done, and says so as a fact rather
than guessing from what is on a screen. It has no screen of its own, keeps running when you
close the UI, and can run on another machine or inside a container. pirs began as a port of
[pi](https://github.com/earendil-works/pi) and still writes session files pi can read, but
its customisations are declarations and separate programs instead of code loaded into the
tool; pi extensions do not run.

## Build

```bash
cargo build --release          # target/release/pirs
```

A recent stable Rust toolchain is all it needs — `rustup toolchain install stable`, or
`mise use rust@stable` if that is how you manage toolchains. `cargo install cargo-deny` is
optional, and only used by `cargo deny check bans`.

## A ten-minute tour

### Ask something

```bash
pirs "why does the build fail?"         # an agent here; the answer streams; it exits
pirs --name "api work" "start on the http client"
pirs --continue "keep going"            # the most recent conversation here
pirs --continue="api work" "add a test" # that one, by name (the = is required)
pirs --list                             # running agents, then this directory's conversations
pirs wait a7f3                          # block until that agent is idle, for scripts
```

A server is started for you if none is running, and exits by itself after ten idle minutes;
`pirs stop` ends it now, `pirs serve` starts it explicitly.

### The interactive UI

`pirs tui`. A sidebar lists every agent with its state, and flags one that has stopped and
that you have not looked at since. Selecting an agent opens its page: the conversation, its
status keys and widgets, and the files it touched this turn. `ctrl-o` opens one of those in
a read-only page that refreshes while the agent keeps working; `ctrl-e` opens `$EDITOR` on
it in a tmux pane. Other defaults: `enter` sends, `alt-enter` sends after the current turn,
`esc` aborts, `/` opens the command list, `tab` / `shift-tab` move between pages, `ctrl-w`
closes one, `up` / `down` walk the sidebar, `pageup` / `pagedown` scroll, `ctrl-t` expands
tool results, `ctrl-n` starts an agent, `ctrl-r` re-reads the config, `ctrl-q` quits. All of
that — keys, theme, the status line format, UI-side commands, and `[[render]]` hooks that
draw a tool call with your own program — lives in `~/.pirs/tui.toml` on the machine running
the UI, never on the server. The UI is one client; `docs/protocol.md` is what another one
would be written against.

### Policy without code

Put a short declaration in `<project>/.pirs/ext/` and pirs behaves differently:

```toml
intent = "A leading ? asks for a brief answer."

[[input]]
match = '^\?(.*)'
replace = "Explain briefly: $1"
```

`pirs check` prints the merged policy for this directory, every conflict between files, and
the fully assembled system prompt the model would receive; it exits 1 when anything
conflicts, so a script can gate on it. Nothing reaches the model that you cannot see here.

Eight slots fill in this way — `input`, `prompt`, `tool_result`, `status`, `widget`, `on`,
`command`, `tool` — plus a `[settings]` table for the model, thinking level and tool set.
`docs/dsl.md` is the whole vocabulary, and `examples/policy/` has six working examples.

You do not have to write the file yourself. Ask the agent — "make `?` give me brief
answers" — and it writes the `.pirs.toml`; the loop notices its own write, re-reads its
policy, and the change is live on your next turn.

### Tools in any language

A declaration names a program, and the model can call it:

```toml
[[tool]]
name = "fetch"
description = "Fetch a URL and return its text"
params.url = { type = "string", description = "Absolute http(s) URL" }
run = "./tools/fetch.py"
```

An executable `run` gets the call as one JSON line on stdin and answers with one JSON line
on stdout, so it is testable before pirs is involved at all:

```bash
echo '{"args":{"url":"file:///etc/hostname"},"id":"x"}' | ./tools/fetch.py
```

`examples/policy/fetch/` is that tool, complete. A `run` with whitespace in it is a shell
string instead, and a program started by `[[on]] event = "start"` may open the socket and
stay alive as a client for the life of the agent (`examples/policy/watch/`).

### A second machine, or a jail

`~/.pirs/servers.toml` names the servers you use and the command that reaches each one:

```toml
[[server]]
name = "local"                              # no command: the local socket

[[server]]
name = "build"
command = "ssh build pirs proxy"            # another machine

[[server]]
name = "jail"
command = "docker exec -i jail pirs proxy"  # a container on this machine
```

Then `pirs --server build "why does the build fail?"`, and the UI's sidebar spans every
server at once, each agent shown as `server:id`. `pirs proxy` forwards protocol lines
between its stdin/stdout and a server's socket; SSH or `docker exec` is the whole of the
transport, and pirs opens no network port of its own. For a repository you do not trust,
the jail is the setup: `Dockerfile` here builds the image, every tool and extension
executes inside the boundary because that is where the server is, and the kernel and the
container runtime are what enforce it. See [`docs/containment.md`](docs/containment.md),
including what the boundary does not cover.

### Say it in a sentence

When no agent is running, `pirs ext new "show the git branch in the status line"` asks the
model to write the declaration from that sentence. It lands in `<cwd>/.pirs/ext/`, and what
`pirs check` makes of it is printed for you. The sentence is kept in the file as its
`intent`, and `pirs ext regen <file>` rebuilds the file from it later, keeping the old one
as `<file>.bak`. The intent is what you share with others, not the file.

```
pirs ext new "show the git branch in the status line"   # writes .pirs/ext/<slug>.pirs.toml
pirs ext regen .pirs/ext/show-the-git-branch.pirs.toml  # writes it again from its intent
```

`--name` picks the file name (without `.pirs.toml`); a slug of the sentence is the default,
and a name already taken gets a suffix unless `--force` says to overwrite it. `--model`
picks the model; without it the agent's own is used. The written `intent` is your sentence
verbatim, whatever the model paraphrased it into, because that is what `regen` reads back.
The command prints the check for the directory and a last `wrote <path>` line, and exits 1
— leaving the file there to be fixed — if the file does not parse or the check conflicts on
it. `regen` takes a file under `<cwd>/.pirs/ext/` or `~/.pirs/ext/` and nothing else. Both
write here, on this machine, so they are refused for a `--server` reached over a bridge:
policy lives with the server, and that is where to run them.

## Models and credentials

- **Anthropic OAuth.** A Claude Code login is used when no API key is set
  (`~/.claude/.credentials.json`, honouring `CLAUDE_CONFIG_DIR`, or the macOS keychain),
  and a pi-style `~/.pirs/auth.json` before either.
- **API keys.** `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`.
- **Other providers.** `~/.pirs/models.json`, and `<project>/.pirs/models.json`: the
  provider catalogue of endpoints, model ids and context windows.
- **Choosing one.** `--model provider/id` or `--model <id>` per command, `[settings] model`
  in a policy file otherwise; `--thinking off|minimal|low|medium|high|xhigh|max`.
- **Offline.** The `faux` provider replays a script, which is what the tests use:

```bash
echo '["Two plus two is four."]' > script.json
PIRS_FAUX_SCRIPT=$PWD/script.json pirs --model faux/scripted "what is two plus two?"
```

`PIRS_FAUX_SCRIPT` is read by the *server*, so the command above works only when it
auto-starts one. Against a server that is already running, start that server with the
variable instead: `PIRS_FAUX_SCRIPT=$PWD/script.json pirs serve`.

## Where things live

`~/.pirs/` on the machine the server runs on (`PIRS_HOME` moves all of it):

| Path | What |
|---|---|
| `pirs.sock` | the server's socket (`$XDG_RUNTIME_DIR/pirs.sock` when that is set; `PIRS_SOCKET` overrides) |
| `sessions/<encoded cwd>/<timestamp>_<id>.jsonl` | the conversations, one file each |
| `ext/*.pirs.toml` | policy for every project |
| `models.json`, `auth.json` | provider catalogue and stored Anthropic login |
| `tui.toml`, `servers.toml` | the UI's config, and the servers it reaches |

`<project>/.pirs/ext/*.pirs.toml` is policy for one project, loaded alongside the global
files.

## Documentation and verification

[`docs/index.md`](docs/index.md) is the map: [`dsl.md`](docs/dsl.md) for the policy
vocabulary, [`protocol.md`](docs/protocol.md) for the wire,
[`containment.md`](docs/containment.md) for jails and remote servers,
[`session-format.md`](docs/session-format.md) for the session files, and
[`design/`](docs/design/00-north-star.md) for the design in layers, from the north star
down to the decision log.

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
scripts/acceptance/phase-0.sh        # … through phase-7.sh
```

Each `scripts/acceptance/phase-N.sh` is one phase's acceptance test and drives the real
binary with the faux provider on a temporary socket and home directory: no network, no
credentials. `scripts/acceptance/README.md` says what each one checks, and `STATUS.md`
records which phases have been built and verified.

## What is not here

- **pi extension compatibility.** pi's TypeScript extensions do not run; a customisation is
  a declaration, or a program in any language behind one (D-22 in
  `docs/design/90-decisions.md`).
- **Permission checks inside the loop.** pirs never decides what the model may do, and
  ships nothing that could be mistaken for such a check; containment is a boundary around
  the server (D-19, and [`docs/containment.md`](docs/containment.md) for how to get one).
