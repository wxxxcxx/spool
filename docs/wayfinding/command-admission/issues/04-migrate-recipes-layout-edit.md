# 04 — Migrate 配方（二）：layout_edit、column_width 与 commands 辅助

Id: 04
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [ADR 0009 — Command Admission](../../../adr/0009-command-admission.md)
Blocked by: 03

## What to build

把其余重复的准入配方改为调用 `Admission`。

- `layout_edit` 的 strip 解析、列范围、列可用性、`Column::Fullscreen` 排除、`layout_transition_pending` 检查。
- `column_width` 的同类检查。
- `commands.rs` 中 `command_entity`/`active_command_entity`/`checked_focus_other_display` 等重新推导的"待定焦点或已确认→可见→可写→归属"链。
- 保持各模块重叠但不同的失败策略与原因码；只消除重复的结构，不统一它们的策略。

## Acceptance criteria

- [x] 上述模块改为调用共享配方，不再各自重写检查顺序。
- [x] 差异化的失败策略被显式保留（不是被"统一"掉）。
- [x] `cargo fmt --check`、`cargo clippy -D warnings`、`cargo test -p spool` 全绿。
