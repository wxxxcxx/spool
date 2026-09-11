#!/usr/bin/env bash
# PROTOTYPE launcher — throwaway. Opens the bar-polish mock in the browser.
#
#   ./run.sh                          preset A, expanded
#   ./run.sh B collapsed              preset A|B|C, then collapsed|expanded
#   ./run.sh A collapsed sn=sfillet   extra args are appended as query params
#
# Keys: ←/→ preset · space collapse/expand · h force hover
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
preset="${1:-A}"
state="${2:-expanded}"
shift 2 2>/dev/null || true

query="variant=${preset}&state=${state}"
for extra in "$@"; do query="${query}&${extra}"; done

url="file://${here}/index.html?${query}"
echo "PROTOTYPE → ${url}"
open "${url}"
