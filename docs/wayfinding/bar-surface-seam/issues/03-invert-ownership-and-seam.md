# 03 — 反转所有权并引入 BarSurface 缝

Id: 03
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [ADR 0008 — Bar surface seam](../../../adr/0008-bar-surface-seam.md)
Blocked by: 02

## What to build

把 Bar 的决策从 AppKit 效果中分离出来，作为一条真正跨生产与测试的缝。

- `Bar` 持有每 display 的交互状态与逻辑面板登记；原生句柄（panel/view/layer）留在 `AppKitSurface`。
- 引入端口 `BarSurface`，生产侧 `AppKitSurface`、测试侧 `RecordingSurface` 两个 adapter 同时存在，使缝为真而非假想。
- `BarView` 只持有 `RenderFrame` 与带 display 归属的 `BarInput` 队列；选择子把 NSEvent 转成 `BarInput` 入队，不再改写状态。
- `BarManager` 包装 `Bar` + `Box<dyn BarSurface>`，把 `BarOutcome.actions` 经 `EventSender` 派发；`EventSender` 从视图上移到外层。
- 接线 `update`、`animate`（hover + 输入排空）、`toggle_collapse`，并更新调用方：`src/ecs/mouse.rs`（`pointer_is_on_chrome`）、`src/ecs/params.rs`（`is_animating`/`mid_frame`）、`src/ecs.rs`（`BarManager` 构造、`update_bar` 的 outcome 派发、`apply_bar_requests` 需要 `now`+surface、`animate_bar`）、`src/ecs/focus.rs` 的 ordering 边。
- 同一提交删除旧的 `ViewState` 所有权与直接改写路径；任意时刻只有一份 Bar 状态。

这是穿过每一层的 tracer。因所有权反转是原子的（状态一旦移走，选择子/hover/绘制必须同步迁移），本工单不拆分；专家已确认拆分会在中间提交产生双份状态或行为回归。

## Blocked by

02 —— 需要纯化的 `ViewState` 与 `RenderFrame` 已经就位。

## Acceptance criteria

- [ ] 任意时刻只有一份 Bar 状态来源；旧的直接改写路径已删除。
- [ ] `BarInput` 携带 display 归属；多显示器下点击/收起非活动 display 的 Bar 路由正确。
- [ ] 动作经 `BarOutcome` 到达 `EventSender`，与删除的 `BarView::dispatch` 行为一致，无静默丢弃。
- [ ] `mouse.rs` 的 `pointer_is_on_chrome` 与 `params.rs` 的 `is_animating`/`mid_frame` 行为不变，空闲 Bar 仍能及时排空输入。
- [ ] `RecordingSurface` 测试覆盖：每个保留 display 恰好一次 `ensure_panel`、消失 display 一次 `remove_panel`；`toggle_collapse` 只 present 活动 display；各输入产生预期动作；`set_interactive`/`sync_toolbar`/`set_button_highlight`/`show_drag_preview`/`hide_drag_preview` 的调用与参数正确。
- [ ] 桌面确认：handle 收起/展开、hover 生长、Mission Control/Show Desktop、Space 与窗口聚焦（含后台 Space）、列重排、跨 Space 拖拽、拖拽预览跟随与清除、notch lane 与 collar、收起时菜单栏点击穿透、Mission Control 下 `Stationary`、多显示器。
