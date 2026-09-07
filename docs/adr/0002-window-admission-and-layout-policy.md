# ADR 0002: Separate Admission, Capability, and Preferences

- Date: 2026-09-06
- Status: Accepted for implementation; desktop acceptance pending

## Context

Regular-only discovery excluded accessory applications with ordinary windows.
Construction mixed identity with subrole assumptions. Initial placement and
manual retile used separate capability checks. Named rules used HashMap order.
A movable, resizable settings window can still be an undesirable default tile.

## Decision

Keep the AX and WindowServer adapters. Use pure admission and layout decisions;
preserve unknown reads as retryable. Observe Regular and Accessory applications,
including those without windows. Tracking requires independent window identity,
not tiling capability. The current strip needs movement and two-axis resizing.

Application purpose belongs in editable Lua defaults, not core heuristics.
Support optional title, bundle, role, and subrole matchers; sort by descending
priority and ascending name, resolving each field from its first explicit value.
Do not rewrite or implicitly merge existing scripts. Use the same capability
decision for initial and manual placement and retain the frame readback pipeline.

## Consequences

No new injection, reduced SIP, screenshot permissions, or private backend is
required. This does not promise universal AX compatibility, fixed-size tiling,
or live revocation of existing identities on rule reload. Existing initial-rule
and restore semantics remain subject to physical capability limits.

See [the policy contract](../WINDOW_POLICY.md) for behavior and migration limits,
and [the research](../research/window-tiling-policy.md) for alternatives.
