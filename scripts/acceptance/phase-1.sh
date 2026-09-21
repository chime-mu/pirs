#!/usr/bin/env bash
# Phase 1 acceptance: the server and the print client (S1, S2).
#
# Every scenario gets a server of its own, started here in the background with
# a temporary HOME, PIRS_HOME, XDG_RUNTIME_DIR, socket and project directory,
# and with PIRS_FAUX_SCRIPT in the *server's* environment (the faux provider
# reads it there, and its script cursor is process-wide, so a scripted
# scenario cannot share a server with another one).
#
#   1. A scripted answer reaches stdout and the command exits; `pirs stop`
#      then finds no running loop and the server is gone.
#   2. Two one-shots in one directory are two agents and two conversations.
#   3. `pirs --continue` appends to the most recent conversation rather than
#      starting another; `--continue=<name>` picks one by name.
#   4. A tool result above the 64 KB threshold travels as a `ref` and
#      `fs.read` serves it in full.
#   5. A client that subscribes with `since: 0` after the run replays the
#      sequenced events, `loop.turn_end` included, and no deltas.
#   6. `pirs serve --idle 1` exits by itself once its last client is gone.
#   7. `pirs tui` is a client of the same server. (Phase 1 ran the old
#      in-process interactive mode here; phase 3 replaced it with `pirs-tui`
#      and deleted the crate the old mode lived in, so what is left to check
#      is that the subcommand still works against this phase's server.)
#
# Usage: scripts/acceptance/phase-1.sh [--build]
#
# It uses `target/release/pirs`, building it when it is missing or when
# `--build` is given. It needs no network and no credentials.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

bin="$root/target/release/pirs"
raw_py="$root/scripts/acceptance/lib/raw.py"
fixtures="$root/scripts/acceptance/fixtures/phase-1"

if [ "${1:-}" = "--build" ] || [ ! -x "$bin" ]; then
  echo "-- building target/release/pirs"
  cargo build --release
fi
[ -x "$bin" ] || { echo "FAIL  $bin was not built"; exit 1; }

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
new_world() {
  stop_scenario
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/pirs-phase1.XXXXXX")"
  mkdir -p "$tmp/home" "$tmp/run" "$tmp/proj"
  proj="$tmp/proj"
  export HOME="$tmp/home"
  export PIRS_HOME="$tmp/home/.pirs"
  export XDG_RUNTIME_DIR="$tmp/run"
  export PIRS_SOCKET="$tmp/pirs.sock"
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

pirs() { "$bin" --cwd "$proj" "$@"; }
raw() { python3 "$raw_py" --socket "$PIRS_SOCKET" "$@"; }

# Lines of `pirs --list`: a conversation is `id  <ISO instant>  [name]`,
# anything else is a running loop.
conversation_lines() { pirs --list | grep -cE '^[^ ]+  [0-9]{4}-[0-9]{2}-[0-9]{2}T' || true; }
running_lines() { pirs --list | grep -cvE '^[^ ]+  [0-9]{4}-[0-9]{2}-[0-9]{2}T' || true; }

# The session log of conversation $1, and how many user messages it holds.
session_file() { find "$PIRS_HOME/sessions" -name "*_$1.jsonl" | head -1; }
user_messages() {
  python3 - "$1" <<'PY'
import json, sys
count = 0
with open(sys.argv[1]) as handle:
    for line in handle:
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue
        if entry.get("type") == "message" and (entry.get("message") or {}).get("role") == "user":
            count += 1
print(count)
PY
}

# ------------------------------------------------- 1: one shot, then no shot

echo "-- check 1: a scripted answer, then nothing left running"
new_world
start_server "$fixtures/hello.json"
answer="$(pirs --model faux/scripted "hi")" && code=0 || code=$?
if [ "$code" = 0 ] && [ "$answer" = "hello from faux" ]; then
  pass "pirs \"hi\" printed the scripted answer and exited 0"
else
  fail "pirs \"hi\" printed $(printf '%q' "$answer") and exited $code"
fi

stopped="$("$bin" stop)"
if [ "$stopped" = "no running loop" ]; then
  pass "pirs stop found no running loop (the one-shot closed its agent)"
else
  fail "pirs stop said $(printf '%q' "$stopped"), expected 'no running loop'"
fi

gone=0
for _ in $(seq 1 100); do
  [ -S "$PIRS_SOCKET" ] || { gone=1; break; }
  sleep 0.05
done
if [ "$gone" = 1 ] && ! kill -0 "$server_pid" 2>/dev/null; then
  pass "pirs stop stopped the server and removed the socket"
else
  fail "the server is still there after pirs stop"
fi
server_pid=""

# ------------------------------------------------ 2: two agents, one project

echo "-- check 2: two one-shots in one directory"
new_world
start_server ""
pirs --model faux/scripted "x one" >"$tmp/a.out" 2>"$tmp/a.err" &
first=$!
pirs --model faux/scripted "x two" >"$tmp/b.out" 2>"$tmp/b.err" &
second=$!
ok=1
wait "$first" || ok=0
wait "$second" || ok=0
if [ "$ok" = 1 ] && grep -q "x one" "$tmp/a.out" && grep -q "x two" "$tmp/b.out"; then
  pass "both one-shots finished and each echoed its own prompt"
else
  fail "the two one-shots did not both finish: $(cat "$tmp/a.out" "$tmp/a.err" "$tmp/b.out" "$tmp/b.err")"
fi

conversations="$(conversation_lines)"
running="$(running_lines)"
if [ "$conversations" = 2 ] && [ "$running" = 0 ]; then
  pass "pirs --list shows two conversations and no running loop"
else
  fail "pirs --list shows $conversations conversations and $running running loops, expected 2 and 0"
fi

# ------------------------------------------------------- 3: continuing

echo "-- check 3: --continue resumes rather than starting another"
new_world
start_server ""
pirs --model faux/scripted "first question" >/dev/null
pirs --model faux/scripted --name notes "named question" >/dev/null

recent="$(pirs --list | head -1 | cut -d' ' -f1)"
recent_file="$(session_file "$recent")"
before="$(user_messages "$recent_file")"
pirs --model faux/scripted --continue "and another thing" >/dev/null
after="$(user_messages "$recent_file")"
conversations="$(conversation_lines)"

if [ "$conversations" = 2 ]; then
  pass "pirs --continue left the directory with two conversations"
else
  fail "pirs --continue left $conversations conversations, expected 2"
fi
if [ "$after" -ge $((before + 1)) ]; then
  pass "the most recent conversation grew from $before to $after user messages"
else
  fail "the most recent conversation still has $before user messages"
fi

notes_id="$(pirs --list | grep '  notes$' | head -1 | cut -d' ' -f1)"
notes_file="$(session_file "$notes_id")"
notes_before="$(user_messages "$notes_file")"
pirs --model faux/scripted --continue=notes "back to the notes" >/dev/null
notes_after="$(user_messages "$notes_file")"
if [ -n "$notes_id" ] && [ "$notes_after" -ge $((notes_before + 1)) ] && [ "$(conversation_lines)" = 2 ]; then
  pass "pirs --continue=notes appended to the conversation named 'notes'"
else
  fail "pirs --continue=notes did not append to 'notes' ($notes_before -> $notes_after)"
fi

# -------------------------------------------------- 4: a tool result by ref

echo "-- check 4: a tool result above 64 KB travels as a ref"
new_world
start_server "$fixtures/big-tool.json"
head -c 70000 /dev/zero | tr '\0' x >"$tmp/big.txt"
# The oversized result comes from a registered handler: every built-in tool
# truncates its output at 50 KB, so `bash` cannot produce one (see the report
# for phase 1 stage C). The handler path is the server's own `tool.<name>`
# slot, which is what a policy-defined tool will use from phase 4 on.
raw --timeout 30 --reply "tool.big=$tmp/big.txt" >"$tmp/big.jsonl" <<EOF
{"method": "loop.create", "params": {"cwd": "$proj", "model": {"model": "faux/scripted"}}}
{"method": "register", "params": {"loop": "\$LOOP", "slot": "tool.big", "timeout": 20000}}
{"method": "subscribe", "params": {"loop": "*"}}
{"method": "loop.prompt", "params": {"loop": "\$LOOP", "text": "go"}}
{"method": "loop.wait", "params": {"loop": "\$LOOP"}}
EOF

loop_id="$(jq -rs '[.[] | select(.result.id != null) | .result.id] | .[0] // ""' "$tmp/big.jsonl")"
ref="$(jq -rs '[.[] | select(.method == "loop.message") | .params.message | select(.role == "toolResult") | .details.ref // .details.refs[0]] | .[0].ref // ""' "$tmp/big.jsonl")"
bytes="$(jq -rs '[.[] | select(.method == "loop.message") | .params.message | select(.role == "toolResult") | .details.ref // .details.refs[0]] | .[0].bytes // 0' "$tmp/big.jsonl")"

if [ -n "$ref" ] && [ "$bytes" -ge 70000 ]; then
  pass "the toolResult event carries a ref of $bytes bytes"
else
  fail "no ref on the toolResult event (ref=$(printf '%q' "$ref") bytes=$bytes)"
fi

if [ -n "$ref" ] && [ -n "$loop_id" ]; then
  raw --timeout 20 >"$tmp/read.jsonl" <<EOF
{"method": "fs.read", "params": {"loop": "$loop_id", "path": "$ref"}}
EOF
  length="$(jq -rs '[.[] | select(.result.content != null) | .result.content | length] | .[0] // 0' "$tmp/read.jsonl")"
  if [ "$length" = "$bytes" ]; then
    pass "fs.read on the ref returned all $length bytes"
  else
    fail "fs.read on the ref returned $length bytes, expected $bytes"
  fi
else
  fail "fs.read was not attempted: no ref to read"
fi

# ------------------------------------------------------- 5: replay by seq

echo "-- check 5: subscribing with since after the run replays it"
new_world
start_server ""
raw --timeout 30 >"$tmp/run.jsonl" <<EOF
{"method": "loop.create", "params": {"cwd": "$proj", "model": {"model": "faux/scripted"}}}
{"method": "loop.prompt", "params": {"loop": "\$LOOP", "text": "say something"}}
{"method": "loop.wait", "params": {"loop": "\$LOOP"}}
EOF
replay_loop="$(jq -rs '[.[] | select(.result.id != null) | .result.id] | .[0] // ""' "$tmp/run.jsonl")"

raw --timeout 10 --until loop.turn_end >"$tmp/replay.jsonl" <<EOF
{"method": "subscribe", "params": {"loop": "$replay_loop", "since": 0}}
EOF

if jq -es --arg loop "$replay_loop" \
  'any(.[]; .method == "loop.turn_end" and .params.loop == $loop)' "$tmp/replay.jsonl" >/dev/null; then
  pass "a fresh connection replayed loop.turn_end for a finished run"
else
  fail "the replay carried no loop.turn_end for $replay_loop"
fi

if jq -es '[.[] | select(.method == "loop.message") | .params] as $m
           | ($m | length) > 0
             and ($m | all(has("seq")))
             and ($m | all(has("delta") | not))' "$tmp/replay.jsonl" >/dev/null; then
  pass "the replayed loop.message events all carry a seq and none is a delta"
else
  fail "the replayed loop.message events are not sequenced complete messages"
fi

# ------------------------------------------------------------ 6: idle exit

echo "-- check 6: the server exits when it has been idle"
new_world
"$bin" serve --idle 1 >"$tmp/server.log" 2>&1 &
server_pid=$!
waited=0
while [ ! -S "$PIRS_SOCKET" ]; do
  sleep 0.05
  waited=$((waited + 1))
  [ "$waited" -gt 200 ] && break
done
pirs --list >/dev/null
exited=0
for _ in $(seq 1 50); do
  if ! kill -0 "$server_pid" 2>/dev/null; then exited=1; break; fi
  sleep 0.2
done
if [ "$exited" = 1 ]; then
  pass "pirs serve --idle 1 exited within 10s of its last client leaving"
else
  fail "pirs serve --idle 1 is still running"
fi
wait "$server_pid" 2>/dev/null || true
server_pid=""

# ----------------------------------------------------- 7: the old TUI crate

echo "-- check 7: pirs tui is a client of the same server"
new_world
start_server ""
# Headless, because there is no terminal here: the UI's own event loop on a
# test backend, driven by a script of JSON lines (crates/pirs-tui/README.md).
"$bin" tui --headless 60x12 --cwd "$proj" >"$tmp/tui.out" 2>"$tmp/tui.err" <<'SCRIPT' && code=0 || code=$?
{"dump":true}
{"quit":true}
SCRIPT
if [ "${code:-0}" = 0 ] && grep -q "agents" "$tmp/tui.out"; then
  pass "pirs tui attached to this phase's server and drew its sidebar"
else
  fail "pirs tui exited ${code:-0}: $(cat "$tmp/tui.out" "$tmp/tui.err")"
fi

# --------------------------------------------------------------------------

stop_scenario
if [ "$failures" = 0 ]; then
  echo "phase-1: OK"
else
  echo "phase-1: $failures check(s) failed"
  exit 1
fi
