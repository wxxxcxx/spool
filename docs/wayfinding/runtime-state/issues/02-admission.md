# 撤掉准入闸门：由结局分类取代按动作的会话可达性

Id: 02
Type: task
Label: wayfinder:task
Status: open
Assignee: none
Parent: [运行期状态与实现对账](../map.md)
Blocked by: 01

## Question

`session_reach` 把 `Action::Window | TargetedWindow | SpaceLayout` 分类为 `Writable`，但 `execute_action` 在唯一的 `session_is_writable` 检查**之前**就调用 `column_width::execute` / `layout_edit::execute`，于是 `SetWidth`/`Balance`/`Maximize`/`Resize(Width)`/`Equalize`/`Move`/`ToggleStack`/`Resize(Height)` 完全绕过闸门，而 `Center` 与 `Action::Layout(plan)` 被拒绝。分类已经不再决定任何事。ADR 0011 决定由结局三分类取代它，本票实现该替换。

## 决策沿用（ADR 0011，已确认，不再询问）

- 布局状态编辑**不再**因会话状态被拒绝：Mission Control、初始化、原生移动在途 → 结局 3（等待），由 01 票的对齐机制兜底。
- **退出是唯一例外**：会话正在交还桌面（`ExitInProgress`）时**全部拒绝**（含纯记忆编辑），因为此刻显示正在被还原、也没有"以后"。
- 初始化不特殊处理：目标是否存在由 recipe 按身份校验（缺目标就是身份拒绝，用户可重试）。
- `Center` 的指针 warp 是一次性副作用、不可动画也不可"过后补"：**能立即执行就执行，否则跳过并在诊断里说明**（居中本身是状态编辑，照常等待）。

## What to build

1. 撤掉 `SessionReach::Writable` 判定与 `session_is_writable` 的 Mission Control / 初始化分支，只保留退出这一条拒绝，并改名以反映它现在只表达"会话正在交还"。
2. `session_reach` 表退化为"是否在退出中即拒"的穷尽声明（新 `Action` 变体仍必须声明，防止漏判）。
3. `Center` 的 warp 按上述语义实现，并在诊断里区分"warp 已执行 / 跳过"。
4. 更新 ADR 0009 中被本决定取代的段落（分类语义与"state edits 延迟而非拒绝"那句），加带日期的修订说明；同步 `ARCHITECTURE.md` 的 Layout Mutation Admission 段落。

## 验收矩阵

- Mission Control 打开时：宽度编辑、排列编辑、`Center` 均被**接受**（进入等待），不再出现 `session_not_writable`。
- 退出中（`ExitInProgress`）：上述编辑全部被拒（含 `space prefer-focus` 这类纯记忆编辑）。
- 初始化期间：目标存在即接受，缺目标返回身份拒绝（不是会话拒绝）。
- 新增一个 `Action` 变体而忘记声明时**编译失败**（穷尽性仍被编译器强制）。
- `Center` 在不能立即 warp 时：窗口仍被居中，诊断显示 warp 跳过；能立即 warp 时行为与今天一致。
- 旧测试 `mission_control_defers_state_edits_instead_of_refusing_them` 按新政策改写（偏好/激活仍轻准入，几何与布局编辑不再被拒）。

## 已实施（2026-09-16）

- `SessionReach::Writable` 退役，换成 `HandingOver`：**只有退出交接**才拒绝（`quit`/`restart` 仍可，重复退出幂等）。Mission Control 与初始化不再拒绝任何编辑——它们由实现结局回答（等待并收敛）。
- 门槛**上移到所有效果之前**（含 effect-only 的 `PrintState`/`MissionControl`/`ShowDesktop`/`ReconcileWindows`/`ToggleBarCollapse`），因此分类不再是死代码：这正是复核发现的缺陷（两个 recipe 在唯一检查之前 dispatch）。
- 拒绝改用类型化 `Rejection::SessionHandingOver`；**wire 字符串仍是 `session_not_writable`**（ADR 0009 的"unchanged wire"规则，脚本按它匹配）。这与票面原先写的"改名"不同，理由记录在此。
- `Center` 的指针 warp 明确化：居中照常是状态编辑；warp 是一次性副作用、没有"以后"，因此仅在**桌面仍由 Spool 移动指针**时执行（Mission Control 打开或退出交接时跳过并记 `debug!` 原因）。
- 文档：ADR 0009 加 2026-09-16 修订说明（`Writable` 退役与理由），`ARCHITECTURE.md` 的两处（admission 流水线与 Layout Mutation Admission）同步。
- 测试改写：`mission_control_accepts_every_edit_instead_of_refusing_it`（原 `..._defers_state_edits_instead_of_refusing_them`）、`center_and_snap_reach_the_strip_under_mission_control_too`（原 `..._obey_the_session_gate_from_the_bus`）、`a_layout_plan_edits_layout_state_while_mission_control_is_open`（原 `..._cannot_edit_an_unwritable_session`）、intake 一致性测试改用退出交接作为拒绝态；新增 `the_exit_handover_refuses_every_edit` 与 `centering_skips_the_pointer_move_while_mission_control_is_open`。
- **证据**：把 Mission Control 重新算进"会话不可写"（旧政策）后，三条新行为测试全部失败（`Rejected` vs `Accepted`、repositions 0、plan 意图未变）。
- 验证：workspace 1209 / 24 / 96 / 6 / 4，`--no-default-features` 1067，fmt 与两套 clippy `-D warnings` 通过。

## 尚未实施：Mission Control 期间的"等待"（需要独立切片）

ADR 0011 把"Mission Control 打开"列为**第 3 类（暂不可判定 → 等待）**，但**实现层目前没有这道等待**：撤掉准入闸门后，Mission Control 期间接受的编辑会立刻尝试写入几何（这也正是旧代码对宽度编辑的实际行为——分类只是宣称有闸门）。要做成"等待"，需要在唯一的实现阶段（提交链路）加一道屏障。

本票试过一版并在验证中撤销：让 `commit_window_frames` 在 Mission Control 期间跳过写入（不消耗尝试）。结果 `tests::stacking::mission_control_defers_stacking_until_it_closes` 的 AX raise 序列从 `[0,3,2]` 变成 `[4,3,2]`——即概览关闭后的第一次 stacking 计划与写入延迟产生交互，需要单独理解与验证，不能在这一票里顺手接受或改写它的预期。

所以本票的结论是：**闸门已撤、退出交接仍在拒；"等待"这一半另立切片**（连同真实桌面验收：概览期间写几何到底有没有害）。在此之前，Mission Control 期间的编辑行为是"立即尝试 → 由结局分类与对齐收敛"，与 ADR 0011 的 outcome-3 列表存在已知差异，已记于此。

## 边界

- 不做：对齐机制本身（01 票）、落盘退役（03 票）、规则字段（04 票）。
- 不引入新的准入政策；本票只把"按动作的可达性"换成"退出即拒 + 结局分类"。

## 验证契约

同 01 票。行为改变处（尤其 Mission Control 与退出两个方向）附旧代码上失败的证据。
