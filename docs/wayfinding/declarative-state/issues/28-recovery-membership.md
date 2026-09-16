# 恢复：Space 归属的候选与导入

Id: 28
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [跨重启恢复的入口票（恢复地图）](25-cross-restart-recovery.md)
Blocked by: none


> **2026-09-16 反转**：本票整体被 [ADR 0010](../../../adr/0010-runtime-state-rebuilt-from-rules.md) 取代——retained state 只在运行期存在，没有落盘、候选、导入缝或 `session restore`。跨会话偏好改由**规则**表达（字段按需增补），未表达者按"观测 → 默认"重建、接受丢失。反转原因与落地见 [运行期状态与实现对账](../../runtime-state/map.md)。

## Question

恢复地图（25 号票）裁决的域顺序是布局 → 归属 → 焦点，并规定在途尝试不恢复、先做显式确认路径。布局与浮动帧已由 [26 号票](26-recovery-first-slice.md) 落地；本票是第二个域：把**声明的 Space** 保存成候选，并接进同一个可信映射入口。

## 决策沿用（不再询问）

- 身份连续性用组合证据；窗口编号只在同一登录会话内、且必须被 pid/bundle 佐证（编号的作用域与复用行为待 [27 号票](27-window-identity-verification.md) 核实）。
- 候选优先、自动绑定另立研究票；导入必须由启动方担责。
- 导入只改状态、不写原生；一条坏绑定拒绝整次导入。

## 已实施（2026-09-15）

- **保存**：`SpoolState` 新增 `membership: Vec<SavedMembership>`（`window_id` / `pid` / `bundle_id` / `space_id`，`#[serde(default)]`，仍不升版本）；三条保存路径与 `window inspect` 的抓取都包含它。保存的 `space_id` 是**候选提示**：Space 编号同样是会话作用域的，导入时由调用者指认现场 Space。
- **导入**：`RestoreBindings` 新增 `membership: Vec<RestoreMembershipBinding>`（候选下标 + 现场窗口编号 + 现场 Space 编号）；`TrustedMembershipBinding::new` 要求候选缓存的 `window_id`/`pid`/`bundle_id` 与现场窗口**三者全等**，否则拒绝。
- **不变量在导入处检查**：目标 Space 必须存在（有条带）且不是原生全屏 Space，否则分别以 `import_target_space_not_found` / `import_target_space_not_user` 拒绝——**不是**先写进去再靠修复纠正。
- **应用**：`import_declared_spaces` 写 `DeclaredSpace::declare`（作者的转移，不记入 `repairs`）；缺失声明时补建。分组顺序与 25 号票一致：列 → 浮动帧 → 归属，全部先校验后应用。
- **CLI 不变**：`spool session restore --bindings <file|->` 的文档多一个 `membership` 组，无需新参数。

## 验收矩阵（已覆盖）

- 可信的归属绑定声明它命名的 Space，且不产生任何修复历史条目（导入是作者状态，不是修复）。
- 候选缓存身份与现场窗口不符 → `import_binding_rejected`，声明保持不变。
- 目标 Space 不存在 → `import_target_space_not_found`，声明保持不变。
- **全有或全无**：浮动组的坏绑定使归属组也不被应用（同一入口的一次导入）。

## 边界

- 不恢复在途尝试、不自动绑定、不做焦点域（下一张票）。
- 声明归属的恢复仍只在 **同一登录会话内**有意义：Space 编号与窗口编号都是会话作用域的，跨登出/重启的连续性需要 27 号票的结论与新的证据设计。
