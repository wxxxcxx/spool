#!/bin/sh
set -eu

probe_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$probe_dir/../.." && pwd)
output="$repo_dir/target/window-lifecycle-probe"

xcrun clang \
  -fobjc-arc \
  -fblocks \
  -Wall \
  -Wextra \
  -F/System/Library/PrivateFrameworks \
  -framework Cocoa \
  -framework ApplicationServices \
  -framework SkyLight \
  "$probe_dir/main.m" \
  -o "$output"

exec "$output" "$@"
