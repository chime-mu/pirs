#!/usr/bin/env bash
# Phase 6 acceptance: remote and contained servers (S18, S19, S20, S21).
#
# Two real servers on two temporary sockets, `one` reached over its socket and
# `two` through the bridge `pirs proxy --socket <sock2>` -- exactly what a
# `servers.toml` entry spawns, only without the `ssh` or `docker exec` in
# front of it (D-05, D-36). Each server has its own PIRS_HOME, so where a
# conversation lands proves which server ran it. The clients have a third
# PIRS_HOME, the one holding `servers.toml`.
#
#   1. `pirs --server two "hi"` prints the scripted answer, and the
#      conversation is in server two's sessions and not in server one's.
#   2. The TUI harness (`pirs tui --headless`) lists loops from both servers,
#      each written `server:id` because two servers can hand out the same id;
#      `--server one` narrows it to one server, where a loop is its bare id
#      again and the phase-3 screens are unchanged.
#   3. A dropped bridge: server two's socket is moved aside and the
#      `pirs proxy` child killed while the TUI is attached to a loop there,
#      so every retry the UI makes fails for as long as the turn lasts. The
#      loop is prompted through raw.py on the socket under its hidden name,
#      the socket is moved back, and the UI -- which cannot have seen that
#      turn live -- reconnects on its own (which re-spawns the bridge --
#      "restarting the bridge" *is* the reconnect), re-subscribes from the
#      last `seq` it saw, and the missed turn appears on the page once and
#      only once (D-06).
#   4. Version refusal: a fake server from another release
#      (`fixtures/phase-6/fake-server.py`, stdlib only) answers `hello` with
#      `protocol_version = "1.0"` and `VERSION_REFUSED`; `pirs --server three`
#      exits non-zero with a message naming both versions. The plan suggested
#      a test feature flag in the real server for this; a fake server tests
#      the same client path without one, and without a flag the shipped
#      binary would carry for ever.
#   5. Paths stay opaque (D-31): clippy passes with the `disallowed-methods`
#      bans in place in both client crates, and the UI splits no server path.
#   6. The Docker path is documented, not run.
#
# Usage: scripts/acceptance/phase-6.sh [--build]
#
# It uses `target/release/pirs`, building it when it is missing or when
# `--build` is given. It needs no network and no credentials.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

bin="$root/target/release/pirs"
raw_py="$root/scripts/acceptance/lib/raw.py"
fixtures="$root/scripts/acceptance/fixtures/phase-6"

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
proj=""
sock1=""
sock2=""
sock3=""
home1=""
home2=""
pid1=""
pid2=""
pid3=""

stop_scenario() {
  for pid in "$pid1" "$pid2" "$pid3"; do
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
  pid1=""; pid2=""; pid3=""
  # Bridges the UI spawned and did not get to kill.
  if [ -n "$sock2" ]; then pkill -f "proxy --socket $sock2" 2>/dev/null || true; fi
  if [ -n "$tmp" ] && [ -d "$tmp" ]; then rm -rf "$tmp"; fi
  tmp=""
}
trap stop_scenario EXIT INT TERM

# A world with two servers: one home per server, one for the clients, and a
# `servers.toml` naming both. `PIRS_SOCKET` is deliberately unset -- which
# server a client talks to comes from `servers.toml` and `--server`, and a
# stray default socket would hide a mistake.
new_world() {
  stop_scenario
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/pirs-phase6.XXXXXX")"
  mkdir -p "$tmp/home/.pirs" "$tmp/one/.pirs" "$tmp/two/.pirs" "$tmp/run" "$tmp/proj"
  proj="$(cd "$tmp/proj" && pwd -P)"
  home1="$tmp/one/.pirs"
  home2="$tmp/two/.pirs"
  sock1="$tmp/one.sock"
  sock2="$tmp/two.sock"
  sock3="$tmp/three.sock"
  export HOME="$tmp/home"
  export PIRS_HOME="$tmp/home/.pirs"
  export XDG_RUNTIME_DIR="$tmp/run"
  unset PIRS_SOCKET
  unset PIRS_FAUX_SCRIPT
  cat >"$PIRS_HOME/servers.toml" <<EOF
[[server]]
name = "one"
socket = "$sock1"

[[server]]
name = "two"
# No auto-start: these servers are started by hand above, and a bridge that
# started one of its own would hide the socket moved aside in check 3 behind
# a second, empty server.
command = "$bin proxy --socket $sock2 --no-start"

[[server]]
name = "three"
command = "$bin proxy --no-start --socket $sock3"
EOF
}

# Start one server. $1 is its socket, $2 its PIRS_HOME, $3 a faux script.
start_server() {
  local socket="$1" home="$2" script="${3:-}" pid waited=0
  (
    export PIRS_HOME="$home"
    if [ -n "$script" ]; then export PIRS_FAUX_SCRIPT="$script"; fi
    exec "$bin" serve --idle 120 --socket "$socket" >"$tmp/$(basename "$socket").log" 2>&1
  ) &
  pid=$!
  while [ ! -S "$socket" ]; do
    sleep 0.05
    waited=$((waited + 1))
    if [ "$waited" -gt 200 ]; then
      echo "FAIL  no server on $socket"
      cat "$tmp/$(basename "$socket").log" || true
      exit 1
    fi
  done
  echo "$pid"
}

raw() { python3 "$raw_py" --socket "$1" "${@:2}"; }

# Create a loop on a socket and print its id.
create_loop() {
  printf '%s\n' "{\"method\": \"loop.create\", \"params\": {\"cwd\": \"$proj\", \"model\": {\"model\": \"faux/scripted\"}}}" |
    raw "$1" --timeout 10 |
    jq -rs '[.[] | select(.result.id != null) | .result.id] | first // empty'
}

# Run the headless UI with the script on stdin; stdout goes to $tmp/tui.out.
tui() { "$bin" tui --headless 100x30 --cwd "$proj" >"$tmp/tui.out" 2>"$tmp/tui.err"; }

tui_trace() { echo "--- stdout ---"; cat "$tmp/tui.out"; echo "--- stderr ---"; cat "$tmp/tui.err"; }

# How many session logs a server's home holds.
sessions_in() { find "$1/sessions" -name '*.jsonl' 2>/dev/null | wc -l | tr -d ' '; }

# ------------------------------------------- 1: a prompt runs on server two

echo "-- check 1: \`pirs --server two\` runs on the server the bridge reaches"
new_world
pid1="$(start_server "$sock1" "$home1" "$fixtures/hello-one.json")"
pid2="$(start_server "$sock2" "$home2" "$fixtures/hello-two.json")"

if out="$("$bin" --cwd "$proj" --server two -m faux/scripted "hi" 2>"$tmp/print.err")"; then
  if grep -q "ANSWER FROM TWO" <<<"$out"; then
    pass "the answer came from server two: $(head -1 <<<"$out")"
  else
    fail "unexpected answer from server two: $out"
  fi
else
  fail "pirs --server two exited non-zero"; cat "$tmp/print.err"
fi
if [ "$(sessions_in "$home2")" = "1" ] && [ "$(sessions_in "$home1")" = "0" ]; then
  pass "the conversation is in server two's home and not in server one's"
else
  fail "the conversation landed in the wrong home (one: $(sessions_in "$home1"), two: $(sessions_in "$home2"))"
fi

# ------------------------------- 2: the sidebar spans both servers

echo "-- check 2: the TUI lists loops from both servers, each named server:id"
new_world
pid1="$(start_server "$sock1" "$home1" "$fixtures/hello-one.json")"
pid2="$(start_server "$sock2" "$home2" "$fixtures/hello-two.json")"
a="$(create_loop "$sock1")"
b="$(create_loop "$sock2")"
[ -n "$a" ] && [ -n "$b" ] || fail "raw.py did not create a loop on each server ($a, $b)"

if tui <<EOF
{"wait":"one:$a","timeout":20000}
{"wait":"two:$b","timeout":20000}
{"dump":true}
EOF
then
  pass "the sidebar shows one:$a and two:$b"
else
  fail "the sidebar does not span both servers"; tui_trace
fi

# ... and `--server` narrows it to one, where a loop is its bare id again.
if "$bin" tui --headless 100x30 --cwd "$proj" --server one >"$tmp/tui-one.out" 2>&1 <<EOF
{"wait":"$a","timeout":20000}
{"dump":true}
EOF
then
  narrowed="$(awk '/^=== screen ===$/{n++;next} /^=== end ===$/{next} n==1' "$tmp/tui-one.out")"
  if grep -q "$b" <<<"$narrowed"; then
    fail "--server one still shows server two's loop:"$'\n'"$narrowed"
  elif grep -q "one:$a" <<<"$narrowed"; then
    fail "one server needs no prefix:"$'\n'"$narrowed"
  else
    pass "--server one narrows the sidebar to that server, the loop unprefixed"
  fi
else
  fail "the headless UI exited non-zero with --server one"; cat "$tmp/tui-one.out"
fi

# ----------------------------- 3: a dropped bridge, and the missed turn

echo "-- check 3: killing the bridge, then a turn the UI catches up on"
new_world
pid1="$(start_server "$sock1" "$home1" "$fixtures/hello-one.json")"
pid2="$(start_server "$sock2" "$home2" "$fixtures/missed.json")"
# Only one loop, on `two`, so the UI selects and attaches to it by itself.
b="$(create_loop "$sock2")"
[ -n "$b" ] || fail "raw.py did not create a loop on server two"

(
  # Long enough for the UI to have attached and subscribed.
  sleep 4
  # The socket goes first and the bridge after it, so there is no moment in
  # between where a retry could succeed. The server keeps listening on the
  # same socket under its hidden name -- a rename does not close it -- so
  # every attempt the UI makes fails for as long as the turn takes, and the
  # turn is missed whatever the retry timing happens to be.
  mv "$sock2" "$sock2.hidden"
  pkill -f "proxy --socket $sock2" || true
  # The turn the UI is away for. It is in server two's session log, so the
  # resumed subscription replays it.
  printf '%s\n' \
    "{\"method\": \"loop.prompt\", \"params\": {\"loop\": \"$b\", \"text\": \"while you were out\"}}" \
    "{\"method\": \"loop.wait\", \"params\": {\"loop\": \"$b\"}}" |
    raw "$sock2.hidden" --timeout 30 >"$tmp/missed.jsonl" 2>&1
  # Only now can the link come back.
  mv "$sock2.hidden" "$sock2"
) &
prompter=$!

if tui <<EOF
{"wait":"two:$b","timeout":20000}
{"wait":"reconnecting","timeout":30000}
{"dump":true}
{"wait":"MISSED THIS ONE","timeout":40000}
{"dump":true}
EOF
then
  gone="$(awk '/^=== screen ===$/{n++;next} /^=== end ===$/{next} n==1' "$tmp/tui.out")"
  back="$(awk '/^=== screen ===$/{n++;next} /^=== end ===$/{next} n==2' "$tmp/tui.out")"
  if grep -q "two offline" <<<"$gone"; then
    pass "the sidebar marked server two offline while the bridge was gone"
  else
    fail "no offline marker for the dropped bridge:"$'\n'"$gone"
  fi
  # Exactly once: the UI was away for the whole turn, so this is the replay,
  # and a replay that overlapped what it already had would show it twice.
  if [ "$(grep -c "MISSED THIS ONE" <<<"$back")" -eq 1 ] && ! grep -q "two offline" <<<"$back"; then
    pass "the UI reconnected and the missed turn is on the page exactly once"
  else
    fail "the missed turn did not arrive exactly once after the reconnect:"$'\n'"$back"
  fi
else
  fail "the headless UI exited non-zero"; tui_trace
fi
wait "$prompter" 2>/dev/null || true

# --------------------------------------- 4: a server of another release

echo "-- check 4: a server speaking another protocol major is refused clearly"
new_world
pid1="$(start_server "$sock1" "$home1" "$fixtures/hello-one.json")"
python3 "$fixtures/fake-server.py" "$sock3" >"$tmp/three.log" 2>&1 &
pid3=$!
waited=0
while [ ! -S "$sock3" ]; do
  sleep 0.05
  waited=$((waited + 1))
  [ "$waited" -gt 200 ] && { fail "the fake server did not create $sock3"; break; }
done

if out="$("$bin" --cwd "$proj" --server three -m faux/scripted "hi" 2>"$tmp/three.err")"; then
  fail "pirs --server three exited 0: $out"
else
  message="$(cat "$tmp/three.err")"
  if grep -q "1.0" <<<"$message" && grep -q "0.1" <<<"$message"; then
    pass "the refusal names both versions: $(head -1 <<<"$message")"
  else
    fail "the refusal does not name both versions: $message"
  fi
fi

# ------------------------------------------------- 5: paths stay opaque

echo "-- check 5: protocol paths are opaque (D-31)"
stop_scenario
for crate in pirs-client pirs-tui; do
  if grep -q 'std::path::Path::join' "$root/crates/$crate/clippy.toml"; then
    pass "$crate/clippy.toml bans Path::join"
  else
    fail "$crate/clippy.toml does not ban Path::join"
  fi
done
if HOME="$real_home" cargo clippy -p pirs-client -p pirs-tui -- -D warnings >"${TMPDIR:-/tmp}/pirs-phase6-clippy.log" 2>&1; then
  pass "cargo clippy -p pirs-client -p pirs-tui is clean with the bans in place"
else
  fail "clippy failed:"$'\n'"$(tail -30 "${TMPDIR:-/tmp}/pirs-phase6-clippy.log")"
fi
# A server path is a label: the UI never takes it apart.
if grep -rnE "(split|rsplit|splitn|rsplitn)\((')?/" "$root/crates/pirs-tui/src" >"${TMPDIR:-/tmp}/pirs-phase6-split.log" 2>&1; then
  fail "the UI splits something on '/':"$'\n'"$(cat "${TMPDIR:-/tmp}/pirs-phase6-split.log")"
else
  pass "no split on '/' anywhere in crates/pirs-tui/src"
fi

# ------------------------------------------- 6: the Docker path, documented

echo "-- check 6: the container path is documented"
# The entrypoint is written in exec form, `ENTRYPOINT ["pirs", "serve"]`, so
# the grep takes either spelling of the same thing.
if [ -f "$root/Dockerfile" ] && grep -qE 'pirs serve|"pirs", *"serve"' "$root/Dockerfile"; then
  pass "the Dockerfile runs \`pirs serve\`"
else
  fail "no Dockerfile with \`pirs serve\` as its entrypoint"
fi
if grep -q 'docker exec -i' "$root/docs/containment.md"; then
  pass "docs/containment.md documents the \`docker exec -i\` bridge command"
else
  fail "docs/containment.md does not document the docker bridge command"
fi

# --------------------------------------------------------------------------

stop_scenario
if [ "$failures" = 0 ]; then
  echo "phase-6: OK"
else
  echo "phase-6: $failures check(s) failed"
  exit 1
fi
