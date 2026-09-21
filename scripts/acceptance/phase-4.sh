#!/usr/bin/env bash
# Phase 4 acceptance: executables and connected handlers (S5, S7, S8, S9).
#
# Where phase-2 asserted policy that is declaration alone, this one asserts
# the policy that runs code: `run =` naming an executable the server spawns
# with one JSON line on stdin, and processes that connect back to
# `PIRS_SOCKET` and register for a slot. The subjects are the six examples
# under `examples/policy/`, installed exactly the way their READMEs say --
# `<example>/*.pirs.toml` into `<project>/.pirs/ext/` and `<example>/tools/`
# into `<project>/tools/` -- so a broken example fails this phase.
#
# Every scenario gets a world of its own: HOME, PIRS_HOME, XDG_RUNTIME_DIR,
# socket, project directory, and a server with PIRS_FAUX_SCRIPT in *its*
# environment, because the faux provider's script cursor is process-wide.
#
#   1. `fetch`: a scripted faux call to `fetch { url: "file://.../data.txt" }`
#      spawns `tools/fetch.py`, and the file's text reaches the model -- the
#      faux echo of the next turn quotes it, and the toolResult in the
#      session log holds it.
#   2. `watch`: `[[on]] event = "start"` spawns `tools/watch.py`, which
#      connects, registers `on.turn_end` and logs every turn; once its pid
#      file is there a turn is asked for, and after `loop.close` its pid is
#      gone within 3 s (D-23).
#   3. A registered `input` handler that never replies is skipped after its
#      `timeout`: the turn completes (`loop.wait` returns idle) and a
#      `ui.notify` warning names the slot.
#   4. `ask`: the call is ordinary JSON in the session log -- `"name":"ask"`
#      with the question and the options -- and `tools/ask.py`'s "stop and
#      wait" answer is the tool result (D-37).
#   5. `git-checkpoint`: a turn that writes a file is followed by a commit
#      with the subject `pirs checkpoint`.
#   6. `input-shortcuts`: `?why` reaches the model rewritten, and `!echo ...`
#      is consumed -- a bashExecution in the log and no model call at all.
#   7. `todo`: the `[[widget]]` is in the manifest's `widget_keys`, its
#      `start` listing arrives as a `ui.widget { key: "todo" }` event, and a
#      turn that writes the file sends a second one with the new lines.
#   8. `pi-ext` is gone: not in `cargo metadata`, no crate directory, no
#      `examples/extensions/`.
#
# Usage: scripts/acceptance/phase-4.sh [--build]
#
# It uses `target/release/pirs`, building it when it is missing or when
# `--build` is given. It needs no network and no credentials.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

bin="$root/target/release/pirs"
raw_py="$root/scripts/acceptance/lib/raw.py"
fixtures="$root/scripts/acceptance/fixtures/phase-4"
examples="$root/examples/policy"

if [ "${1:-}" = "--build" ] || [ ! -x "$bin" ]; then
  echo "-- building target/release/pirs"
  cargo build --release
fi
[ -x "$bin" ] || { echo "FAIL  $bin was not built"; exit 1; }

# Every scenario moves HOME into its own temporary world, so the real one is
# kept here for the commands (cargo) that need a home of their own.
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
new_world() {
  stop_scenario
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/pirs-phase4.XXXXXX")"
  mkdir -p "$tmp/home/.pirs" "$tmp/run" "$tmp/proj/.pirs/ext"
  # Canonical, so the cwd a raw client sends is the one the loop reports.
  proj="$(cd "$tmp/proj" && pwd -P)"
  export HOME="$tmp/home"
  export PIRS_HOME="$tmp/home/.pirs"
  export XDG_RUNTIME_DIR="$tmp/run"
  export PIRS_SOCKET="$tmp/pirs.sock"
  # The clients must never see the script: it belongs to the server.
  unset PIRS_FAUX_SCRIPT
}

# Install one example the way its README says: the policy files into
# `<project>/.pirs/ext/`, the scripts into `<project>/tools/`, executable.
install_example() {
  local name="$1"
  cp "$examples/$name"/*.pirs.toml "$proj/.pirs/ext/"
  if [ -d "$examples/$name/tools" ]; then
    mkdir -p "$proj/tools"
    for script in "$examples/$name/tools"/*.py; do
      cp "$script" "$proj/tools/"
      chmod +x "$proj/tools/$(basename "$script")"
    done
  fi
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

# A faux script with @PROJECT@ replaced by this scenario's project directory.
faux_script() {
  sed "s|@PROJECT@|$proj|g" "$fixtures/$1" >"$tmp/$1"
  echo "$tmp/$1"
}

pirs() { "$bin" --cwd "$proj" "$@"; }
raw() { python3 "$raw_py" --socket "$PIRS_SOCKET" "$@"; }

# The one session log of this world.
session_file() { find "$PIRS_HOME/sessions" -name '*.jsonl' | head -1; }

# Count the entries of a session log matching a jq filter.
entries_matching() {
  local file="$1" filter="$2"
  [ -n "$file" ] && [ -f "$file" ] || { echo 0; return; }
  jq -s "[.[] | select($filter)] | length" "$file"
}

# Create one loop in the project through the raw client and print its id.
create_loop() {
  printf '%s\n' "{\"method\": \"loop.create\", \"params\": {\"cwd\": \"$proj\", \"model\": {\"model\": \"faux/scripted\"}}}" |
    raw --timeout 10 |
    jq -rs '[.[] | select(.result.id != null) | .result.id] | first // empty'
}

# Wait up to $2 tenths of a second for the file $1 to hold the pattern $3.
wait_for_line() {
  local file="$1" tries="$2" pattern="$3" waited=0
  while [ "$waited" -lt "$tries" ]; do
    if [ -f "$file" ] && grep -q "$pattern" "$file"; then return 0; fi
    sleep 0.1
    waited=$((waited + 1))
  done
  return 1
}

# --------------------------------------------- 1: an executable tool, fetch

echo "-- check 1: the fetch example's script runs and its text reaches the model"
new_world
install_example fetch
printf 'DATA-FROM-THE-FILE\n' >"$proj/data.txt"
start_server "$(faux_script fetch.json)"
answer="$(pirs --model faux/scripted "read the file" 2>"$tmp/err")" && code=0 || code=$?
log="$(session_file)"

if [ "$code" = 0 ] && grep -q "DATA-FROM-THE-FILE" <<<"$answer"; then
  pass "the model's next message quotes the file: $(printf '%q' "$answer")"
else
  fail "the file's text did not reach the model: $(printf '%q' "$answer") ($code, $(cat "$tmp/err"))"
fi
if [ "$(entries_matching "$log" '.type == "message" and .message.role == "toolResult" and ((.message.content | tostring) | contains("DATA-FROM-THE-FILE"))')" -ge 1 ]; then
  pass "the toolResult entry of the session log holds the file's text"
else
  fail "no toolResult with the file's text in $log"
fi

# ------------------------------------------- 2: a connected process, watch

echo "-- check 2: the watch example logs the turn and dies with the loop"
new_world
install_example watch
start_server ""
loop_id="$(create_loop)"
[ -n "$loop_id" ] || { echo "FAIL  no loop was created"; cat "$tmp/server.log"; exit 1; }

# `[[on]] event = "start"` is fire and forget, so the watcher connects and
# registers on its own time and the pid file is how it says it is up. Waiting
# for that instead of racing a turn against it is what makes this check
# deterministic; only then is a turn worth asking for.
if wait_for_line "$proj/.pirs/watch.pid" 50 "[0-9]"; then
  watch_pid="$(tr -d '[:space:]' <"$proj/.pirs/watch.pid")"
  pass "the watcher started and wrote its pid $watch_pid"
else
  fail "the watcher never wrote $proj/.pirs/watch.pid ($(cat "$tmp/server.log"))"
  watch_pid=""
fi

printf '%s\n' \
  "{\"method\": \"loop.prompt\", \"params\": {\"loop\": \"$loop_id\", \"text\": \"hi\"}}" \
  "{\"method\": \"loop.wait\", \"params\": {\"loop\": \"$loop_id\"}}" |
  raw --timeout 20 >"$tmp/watch.out" 2>"$tmp/watch.err" || true
if wait_for_line "$proj/.pirs/watch.log" 50 "turn_end"; then
  pass "the connected watcher received on.turn_end and logged it"
else
  fail "no turn_end line in $proj/.pirs/watch.log ($(cat "$tmp/watch.err" "$tmp/server.log"))"
fi

printf '%s\n' "{\"method\": \"loop.close\", \"params\": {\"loop\": \"$loop_id\"}}" |
  raw --timeout 10 >"$tmp/close.out" 2>"$tmp/close.err" || true
if [ -n "$watch_pid" ]; then
  dead=0
  for _ in $(seq 1 30); do
    kill -0 "$watch_pid" 2>/dev/null || { dead=1; break; }
    sleep 0.1
  done
  if [ "$dead" = 1 ]; then
    pass "the watcher's pid $watch_pid is gone within 3 s of loop.close (D-23)"
  else
    fail "the watcher $watch_pid is still alive after loop.close"
  fi
fi

# ------------------------------------- 3: a registered handler that is slow

echo "-- check 3: an input handler that never replies is skipped with a warning"
new_world
start_server ""
loop_id="$(create_loop)"
[ -n "$loop_id" ] || { echo "FAIL  no loop was created"; cat "$tmp/server.log"; exit 1; }

# A client that registers for `input` with a 300 ms timeout and then does
# nothing at all with the request it is sent. It stays subscribed, so the
# warning the server emits when it gives up is on its stdout.
printf '%s\n' \
  "{\"method\": \"register\", \"params\": {\"loop\": \"$loop_id\", \"slot\": \"input\", \"timeout\": 300}}" \
  "{\"method\": \"subscribe\", \"params\": {\"loop\": \"$loop_id\", \"since\": 0}}" |
  raw --timeout 20 --until loop.run_end >"$tmp/handler.out" 2>"$tmp/handler.err" &
handler_job=$!
registered=0
for _ in $(seq 1 100); do
  # hello, register, subscribe: three answered requests and it is listening.
  if [ -f "$tmp/handler.out" ] && [ "$(grep -c '"result"' "$tmp/handler.out" || true)" -ge 3 ]; then
    registered=1
    break
  fi
  sleep 0.1
done
if [ "$registered" = 1 ]; then
  pass "a raw client registered for input with timeout 300"
else
  fail "the raw client did not register: $(cat "$tmp/handler.out" "$tmp/handler.err")"
fi

started="$(date +%s%N)"
printf '%s\n' \
  "{\"method\": \"loop.prompt\", \"params\": {\"loop\": \"$loop_id\", \"text\": \"hello\"}}" \
  "{\"method\": \"loop.wait\", \"params\": {\"loop\": \"$loop_id\"}}" |
  raw --timeout 15 >"$tmp/prompt.out" 2>"$tmp/prompt.err" || true
elapsed_ms=$((($(date +%s%N) - started) / 1000000))
state="$(jq -rs '[.[] | select(.result.state != null) | .result.state] | first // empty' "$tmp/prompt.out")"

if [ "$state" = "idle" ] && [ "$elapsed_ms" -lt 5000 ]; then
  pass "loop.wait returned idle in ${elapsed_ms} ms although the handler never answered"
else
  fail "loop.wait returned $(printf '%q' "$state") after ${elapsed_ms} ms: $(cat "$tmp/prompt.err")"
fi
wait "$handler_job" 2>/dev/null || true
if jq -es 'any(.[]; .method == "ui.notify" and .params.level == "warning" and (.params.text | contains("input")))' \
   "$tmp/handler.out" >/dev/null 2>&1; then
  pass "a ui.notify warning says the input handler was skipped"
else
  fail "no ui.notify warning for the input slot: $(cat "$tmp/handler.out")"
fi

# ------------------------------------------------ 4: ask, as JSON in the log

echo "-- check 4: the ask example's call is ordinary JSON in the session log"
new_world
install_example ask
start_server "$(faux_script ask.json)"
pirs --model faux/scripted "should we deploy" >"$tmp/out" 2>"$tmp/err" || true
log="$(session_file)"

if [ "$(entries_matching "$log" '.type == "message" and .message.role == "assistant" and ((.message.content | tostring) | contains("\"name\":\"ask\"")) and ((.message.content | tostring) | contains("\"options\":[\"yes\",\"no\"]")) and ((.message.content | tostring) | contains("Deploy?"))')" -ge 1 ]; then
  pass "the assistant's toolCall carries the question and the options as JSON"
else
  fail "no ask toolCall with its arguments in $log"
fi
if [ "$(entries_matching "$log" '.type == "message" and .message.role == "toolResult" and ((.message.content | tostring) | contains("The question has been shown to the user"))')" -ge 1 ]; then
  pass "the tool result is what tools/ask.py printed"
else
  fail "no tool result from ask.py in $log"
fi

# --------------------------------------------------- 5: git-checkpoint

echo "-- check 5: a turn that writes is followed by a pirs checkpoint commit"
new_world
install_example git-checkpoint
git -C "$proj" init -q
git -C "$proj" config user.name "pirs acceptance"
git -C "$proj" config user.email "acceptance@pirs.invalid"
printf 'start\n' >"$proj/README"
git -C "$proj" add -A
git -C "$proj" -c commit.gpgsign=false commit -qm "initial"
before="$(git -C "$proj" rev-list --count HEAD)"
start_server "$(faux_script write.json)"
pirs --model faux/scripted "write the note" >"$tmp/out" 2>"$tmp/err" || true

committed=0
for _ in $(seq 1 30); do
  if git -C "$proj" log --oneline | head -1 | grep -q "pirs checkpoint"; then committed=1; break; fi
  sleep 0.1
done
after="$(git -C "$proj" rev-list --count HEAD)"
if [ "$committed" = 1 ] && [ "$after" -gt "$before" ]; then
  pass "the turn_end entry committed: $(git -C "$proj" log --oneline | head -1)"
else
  fail "no pirs checkpoint commit after the turn: $(git -C "$proj" log --oneline | head -3)"
fi
if [ -f "$proj/notes.txt" ] && git -C "$proj" show --name-only --format= HEAD | grep -q "notes.txt"; then
  pass "the file the turn wrote is in the checkpoint"
else
  fail "notes.txt is not in the checkpoint: $(git -C "$proj" show --name-only --format= HEAD)"
fi

# ------------------------------------------------------ 6: input-shortcuts

echo "-- check 6: ? rewrites the input and ! never reaches the model"
new_world
install_example input-shortcuts
start_server ""
answer="$(pirs --model faux/scripted "?why" 2>"$tmp/err")" && code=0 || code=$?
if [ "$code" = 0 ] && [ "$answer" = "(faux) Explain briefly: why" ]; then
  pass "pirs \"?why\" printed $(printf '%q' "$answer")"
else
  fail "pirs \"?why\" printed $(printf '%q' "$answer") and exited $code ($(cat "$tmp/err"))"
fi

new_world
install_example input-shortcuts
start_server ""
pirs --model faux/scripted "!echo shell-ran" >"$tmp/out" 2>"$tmp/err" || true
log="$(session_file)"
if [ "$(entries_matching "$log" '.type == "message" and .message.role == "bashExecution" and ((.message.output | tostring) | contains("shell-ran"))')" -ge 1 ]; then
  pass "the shell command's output is in the conversation as a bashExecution"
else
  fail "no bashExecution with shell-ran in $log"
fi
if [ "$(entries_matching "$log" '.type == "message" and .message.role == "assistant"')" = 0 ]; then
  pass "no model turn ran for the consumed input"
else
  fail "the consumed input still reached the model"
fi

# ------------------------------------------------------------- 7: the todo

echo "-- check 7: the todo widget is in the manifest, fires on start and refreshes"
new_world
install_example todo
printf -- '- [ ] finish phase 4\n' >"$proj/.pirs/todo.md"
# The scripted turn rewrites the todo file with a `write` call, so the
# `on = ["start", "tool_result"]` widget has something new to say afterwards.
start_server "$(faux_script todo.json)"
loop_id="$(create_loop)"
[ -n "$loop_id" ] || { echo "FAIL  no loop was created"; cat "$tmp/server.log"; exit 1; }
printf '%s\n' \
  "{\"method\": \"subscribe\", \"params\": {\"loop\": \"$loop_id\", \"since\": 0}}" \
  "{\"method\": \"loop.attach\", \"params\": {\"loop\": \"$loop_id\"}}" \
  "{\"method\": \"loop.prompt\", \"params\": {\"loop\": \"$loop_id\", \"text\": \"plan something\"}}" |
  raw --timeout 15 --until loop.run_end >"$tmp/todo.out" 2>"$tmp/todo.err" || true

if jq -es 'any(.[]; .method == "ui.widget" and .params.key == "todo" and (.params.lines | tostring | contains("finish phase 4")))' \
   "$tmp/todo.out" >/dev/null 2>&1; then
  pass "a ui.widget event carries the todo file's lines"
else
  fail "no ui.widget for the todo key: $(cat "$tmp/todo.out" "$tmp/todo.err")"
fi
widgets="$(jq -s '[.[] | select(.method == "ui.widget" and .params.key == "todo")] | length' "$tmp/todo.out")"
if [ "$widgets" -ge 2 ] &&
   jq -es 'any(.[]; .method == "ui.widget" and .params.key == "todo" and (.params.lines | tostring | contains("ship it")))' \
   "$tmp/todo.out" >/dev/null 2>&1; then
  pass "the widget refreshed after the toolResult: $widgets ui.widget events, the later one has the new lines"
else
  fail "the todo widget did not refresh on tool_result ($widgets events): $(cat "$tmp/todo.out" "$tmp/todo.err")"
fi
if jq -es 'any(.[]; .result.manifest.widget_keys // [] | index("todo"))' "$tmp/todo.out" >/dev/null 2>&1; then
  pass "loop.attach lists todo in the manifest's widget_keys"
else
  fail "todo is not in the attach manifest: $(cat "$tmp/todo.out")"
fi

stop_scenario
export HOME="$real_home"

# ------------------------------------------------------- 8: pi-ext is gone

echo "-- check 8: pi-ext and the old extensions are gone"
if cargo metadata --no-deps --format-version 1 |
   jq -e '[.packages[].name] | index("pi-ext") == null' >/dev/null; then
  pass "pi-ext is not a package of the workspace"
else
  fail "pi-ext is still in cargo metadata"
fi
if [ ! -d "$root/crates/pi-ext" ]; then
  pass "crates/pi-ext is gone"
else
  fail "crates/pi-ext still exists"
fi
if [ ! -d "$root/examples/extensions" ]; then
  pass "examples/extensions is gone"
else
  fail "examples/extensions still exists"
fi

# --------------------------------------------------------------------------

if [ "$failures" = 0 ]; then
  echo "phase-4: OK"
else
  echo "phase-4: $failures check(s) failed"
  exit 1
fi
