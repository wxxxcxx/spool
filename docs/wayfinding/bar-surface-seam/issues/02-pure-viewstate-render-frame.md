# 02 — ViewState 纯化并让绘制消费 RenderFrame

Id: 02
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [ADR 0008 — Bar surface seam](../../../adr/0008-bar-surface-seam.md)
Blocked by: 01

## What to build

把交互状态变成与 AppKit 无关的纯状态，并让"画什么"成为一个可以先算好的值。

- `ViewState`、`EasedProgress` 与新的 `render_frame()` 移入 `src/bar/runtime.rs`（早先的设计草稿 `runtime-sketch.rs` 尚未接线、且与几何结构体同名冲突；落地时冲突已解决，草稿已删除）。
- 去掉状态的全部 FFI：`chrome_path` 的构造移到视图层；坐标用元组而非 `NSPoint`；屏幕几何与字体度量作为数据注入（见 01 的 `label_width`）。
- 所有绘制入口（Bar、strip、space、window、focus、placeholder）、面板 mask、工具栏布局、拖拽预览改为消费 `&RenderFrame`，不再借用 `RefCell<ViewState>`。
- `BarView` 仍持有 `RefCell<ViewState>`，选择子的语义不变；仅做坐标/方法名的机械调整。
- 交互测试迁入 `runtime.rs`；纯 AppKit 测试（`chrome_path` 几何、`Stationary`、文本居中）留在视图层。

同时把 `layout` 中的几何结构体 `BarSurface` 改名为 `BarSurfaceGeometry`（或等效别名），为端口名 `BarSurface` 让路。

行为零改变。

## Acceptance criteria

- [ ] `runtime.rs` 不导入 `objc2`/AppKit；状态不含 `NSPoint`、`NSImage`、`NSBezierPath`。
- [ ] 所有绘制入口只读 `&RenderFrame`；`RenderFrame` 承载 band、handle、chrome 进度、presented、drag 及面板 mask/工具栏/拖拽预览所需的一切。
- [ ] 端口名 `BarSurface` 不再与几何结构体冲突；`cargo fmt --check`、`cargo clippy -D warnings`、`cargo test` 全绿。
- [ ] 交互测试迁移后全绿，视图层测试相应减少。
- [ ] 桌面确认：展开/收起、hover、点击、拖拽视觉与现状一致。
