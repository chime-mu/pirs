#!/usr/bin/env bash
# Phase 3 acceptance: the TUI as a client (S6, S10-S14).
#
# The real binary drives the real server: `pirs tui --headless 100x30` runs
# the UI's own event loop on a `ratatui` test backend, reads a script of JSON
# lines on stdin and writes the screens it is asked for to stdout (the
# commands are listed in `crates/pirs-tui/README.md`). Every scenario gets a
# world of its own -- HOME, PIRS_HOME, XDG_RUNTIME_DIR, socket, project
# directory and a server with PIRS_FAUX_SCRIPT in *its* environment, because
# the faux provider's script cursor is process-wide.
#
#   1. Two agents appear in the sidebar with their states: one working
#      (its scripted turn is sleeping in `bash`) and one idle.
#   2. The attention flag appears when an agent goes idle and clears when its
#      page is viewed. The flag is the `!` the sidebar draws between the
#      selection cursor and the label (`crates/pirs-tui/src/render.rs`,
#      `draw_sidebar`: `{cursor}{flag} {label} {state}`, where `flag` is `!`
#      for an agent the server says is idle and the UI has not drawn since
#      -- `Agent::attention`), so the marker looked for below is `! <id>`.
#   3. A scripted tool write updates an open file page: the write puts the
#      file in the jump list, `1` opens its page, and a second write to the
#      same file refreshes the page that is already open (`fs.changed`).
#   4. A `[[render]]` command for a scripted `ask` tool call produces a
#      picker, and the chosen option is sent as the next prompt (S6, D-37).
#      There is no `ask` tool on the server, so the call's result is an
#      error; the call is in the message stream either way, which is the
#      whole point of the extension point.
#   5. The old interactive mode is gone: `pi-cli` is not in `cargo metadata`
#      and `pirs tui --help` no longer mentions `--extension`.
#
# Usage: scripts/acceptance/phase-3.sh [--build]
#
# It uses `target/release/pirs`, building it when it is missing or when
# `--build` is given. It needs no network and no credentials.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

bin="$root/target/release/pirs"
raw_py="$root/scripts/acceptance/lib/raw.py"
fixtures="$root/scripts/acceptance/fixtures/phase-3"

if [ "${1:-}" = "--build" ] || [ ! -x "$bin" ]; then
  echo "-- building target/release/pirs"
  cargo build --release
fi
[ -x "$bin" ] || { echo "FAIL  $bin was not built"; exit 1; }

# Every scenario moves HOME into its own temporary world, so the real one is
# kept here for the commands (cargo, rustup) that need a home of their own.
real_home="$HOME"

failures=0
pass() { echo "PASS  $*"; }
fail() { echo "FAIL  $*"; failures=$((failures + 1)); }

# ---------------------------------------------------------------- scenarios

tmp=""
server_pid=""
proj=""

stop_scenario() {
  if [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  server_pid=""
  if [ -n "$tmp" ] && [ -d "$tmp" ]; then rm -rf "$tmp"; fi
  tmp=""
}
trap stop_scenario EXIT INT TERM

# A temporary world: its own home, runtime directory, socket and project.
# `$PIRS_HOME/tui.toml` is the fixture, with the fixture directory patched
# into the `[[render]] run` command line.
new_world() {
  stop_scenario
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/pirs-phase3.XXXXXX")"
  mkdir -p "$tmp/home/.pirs" "$tmp/run" "$tmp/proj"
  # Canonical, so the directory the TUI sends in `loop.list { cwd }` is the
  # one raw.py created the loops in.
  proj="$(cd "$tmp/proj" && pwd -P)"
  export HOME="$tmp/home"
  export PIRS_HOME="$tmp/home/.pirs"
  export XDG_RUNTIME_DIR="$tmp/run"
  export PIRS_SOCKET="$tmp/pirs.sock"
  sed "s|@FIXTURES@|$fixtures|g" "$fixtures/tui.toml" >"$PIRS_HOME/tui.toml"
  # The clients must never see the script: it belongs to the server.
  unset PIRS_FAUX_SCRIPT
}

# Start this scenario's server. $1 is a faux script file, or "" for echo mode.
start_server() {
  local script="${1:-}"
  if [ -n "$script" ]; then
    PIRS_FAUX_SCRIPT="$script" "$bin" serve --idle 120 >"$tmp/server.log" 2>&1 &
  else
    "$bin" serve --idle 120 >"$tmp/server.log" 2>&1 &
  fi
  server_pid=$!
  local waited=0
  while [ ! -S "$PIRS_SOCKET" ]; do
    sleep 0.05
    waited=$((waited + 1))
    if [ "$waited" -gt 200 ]; then
      echo "FAIL  the server did not create $PIRS_SOCKET"
      cat "$tmp/server.log" || true
      exit 1
    fi
  done
}

raw() { python3 "$raw_py" --socket "$PIRS_SOCKET" "$@"; }

# Create one loop in the project and print its id. $1 tags the recording,
# $2 names the loop (optional). The sidebar's label is the name, or the id
# when there is none, and that label is what the screens are asserted on.
create_loop() {
  local out="$tmp/create-$1.jsonl" name="${2:-}" params
  params="{\"cwd\": \"$proj\", \"model\": {\"model\": \"faux/scripted\"}"
  if [ -n "$name" ]; then params="$params, \"name\": \"$name\""; fi
  raw >"$out" <<EOF
{"method": "loop.create", "params": $params}}
EOF
  jq -rs '[.[] | select(.result.id != null) | .result.id] | .[0] // ""' "$out"
}

# Run the headless UI on this world with the script on stdin; stdout goes to
# $tmp/tui.out and the exit code is returned.
tui() {
  "$bin" tui --headless 100x30 --cwd "$proj" >"$tmp/tui.out" 2>"$tmp/tui.err"
}

# The $1-th `=== screen ===` block of $tmp/tui.out (1-based).
dump() {
  awk -v want="$1" '
    /^=== screen ===$/ { n++; next }
    /^=== end ===$/ { next }
    n == want { print }
  ' "$tmp/tui.out"
}

# Everything the UI printed, for a failure message.
tui_trace() { echo "--- stdout ---"; cat "$tmp/tui.out"; echo "--- stderr ---"; cat "$tmp/tui.err"; }

# The one session log of this world.
session_file() { find "$PIRS_HOME/sessions" -name '*.jsonl' | head -1; }

# ------------------------------------------- 1: two agents, two states

echo "-- check 1: two agents in the sidebar, one working and one idle"
new_world
start_server "$fixtures/slow.json"
a="$(create_loop a)"
b="$(create_loop b)"
# `bash sleep 3` keeps this one working long enough for the UI to start,
# list both loops and draw them.
raw >/dev/null <<EOF
{"method": "loop.prompt", "params": {"loop": "$a", "text": "take your time"}}
EOF

if tui <<EOF
{"wait":"working","timeout":15000}
{"dump":true}
EOF
then
  screen="$(dump 1)"
  if grep -qE "$a[[:space:]]+working" <<<"$screen" && grep -qE "$b[[:space:]]+idle" <<<"$screen"; then
    pass "the sidebar shows $a working and $b idle"
  else
    fail "the sidebar does not show both agents with their states:"$'\n'"$screen"
  fi
else
  fail "the headless UI exited non-zero"; tui_trace
fi

# ------------------------------------- 2: the attention flag, and viewing it

echo "-- check 2: the attention flag appears on idle and clears when viewed"
new_world
start_server "$fixtures/slow.json"
# Only `alpha` exists when the UI starts, so `alpha` is the agent it selects
# and the one whose page is on screen; `beta` arrives afterwards and is never
# selected by itself, which is what makes it the unviewed one. Both are
# named, because `loop.list` does not promise an order and the assertions
# below must know which row is which. With two agents and no stored
# conversations the sidebar has two rows, so one `down` from `alpha` always
# lands on `beta`, whichever way round they are listed.
create_loop alpha alpha >/dev/null
(
  sleep 1
  raw --timeout 30 >"$tmp/beta.jsonl" <<EOF
{"method": "loop.create", "params": {"cwd": "$proj", "model": {"model": "faux/scripted"}, "name": "beta"}}
{"method": "loop.prompt", "params": {"loop": "\$LOOP", "text": "answer me"}}
{"method": "loop.wait", "params": {"loop": "\$LOOP"}}
EOF
) &
prompter=$!

if tui <<EOF
{"wait":"alpha","timeout":10000}
{"dump":true}
{"wait":"! beta","timeout":30000}
{"dump":true}
{"key":"down"}
{"settle":400}
{"dump":true}
EOF
then
  first="$(dump 1)"
  flagged="$(dump 2)"
  viewed="$(dump 3)"
  if grep -qE "^>[[:space:]]+alpha[[:space:]]" <<<"$first"; then
    pass "the UI selected the one agent it started with, alpha"
  else
    fail "the UI did not select alpha:"$'\n'"$first"
  fi
  if grep -q "! beta" <<<"$flagged"; then
    pass "the attention flag \`! beta\` appeared when beta went idle"
  else
    fail "no attention flag next to beta after its run:"$'\n'"$flagged"
  fi
  if grep -q "! beta" <<<"$viewed"; then
    fail "the attention flag is still there after viewing beta's page:"$'\n'"$viewed"
  elif grep -qE "^>[[:space:]]+beta[[:space:]]" <<<"$viewed"; then
    pass "selecting beta drew its page and cleared the flag"
  else
    fail "the selection did not move to beta:"$'\n'"$viewed"
  fi
else
  fail "the headless UI exited non-zero"; tui_trace
fi
wait "$prompter" 2>/dev/null || true

# ----------------------------------- 3: a tool write updates an open page

echo "-- check 3: a scripted write updates the file page that is open"
new_world
start_server "$fixtures/write.json"
a="$(create_loop a)"

if tui <<EOF
{"wait":"$a","timeout":10000}
{"text":"write the note"}
{"key":"enter"}
{"wait":"files: 1 ","timeout":20000}
{"key":"1"}
{"wait":"FIRST","timeout":10000}
{"dump":true}
{"text":"now change it"}
{"key":"enter"}
{"wait":"SECOND","timeout":20000}
{"dump":true}
EOF
then
  before="$(dump 1)"
  after="$(dump 2)"
  if grep -q "notes.txt" <<<"$before" && grep -q "FIRST" <<<"$before"; then
    pass "the jump list opened notes.txt and the file page shows FIRST"
  else
    fail "the file page does not show the first write:"$'\n'"$before"
  fi
  if grep -q "SECOND" <<<"$after" && ! grep -q "FIRST" <<<"$after"; then
    pass "the second write refreshed the open page to SECOND"
  else
    fail "the open page was not refreshed by the second write:"$'\n'"$after"
  fi
else
  fail "the headless UI exited non-zero"; tui_trace
fi

# --------------------------------- 4: a [[render]] picker and its choice

echo "-- check 4: a [[render]] hook draws a picker and the choice is a prompt"
new_world
start_server "$fixtures/ask.json"
a="$(create_loop a)"

if tui <<EOF
{"wait":"$a","timeout":10000}
{"text":"ask me something"}
{"key":"enter"}
{"wait":"choose (enter picks","timeout":20000}
{"dump":true}
{"key":"down"}
{"key":"enter"}
{"wait":"you chose it","timeout":20000}
{"dump":true}
EOF
then
  picker="$(dump 1)"
  if grep -q "Which one?" <<<"$picker" && grep -q "alpha" <<<"$picker" && grep -q "beta" <<<"$picker"; then
    pass "the ask call drew the hook's picker with both options"
  else
    fail "no picker for the ask call:"$'\n'"$picker"
  fi
  log="$(session_file)"
  # The prompts in order: what was typed, then the option that was chosen.
  prompts="$(jq -rs '[.[] | select(.type == "message" and .message.role == "user")
                       | .message.content
                       | if type == "string" then . else (map(.text // empty) | join("")) end]
                     | join("|")' "$log")"
  if [ "$prompts" = "ask me something|beta" ]; then
    pass "the chosen option was sent as the next prompt (user messages: $prompts)"
  else
    fail "the next prompt was not the chosen option (user messages: $prompts)"
  fi
else
  fail "the headless UI exited non-zero"; tui_trace
fi

# ---------------------------------------------- 5: the old mode is gone

echo "-- check 5: the old interactive mode and its crate are gone"
stop_scenario
if HOME="$real_home" cargo metadata --no-deps --format-version 1 \
  | jq -e '[.packages[].name] | index("pi-cli") == null' >/dev/null; then
  pass "pi-cli is not a workspace member any more (D-04)"
else
  fail "cargo metadata still lists pi-cli"
fi
if "$bin" tui --help 2>&1 | grep -q -- "--extension"; then
  fail "pirs tui --help still mentions --extension"
else
  pass "pirs tui --help is the new client's help: no --extension"
fi

# --------------------------------------------------------------------------

stop_scenario
if [ "$failures" = 0 ]; then
  echo "phase-3: OK"
else
  echo "phase-3: $failures check(s) failed"
  exit 1
fi
