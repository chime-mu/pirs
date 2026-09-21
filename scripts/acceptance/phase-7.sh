#!/usr/bin/env bash
# Phase 7 acceptance: the policy file written from a sentence (S10).
#
# The faux script under fixtures/phase-7/ is one reply whose single fenced
# toml block is a `[[status]]` file — with an `intent` line that deliberately
# paraphrases the sentence, so the normalisation has something to correct.
# Each scenario drives the real binary against a server of its own, exactly as
# phase-2.sh does. The model is never named on the command line: a global
# `[settings] model = "faux/scripted"` makes the faux provider the loop's
# default, which is what `pirs ext new` uses (PLAN.md, "Defaults").
#
#   1. `pirs ext new "show the git branch in the status line"` exits 0, writes
#      .pirs/ext/show-the-git-branch-in-the-status-line.pirs.toml, the file's
#      `intent` is the argument exactly (python3 tomllib, not a grep) although
#      the model paraphrased it, and `pirs check` on the directory is clean.
#   2. `pirs ext regen` on that file, against a fresh server with the same
#      script, reproduces it byte for byte and leaves <file>.bak behind.
#   3. A reply with no toml block in it exits 1, says so, and writes nothing.
#   4. `pirs ext new --name custom "..."` writes custom.pirs.toml, and a
#      --name that is not a file name is refused without asking a model.
#   5. `pirs --server <bridge> ext new` is refused before it connects: policy
#      lives with the server (D-26).
#   6. `pirs ext regen` on a file outside .pirs/ext/ is refused and writes
#      nothing.
#
# Usage: scripts/acceptance/phase-7.sh [--build]
#
# It uses `target/release/pirs`, building it when it is missing or when
# `--build` is given. It needs no network and no credentials.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

bin="$root/target/release/pirs"
fixtures="$root/scripts/acceptance/fixtures/phase-7"

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

stop_server() {
  if [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  server_pid=""
}

stop_scenario() {
  stop_server
  if [ -n "$tmp" ] && [ -d "$tmp" ]; then rm -rf "$tmp"; fi
  tmp=""
}
trap stop_scenario EXIT INT TERM

# A temporary world: its own home, runtime directory, socket and project. The
# project starts without a .pirs directory at all, so "wrote nothing" is a
# question the filesystem can answer.
new_world() {
  stop_scenario
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/pirs-phase7.XXXXXX")"
  mkdir -p "$tmp/home/.pirs/ext" "$tmp/run" "$tmp/proj"
  proj="$tmp/proj"
  export HOME="$tmp/home"
  export PIRS_HOME="$tmp/home/.pirs"
  export XDG_RUNTIME_DIR="$tmp/run"
  export PIRS_SOCKET="$tmp/pirs.sock"
  # The clients must never see the script: it belongs to the server.
  unset PIRS_FAUX_SCRIPT
  # The loop's default model, so no command line names one.
  cat >"$PIRS_HOME/ext/model.pirs.toml" <<'EOF'
intent = "Run every agent on the scripted provider, for the acceptance tests."

[settings]
model = "faux/scripted"
EOF
}

# Start this scenario's server on the world's socket, with a faux script.
start_server() {
  PIRS_FAUX_SCRIPT="$1" "$bin" serve --idle 120 >"$tmp/server.log" 2>&1 &
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

# The top-level `intent` of a policy file, as TOML says it is.
intent_of() {
  python3 -c '
import sys, tomllib
with open(sys.argv[1], "rb") as f:
    print(tomllib.load(f).get("intent", "<none>"), end="")
' "$1"
}

sentence="show the git branch in the status line"
slug="show-the-git-branch-in-the-status-line"

# ------------------------------------------- 1: the file the sentence makes

echo "-- check 1: pirs ext new writes a checked file whose intent is the sentence"
new_world
start_server "$fixtures/policy.json"
out="$(pirs ext new "$sentence" 2>"$tmp/err")" && code=0 || code=$?
file="$proj/.pirs/ext/$slug.pirs.toml"

if [ "$code" = 0 ]; then
  pass "pirs ext new exited 0"
else
  fail "pirs ext new exited $code ($(cat "$tmp/err"))"
fi
if [ -f "$file" ]; then
  pass "it wrote .pirs/ext/$slug.pirs.toml"
else
  fail "no file at $file; the directory holds: $(ls -A "$proj/.pirs/ext" 2>/dev/null || echo nothing)"
fi
if [ "$(tail -1 <<<"$out")" = "wrote $file" ]; then
  pass "the last line of stdout names the file it wrote"
else
  fail "the last line is $(printf '%q' "$(tail -1 <<<"$out")"), expected \"wrote $file\""
fi
if [ -f "$file" ] && [ "$(intent_of "$file")" = "$sentence" ]; then
  pass "the file's intent is the argument, verbatim, though the model paraphrased it"
else
  fail "the intent is $(printf '%q' "$(intent_of "$file" 2>&1)"), expected $(printf '%q' "$sentence")"
fi
if grep -q "Display the current Git branch" "$file" 2>/dev/null; then
  fail "the model's paraphrase is still in the file"
else
  pass "the model's paraphrase was replaced, not appended"
fi
# Everything else the model wrote is still there.
if grep -q 'run = "git branch --show-current"' "$file" 2>/dev/null; then
  pass "the rest of the model's file survived the normalisation"
else
  fail "the [[status]] entry is gone: $(cat "$file" 2>/dev/null)"
fi
# The check the command printed, and the same check run on its own.
if grep -q "status keys: branch" <<<"$out" && ! grep -q "^conflict:" <<<"$out"; then
  pass "the report pirs ext new printed is a clean check with the new status key"
else
  fail "the printed check is not clean: $out"
fi
report="$(pirs check)" && code=0 || code=$?
if [ "$code" = 0 ] && ! grep -q "^conflict:" <<<"$report"; then
  pass "pirs check on the directory exits 0 with no conflict"
else
  fail "pirs check exited $code: $report"
fi

# --------------------------------------------------- 2: regen reproduces it

echo "-- check 2: pirs ext regen rebuilds the same file from its intent"
cp "$file" "$tmp/before.pirs.toml"
stop_server
start_server "$fixtures/policy.json"
out="$(pirs ext regen "$file" 2>"$tmp/err")" && code=0 || code=$?

if [ "$code" = 0 ]; then
  pass "pirs ext regen exited 0"
else
  fail "pirs ext regen exited $code ($(cat "$tmp/err"))"
fi
# Byte for byte: the sentence goes back to the model unchanged, the script
# answers with the same body, and the normalisation is deterministic.
if cmp -s "$tmp/before.pirs.toml" "$file"; then
  pass "the rebuilt file is byte for byte the one that was there"
else
  fail "regen changed the file: $(diff "$tmp/before.pirs.toml" "$file" || true)"
fi
if [ -f "$file.bak" ] && cmp -s "$tmp/before.pirs.toml" "$file.bak"; then
  pass "the file that was there is at $slug.pirs.toml.bak"
else
  fail "no backup of the old file beside it"
fi
if [ "$(intent_of "$file")" = "$sentence" ]; then
  pass "the rebuilt file still carries the sentence as its intent"
else
  fail "regen lost the intent: $(intent_of "$file")"
fi

# ------------------------------------------------- 3: a reply with no file

echo "-- check 3: a reply with no policy file in it writes nothing"
new_world
start_server "$fixtures/prose.json"
out="$(pirs ext new "$sentence" 2>"$tmp/err")" && code=0 || code=$?

if [ "$code" = 1 ]; then
  pass "pirs ext new exited 1"
else
  fail "pirs ext new exited $code, expected 1"
fi
if grep -q "did not answer with a policy file" "$tmp/err"; then
  pass "it said why, on stderr: $(grep -m1 "did not answer with a policy file" "$tmp/err" | cut -c1-90)"
else
  fail "no readable message: $(cat "$tmp/err")"
fi
if [ -z "$(ls -A "$proj/.pirs/ext" 2>/dev/null || true)" ]; then
  pass "nothing was written"
else
  fail "it wrote $(ls -A "$proj/.pirs/ext")"
fi
if [ -z "$out" ]; then
  pass "stdout stayed empty: the model's prose is not an answer"
else
  fail "stdout carried $(printf '%q' "$out")"
fi

# ---------------------------------------------------------- 4: --name

echo "-- check 4: --name picks the file name"
new_world
start_server "$fixtures/policy.json"
out="$(pirs ext new --name custom "$sentence" 2>"$tmp/err")" && code=0 || code=$?

if [ "$code" = 0 ] && [ -f "$proj/.pirs/ext/custom.pirs.toml" ]; then
  pass "pirs ext new --name custom wrote custom.pirs.toml"
else
  fail "exited $code and the directory holds: $(ls -A "$proj/.pirs/ext" 2>/dev/null || echo nothing) ($(cat "$tmp/err"))"
fi
if [ ! -f "$proj/.pirs/ext/$slug.pirs.toml" ]; then
  pass "the slug of the sentence was not used as well"
else
  fail "it also wrote $slug.pirs.toml"
fi
if [ "$(intent_of "$proj/.pirs/ext/custom.pirs.toml" 2>/dev/null)" = "$sentence" ]; then
  pass "the named file carries the sentence too"
else
  fail "the named file's intent is not the sentence"
fi
# A name that is not a file name is refused, and nothing else is written.
before="$(ls -A "$proj/.pirs/ext")"
out="$(pirs ext new --name "not a name/../x" "$sentence" 2>"$tmp/err")" && code=0 || code=$?
if [ "$code" = 1 ] && grep -q "is not a file name" "$tmp/err"; then
  pass "--name 'not a name/../x' is refused: $(head -1 "$tmp/err" | cut -c1-90)"
else
  fail "exited $code: $(cat "$tmp/err")"
fi
if [ "$(ls -A "$proj/.pirs/ext")" = "$before" ]; then
  pass "the refused name wrote nothing"
else
  fail "it wrote $(ls -A "$proj/.pirs/ext")"
fi
# A name that carries the suffix is the same name.
out="$(pirs ext new --name suffixed.pirs.toml "$sentence" 2>"$tmp/err")" && code=0 || code=$?
if [ "$code" = 0 ] && [ -f "$proj/.pirs/ext/suffixed.pirs.toml" ]; then
  pass "--name suffixed.pirs.toml wrote suffixed.pirs.toml, not suffixed.pirs.toml.pirs.toml"
else
  fail "exited $code and the directory holds: $(ls -A "$proj/.pirs/ext") ($(cat "$tmp/err"))"
fi

# ------------------------------------------- 5: a server over a bridge

echo "-- check 5: pirs ext is refused for a server reached over a bridge"
new_world
cat >"$PIRS_HOME/servers.toml" <<'EOF'
[[server]]
name = "build"
command = "false"
EOF
out="$("$bin" --cwd "$proj" --server build ext new "$sentence" 2>"$tmp/err")" && code=0 || code=$?

if [ "$code" = 1 ] && grep -q "policy lives with the server (D-26)" "$tmp/err"; then
  pass "it exited 1 and said why: $(head -1 "$tmp/err" | cut -c1-100)"
else
  fail "exited $code: $(cat "$tmp/err")"
fi
if grep -q 'run `pirs ext` on `build` itself' "$tmp/err"; then
  pass "the message names the server to run it on"
else
  fail "the message does not name the server: $(cat "$tmp/err")"
fi
if [ -z "$(ls -A "$proj/.pirs/ext" 2>/dev/null || true)" ] && [ -z "$out" ]; then
  pass "nothing was written and stdout stayed empty"
else
  fail "it wrote $(ls -A "$proj/.pirs/ext" 2>/dev/null) and printed $(printf '%q' "$out")"
fi

# --------------------------------- 6: regen outside the policy locations

echo "-- check 6: pirs ext regen refuses a file that is not policy"
cat >"$proj/stray.pirs.toml" <<'EOF'
intent = "a file that is not in a policy directory"
EOF
out="$("$bin" --cwd "$proj" --no-start ext regen "$proj/stray.pirs.toml" 2>"$tmp/err")" && code=0 || code=$?

if [ "$code" = 1 ] && grep -q "not a policy location" "$tmp/err"; then
  pass "it exited 1 and said where policy lives: $(head -1 "$tmp/err" | cut -c1-100)"
else
  fail "exited $code: $(cat "$tmp/err")"
fi
if [ ! -f "$proj/stray.pirs.toml.bak" ] && [ -z "$(ls -A "$proj/.pirs/ext" 2>/dev/null || true)" ]; then
  pass "it wrote nothing, not even a backup"
else
  fail "it wrote a backup or a policy file"
fi

# --------------------------------------------------------------------------

stop_scenario
if [ "$failures" = 0 ]; then
  echo "phase-7: OK"
else
  echo "phase-7: $failures check(s) failed"
  exit 1
fi
