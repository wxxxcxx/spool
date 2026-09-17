#!/usr/bin/env bash
# Summarise the accessibility census a Spool run wrote into its log.
#
# The daemon writes one cumulative `ax_census` block every five minutes (see
# src/manager/ax_census.rs). This prints the last block, which is the whole run,
# sorted worst-first by the daemon itself.
#
# Capture a run first:
#   RUST_LOG=spool=info ./target/debug/spool service run > /tmp/spool-ax.log 2>&1
# or read an installed service's capture:
#   spool logs -n all > /tmp/spool-ax.log
#
# Usage: scripts/aggregate-ax-census.sh [log-file ...]
set -euo pipefail

files=("$@")
if [ ${#files[@]} -eq 0 ]; then
  files=(/tmp/spool-ax.log)
fi

for file in "${files[@]}"; do
  if [ ! -r "$file" ]; then
    echo "cannot read $file" >&2
    continue
  fi
  echo "=== $file"
  awk '
    /ax_census summary/ { block = $0 "\n"; next }
    /ax_census /        { block = block $0 "\n"; next }
    END                 { if (block != "") printf "%s", block; else print "(no census block found: no AX or CG call failed, or the run was shorter than five minutes)" }
  ' "$file"
done
