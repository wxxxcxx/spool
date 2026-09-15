# 02 — Route：所有状态动作经准入

Id: 02
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [ADR 0009 — Command Admission](../../adr/0009-command-admission.md)
Blocked by: 01

## What to build

让 `admission::execute` 成为所有 `Action` 的唯一读取者，删除 `dispatch_actions` 里绕开准入的分支。

- `Event::ActionRequested` 的每个动作都交给 `admission::execute`；效果型动作（`MissionControl`/`ShowDesktop`/`PrintState`/`ReconcileWindows`/`Quit`/`Restart`/`ToggleBarCollapse`）作为准入内的无校验分支，保证排序只有一处。
- `Event::LayoutSpaceRequested` 与 `CheckedActionRequested` 保持其既有的准入调用与回执语义。
- `Layout(plan)` 仍是独立入口（计划边界），但其内部操作继续重新进入准入；新增测试锁定这一边界。
- 删除现在绕开准入的直接系统调用分支（`Focus`/`FocusStep`、`ToNextDisplay`、`Center`/`ToggleFloating`/`Snap`/`FocusFloating`/`FocusTiled`/`FocusOtherLayer`、`ToggleTiledVisibility`、`Mouse::ToNextDisplay`、原生 Space 动作等），除非它是计划边界。
- 对每个被折叠的分支，用测试证明新路径与旧路径的最终效果一致（尤其是此前走不同系统的动作，如 `Center`）。

不迁移各领域模块内部的重复配方——那是 03/04。

## Acceptance criteria

- [ ] `dispatch_actions` 不再包含绕开准入的状态动作分支。
- [ ] 每个动作的可观察结果与迁移前一致（既有 `command_dispatch.rs` + 新增等价性测试）。
- [ ] `Layout(plan)` 的内部操作确实重新进入准入，且有测试锁定。
- [ ] `cargo fmt --check`、`cargo clippy -D warnings`、`cargo test -p spool` 全绿。
