# Overlay Rendering Probe

The probe invokes the production `DecorationView` painter on the macOS main
thread, drawing into memory bitmaps. It does not start a daemon, order windows,
move native Spaces, or change focus. The native Space-binding stub returns an
error if accidentally reached.

```sh
cargo build -p spool --example overlay_render_probe
target/debug/examples/overlay_render_probe --full-redraw
target/debug/examples/overlay_render_probe
cargo test -p spool --bin spool overlay::tests -- --test-threads=1
```

## What It Measures

Both modes use the same production painter and synthetic geometry sequence.
`--full-redraw` forces full-bitmap invalidation for all 300 updates, as the old
renderer did. The default uses the production damage calculation and skips
unchanged states. Each mode reports median, p95, and maximum update/draw time.

The timing bitmap is 2940 x 1912 pixels. Drawing uses a fixed-coordinate clip
region, matching a view's backing coordinates. An unattached view's
`displayRectIgnoringOpacity:inContext:` relocates partial rectangles, so it is
not used to simulate incremental drawing. A nonempty-pixel assertion prevents
mistaking a no-op drawing path for a fast renderer.

Pixel verification compares incremental and full redraws byte-for-byte at 1x
and 2x scale. Cases include fractional movement, resize, unchanged geometry,
clipping on both sides of the backing, tiny rounded windows, target removal,
and style changes, with and without dimming. A mismatch fails the probe and
writes diagnostic PNGs to `/tmp/spool-overlay-{actual,reference}.png`.

## Local Comparison, September 8, 2026

Representative debug-build results from the fixed-coordinate probe:

| Moving Surface | Full Median / p95 | Incremental Median / p95 |
| --- | --- | --- |
| Border only | 2.618 / 3.530 ms | 1.405 / 1.710 ms |
| Border and 20% dim | 22.401 / 26.507 ms | 5.304 / 5.965 ms |

For a stationary surface, full redraw performs 300 paints; incremental rendering
performs one paint. Initial appearance, large jumps, style changes, and backing
changes can still require large or full redraws. These are observations, not
timing thresholds enforced by tests.

A later run alongside the test suite measured a 7.637 ms median and 8.962 ms
p95 for incremental dim animation. The timings vary with system load; the
offscreen improvement must not be interpreted as a guaranteed refresh rate.

Validation on the final change:

- Full test suite: 874 passed, 2 ignored. Six IPC tests initially failed inside
  the sandbox; the full suite passed with permission to create isolated local
  test endpoints outside it.
- Pixel comparison: 22 frames at each of 1x and 2x, zero byte differences.
- `cargo check -p spool`, `cargo clippy -p spool --all-targets -- -D warnings`,
  `cargo fmt --all -- --check`, and `git diff --check` passed.

## Production Changes

- Preserve the common transparent interior of old/new dim cutouts; invalidate
  their difference and rounded boundaries instead of the entire display.
- Invalidate both old and new border outlines. Round invalidation outward to
  integral points to avoid fractional clipping leaving stale alpha pixels.
- Cache draw state and backing bounds. Native view invalidation still requests
  a redraw even when the declarative state is unchanged.
- Stage targets, then advance and render once in `PostUpdate`. Retain immediate
  backing display before ordering a newly shown or refocused overlay.
- Reorder on focus/display/show transitions, not every animation frame.
- Advance every Space presentation without the previous `Iterator::any`
  short-circuit, which stalled later animations while an earlier one advanced.

## Limits and Desktop Acceptance

This measures the CPU drawing path, not WindowServer composition or end-to-end
frame intervals. It excludes AX calls, application resize latency, real display
refresh scheduling, and native window ordering. No desktop smoothness claim
follows from these numbers alone.

After an authorized deployment, verify rapid cross-application focus changes,
focus-triggered resizing/scrolling, floating-window dragging, display changes,
and Mission Control/Space transitions. Confirm borders and dim holes remain
aligned and that hidden surfaces reappear correctly. If stutter remains, profile
main-thread/AX frame intervals before choosing display-link scheduling or a
Core Animation rewrite.
