# 01 — 共享 Bar 的纯部件

Id: 01
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [ADR 0008 — Bar surface seam](../../../adr/0008-bar-surface-seam.md)
Blocked by: none — 可立即开始

## What to build

把 Bar 中已经纯、却被重复的部件收敛到共享的纯模块，使后续"状态纯化"只需搬迁而非重写。交付后行为与现状完全一致，只是每件事各有一处定义：

- `EasedProgress`（收起与 handle 生长的缓动）与拖拽释放宽限 `DRAG_RELEASE_GRACE` 各有唯一实现。
- 命中测试（窗口命中、Space 命中、slot 内约束）各有一处定义，被交互状态与拖拽共用。
- 图标缓存从交互状态中移出，交给 AppKit 视图层持有；交互状态不再包含任何 `Retained<NSImage>`。
- 拆分 `display_metrics`：AppKit 字体测量只留在 adapter，并只回答该 display 的 `label_width`（`show_workspace_labels` 关闭时为 0）；纯侧仍以 `workspace_label_width` 为上界取 max。

这是"让改动变简单，再做简单的改动"的第一步，直接缩小后续工单的改动面。

## Acceptance criteria

- [ ] `EasedProgress`、拖拽释放宽限、三项命中测试各只有一处定义，调用方共享同一份。
- [ ] 交互状态不再持有图标缓存或任何 AppKit 图像类型。
- [ ] 字体测量只在 adapter 执行；纯侧收到的 `label_width` 在标签关闭时为 `0`，且与 `workspace_label_width` 取 max 后，布局与现状逐 display 一致。
- [ ] `cargo fmt --check`、`cargo clippy -D warnings`、`cargo test` 全绿。
- [ ] 桌面确认：展开/收起、hover、点击、拖拽的视觉与交互无可见变化。
