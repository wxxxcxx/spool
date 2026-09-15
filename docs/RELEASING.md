# Release Verification

The release gate validates a candidate; it does not install Spool, start a
daemon, manipulate desktop windows, commit changes, create a tag, or publish.
The current evidence and unresolved findings are in
[Release readiness](reviews/release-readiness.md).

## Automated Gate

Run from a native macOS development environment with the Rust version declared
in `Cargo.toml`, Xcode Command Line Tools, and the rustfmt and Clippy components:

```sh
bash scripts/verify-release.sh
```

The script checks formatting, locked compilation, strict Clippy, and workspace
tests (in parallel; see [Development](DEVELOPMENT.md) for why the suite no longer
needs `--test-threads=1`). It separately checks and tests the Lua-free daemon,
then builds both release variants and the independently loadable LuaJIT module.
Do not use `--workspace --all-features` as a substitute: the daemon embeds
LuaJIT, while the module resolves its Lua symbols from its host.

The script propagates Rust's resolved `MACOSX_DEPLOYMENT_TARGET` to native C and
Objective-C dependencies so they do not silently inherit a newer SDK default.
Set that variable explicitly for the release's intended minimum OS version.
The compiler default is build metadata, not a claim of desktop compatibility;
the actual supported minimum must be confirmed by acceptance testing.

The IPC tests bind temporary Unix sockets. A sandbox that forbids local sockets
cannot run the complete gate. Use an explicitly approved execution environment
instead of interpreting permission failures as code regressions.

Artifacts are written under `target/release-check/<host>/` (or under the chosen
`CARGO_TARGET_DIR`):

- `default/spool`: the standard Lua-enabled daemon and CLI.
- `without-lua/spool`: the daemon and CLI using built-in defaults only.
- `lua/spool.so`: the loadable module for a compatible LuaJIT host.
- `SHA256SUMS`: checksums of all three artifacts.

Version/help invocations do not start a daemon. An isolated client script loads
the standalone module into the candidate's embedded LuaJIT and checks its API
without invoking any daemon operation. The CI workflow runs the gate separately
on Apple Silicon and Intel; local native validation is not evidence that the
other architecture passed.

## Nix Gate

Nix package metadata checks are separate from Cargo verification:

```sh
nix eval --no-write-lock-file --raw .#checks.aarch64-darwin.package-contract.drvPath
```

These are evaluation assertions, not a Nix package build or service installation
test. They check the executable path, feature variants, and overlay architecture.
The currently locked nixpkgs input rejects Intel macOS. Intel Nix support remains
an open release decision; the overlay must not silently substitute an ARM binary.
Do not update the lock or remove a promised platform just to make checks green.

## Desktop Acceptance

Only after explicit authorization, validate the exact candidate on the target
macOS versions and hardware. Record the OS version, architecture, display
arrangement, relevant configuration, candidate checksum, steps, and outcome.

- Single display, multiple displays, and negative or offset display origins.
- Unplug/replug, primary-display changes, sleep/wake, and temporary AX loss.
- Enter/exit fullscreen, Mission Control, native Space switching and moves.
- Application launch/exit, reused window IDs, delayed discovery, and restored
  sessions with missing or reopened windows.
- Tiling/floating capability changes, native tabs, drag/resize, and animation.
- Bar focus, grouping, floating areas, hit testing, and multi-display placement.
- Configuration reload success/failure and old callbacks completing after reload.

Mock ECS tests are necessary, but they do not establish WindowServer timing or
visual correctness. The two ignored private SkyLight runtime tests also require
their documented OS prerequisites and authorization.

## Release Decision

Review the final working-tree changes, rerun the gate after the last production
edit, and resolve all known blockers. Confirm the intended version, supported
platforms, upgrade notes, and any signing/notarization requirements before
tagging or publishing. No automatic check alone declares the release ready.
