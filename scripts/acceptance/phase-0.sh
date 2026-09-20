#!/usr/bin/env bash
# Phase 0 acceptance: the protocol crate is the design freeze.
#
#   1. `cargo test -p pirs-protocol` is green.
#   2. Taking a field of a protocol type off the wire makes it red.
#   3. Renaming one on the wire makes the schema snapshot red, specifically.
#   4. The tree is left exactly as it was found.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

src="crates/pirs-protocol/src"
[ -d "$src" ] || { echo "FAIL  $src does not exist"; exit 1; }

victim=""
backup=""
logs="$(mktemp -d)"
restore() {
  if [ -n "$backup" ] && [ -f "$backup" ]; then
    cp "$backup" "$victim"
    rm -f "$backup"
    backup=""
  fi
}
cleanup() { restore; rm -rf "$logs"; }
trap cleanup EXIT INT TERM

mutate_start() { backup="$(mktemp)"; cp "$victim" "$backup"; }

echo "-- check 1: cargo test -p pirs-protocol"
if cargo test -p pirs-protocol >"$logs/baseline.log" 2>&1; then
  echo "PASS  protocol tests are green"
else
  echo "FAIL  protocol tests are not green"; tail -40 "$logs/baseline.log"; exit 1
fi

# The victim field: the first `pub <name>: ...` line in the first source file that has at
# least two of them (a struct's only field is a different experiment).
for f in $(find "$src" -name '*.rs' | sort); do
  n="$(grep -cE '^[[:space:]]+pub [a-z_]+: ' "$f" || true)"
  if [ "${n:-0}" -ge 2 ]; then
    victim="$f"
    lineno="$(grep -nE '^[[:space:]]+pub [a-z_]+: ' "$f" | head -1 | cut -d: -f1)"
    field="$(sed -n "${lineno}p" "$f" | sed -E 's/^[[:space:]]*pub ([a-z_]+):.*/\1/')"
    break
  fi
done
[ -n "$victim" ] || { echo "FAIL  no struct with two 'pub <field>:' lines under $src"; exit 1; }
echo "   victim field: $victim:$lineno ($field)"

echo "-- check 2: taking that field off the wire fails cargo test -p pirs-protocol"
mutate_start
indent="$(sed -n "${lineno}p" "$victim" | sed -E 's/^([[:space:]]*).*/\1/')"
sed -i "${lineno}i ${indent}#[serde(skip)]" "$victim"
if cargo test -p pirs-protocol --test snapshot >"$logs/skipped.log" 2>&1; then
  echo "FAIL  the field left the wire and the tests are still green"; restore; exit 1
fi
if grep -q 'could not compile' "$logs/skipped.log"; then
  echo "FAIL  the crate stopped compiling, so the tests never ran"; tail -20 "$logs/skipped.log"; restore; exit 1
fi
restore
echo "PASS  dropping $field from the wire fails the tests"

echo "-- check 3: renaming that field on the wire fails the schema snapshot"
mutate_start
indent="$(sed -n "${lineno}p" "$victim" | sed -E 's/^([[:space:]]*).*/\1/')"
sed -i "${lineno}i ${indent}#[serde(rename = \"pirs_acceptance_renamed\")]" "$victim"
if cargo test -p pirs-protocol --test snapshot schema_matches_snapshot >"$logs/renamed.log" 2>&1; then
  echo "FAIL  the schema changed and the snapshot test did not notice"; restore; exit 1
fi
if grep -q 'could not compile' "$logs/renamed.log"; then
  echo "FAIL  the crate stopped compiling, so the snapshot test never ran"; tail -20 "$logs/renamed.log"; restore; exit 1
fi
restore
echo "PASS  renaming $field fails docs/protocol.schema.json's snapshot test"

echo "-- check 4: the tree is restored"
if cargo test -p pirs-protocol >"$logs/restored.log" 2>&1; then
  echo "PASS  protocol tests are green again"
else
  echo "FAIL  protocol tests are red after restoring"; tail -40 "$logs/restored.log"; exit 1
fi

echo "phase-0: OK"
