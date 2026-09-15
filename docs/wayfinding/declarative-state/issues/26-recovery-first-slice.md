# 恢复第一个切片：可信映射的运行时入口

Id: 26
Type: task
Label: wayfinder:task
Status: open
Assignee: none
Parent: [跨重启恢复的入口票（恢复地图）](25-cross-restart-recovery.md)
Blocked by: none

## Question

恢复地图（25 号票）已裁决七项。但**今天所有恢复能力都是惰性的**：候选与可信映射导入缝（`RestoreCandidates` / `import_intents`）已经存在且经过测试，却**没有任何运行时调用者**——`restore.rs` 的注释直接写着 *"No automatic binding provider exists"*、*"A caller owning a future startup protocol must close its frozen candidate set when the startup window ends."*。本票确定第一个可实施切片的边界与**接口形状**（接口是用户可见 API，因此先提案再实施）。

## 当前证据（只读核对，基线 `ccbc45f`）

1. `import_intents(candidates, target: &mut LayoutStrip, bindings: &[TrustedColumnBinding])` 是纯函数、按 Space 导入列宽/高度意图：要求候选未过期、`freeze_initial_layouts` 已冻结基线、每个 binding 的 `target_space`/`target_column`/`intent_revision`/`structure_revision` 与冻结基线一致，且同一列不得重复。
2. 冻结与关闭窗口由"启动方"调用：`freeze_initial_layouts`（一次）与 `close_import_window`（启动窗口结束时）。
3. 保存侧已覆盖：列宽、stack 高度权重、成员 slot 提示、以及**浮动帧**（`SavedFloatingWindow`）。**声明归属（`DeclaredSpace`）尚未保存**（23 号票明示为会话内状态）。
4. 没有任何 CLI/Lua 入口、也没有启动协议所有者会调用上述两处；`spool session inspect` 只读诊断。

## 切片提案

**目标**：让 25 号票的 ①②③ 从"有缝但没人用"变成"有一个所有者、可被显式确认驱动"，并且只做**布局 + 浮动帧**这两类（归属与焦点留后续票）。

1. **启动协议所有者**：在 daemon 启动完成后（`finish_setup` 之后、首次接受编辑之前）调用 `freeze_initial_layouts`；启动窗口在**显式恢复请求**或一次超时后由同一所有者 `close_import_window`。
2. **可信映射入口（需要你确认表面）**：新增一个资源式子命令，由调用者提交"可信映射"，daemon 校验后导入。建议形状：
   - `spool session restore --bindings <path|->`（JSON），内容按域分组：
     - `columns: [{space, column, structure_revision, intent_revision}]`（沿用既有 `TrustedColumnBinding` 语义）
     - `floating: [{window_id, frame}]`（新；`frame` 必须与候选一致，否则拒绝整条）
   - **绑定必须被佐证**：`window_id` 是调用者的声明，不是本模块的推断（沿用 `restore.rs` 既有规则："the caller must prove the mapping; this module never infers it from IDs, titles, positions, PID, or process-local native hashes"）。daemon 侧除校验编号指向一个**已跟踪**窗口外，还要求候选缓存的 `pid` 与 `bundle_id` 与之一致；只有编号而无佐证时拒绝该条（编号的作用域是当前登录会话；寿命与复用行为无文档规定，"会被复用"是待核实的推断，见 [27 号票](27-window-identity-verification.md)）。
   - 退出码沿用资源 CLI 约定（0 完成 / 1 失败 / 2 参数 / 3 部分）；未知空间/列/窗口或 revisions 不符 → 拒绝并说明，不部分应用。
3. **浮动帧导入**：新增纯函数 `import_floating_frames(candidates, windows, bindings)`，语义与列版一致（候选未过期、基线已冻结、逐条校验、重复拒绝、整体拒绝而不部分应用）。
4. **声明归属仍不保存**（留后续票）；本片不引入自动绑定、不恢复在途尝试、不新增跳变路径。

## 验收矩阵（草案）

- 无调用者时：候选存在但不改变任何状态（回归既有行为）。
- `session restore` 提交与候选一致的列/浮动映射 → 意图被导入、原生零写入（导入只改状态）。
- 任一条 binding 的 revisions/身份与冻结基线不符 → 整体拒绝、无部分应用、退出码 1 且原因可查。
- 候选已过期（`close_import_window` 之后）→ 拒绝。
- 重复列/重复窗口 → 拒绝。
- 导入后编辑不被覆盖（沿用既有"导入不能覆盖晚到编辑"的语义）。
- 平铺/浮动既有行为不变（全量回归）。

## 边界

- 不做：自动绑定（另立研究票）、归属与焦点的恢复、在途尝试恢复、恢复 UI、真实桌面验收。
- 接口一旦确认即按既有验证契约实施（`fmt` + `clippy` 双 feature + `--workspace --locked`，行为改变处附"旧代码上失败"的证据）。

## Resolution

待用户确认接口形状（一节"切片提案"第 2 条）后实施。

## Implementation follow-up

未实施。
