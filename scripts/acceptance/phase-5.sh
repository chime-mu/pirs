#!/usr/bin/env bash
# Phase 5 acceptance: several agents (S16, S17).
#
# One `[[tool]]` with `loop = { model, prompt, wait = "idle" }` is the whole
# subject: the server starts a second agent in the same directory, prompts it
# with the interpolated prompt, waits for it to go idle and hands its final
# message back as the tool result.
#
# The faux provider's script cursor is process-wide, so the parent and the
# child take turns from one ordered script:
#
#   1. the parent calls `review { text: "the diff" }`
#   2. the child answers "CHILD-VERDICT: fine"
#   3. the parent says "parent says: done"
#
# Every scenario gets a world of its own: HOME, PIRS_HOME, XDG_RUNTIME_DIR,
# socket, project directory, and a server with PIRS_FAUX_SCRIPT in *its*
# environment.
#
#   1. `pirs "go"` prints the parent's own final message, and the parent's
#      session log holds the review toolResult with the child's answer and
#      `details.loop` naming the loop that produced it.
#   2. With the parent created through raw.py and left running, `loop.list`
#      holds two loops and the child's `parent` is the parent's id.
#   3. The child's answer reaches the model: with a script that stops after
#      the child's answer (step 3 absent), the parent's next message is the
#      faux echo of the tool result, so it quotes CHILD-VERDICT.
#   4. `pirs wait <parent>`, started before the parent is prompted and while
#      the child takes two seconds over its turn, prints `idle`, exits 0 and
#      took at least a second to do it — it waited for the run instead of
#      answering from the parent's state at the time. An agent that does not
#      exist exits 1.
#   5. `loop.close` on the parent takes the child with it: `loop.list` is
#      empty afterwards (D-28).
#
# Usage: scripts/acceptance/phase-5.sh [--build]
#
# It uses `target/release/pirs`, building it when it is missing or when
# `--build` is given. It needs no network and no credentials.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

bin="$root/target/release/pirs"
raw_py="$root/scripts/acceptance/lib/raw.py"
fixtures="$root/scripts/acceptance/fixtures/phase-5"

if [ "${1:-}" = "--build" ] || [ ! -x "$bin" ]; then
  echo "-- building target/release/pirs"
  cargo build --release
fi
[ -x "$bin" ] || { echo "FAIL  $bin was not built"; exit 1; }

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

# A temporary world with the review policy already installed.
new_world() {
  stop_scenario
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/pirs-phase5.XXXXXX")"
  mkdir -p "$tmp/home/.pirs" "$tmp/run" "$tmp/proj/.pirs/ext"
  proj="$(cd "$tmp/proj" && pwd -P)"
  cp "$fixtures/review.pirs.toml" "$proj/.pirs/ext/"
  export HOME="$tmp/home"
  export PIRS_HOME="$tmp/home/.pirs"
  export XDG_RUNTIME_DIR="$tmp/run"
  export PIRS_SOCKET="$tmp/pirs.sock"
  unset PIRS_FAUX_SCRIPT
}

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

# Every session log of this world (the parent's and the child's).
session_files() { find "$PIRS_HOME/sessions" -name '*.jsonl' 2>/dev/null; }

# Count the entries of every session log matching a jq filter.
entries_matching() {
  local filter="$1" files total=0 count
  files="$(session_files)"
  [ -n "$files" ] || { echo 0; return; }
  for file in $files; do
    count="$(jq -s "[.[] | select($filter)] | length" "$file")"
    total=$((total + count))
  done
  echo "$total"
}

# Create a loop in the project through the raw client and print its id.
create_loop() {
  printf '%s\n' "{\"method\": \"loop.create\", \"params\": {\"cwd\": \"$proj\", \"model\": {\"model\": \"faux/scripted\"}, \"name\": \"parent\"}}" |
    raw --timeout 10 |
    jq -rs '[.[] | select(.result.id != null) | .result.id] | first // empty'
}

# `loop.list`, as one JSON array of loops on stdout.
list_loops() {
  printf '%s\n' '{"method": "loop.list", "params": {}}' |
    raw --timeout 10 |
    jq -s '[.[] | select(.result.loops != null)] | last | .result.loops // []'
}

# ------------------------------- 1: a second agent answers, and it is logged

echo "-- check 1: pirs prints the parent's answer and logs the child's verdict"
new_world
start_server "$fixtures/review.json"
answer="$(pirs --model faux/scripted "go" 2>"$tmp/err")" && code=0 || code=$?

if [ "$code" = 0 ] && grep -q "parent says: done" <<<"$answer"; then
  pass "the parent's final message reached stdout: $(printf '%q' "$answer")"
else
  fail "pirs printed $(printf '%q' "$answer") and exited $code ($(cat "$tmp/err"))"
fi
if [ "$(session_files | wc -l)" = 2 ]; then
  pass "two conversations on disk: one per agent"
else
  fail "expected two session logs, found: $(session_files | tr '\n' ' ')"
fi
if [ "$(entries_matching '.type == "message" and .message.role == "toolResult" and .message.toolName == "review" and ((.message.content | tostring) | contains("CHILD-VERDICT"))')" -ge 1 ]; then
  pass "the review toolResult holds the child's verdict"
else
  fail "no review toolResult with CHILD-VERDICT in $(session_files | tr '\n' ' ')"
fi
if [ "$(entries_matching '.type == "message" and .message.role == "toolResult" and .message.toolName == "review" and (.message.details.loop | type) == "string" and (.message.details.conversation | type) == "string"')" -ge 1 ]; then
  pass "its details name the loop and the conversation the answer came from"
else
  fail "the review toolResult has no details.loop: $(jq -c 'select(.message.role == "toolResult")' $(session_files) 2>/dev/null)"
fi

# ------------------------------------ 2: the child is a loop with a parent

echo "-- check 2: loop.list shows the child under its parent"
new_world
# This world's child sleeps two seconds before it answers, so the run is
# long enough for `pirs wait` to be caught blocking (check 4).
start_server "$fixtures/slow-review.json"
parent="$(create_loop)"
[ -n "$parent" ] || { echo "FAIL  no loop was created"; cat "$tmp/server.log"; exit 1; }

# The waiter is started before the parent is prompted and is what ends the
# run for this script: it returns when the parent goes idle, not before.
# It settles for a moment first, because a `loop.wait` that arrives before
# the prompt finds an idle loop and returns at once — as it should.
(
  sleep 0.5
  started="$(date +%s%N)"
  "$bin" --cwd "$proj" wait "$parent" >"$tmp/wait.out" 2>"$tmp/wait.err"
  echo $? >"$tmp/wait.code"
  echo $((($(date +%s%N) - started) / 1000000)) >"$tmp/wait.ms"
) &
waiter=$!

printf '%s\n' \
  "{\"method\": \"loop.prompt\", \"params\": {\"loop\": \"$parent\", \"text\": \"go\"}}" |
  raw --timeout 30 >"$tmp/run.out" 2>"$tmp/run.err" || true
wait "$waiter"

loops="$(list_loops)"
count="$(jq 'length' <<<"$loops")"
child="$(jq -r --arg p "$parent" '[.[] | select(.parent == $p)] | first // {} | .id // empty' <<<"$loops")"
if [ "$count" = 2 ] && [ -n "$child" ]; then
  pass "two loops are running and $child names $parent as its parent"
else
  fail "loop.list is $(jq -c . <<<"$loops")"
fi
if [ "$(jq -r --arg c "$child" '[.[] | select(.id == $c)] | first | .name // empty' <<<"$loops")" = "parent/review" ]; then
  pass "the child is named after the loop and the tool that started it"
else
  fail "the child's name is $(jq -c --arg c "$child" '[.[] | select(.id == $c)] | first | .name' <<<"$loops")"
fi

# ------------------------------------------------- 4: pirs wait blocks
# (The waiter check 2 started, and the world it left standing.)

echo "-- check 4: pirs wait blocked until the parent was idle"
code="$(cat "$tmp/wait.code" 2>/dev/null || echo missing)"
state="$(cat "$tmp/wait.out" 2>/dev/null || true)"
elapsed="$(cat "$tmp/wait.ms" 2>/dev/null || echo 0)"
if [ "$code" = 0 ] && [ "$state" = "idle" ]; then
  pass "pirs wait $parent printed idle and exited 0"
else
  fail "pirs wait $parent printed $(printf '%q' "$state") and exited $code ($(cat "$tmp/wait.err" 2>/dev/null))"
fi
if [ "$elapsed" -ge 1000 ]; then
  pass "it blocked for ${elapsed}ms while the child worked, rather than answering at once"
else
  fail "pirs wait returned after ${elapsed}ms: it did not wait for the run"
fi
state="$(pirs wait "$child" 2>"$tmp/wait2.err")" && code=0 || code=$?
if [ "$code" = 0 ] && [ "$state" = "idle" ]; then
  pass "and waiting on the idle child returns idle at once"
else
  fail "pirs wait $child printed $(printf '%q' "$state") and exited $code ($(cat "$tmp/wait2.err"))"
fi
pirs wait no-such-agent >"$tmp/wait3.out" 2>"$tmp/wait3.err" && code=0 || code=$?
if [ "$code" = 1 ] && grep -q "no running agent" "$tmp/wait3.err"; then
  pass "an agent that does not exist exits 1 with a message"
else
  fail "pirs wait no-such-agent exited $code: $(cat "$tmp/wait3.out" "$tmp/wait3.err")"
fi

# -------------------------------- 5: closing the parent closes the child

echo "-- check 5: loop.close on the parent takes the child with it"
printf '%s\n' "{\"method\": \"loop.close\", \"params\": {\"loop\": \"$parent\"}}" |
  raw --timeout 15 >"$tmp/close.out" 2>"$tmp/close.err" || true
loops="$(list_loops)"
if [ "$(jq 'length' <<<"$loops")" = 0 ]; then
  pass "no loop is left after closing the parent (D-28)"
else
  fail "still running after loop.close: $(jq -c . <<<"$loops")"
fi

# ------------------------- 3: the child's answer reaches the model itself

echo "-- check 3: the parent's next message quotes the child's answer"
# The script stops after the child's answer, so the parent's next turn is the
# faux echo mode: it repeats the tool result it was given. That the echo
# carries CHILD-VERDICT is the proof that the answer reached the model, not
# only the log.
new_world
start_server "$fixtures/echo.json"
answer="$(pirs --model faux/scripted "go" 2>"$tmp/err")" && code=0 || code=$?
if [ "$code" = 0 ] && grep -q "CHILD-VERDICT: fine" <<<"$answer"; then
  pass "the parent's next message echoes the child: $(printf '%q' "$answer")"
else
  fail "the child's answer did not reach the model: $(printf '%q' "$answer") ($code, $(cat "$tmp/err"))"
fi

stop_scenario
export HOME="$real_home"

# --------------------------------------------------------------------------

if [ "$failures" = 0 ]; then
  echo "phase-5: OK"
else
  echo "phase-5: $failures check(s) failed"
  exit 1
fi
