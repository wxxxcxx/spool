# Logs and diagnostics

## Available now

```sh
spool log                  # All retained launchd output, then exit
spool log --tail 200        # Last 200 lines from each captured stream
spool log -f                # History, followed by new output
spool log -f --tail 0       # Only new output
spool log -f --tail 100     # Recent context, then follow
spool log > spool.log       # Export captured output
```

`spool logs` is an alias. The interface follows Docker's history-then-follow
model; `--tail` defaults to `all`. This is a local file reader, not a Docker
logging driver or a new daemon IPC subscription.

The reader uses `StandardOutPath` and `StandardErrorPath` from the installed
LaunchAgent. Without an installed agent it uses the service builder's default
paths, allowing retained logs to be read after uninstall. Identical paths are
read only once. Historical stdout precedes historical stderr; the two files
cannot provide a strict merged chronological order. Output is copied unchanged
to stdout, without file headers. Reader errors go to stderr.

The command uses macOS `/usr/bin/tail`: `-f` selects its `-F` name-following
behavior, including reopening replaced files. Ctrl-C terminates only the reader.
It does not start, stop, reconnect to, or modify the daemon. Retained output can
be read after a crash or stop, and can contain multiple daemon runs. Follow stays
open after daemon exit so a later restart can be observed. Missing files are
reported by tail in follow mode; snapshot mode skips absent streams and fails
if no captured file exists. Permission errors remain errors.

Limitations:

- This version reads **launchd-captured service output**, not arbitrary process
  stdout. Foreground `spool launch` output is only in that terminal unless the
  caller redirects it. It cannot retroactively recover uncaptured output.
- An edited but not reloaded plist may differ from the running agent's capture
  paths. Reinstall/reload is an operator action, never performed by `log`.
- Existing service files have no application-owned retention or rotation limit.
  `--tail N` limits reading, not disk use. External copy/truncate rotation can
  lose bytes between polling observations; name-following is not a durable cursor.
- The reader cannot recover DEBUG/TRACE records that the daemon did not emit.
  `RUST_LOG=debug spool log` changes the reader environment, not the running
  daemon's filter. The service installer captures `RUST_LOG` in its plist;
  changing service verbosity currently requires an explicitly authorized reload.

## Diagnostic workflow

### Focus border tracing

For a targeted capture, start the daemon with
`RUST_LOG=info,spool::focus_diagnostics=debug`. This must be applied to the
daemon process, not to `spool log`. The category records focus generations,
window/entity IDs, changed frame readbacks, overlay targets and rendered
presentations. It does not include window titles or document contents.
Per-frame records can be verbose; enable this category only during diagnosis.

For focus-animation stalls, compare consecutive `overlay_draw` timestamps while
`animating: true`, not idle redraw intervals. `focused_window_query` reports
`query_us`; `tiled_stacking_completed` reports the entire raise batch's `raise_us`.
Both `frame_commit` and `frame_commit_failed` report `write_us`, so a failed AX
request is not omitted from timing analysis. These stages do not cover all main
thread work or WindowServer composition, and verbose terminal output may affect
timing. Decorations use a per-transition monotonic clock, independent of the ECS
frame delta; a newly selected target does not inherit time before its creation.

```sh
spool log --tail 2000 | rg 'spool::focus_diagnostics'
```

Compare `frame_commit` readback with the subsequent `overlay_draw` target,
allowing for configured window padding and CG/AppKit coordinate conversion.
The animated `presented` rectangle may lag intentionally; the target must not
remain on an older readback. Include periods with no `FocusedMarker`: overlays
can still use the requested or last navigation window while AX focus resolves.

### Capture steps

For overlapping edge windows, the focus diagnostic category emits
`tiled_stacking_requested_bottom_to_top` with a Space ID and window IDs in
request order, only when a new order or explicit tiled-layer raise is requested.
`unable to raise window without focus` reports an AX failure and window ID.
Neither a successful AX return nor the request-order log proves WindowServer
z-order; verify left/right edge hits across applications on the desktop.

For stale Bar icons, enable `spool::bar=debug` on the daemon. The
`bar_window_identities` record lists `(display_id, space_id, window_id)` only
when that list changes. It includes read-only fallback icons but no titles.
Compare this presentation list with `query state`: unavailable tracked windows
can retain layout identity while no longer being actionable or shown in the Bar.

1. Save `spool log --tail 300` before restarting anything.
2. Run `spool log -f --tail 100`, reproduce once, and record the time and symptom.
3. Capture `spool query state --json` and, when relevant,
   `spool subscribe --json --raw` in another terminal. These are state evidence,
   not a replacement for error logs.
4. Correlate module target, window ID, Space ID, display ID, and macOS error code
   where present. Redact application titles, paths and script output before sharing.

## Log information plan (not yet implemented)

### Event contract

Keep `tracing` as the producer API. Introduce stable `event` names and structured
fields instead of requiring tools to parse prose. Each record should carry UTC
timestamp, level, target, `run_id`, PID, and a per-run sequence number. Use
`operation_id` for multi-step commands and moves. Include window ID **and
incarnation** when available; window IDs alone can be reused. Add display and
Space IDs only when known, never substitute the active display for an unknown one.

Failures should carry `operation`, `reason`, native `error_code`, `attempt`,
`retry_after_ms`, and `outcome` (rejected, deferred, failed, recovered). Distinguish
an accepted IPC request from a successfully observed macOS effect. Do not report
a desired frame as the actual window frame.

### Levels and priority instrumentation

| Area / implementation location | INFO | WARN / ERROR | DEBUG / TRACE |
| --- | --- | --- | --- |
| Startup / `main.rs` | run/version, ready, stop reason | permission unavailable, startup failure | enabled capability summary |
| Config / `config`, `lua/worker.rs` | config revision loaded/reloaded | validation failure with previous config retained; worker failure | handler duration, no script body |
| IPC / `reader.rs`, `commands.rs` | meaningful completed user operation | malformed request, rejection, response failure | operation ID and dispatch timing |
| Topology / `ecs/topology.rs`, `native_space.rs` | confirmed display/Space changes | incomplete observation transition, timeout | observation generation and deferred reason |
| Window lifecycle / `ecs/systems.rs`, `triggers.rs` | admission/removal with identity | observer registration or discovery failure | admission/exclusion reason |
| Frame / `ecs/window_frame.rs`, `reconcile.rs` | recovery summary | commit failure, retry budget exhausted | desired/presented/observed frames and error code |
| Native move / `ecs/native_space.rs` | transaction started/completed | partial membership, timeout, freeze retained | source/target and observed membership |
| Focus / `ecs/focus.rs` | significant recovery | denied focus, convergence timeout | requested/observed focus and owner |
| Bar / `bar` | user action outcome | canceled or failed drag/drop | input-to-command correlation, not every pointer move |

INFO must remain usable during ordinary operation. Log repeated transient failures
once on entry and once on recovery, with a suppression count. WARN means degraded
but recoverable; ERROR means a failed operation or unavailable subsystem. Expected
no-ops belong at DEBUG. Do not log per-frame animation, polling or mouse events at
INFO; reserve sampled detail for TRACE. Logging must not change retry budgets.

### Delivery stages and acceptance

1. **Lifecycle and identity:** add run IDs, stable event names, startup/ready/exit
   summaries, config revisions and failure context. Test field presence and that
   an IPC acknowledgment is not logged as completion.
2. **Failure chains:** instrument topology, native moves, frame retries and focus
   using operation/window identities. Test incomplete topology, ID reuse, partial
   moves and cooldown/recovery logs with the existing mock ECS harness. Verify
   bounded log volume during prolonged failure.
3. **Unified bounded storage:** use one private per-user sink for both launchd and
   foreground daemon launches, retaining console output without duplicate capture.
   Proposed budget: 10 MiB per file, five files total, directory mode 0700 and files
   0600. Use a bounded off-main-thread writer; surface dropped-record counts and
   disk errors without blocking the AppKit event loop. Preserve panic/crash stderr
   as a separate fallback; integrate reader discovery without masking old logs.
   Test rotation, disk-full, slow storage, shutdown flush, and reader backpressure.
4. **Structured retrieval:** after storage has a versioned record format, add
   `--since`, `--until`, `--level`, `--json`, and run selection. Define merged order
   and `--tail` across retained segments. Test restart boundaries and UTC parsing.

Never log access tokens, complete IPC payloads, Lua source, window titles, or
document URLs by default. DEBUG is not permission to expose credentials. Audit
existing `%error` and `?event` sites before claiming the logs are share-safe.
