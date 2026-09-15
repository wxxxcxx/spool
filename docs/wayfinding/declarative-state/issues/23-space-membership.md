# 归属状态的不变量与修复

Id: 23
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [Spool 声明式状态模型与迁移边界](../map.md)
Blocked by: none

## Question

归属迁移的代码替换边界、状态协议与验收场景。原设想是"保留目标、条件允许时实现"（见本文件的提交历史）。用户于 2026-09-15 重新界定为**不变量 + 修复**：状态必须始终有效，外部事件触发修复，而不是挂着一个不可能的目标等待。

## Answer

### 用户裁决（优先于任何从 ADR 0006 推出的推论）

1. **归属是状态的一部分，且必须始终指向一个当前存在的用户 Space。** 不存在"等待中的目标"。
2. **编辑**：会让状态无效、或当前无法尝试其效果的编辑，**当场拒绝并给出具体原因**。不保留、不排队、不重试。
3. **外部事件**让原本有效的状态失效时（Space 被销毁/合并、显示器断开、窗口被外部移动或实例退休），**修复**状态去对齐现实。
4. **修复只改状态，不写原生。** 把窗口真正搬过去是效果层按状态做的事。
5. **修复兜底顺序**：① 观测到的 Space → ② 隐藏/观测不到时用上次所在的 Space（`PreviousTiledStrip`）→ ③ 仍不明确则标"未知"，**不猜、不写**。
6. **一次尝试**：编辑被接纳时提交一次原生移动；确认窗口沿用现有 2 秒；未确认则**修复回观测**并报告，不自动重试。
7. **保留与否的判据**：只有当"用户无法便宜地重试 **且** 卡住状态常见且持久"时才保留状态、延迟实现。尺寸/排列/高度满足（已上线，本票不改）；**归属不满足**。

对用户的直接后果：把窗口挪到当前不可达的 Space 仍然当场失败并说明原因，用户看着目标重试一次；**不会**出现"Space 后来被建出来，窗口突然自己跑过去"。

### 与既有实现的关系

模型的这几条，代码里已经想清楚的地方就是这么做的，本票只是把它确立为归属这条线的成文规则：

- `DetachedSpace`（显示器断开）的注释原文：*"Only observed topology and membership, never elapsed time, end this state."*
- Space 删除后的合并（[workspace.rs](../../../../src/ecs/workspace.rs) 的 `move_layout_group` / `append_strip`）按观测把成员并入存活条带；`PreviousTiledStrip` 记录隐藏窗口的原 strip 与 index。
- [04 号票](04-lifecycle.md)：同名、同原生 ID、同应用重开都不自动继承旧意图 —— 修复不得静默绑定新对象。
- 已上线的尺寸/排列/高度是"保留状态、条件允许时实现"，本票不改变它。

### 当前证据（只读核对，基线 `5b9b2f8`）

1. **两条入口的"当场可写"前提不同。**
   - `move-to-space <id>`（[native_space.rs](../../../../src/ecs/native_space.rs) `execute_native_space_command`）：要求目标是当前 `observe_displays()` 中的**已知**用户 Space、非全屏，且 `SpaceControl` 允许 `MoveWindows`；成员暂时不可用、快照身份不符、该窗口已有在途移动都会拒绝（`native_precondition_failed` / `capability_unavailable` / `native_move_pending`）。目标**不必可见**。
   - 跨显示器转移（[transfer.rs](../../../../src/commands/transfer.rs) 的 `execute`）：经 `admission::target_space` → `visible_space_on_display`，要求目标显示器**当前正显示**该 Space，否则 `target_space_unavailable`（唯一调用点 `transfer.rs:49`）。
2. **接纳即提交**：`transfer::execute` 在同一事务内改源/目的 strip（`take_windows_preserving_layout` / `append_strip`，校验 `width_budget_is_valid`），随后 `submit_display_move` 冻结源列、插入 `NativeMoveOwner` + `DisplayTransferFrame`、push `PendingMove`。
3. **一次尝试 + 2 秒 + 观测接管**：`reconcile_native_space_transactions` 用 `MOVE_TIMEOUT = 2s` 确认（要求目标非全屏、映射到期望显示、目标 strip 存在且不在销毁中、成员可用、`observe_memberships().unique_space(id) == target`）；超时释放 `NativeMoveOwner` / `DisplayTransferFrame` / `DisplayTransferReadback`，**保留** `WindowSpaceReassignmentPending`（几何屏障），由成员关系审计按观测裁定归属。
4. **所以"未确认即由观测接管"这条今天就符合本票模型。** 缺的是三件：拒绝原因不具体（`native_precondition_failed` 混装多种情形）、诊断没有"声明的归属 / 尝试未确认 / 修复原因"、以及**声明归属本身不存在**（今天只有观测值 + 在途事务，没有一份可供修复的状态）。
5. **诊断现状**：`inspection/spool.rs` 的 window row 报告 `layout.space_id`（唯一 owner 或 previous）、`display_id`、`space_visible`，以及几何的 `attempts` / `blocked` / `confirmed`；没有归属这一层。

### 状态与接口

- 归属成为状态中的**声明目标 Space**（每 tracked 窗口一个，浮动窗口也有），带接纳版本；不变量：必须引用一个当前存在的用户 Space。
- 写入者：**归属领域转移**（编辑）与**修复**（外部事件）。观测（`observe_memberships`）是修复的输入，不是直接写者。
- 效果层由状态派生：声明 ≠ 观测时提交一次原生移动；**修复不触发原生写**。
- `WindowSpaceReassignmentPending` 保留为几何屏障（AGENTS.md 要求保留现有写保护），但不再兼任"意图"。

### 修复触发器（外部事件，需全部覆盖）

| 事件 | 修复动作 |
| --- | --- |
| 目标 Space 被销毁 | 修复为观测到的 Space（macOS 会把窗口挪走），记录原因 |
| 目标 Space 被合并进其它 Space | 同上（观测到哪就是哪，不"跟随"用户没说过的新目标） |
| 显示器断开（`DetachedSpace`） | 目标仍存在时不修；观测不可达时标未知 |
| 窗口被外部移动（用户拖走） | 修复为观测值（[02](02-external-policy.md)：外部操作取代旧意图） |
| 窗口实例退休（销毁/换实例） | 删除该窗口的声明归属（[04](04-lifecycle.md)：不继承） |
| 目标变成原生全屏 Space | 不是用户 Space → 修复/拒绝，理由入诊断 |
| 观测读失败 | 标"未知"，**不改、不写**（读失败不是缺席证据） |

### 编辑时的拒绝原因（需具体）

已实施（task-1，2026-09-15）：

- 目标 Space 不存在 / 不是用户 Space → `target_space_unavailable`
- 目标已原生全屏 → `fullscreen_space`
- 目标显示器当前未显示该 Space（跨显示器转移）→ `space_not_visible`（“现在不行”，与上一行的“不是用户 Space”分开）
- 能力关闭（`experimental_space_control`）→ `capability_unavailable`
- 该窗口已有一次移动在途 → `native_move_pending`
- 列/成员已不可用 → `window_unavailable`；列不存在 → `layout_not_found`；浮动窗口不属列 → `ineligible_layout_entry`
- 脚本计划的快照绑定已失效 → `snapshot_binding_stale`（新增）
- 显示器清单读取失败 → `topology_unresolved`（读取失败不是“不存在”）

测试：`space_move_refusals_name_their_own_cause`（每种原因一个断言）、`an_unreachable_target_is_not_a_fullscreen_one`。两测在改动前的代码上均失败（分别得到 `native_precondition_failed` 与 `TargetSpaceUnavailable`）。

### 实施顺序与移除清单

1. **状态层**：声明归属 + 不变量 + 修复（含触发器与原因记录）+ 诊断字段。不动原生写。
2. **拒绝原因层**：拆开编辑路径的拒绝原因；诊断显示"声明的归属 / 观测的归属 / 最近一次尝试与结果 / 修复历史与原因"。
3. **效果层**：确认"一次尝试 → 2 秒 → 未确认即修复回观测"这条链，并让超时不再静默（今天只有一条 `warn!` 日志）。
4. **评估跨显示器那道可见性门**：有真实技术理由则保留并明确报告；若是遗留限制则移除，让效果层在可尝试时提交。
5. **移除清单**：`native_precondition_failed` 作为归属路径的混装原因（已移除）；事务超时后靠“忘记”处理归属的隐含语义（待 task-3）。

### 验收矩阵（mock）

- **用户场景**：声明"窗口 1 在 space3"，space3 被手动关闭 → 声明归属修复为观测到的 space1、原因可查、零原生写入、状态中无悬空目标；此后 space3 被重新建出（新 id）→ 窗口 1 **不会**被移动过去。
- 编辑目标不存在 / 不是用户 Space / 已全屏 → 当场拒绝、码具体、状态不变。
- 能力关闭 → 当场拒绝 `capability_unavailable`、状态不变、不进入等待。
- 一次尝试未确认（mock 让原生移动不生效）→ 2 秒后声明归属修复回观测，诊断记录"尝试未确认"。
- 外部把窗口拖到别的 Space → 声明归属修复为观测值，零原生写入。
- 观测读失败 → 声明归属保留、标未知、零原生写入。
- 窗口实例退休 → 该窗口的声明归属删除，不继承到新实例。
- 已上线的尺寸/排列/高度行为不变（回归）。

### 与 ADR 0006 的关系

ADR 0006 的"不可见 Space 也应接受 desired-layout 编辑"在本票被**明确界定**：该承诺适用于**尺寸与排列**（已实现，且满足保留判据）；**归属**这一维不适用 —— 归属编辑要求目标有效且当场可尝试，外部事件走修复。界定已记入 [ADR 0006](../../../adr/0006-declarative-state-driven-window-management.md)，以免以后从 ADR 再推出"挂着等"的实现。

### 边界

- 不做：挂起/重试/排队的归属移动；跨重启恢复归属；Space 创建与删除的声明式化；浮动窗口独立几何的全面迁移。
- mock 通过只证明模型与调用协议；真实 macOS Space 动画、约束、回声与原生 tab 边界仍需另行授权验收。

## 实施进展

### task-1：拒绝原因具体化 + 最近一次尝试可查（已实施，2026-09-15）

- 归属路径的混装 `native_precondition_failed` 拆开（见上文“编辑时的拒绝原因”）；跨显示器转移不再把“看不见”与“全屏”合并成 `target_space_unavailable`（`Rejection::TargetSpaceUnavailable` 因此不再被任何共享配方构造，已删除）。
- 新增**只读诊断记录** `SpaceMoveAttempt`（`InFlight` / `Confirmed` / `TimedOut` / `Retired` / `Refused(code)`）：编辑被拒时记下调用方拿到的原因；提交时记“在途”；审计确认/超时/实例退休时收尾。它**不被布局、效果或准入读取**，仅用于诊断（替换/退休的实体用 `try_insert`，不会因记录而对已 despawn 的实体发命令）。
- `window inspect --source spool` 的 window row 新增 `membership.attempt`（`target_space_id` / `result` / `code`），并把 `membership` 加入默认选择与可选路径。
- 测试：`the_last_membership_attempt_is_recorded`（五种结果各自的断言）、`window_detail_reports_the_last_membership_attempt`（诊断投影）。

### task-2a：声明归属状态 + 修复 + 三栏诊断（已实施，2026-09-15）

- 新增 `DeclaredSpace`（`target` / `observed` / `repairs`）：`target` 是声明的归属，必须指向一个存在且非全屏的用户 Space，或在没有有效目标时为 `None`；`observed` 是本次协调看到的归属；`repairs` 是最近 4 次修复（`from` / `to` / `reason`）。
- 新增 `reconcile_declared_space`（调度在 `Last`：销毁处理 → 事务协调 → 声明协调，这样修复命名的是窗口**实际所在**的 Space 而不是事务即将离开的那个）。**不写原生**：搬窗口是效果层的事。
- 修复触发器已覆盖并各有测试：目标 Space 被销毁/合并（旗舰场景：手动关掉声明所在的 Space → 修复为窗口实际所在的 Space、原因 `target_space_destroyed`、零原生写入；此后新建的 Space 不会被自动吸引）、外部移动（`membership_changed`）、目标变原生全屏（`target_space_fullscreen`）、显示器断开（目标仍存在 → **不修**）、实例退休（旧实体的声明随实体消失，新实例不继承修复历史）、读失败（不可读时**不修**，未知不等于缺席）。
- 诊断：window row 的 `membership` 现在含 `declared` / `observed` / `repairs`（`membership` 已在默认选择内，叶子路径 `membership.declared|observed|repairs` 可单独选）。
- 测试在改动前的代码上无法编译（`cannot find type DeclaredSpace`）——新状态类型的固有形态；行为断言由此建立。

## Resolution

用户于 2026-09-15 逐项裁决：不变量 + 修复、修复不写原生、兜底顺序（观测 → 上次所在 → 未知）、一次尝试（2 秒，不自动重试）、以及"保留只在用户无法便宜重试且卡住状态常见持久时才做"这条判据。见上文 Answer。

## Implementation follow-up

未实施。按"状态与修复 → 拒绝原因与诊断 → 效果链确认 → 可见性门评估"四步推进，每步以 `cargo fmt --check`、`cargo clippy --all-targets`、`cargo test -p spool` 收尾；真实桌面验收另行授权。
