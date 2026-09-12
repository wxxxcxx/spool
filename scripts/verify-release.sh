#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

host=$(rustc --version --verbose | awk '$1 == "host:" { print $2 }')
case "$host" in
  aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *) printf 'Release verification requires a native macOS Rust toolchain, got %s\n' "$host" >&2; exit 1 ;;
esac

deployment_target=$(rustc --target "$host" --print deployment-target)
case "$deployment_target" in
  MACOSX_DEPLOYMENT_TARGET=*) export MACOSX_DEPLOYMENT_TARGET="${deployment_target#MACOSX_DEPLOYMENT_TARGET=}" ;;
  *) printf 'Unexpected deployment target: %s\n' "$deployment_target" >&2; exit 1 ;;
esac
printf 'Native deployment target: %s\n' "$MACOSX_DEPLOYMENT_TARGET"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo_root/target}"
export RUST_LOG="${RUST_LOG:-off}"
artifacts="$CARGO_TARGET_DIR/release-check/$host"
mkdir -p "$artifacts/default" "$artifacts/without-lua" "$artifacts/lua"

cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked -- --test-threads=1
cargo check -p spool --no-default-features --locked
cargo clippy -p spool --all-targets --no-default-features --locked -- -D warnings
cargo test -p spool --no-default-features --locked -- --test-threads=1
cargo clippy -p spool-lua --lib --no-default-features --features module --locked -- -D warnings

build_candidate() {
  local variant=$1
  shift
  cargo build -p spool --target "$host" --release --locked "$@"
  install -m 755 "$CARGO_TARGET_DIR/$host/release/spool" "$artifacts/$variant/spool"
  "$artifacts/$variant/spool" --version
  "$artifacts/$variant/spool" --help >/dev/null
}

build_candidate without-lua --no-default-features
build_candidate default
"$artifacts/default/spool" script run --help >/dev/null

cargo build -p spool-lua --lib --target "$host" --release --no-default-features --features module --locked
install -m 755 "$CARGO_TARGET_DIR/$host/release/libspool_lua.dylib" "$artifacts/lua/spool.so"
"$artifacts/default/spool" script run "$repo_root/scripts/verify-lua-module.lua" -- "$artifacts/lua/spool.so"
(
  cd "$artifacts"
  shasum -a 256 default/spool without-lua/spool lua/spool.so >SHA256SUMS
  shasum -a 256 -c SHA256SUMS
)
printf 'Verified native release artifacts: %s\n' "$artifacts"
