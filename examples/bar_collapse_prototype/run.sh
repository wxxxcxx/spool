#!/usr/bin/env bash
# PROTOTYPE launcher — throwaway. Opens the bar-collapse mock in the browser.
#
#   ./run.sh                 variant A, expanded, notched screen
#   ./run.sh B collapsed     2nd arg: A|B|C
#   ./run.sh A collapsed plain
#
# Keys: ←/→ variant · space collapse/expand · n notched/plain
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
variant="${1:-A}"
state="${2:-expanded}"
screen="${3:-notched}"

url="file://${here}/index.html?variant=${variant}&state=${state}&screen=${screen}"
echo "PROTOTYPE → ${url}"
open "${url}"
