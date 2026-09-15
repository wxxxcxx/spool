# 09 — 收口：延迟延续的门槛、两处可达性的理由、Lua 边界

Id: 09
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [ADR 0009 — Command Admission](../../adr/0009-command-admission.md)
Blocked by: 08

## Why

07/08 折叠后留下四笔「分类了但没人管」的账。逐条查过之后，只有两笔需要改行为，
其余两笔要改的是**理由**（原有的理由经查不成立），并以测试钉住。

## 逐条结论

### 1. `Event::LayoutSpaceRequested` 的门槛（真缺口，已修）

它是脚本计划里 Space/焦点操作的**延迟延续**。`dispatch_actions` 原本直接把它送进
`apply_native_space_command`，绕过准入 —— 于是同一个命令，从任何其它 intake 都会
在会话未就绪时被拒，只有这条延迟路径会照常执行。

- 抽出唯一的一处生命周期判断 `admission::session_is_ready(world)`，由有序流水线
  与新增的 `admission::admit_deferred` 共同调用；后者是第三个入口点（前两个是
  `execute` 与 `admit`），专门承接这条非 action 的延续。
- 新增 `a_deferred_space_command_obeys_the_same_gate_as_its_action`：同一会话状态
  下，延续与其 action 必须同判。**该测试在旧代码上失败**（`stopping=true` 时延续
  到达了平台 `1`，而直接 action 被拒 `0`），修复后通过。

### 2. `SetSpaceFocusPreference` 与 native Space 命令：保持 `Running`（不改行为）

原注释（沿用自并行分支）说「native 命令系统自行复核会话」。查证后**不成立**：
`execute_native_space_command` 里没有任何 Mission Control / 初始化 / 退出检查，它
复核的是 `LayoutSession::accepts` 的**快照身份**，与这里的门槛是两回事。

真正的原因是另一条，而且方向相反 —— 不该提升为 `Writable`：

- 焦点偏好是纯状态编辑（`set_space_preference` 不查 Mission Control）；
- 激活请求由 `focus::reconcile_activation`（持有 `Res<MissionControlActive>`）在
  Mission Control 期间挂起，可用性无法证明时报告 blocked —— 这是**延迟**，不是拒绝。

按 ADR 0006「接受状态与实现分离」，在此处拒绝会与它冲突，并破坏 ADR 0009 自己列出
的 Activation Intent 延迟例外。因此保留 `Running`，把理由写进表内注释，并新增
`mission_control_defers_state_edits_instead_of_refusing_them`：同一 Mission Control
状态下，偏好被接受、激活不是 `session_not_writable`、而几何编辑（`Center`）确实是
`session_not_writable` —— 这条测试专门挡住未来"顺手提升"的改动。

### 3. `Action::Lua`：由 Lua worker 拥有（不改行为，补文档与契约测试）

`Action::Lua(u32)` 在 wire 词汇里（`commands.rs`：按注册 id 调用绑定回调），
但读者是 `lua::command_lua_handler`（过滤 `Event::ActionRequested` 后交给 worker），
不是这张表。checked 路径因此拒绝它：处理函数是时长无界的用户代码，任何回执都无法
承诺"它跑过了"。新增 `a_checked_lua_callback_is_refused_rather_than_owed_a_receipt`
钉住该拒绝码，并在表中注明归属。

### 4. `Admission` 的 4 处 `#[cfg(test)]` 适配器（已删）

`focus` 字段与 `focus_target`、`unique_strip`、`eligible_column` 三个适配器全部移除：
测试改为传它自己的 `Query`/`Windows`/`Res<FocusCoordinator>` 并调用**生产同款**的
`focused_target_in`、`space_scope_strip_in`、`eligible_column_in`。副作用：生产端的
`Admission` 不再为了测试携带一个它不用的 `FocusCoordinator`。

## Acceptance criteria

- [x] 延迟延续与其 action 同门槛，且测试在旧代码上失败过。
- [x] 两处 `Running` 的理由写在表内，并由测试钉住（而非提升为 `Writable`）。
- [x] checked `Action::Lua` 的拒绝码有测试。
- [x] 4 处测试适配器消失，测试与生产走同一入口。
- [x] `cargo fmt --check`、`cargo clippy --all-targets`、`cargo test -p spool` 全绿（1149 通过）。

## Left open

- bus 上的 `Action::Lua`（键位绑定路径）不经这张表，因此也不经生命周期门槛。这是
  记录在案的边界，不是疏漏：worker 拥有该 action。若将来要求键位回调也在关闭期间
  被拒，那是对 `command_lua_handler` 的改动，需要自己的测试。
- 完整跨重启恢复与目标 Space 归属的声明式事务仍属 `declarative-state` 主线，不在本主题内。
