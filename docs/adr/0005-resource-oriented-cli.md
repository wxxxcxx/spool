# Resource-oriented CLI with explicit read sources

Group control and inspection under singular resources using `spool <resource> <operation>`, so users can discover operations for windows, Spaces, and displays together. `list` returns filtered summaries and `inspect <id>` returns object detail; both accept `--source spool|native` and default to `spool`, keeping output scope independent from evidence provenance. Native collection remains daemon-independent, while control operations use the daemon.

Migrate the CLI in one breaking change: remove the legacy `action` and `query` entry points, including aliases that preserve those entry points, without a compatibility period. Update repository-owned CLI callers, examples, help, and tests together; external callers must migrate. Migrate Lua's `spool.run` action grammar in the same change so it shares the CLI's control vocabulary; this does not automatically rename typed Lua action methods or query APIs.

Numeric positional arguments to `window focus` identify native windows. The previous numeric layout-position meaning moves to explicit one-based `--nth`, preventing a number's interpretation from changing with the live window inventory; unresolved IDs fail rather than falling back to a layout ordinal.

Individual-window mutations retain focused-window defaults and accept explicit `--window <id>` targets. The daemon resolves and operates on explicit targets directly; selecting a target must not be implemented as an implicit focus operation followed by a focused-window command.

Expose Spool-owned layout inspection and arrangement operations under `space layout`, including `balance`, `equalize`, and tiled-visibility operations. Each layout belongs to one Native Space and column ordinals are meaningful only within that layout, so there is no top-level `layout` resource. The subgroup distinguishes arrangement from native Space lifecycle operations and does not support a native read source.

Use the singleton `session` resource for the current Desktop Session's aggregate reads and notifications. `session inspect` replaces `query state`, `session inspect --show active` replaces `query active`, and `session watch` replaces `subscribe`; inspect selects one read source, while first-version watch retains daemon-backed subscriptions, including JSON and raw-event output.

Group daemon execution and service management under `service`, including foreground `run`, service `start`/`stop`/`restart`, registration, logs, and process `quit`. Keep graphical launcher installation under a separate `launcher install|uninstall` resource; the launcher remains a clickable entry point that starts the service, while `app` identifies desktop applications.

Bare `spool` and bare resource groups display help. Foreground execution requires explicit `service run`; generated service launch arguments and repository-owned launch callers must migrate so startup does not depend on the former no-argument behavior.
