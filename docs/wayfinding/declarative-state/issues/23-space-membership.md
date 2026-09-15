# 目标 Space 归属的完整迁移切片

Id: 23
Type: task
Label: wayfinder:task
Status: open
Assignee: none
Parent: [Spool 声明式状态模型与迁移边界](../map.md)
Blocked by: none

## Question

对照已裁决的归属归属权、协调资格、生命周期与让权政策，确定**目标 Space 归属**迁移的代码替换边界、状态协议与验收场景，不重新表决已有政策。

已定政策（本票只做推论，不再询问）：[状态归属与迁移规格](../spec.md)、[意图、焦点记忆与原生事实分别由谁写入](05-state-ownership.md)、[最新意图的执行资格、重试和回执如何协调](07-reconciliation.md)、[意图寿命](04-lifecycle.md)、[布局生命周期](08-topology-lifecycle.md)、[外部操作如何取代旧意图](02-external-policy.md)、[接纳证据](06-evidence.md)、[迁移次序](10-migration.md)、[焦点让权](13-focus-yield.md)、[聚焦不自动重试](15-focus-retry.md)。

## Answer

### 当前证据与必须替换的行为

只读核对，基线 `5b9b2f8`。列宽与排列/高度、焦点三片已迁移；归属仍是命令形状。

1. **两条入口，两种"当场可写"前提，都没有留存的目标。**
   - `Action::MoveWindowToSpace` / `MoveColumnToSpace`（[native_space.rs](../../../../src/ecs/native_space.rs) 的 `execute_native_space_command`，约 629–780）：要求 `SpaceControl::effective(...).allows(SpaceOperation::MoveWindows)`（即 `experimental_space_control`），否则 `capability_unavailable`；要求目标是**已知**用户 Space（`observe_displays()` 的 `spaces.contains(&space_id)`）且非原生全屏，否则 `native_precondition_failed`；成员临时不可用、快照身份不匹配、或该窗口已有在途移动（`native_move_pending`）同样直接拒绝。目标**不必可见**，但必须"存在且现在能写"。
   - 跨显示器转移（[transfer.rs](../../../../src/commands/transfer.rs) 的 `execute`）：经 `admission::target_space` → `visible_space_on_display`，即**要求目标显示器当前正显示该 Space**，否则 `target_space_unavailable`。这就是 ADR 0006 称为临时实现限制的那道 visible-only 门（唯一调用点 `transfer.rs:49`）。
2. **编辑与原生提交同体。** `transfer::execute` 在同一事务里构造源/目的 strip（`take_windows_preserving_layout` / `append_strip`，并校验 `width_budget_is_valid`），随后 `NativeSpaceTransactions::submit_display_move` 冻结源列（`freeze_window_for_space_reassignment`）、插入 `NativeMoveOwner` + `DisplayTransferFrame`、push `PendingMove`（native_space.rs:344 起）。**接纳即提交**。
3. **意图只活在在途事务里。** `NativeSpaceTransactions { moves, follows }`；意图没有独立表示，撤下事务即无意图。
4. **协调是"一次提交 + 2s 窗口 + 观察裁定"。** `reconcile_native_space_transactions`（native_space.rs:910 起）用 `MOVE_TIMEOUT = 2s` / `FOLLOW_TIMEOUT = 5s`；确认条件为目标非全屏、映射到期望显示、目标 strip 存在且不在销毁中、成员可用、且 `topology.observe_memberships().unique_space(id) == target`。超时 → 释放 `NativeMoveOwner` / `DisplayTransferFrame` / `DisplayTransferReadback`，**保留** `WindowSpaceReassignmentPending`，注释原文：*"A timeout releases ownership, not the geometry barrier. The existing membership audit resolves the actual destination before resuming writes."*
5. **所以未实现的意图会被忘记，最终由观测裁定归属。** 屏障由观测侧释放（[workspace.rs](../../../../src/ecs/workspace.rs) 的 `release_reassignment_barriers`，Space 删除重归等路径）。`PreviousTiledStrip` 是记忆（回到哪个 strip/index），不是权威目标。
6. **不乐观投影今天是达标的**：呈现、导航、命中检测用当前已确认条件，不把目标归属当已实现（符合 05 的不变量）。迁移不得破坏这一条。
7. **诊断缺一层。** `inspection/spool.rs` 的 window row 报告 `layout.space_id`（唯一 owner 或 previous）、`display_id`、`space_visible`，以及几何的 `attempts/blocked/confirmed`；**没有**"目标归属 / 已确认归属 / 阻塞原因 / 接纳版本"。
8. **保存是隐含的。** `state.rs` 的 `SavedState { spaces: Vec<SavedSpace> }` 按 strip 存列与宽度/高度意图（列成员即隐含归属），启动按隔离候选读取，不从保存数据自动应用归属或绑定身份。

**小结**：今天归属是"当场可写则写一次、2 秒内确认、否则忘掉并由观测裁定"。声明式目标要求它变成"编辑被接纳、目标留存、条件许可时实现、不可实现时 blocked 且可查"（ADR 0006、07）。

### 状态与接口

依据 [05 的状态归属表](05-state-ownership.md) 中「**目标 Space 归属 → 窗口意图 → 归属领域转移**」一行，以及对 07/17 意图形态的既有决定：

- **新增归属意图**：每个 tracked 窗口一个 `TargetSpace`（浮动窗口也有，但不进 tiled strip）。与列宽/高度意图同形：原始选择 + revision + 接纳证据。它的存在**不依赖**目标可见、不依赖能力当前可用、不依赖没有在途事务。
- **与布局结构原子（05 原文）**：「strip 中的排列成员是其结构索引，二者与源/目的布局修改在同一领域事务中验证并提交」——即归属编辑与其结构影响一次校验、一次提交，不出现"归属改了而 strip 没跟上"的中间态可供他人观察。
- **观测与意图分离**：`observe_memberships` / `unique_space` 的结果是**实际归属**，只驱动呈现与命中；意图只驱动效果。
- **唯一 writer**：归属领域转移（新模块，或 `native_space` 内的归属域）。`NativeSpaceTransactions` 降级为**效果协调器**：尝试次数、预算、阻塞与完成证据；它不再持有意图。
- **保留写保护**：`WindowSpaceReassignmentPending` 继续作为几何屏障存在（AGENTS.md 要求保留现有原生身份/转换/写保护），但不再兼任"意图"。

### 协调协议（沿用已裁政策）

- **资格（07）**：目标 Space 存在于当前拓扑、不在销毁中、非原生全屏、目的 viewport 可解析（未知则 blocked）、成员身份现任、`SpaceControl` 允许 `MoveWindows`。任一不成立 → **blocked + 原因**，意图保留。
- **能力不是拒绝理由**：`experimental_space_control` 关闭时，编辑仍应被接纳，效果侧 blocked（对应 CONTEXT.md 的 Layout Capability 与 ADR 0006 的"接受状态 ≠ 原生完成"）。
- **尝试与预算**：一次原生提交占一次；底层调用各自记录结果但不各算一次意图（19 的口径，本片无动画段）。超时**不重放陈旧命令**（06：稳定读回不吞掉未满足意图）。
- **证据与让权**：确认要求"我们提交的那次移动"与观测归属一致，且身份（window id + incarnation + pid）现任；两次同一完整样本间隔 ≥250ms 才可让权（13/21 已用的阈值口径）。
- **取代（02/13）**：新的明确输入取代旧意图并重新计预算；未确认的被替换尝试保留为自身迟到效果嫌疑，不用于让权、不能被当作"B 即 A 成功"。
- **不隐式**：源 Space 不可见时不隐式激活；move-follow 只在目的归属确认后提交（[implementation.md](../implementation.md) 已记录）。

### 实施顺序与移除清单

1. **状态层**：目标归属意图 + 接纳/拒绝 + `inspection` 投影（目标 / 已确认 / 阻塞 / 版本）。不动原生写。
2. **接纳层**：把"当场可写"前提从接纳移到效果资格——`admission::target_space` 的 visible-only 门、`capability_unavailable`、`native_move_pending` 都不再是否决编辑的理由；CLI / Lua / Bar 拖拽统一到归属领域转移。
3. **效果层**：`NativeSpaceTransactions` 变成意图协调器（资格恢复时实现最新意图、bounded、blocked 可查）；删除"超时即忘记"。
4. **保存与导入**：目标归属进入新格式、候选隔离、明确不跨重启自动应用（沿用 09/16 的范围）。
5. **移除清单**：`transfer.rs` 的可见性前提；`native_move_pending` 的"第二次移动直接拒绝"改为**取代**语义；`release_reassignment_barriers` 不再承担归属裁定者角色（退回纯屏障释放）。

### 验收矩阵（mock）

- 后台（不可见）目标 Space 的归属编辑：**被接纳**、零原生写入、诊断显示目标 + blocked 原因；目标变可见后实现**最新**意图（不是最旧命令）。
- 目标不可执行（能力关闭 / 空间在销毁 / 未知 viewport）：编辑仍被接纳，效果 blocked，原因可查。
- 一次跨 Space 编辑：不出现双重布局所有权（源 strip 与目的 strip 的成员状态在同一事务内一致）。
- 未确认的移动被新输入取代：旧尝试不计入新预算，且不用于让权；身份退休（destroy/换 instance）终止该意图。
- 超时后：意图仍在；资格恢复（拓扑 generation 变化）时实现最新意图，而非重放陈旧命令。
- 呈现/导航/命中检测在确认前仍按已确认归属（不乐观推进）。
- move-follow 只在目的归属确认后提交；源 Space 不可见时不隐式激活现有行为不变。
- 保存 → 重启 → 候选隔离读取：归属意图不被自动应用，也不自动绑定身份。

### 实现前需具体化的证据参数

- 归属意图的确认所需样本数与间隔（沿用 13/21 的 250ms 下限，是否要第二次完整样本）。
- 资格恢复的触发：拓扑 generation 变化 vs 定时复检（不得逐帧查询）。
- `blocked` 的原因集合（与现有 `Rejection` 码的关系：新增还是复用 `target_space_unavailable` / `capability_unavailable` / `native_precondition_failed`）。
- 目标是已知但**从未被观测为可见**的 Space 时，`viewport`/宽度投影如何取值或如何报告 unknown。

## 需要用户裁决

1. **意图粒度**：目标归属只记"属于哪个 Space"，还是同时记"在目的 strip 中的位置/顺序"？（我倾向首片只记 Space，位置在实现时按最新结构决定——与列宽片"只保存原始意图"的选择一致。）
2. **不可见目标的边界**：首片放开的范围是 (a) 任意已知 Space（含其它显示器）、(b) 仅当前显示器上的 Space、(c) 仅"当前可见显示器的隐藏 Space"？（ADR 0006 的终点是 (a)；风险最低是 (c)。）
3. **资格恢复后的重试**：意图保留后，是在**新的证据**（拓扑 generation 变化、能力恢复）时实现一次，还是允许有界定时重试？（13/15 对焦点选择了"不自动重试"，归属是否沿用？）
4. **让权**：观测证明窗口被外部移回原 Space、而我们的意图尚未确认时，意图是否让权（13 的焦点让权类比）？
5. **保存**：目标归属是否进新格式（候选隔离、不自动应用）？我倾向"进"，与 09/16 一致。

## Resolution

待用户逐项裁决后填写；裁决前不实施。

## Implementation follow-up

未实施。实施分四步（状态 → 接纳 → 效果 → 保存），每步以 `cargo fmt --check`、`cargo clippy --all-targets`、`cargo test -p spool` 收尾，并保持真实桌面验收另行授权。

## 边界

- 本片不含：完整跨重启恢复（焦点/归属/执行进度与通用停机冲突解决，仍在 Out of scope）、Space 创建/删除的声明式化、浮动窗口独立几何的全面迁移。
- mock 通过只证明模型与调用协议；真实 macOS Space 动画、约束、回声与原生 tab 边界仍需单独验收。
