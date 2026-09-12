# Agent Instructions: Spool macOS Window Manager (Bevy-based)

This document provides project-specific guidance for AI agents contributing to Spool. It builds upon the core philosophy and technical architecture of the codebase.

## Documentation Location

Keep project documentation in `docs/`; use [docs/README.md](docs/README.md) as the index.
The domain glossary is [docs/CONTEXT.md](docs/CONTEXT.md), and architecture guidance
is in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). When a skill refers to root-level
`CONTEXT.md`, read and update `docs/CONTEXT.md` instead. Keep research in
`docs/research/`, reviews in `docs/reviews/`, and decisions in `docs/adr/`.
Only the repository entry point (`README.md`), agent instructions (`AGENTS.md`),
license, and tool-owned skill files stay outside `docs/`.

## 1. Bevy ECS Architecture & Patterns

Spool is built on Bevy and follows a strict Data-Driven Design (ECS).

*   **Marker Components:** Use markers extensively for filtering and state tracking (e.g., `ActiveWorkspaceMarker`, `FocusedMarker`, `FreshMarker`, `Floating`, `WindowVisibility`). Most markers are found in `src/ecs.rs` or `src/ecs/mod.rs`.
*   **Triggers & Observers:** Prefer Bevy's observer pattern for reactive logic. See `src/ecs/triggers.rs` and `src/ecs/workspace.rs` for examples like `SpawnWindowTrigger` and `WMEventTrigger`.
*   **System Grouping:** Systems are registered in `src/ecs.rs` via `register_systems`. Follow the existing schedule-based organization (`PreUpdate`, `Update`, `PostUpdate`).
*   **System Params:** Use custom system parameters like `Windows` and `ActiveDisplay` (defined in `src/ecs/params.rs`) to simplify queries.

## 2. macOS & AppKit Integration (The Bridge)

*   **Main Thread Constraint:** All AppKit/CoreGraphics calls MUST happen on the main thread.
*   **NonSend Resources:** Use `NonSend` and `NonSendMut` for resources that are not thread-safe (e.g., `WindowManager`, `OverlayManager`).
*   **FFI Wrappers:** Interact with macOS via the abstractions in `src/manager/` and `src/platform/`. Avoid direct `objc2` or `icrate` calls in ECS systems; use the `WindowManager` API.
*   **Change Detection:** Use `Changed<T>` to trigger expensive macOS API updates (like window repositioning) only when the ECS state actually changes.
*   **The Lua Worker:** The scripting runtime (`src/lua/worker.rs`, `lua` feature) runs on its own thread — handlers are user code of unbounded duration and must never stall `pump_events`. Anything crossing that boundary must be plain `Send` data, never a Lua value or an ECS borrow; world access from a script goes through the `serve_lua_queries` round-trip. If you add a main-thread-only FFI call to a path a script can reach (`resolve_chord` is the existing example), compute it on the main thread and cache it — see `config::prime_virtual_keymap`.

## 3. Layout & Workspace Logic

*   **Declarative State-Driven Goal:** Spool's long-term architecture is fully state-driven declarative window management; follow [ADR 0006](docs/adr/0006-declarative-state-driven-window-management.md) in future designs, implementations, and reviews. Commands/configuration/scripts express state transitions, and platform effects reconcile explicit intent with native observations. Separate valid state edits from whether a macOS effect can execute now: invisible Spaces should ultimately accept desired-layout edits without implicit focus or Space switching. Current visible-only mutation restrictions are temporary implementation limitations, not permanent domain rules. Preserve existing native identity/transition/write protections until that separation is implemented, and distinguish accepted state from native completion.
*   **LayoutStrip:** The core layout data structure is `LayoutStrip` (in `src/ecs/layout.rs`). It manages columns, stacks, and tabs.
*   **Native Spaces:** Spool owns exactly one `LayoutStrip` per macOS Space. macOS owns topology and visibility; see `src/ecs/native_space.rs` for observation, optional private commands, and reconciliation.
*   **Coordinate Systems:** Be aware of the difference between Bevy's coordinate system (often Y-up) and macOS/AppKit (Y-down). Use the `Position` and `Size` abstractions to handle conversions.

## 4. Coding Standards & Idioms

*   **Clippy:** Spool enforces strict Clippy lints. Run `cargo clippy` before finalizing changes.
*   **Formatting:** All code must be formatted using `cargo fmt`.
*   **Tracing:** Use the `tracing` crate for logging. Use `#[instrument(level = Level::DEBUG, skip_all, fields(...))]` for complex systems.
*   **Error Handling:** Use the project's `Result` type and `Error` enum in `src/errors.rs`. Avoid `unwrap()` in systems; log errors or use `inspect_err`.

## 5. Testing Strategy

*   **Mocking:** When adding features that interact with macOS, ensure the logic is separable so it can be tested with a mock `WindowManager`.
*   **System Tests:** Add tests to `src/tests.rs` or new files in `src/tests/` that drive a mock Bevy `World`.
*   **Pure Functions:** Extract complex layout math into pure functions (e.g., in `src/ecs/layout.rs`) and add unit tests.

## 6. Contribution Workflow

*   **Research:** Before implementing, check `src/ecs/systems.rs` to see if a similar system already exists.
*   **Implementation:** Follow the **Plan -> Act -> Validate** cycle.
*   **Verification:** Run `cargo fmt`, `cargo check`, and relevant tests. If the change affects layout, verify it doesn't break existing tiling behavior.
