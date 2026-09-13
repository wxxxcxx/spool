# Spool Documentation

## Guides

- [Configuration](CONFIGURATION.md)
- [Window admission and layout policy](WINDOW_POLICY.md)
- [Lua scripting](SCRIPTING.md)
- [Query and subscribe format](QUERY_AND_SUBSCRIBE_FORMAT.md)
- [Logs and diagnostic information plan](LOGGING.md)
- [Built-in bar](BAR.md)
- [Nix installation](NIX.md)
- [Window lifecycle probe](examples/window_lifecycle_probe.md)
- [Overlay rendering probe](examples/overlay_render_probe.md)
- [Release verification](RELEASING.md)

## Architecture

- [Architecture guide](ARCHITECTURE.md)
- [Domain glossary](CONTEXT.md)
- [Architecture decisions](adr/)
- [Declarative, state-driven window management](adr/0006-declarative-state-driven-window-management.md)
- [Declarative state model and migration map](wayfinding/declarative-state/map.md)
- [Declarative state implementation specification index](wayfinding/declarative-state/spec.md)
- [Declarative column-width implementation and verification](wayfinding/declarative-state/implementation.md)
- [Explicit intent ownership and bounded realization](adr/0007-explicit-intent-ownership-and-realization.md)
- [Resource CLI and native inspection design (in progress)](NATIVE_INSPECTION.md)
- [Resource CLI implementation plan](CLI_IMPLEMENTATION_PLAN.md)

## Research

- [Window exclusion policy: DockDoor and AltTab](research/window-exclusion-policy.md)
- [Window tiling policy: yabai, AeroSpace and Rift](research/window-tiling-policy.md)
- [Bar click to window switch latency](research/bar-click-focus-latency-2026-09-12.md)
- [Native tabs as one layout window](research/native-tab-platform-observation-2026-09-11.md)
- [macOS window capability matrix](research/macos-window-capability-matrix.md)
- [Private `SLSOrderWindow`: signature, permission and local measurements](research/sls-order-window-research.md)
- [macOS window properties](research/macos-window-properties.md)
- [macOS window control backends](research/macos-window-control-backends.md)
- [Focus acquisition and control](research/FOCUS_REFERENCE_RESEARCH.md)
- [Floating window actions](research/FLOATING_WINDOW_ACTIONS_RESEARCH.md)
- [Native Spaces migration assessment](research/NATIVE_SPACES_MIGRATION_ASSESSMENT.md)
- [Private Spaces API projects](research/MACOS_PRIVATE_SPACES_API_PROJECTS.md)

## Reviews

- [Resource CLI and native inspection design review](reviews/resource-cli-design-review-2026-09-13.md)
- [Main program boundary review](reviews/main-program-boundaries-2026-09-07.md)
- [Release readiness](reviews/release-readiness.md)
- [Code reviews and refactor reports](reviews/)

See the [repository README](../README.md) for the project overview and
[agent instructions](../AGENTS.md) for contribution conventions.

- [Resource CLI implementation review](reviews/resource-cli-implementation-review-2026-09-13.md)
