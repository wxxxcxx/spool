# 07 — Fold：并入并行分支的 session-reach 表与单一实现

Id: 07
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [ADR 0009 — Command Admission](../../adr/0009-command-admission.md)
Blocked by: 06

## Why

另一条并行工作线（`wxxxcxx/main`，ADR 草稿 `0009-uniform-action-admission.md`）
独立得出了同一结论：admission 必须与 intake 无关。但它的实现停在 23 条路由分支
（`dispatch_actions` 仍把 8 类 action 直接送进领域系统），也没有类型化拒绝。
它的两块增量与本线正交，已并入本线：

1. **`SessionReach` 穷尽分类**：把 `execute_action` 里手写的
   `matches!(TargetedWindow | Window | SpaceLayout)` 换成覆盖每个 `Action`
   变体的 match，新增变体若忘记声明可达性就无法编译。
2. **`invoked()` 错误翻译**：收敛 29 处重复的
   `.map_err(Error::rejection_with_cause("execution_unavailable", _))`。
3. **`window center` 的指针修复**（其 `c56270a`）：warp 曾无视
   `mouse_follows_focus`，且只在 keybinding 路径上生效。

## What to build

- 把 `session_reach` / `session_is_writable` / `invoked` 落在
  `src/commands/admission.rs`；`MissionControl`/`ShowDesktop`/`PrintState`/
  `ReconcileWindows`/`ToggleBarCollapse` 仍排在编排配方之前，其可达性由表中
  的 `Running` 声明（语义与改动前一致）。
- `Center`/`Snap`/`ToggleFloating`/`ToNextDisplay` 的第二个实现（keybinding
  路径上的 `command_center_window`、`snap_window`、`toggle_floating_window`、
  `move_to_display`）删除，统一走显式目标命令；`window center` 的指针 warp
  移入唯一实现并受 `mouse_follows_focus` 约束。
- **floating 分类保留更轻的准入**：它改 Layout State 但不改几何，因此 native
  move 的所有权屏障不拒绝它（`native_move_layout_admission_preserves_focus_and_floating_classification`
  已锁定该行为）。该例外改为写在唯一实现内部，而不是靠第二个实现。
- `Layout(plan)` 批次像其他 Layout State 编辑一样被准入：计划不能再编辑不可写
  的会话；replay 仍按 `LayoutSnapshot` 逐操作进行。
- `remove` 掉只服务于已删除实现的 `Admission::active_space_target(_in)`；
  `layout_edit` 中对应的两条裸字符串改用类型化 `Rejection`。
- 文档：ADR 0009 合并两条线的决定与三条刻意例外（Activation Intent 延迟、
  floating 轻准入、终止在回执之后）；`ARCHITECTURE.md` 的 Layout Mutation
  Admission 段落、`CONTEXT.md` 的 Action Intake / Action Admission 词条、
  `CONFIGURATION.md` 的 `mouse_follows_focus` 说明。

## Acceptance criteria

- [x] `session_reach` 覆盖每个 `Action` 变体（编译器穷尽检查）。
- [x] `Center`/`Snap`/`ToNextDisplay` 只有一个实现；`ToggleFloating` 的轻准入
      写在唯一实现内部。
- [x] `center_moves_the_pointer_only_when_configured_to` 覆盖默认目标与显式目标
      两种形式，并断言 `mouse_follows_focus` 的两态。
- [x] `one_action_is_admitted_the_same_way_from_either_intake` 锁定 intake 等价。
- [x] `a_layout_plan_cannot_edit_an_unwritable_session` 锁定计划的会话门槛。
- [x] `cargo fmt --check`、`cargo clippy --all-targets`、`cargo test -p spool` 全绿。

## Left open

- `SetSpaceFocusPreference` 与 native Space 命令仍分类为 `Running`：它们确实
  改状态，但 native 命令系统自行复核会话。提升为 `Writable` 是独立改动，需要
  自己的测试。
- `Action::Lua` 由 reader 异步处理（脚本返回的计划稍后入队），因此分类为
  `Running` 且当前没有执行分支。
- `Admission` 仍有 5 处 `#[cfg(test)]` 适配器；`unique_strip` 已改为复用
  `space_scope_strip_in`，其余四处只是转调 `_in` 形式。
- 并行分支的 `0009-uniform-action-admission.md` 必须**不要**再落到 main：
  同一编号只能有一份 ADR，其内容已并入本文件所记录的 ADR。
