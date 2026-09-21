#!/usr/bin/env bash
# Phase 2 acceptance: policy without code (S3, S4, S5).
#
# The fixtures under fixtures/phase-2/ are one policy file with every slot in
# it, a rules file for `[[prompt]] files`, two files that claim the same
# status key, and a file that is not TOML. Each scenario copies what it needs
# into a fresh temporary project and drives the real binary against a server
# of its own, exactly as phase-1.sh does.
#
#   1. `pirs check` prints the assembled system prompt with the prompt file's
#      text in it, prints the merged policy with each entry's origin (D-40),
#      reports the duplicate key naming both files, and exits 1.
#   2. A faux echo run of `?why` prints "(faux) Explain briefly: why": the
#      rewritten input, not what was typed, is what reached the model.
#   3. The `on turn_end` entry touched its file by the end of the turn, and
#      the status command's value is in the log.
#   4. A scripted `bash` call is rewritten before the model reads it, and the
#      session log holds the original next to it (D-21).
#   5. `/handoff` runs its script, its output is in the conversation as a
#      bashExecution, and no model call happened.
#   6. The model writes `.pirs/ext/new.pirs.toml`; the system message of its
#      next request already carries that file's `[[prompt]]` text (D-33).
#   7. A policy file that will not parse is a `[warning]` on stderr, and the
#      rest of the policy still works.
#
# Usage: scripts/acceptance/phase-2.sh [--build]
#
# It uses `target/release/pirs`, building it when it is missing or when
# `--build` is given. It needs no network and no credentials.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

bin="$root/target/release/pirs"
fixtures="$root/scripts/acceptance/fixtures/phase-2"

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
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/pirs-phase2.XXXXXX")"
  mkdir -p "$tmp/home" "$tmp/run" "$tmp/proj/.pirs/ext"
  proj="$tmp/proj"
  export HOME="$tmp/home"
  export PIRS_HOME="$tmp/home/.pirs"
  export XDG_RUNTIME_DIR="$tmp/run"
  export PIRS_SOCKET="$tmp/pirs.sock"
  # The clients must never see the script: it belongs to the server.
  unset PIRS_FAUX_SCRIPT
}

# Install a fixture directory of policy files into the project.
install_policy() { cp "$fixtures/$1"/*.pirs.toml "$proj/.pirs/ext/"; }

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

# The one session log of this world.
session_file() { find "$PIRS_HOME/sessions" -name '*.jsonl' | head -1; }

# Count the entries of a session log matching a jq filter.
entries_matching() {
  local file="$1" filter="$2"
  [ -n "$file" ] && [ -f "$file" ] || { echo 0; return; }
  jq -s --argjson zero 0 "[.[] | select($filter)] | length" "$file"
}

# ------------------------------------------- 1: pirs check shows everything

echo "-- check 1: pirs check prints the prompt and every conflict"
new_world
install_policy policy
install_policy conflict
cp -r "$fixtures/rules" "$proj/rules"
start_server ""
report="$("$bin" check --cwd "$proj")" && code=0 || code=$?

if [ "$code" = 1 ]; then
  pass "pirs check exited 1 because the policy conflicts"
else
  fail "pirs check exited $code, expected 1"
fi
if grep -q "Prefer small commits" <<<"$report" && grep -q "<policy>" <<<"$report"; then
  pass "the assembled system prompt carries the prompt file's text"
else
  fail "the assembled prompt does not show the prompt file: $report"
fi
if grep -q "duplicate status key" <<<"$report" &&
   grep -q "one.pirs.toml" <<<"$report" &&
   grep -q "two.pirs.toml" <<<"$report"; then
  pass "the duplicate status key is reported with both file names"
else
  fail "the conflict does not name both files: $report"
fi
if grep -q "/handoff" <<<"$report" && grep -q "status keys: branch" <<<"$report"; then
  pass "pirs check lists the commands and status keys of the manifest"
else
  fail "the manifest lines are missing: $report"
fi
# The merged policy itself, rendered by the server (D-40): every slot entry
# with the file and position it came from.
policy_section="$(sed -n '/^--- policy ---$/,/^--- system prompt ---$/p' <<<"$report")"
if grep -q "main.pirs.toml: \[\[input\]\] #" <<<"$policy_section"; then
  pass "the rendered policy names an [[input]] entry with its origin"
else
  fail "no [[input]] origin in the rendered policy: $policy_section"
fi

# ------------------------------------- 2, 3: the rewrite, the hook, the status

echo "-- check 2: the rewritten input is what reaches the model"
new_world
install_policy policy
cp -r "$fixtures/rules" "$proj/rules"
start_server ""
answer="$(pirs --model faux/scripted "?why" 2>"$tmp/err")" && code=0 || code=$?
if [ "$code" = 0 ] && [ "$answer" = "(faux) Explain briefly: why" ]; then
  pass "pirs \"?why\" printed $(printf '%q' "$answer")"
else
  fail "pirs \"?why\" printed $(printf '%q' "$answer") and exited $code ($(cat "$tmp/err"))"
fi

echo "-- check 3: the turn_end entry ran and the status command was emitted"
touched=0
for _ in $(seq 1 100); do
  [ -f "$proj/checkpoint" ] && { touched=1; break; }
  sleep 0.05
done
if [ "$touched" = 1 ]; then
  pass "the on turn_end entry touched its file in the project directory"
else
  fail "no checkpoint file after the turn"
fi

log="$(session_file)"
if [ "$(entries_matching "$log" '.customType == "pirs.ui.status" and .data.key == "branch" and .data.text == "main"')" -ge 1 ]; then
  pass "the status command's value was emitted as ui.status"
else
  fail "no ui.status entry for the branch key in $log"
fi

# --------------------------------------------- 4: the tool_result rewrite

echo "-- check 4: a bash result is rewritten and the original is kept"
new_world
install_policy policy
cp -r "$fixtures/rules" "$proj/rules"
start_server "$fixtures/bash.json"
pirs --model faux/scripted "run the tests" >"$tmp/out" 2>"$tmp/err" || true
log="$(session_file)"

if [ "$(entries_matching "$log" '.type == "message" and .message.role == "toolResult" and ((.message.content | tostring) | contains("rewritten: bash"))')" -ge 1 ]; then
  pass "the session log holds the rewritten bash result"
else
  fail "no rewritten tool result in $log"
fi
if [ "$(entries_matching "$log" '.customType == "pirs.tool_result_rewrite" and ((.data.original | tostring) | contains("raw-output")) and ((.data.by | tostring) | contains("tool_result"))')" -ge 1 ]; then
  pass "a pirs.tool_result_rewrite entry holds the original and names the entry"
else
  fail "no pirs.tool_result_rewrite entry with the original in $log"
fi

# ---------------------------------------------------- 5: a slash command

echo "-- check 5: /handoff runs its script and never reaches the model"
new_world
install_policy policy
cp -r "$fixtures/rules" "$proj/rules"
start_server ""
pirs --model faux/scripted "/handoff notes.md" >"$tmp/out" 2>"$tmp/err" || true
log="$(session_file)"

if [ "$(entries_matching "$log" '.type == "message" and .message.role == "bashExecution" and (.message.output | contains("handoff-ran notes.md"))')" -ge 1 ]; then
  pass "the command's output is in the conversation as a bashExecution"
else
  fail "no bashExecution with the command's output in $log"
fi
if [ "$(entries_matching "$log" '.type == "message" and .message.role == "assistant"')" = 0 ]; then
  pass "no model call happened for the consumed command"
else
  fail "the consumed command still reached the model"
fi

# ------------------------------------------- 6: ask, write, live (D-33)

echo "-- check 6: a policy the model writes shapes its very next request"
new_world
start_server "$fixtures/write-policy.json"
pirs --model faux/scripted "give yourself a marker" >"$tmp/out" 2>"$tmp/err" || true
log="$(session_file)"

if [ -f "$proj/.pirs/ext/new.pirs.toml" ]; then
  pass "the model wrote .pirs/ext/new.pirs.toml"
else
  fail "the scripted write did not land"
fi
if [ "$(entries_matching "$log" '.type == "message" and .message.role == "system" and ((.message | tostring) | contains("MARKER-42"))')" -ge 1 ]; then
  pass "a system message of the same run carries MARKER-42"
else
  fail "no system message with MARKER-42 in $log"
fi

# ------------------------------------------------- 7: a file that is broken

echo "-- check 7: a broken policy file is a warning, not a failure"
new_world
install_policy policy
install_policy broken
cp -r "$fixtures/rules" "$proj/rules"
start_server ""
answer="$(pirs --model faux/scripted "?why" 2>"$tmp/err")" && code=0 || code=$?
if grep -q '^\[warning\]' "$tmp/err" && grep -q "bad.pirs.toml" "$tmp/err"; then
  pass "print mode showed the parse error as a [warning] on stderr"
else
  fail "no [warning] naming the broken file: $(cat "$tmp/err")"
fi
if [ "$code" = 0 ] && [ "$answer" = "(faux) Explain briefly: why" ]; then
  pass "the entries that did load still work"
else
  fail "the run did not survive the broken file: $(printf '%q' "$answer") ($code)"
fi

# --------------------------------------------------------------------------

stop_scenario
if [ "$failures" = 0 ]; then
  echo "phase-2: OK"
else
  echo "phase-2: $failures check(s) failed"
  exit 1
fi
