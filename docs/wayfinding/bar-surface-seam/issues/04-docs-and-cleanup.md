# 04 — 文档与清理

Id: 04
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [ADR 0008 — Bar surface seam](../../../adr/0008-bar-surface-seam.md)
Blocked by: 03

## What to build

把新的架构关系写进文档，并清理迁移留下的残留。

- 更新 `docs/ARCHITECTURE.md` 的 Bar 小节，描述 `Bar`/`BarSurface`/`AppKitSurface` 的缝、数据进效果出的方向，以及主线程专用的约束。
- 在 `docs/BAR.md` 的 Validation Boundary 说明哪些行为由 `RecordingSurface` 覆盖、哪些仍需桌面确认。
- 更新 ADR 0008 的状态（从设计接受改为已实现）。
- 清理因迁移不再使用的常量、测试与旧类型，确保无死代码。
- 运行 `cargo fmt` 与 `cargo clippy`。

## Acceptance criteria

- [ ] 文档描述了缝、两个 adapter 与主线程约束，并更新了验证边界。
- [ ] ADR 0008 状态已更新。
- [ ] 无遗留死代码或重复常量。
- [ ] `cargo fmt --check`、`cargo clippy -D warnings`、`cargo test` 全绿。
