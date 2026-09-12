#!/usr/bin/env bash
# PROTOTYPE launcher — throwaway. Opens the Bar Handle mock in the browser.
#
#   ./run.sh                       expanded
#   ./run.sh collapsed            collapsed
#   ./run.sh expanded p=0.5       extra args are appended as query params
#   ./run.sh expanded only=notched guides=1
#
# Params: state=expanded|collapsed · p=0..1 freeze the slide · w/h/r/ear sizes ·
#         black=all|handle · ride=ride|pinned · cv=ease|spring|smooth ·
#         period=ms · only=plain|notched · guides=1 · hover=1
# Keys:   space collapse/expand · h force hover · 1/2/3 freeze p=1/0.5/0 · p resume
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
state="${1:-expanded}"
shift 1 2>/dev/null || true

query="state=${state}"
for extra in "$@"; do query="${query}&${extra}"; done

url="file://${here}/index.html?${query}"
echo "PROTOTYPE → ${url}"
open "${url}"
