# 跨重启恢复的入口票（恢复地图）

Id: 25
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [Spool 声明式状态模型与迁移边界](../map.md)
Blocked by: none


> **2026-09-16 反转**：本票整体被 [ADR 0010](../../../adr/0010-runtime-state-rebuilt-from-rules.md) 取代——retained state 只在运行期存在，没有落盘、候选、导入缝或 `session restore`。跨会话偏好改由**规则**表达（字段按需增补），未表达者按"观测 → 默认"重建、接受丢失。反转原因与落地见 [运行期状态与实现对账](../../runtime-state/map.md)。

## Question

地图把"完整跨重启恢复所有焦点/归属/执行进度及通用停机冲突解决"划在规划轮之外，并要求"另立恢复地图"。本票是那张地图的入口：只做只读核对，列出地图必须回答的问题与倾向，等用户裁决后再拆具体研究或实现票。已有政策不再重新表决。

## 当前证据（只读核对，基线 `969ae73`）

1. **候选隔离已经存在。** `state.json` 的布局意图（列宽、stack 原始高度权重、每个保留成员一个 slot、以及浮动帧）读入 `RestoreCandidates { state, import_window_open, initial_layouts }`；`import_intents` 是"纯可信映射导入缝"，其注释写着 *"the first intent slice exposes only a trusted-mapping pure import seam; no runtime automatic binding provider exists"*，导入只接受一次，且不能覆盖晚到的高度或结构编辑。
2. **没有任何跨 daemon 身份绑定。** 成员 slot 只保存"该 item 期望几个成员"与顺序；`hint` 是缓存的身份提示，`hint: null` 表示保存时解析不到——两者都不授权自动绑定（implementation.md 明写）。
3. **在场的身份证据都是会话内的。** `WindowIncarnation` 是 `CFHash(AXUIElement)`（进程内对象哈希，见 `manager/windows.rs`）；`LayoutPlan` 快照带 daemon 会话 UUID；`NativeMoveOwner` / `WindowSpaceReassignmentPending` 等屏障都是 ECS 内状态。跨 daemon 全部无效。
4. **持久身份只有"应用 + pid + bundle"，加一个会话内的窗口编号。** `SavedWindow { window_id, pid, bundle_id }`；`window_id` 是 WindowServer 的窗口编号（`kCGWindowNumber`，Spool 经私有 `_AXUIElementGetWindow` 取得）。它的**文档化范围是"当前用户会话内唯一"**（[native-tab 研究](../../../research/native-tab-platform-observation-2026-09-11.md) 引 Apple 文档：`NSWindow.windowNumber` 是应用内的 window-device 编号，与服务端的全局编号不是一回事；`kCGWindowNumber` 在当前用户会话内唯一），而**寿命与复用行为两家文档都没有规定**（该研究因此写着 *"Do not turn common runtime mappings into an unconditional identity guarantee."*）；"会被复用"本身是**待核实的推断**——仓库代码注释里有这个断言但没有一手出处，核实清单见 [窗口唯一标识的核实](27-window-identity-verification.md)。 所以它能在**同一次登录会话内**作为最强的单条证据（Spool 重启后窗口编号不变），但不能单独充当身份，也不能跨登出/重启；本仓的 `state.rs` 里"numeric IDs are candidate hints, not cross-daemon identity" 说的是 **Space 与列的编号**，不覆盖窗口编号。
5. **`platform/service/ownership.rs` 的 `schema_version` 戳是安装归属**（谁拥有这个安装与 launch agent），不是窗口或意图身份。
6. **退出时不写执行进度。** `exit_restore.rs` 的 capture 是会话内的启动帧恢复（让窗口回到屏幕上），不是可恢复的意图；在途尝试、重试预算、让权证据都不落盘。
7. **恢复的存在性缺口**（implementation.md 已承认）：焦点（每 Space 偏好与逻辑选择）、归属（声明 Space）、执行进度（attempts/budget/失败/阻塞）都没有跨重启恢复；IPC 协议版本（7）与磁盘布局版本（v6）是两个独立版本面。

## 恢复地图必须回答的问题（附我的倾向）

1. **身份连续性证据的层级**：什么算"同一个窗口"跨 daemon？倾向：应用 bundle + PID 存活 + AX 可读属性（role/subrole/title 精确匹配，或几何匹配）的**组合证据**，且证据不足时不绑定。窗口编号（`window_id`）是同一登录会话内**最强的单条证据**，但必须由上述证据佐证：它的作用域是当前用户会话（跨登出/重启无效），而寿命与复用行为**无文档规定**，单独使用有把陈旧候选绑到后来拿到同一编号的别的窗口上的风险（该风险是待核实的推断，见 [27 号票](27-window-identity-verification.md)）。
2. **恢复哪些域、按什么顺序**：布局（列宽/高度/浮动帧，候选已有）→ 归属（声明 Space）→ 焦点（每 Space 偏好/选择）→ 执行进度（在途尝试）。倾向：前三者作为候选恢复；**在途尝试不恢复**——重启后一律视为未确认，避免重放陈旧命令（06 的既有精神）。
3. **候选还是自动绑定**：是否引入运行时自动绑定提供者（今天只有"可信映射"纯导入缝）？倾向：先做**显式确认**路径（CLI/Lua 提交可信映射）；自动绑定另立研究票，需要证据门槛与误绑定代价分析。
4. **停机冲突解决**：退出时仍在途的尝试如何收尾？倾向：退出不改写意图；未确认的尝试在下次启动时以"未确认"呈现在诊断里，不重放。
5. **格式与迁移**：延续"新增可选字段不升版本、旧格式忽略"的策略？倾向：延续，并在地图里写明版本演进规则与何时必须升版（例如语义不可兼容时）。
6. **恢复对呈现的影响**：恢复时允许动画还是跳变？倾向：与既有修复一致——派生后交给动画管道，不新增专用跳变路径。
7. **明确不做**：安装与部署、真实桌面验收、多 daemon 同时写同一 `state.json` 的所有权争议、以及恢复 UI。

## 与既有政策的关系

[04 意图寿命](04-lifecycle.md)、[05 状态归属](05-state-ownership.md)、[06 接纳证据](06-evidence.md)、[02 外部操作取代旧意图](02-external-policy.md)，以及 [23](23-space-membership.md)/[24](24-floating-windows.md) 的"不变量 + 修复"与"候选隔离"都直接适用。本票只确定地图要回答什么，不实施。

## Resolution

用户于 2026-09-15 确认按票中倾向执行（七项）：① 身份连续性用 bundle + PID 存活 + AX 属性/几何的**组合证据**，证据不足不绑定；窗口编号只在同一登录会话内、且作为必须被佐证的证据使用（作用域无文档保证，复用行为待核实：见 27 号票）；② 恢复域与顺序为布局 → 归属 → 焦点，**在途尝试不恢复**（重启后一律视为未确认，不重放）；③ 先做**显式确认**路径（可信映射由启动方提交），自动绑定另立研究票；④ 退出不改写意图，未确认的尝试只作为诊断呈现；⑤ 延续"新增可选字段不升版本、旧格式忽略"；⑥ 恢复的呈现交给既有动画管道，不加专用跳变路径；⑦ 不做安装部署、真实桌面验证、多 daemon 争写与恢复 UI。

## Implementation follow-up

未实施。本票是恢复地图的入口；裁决后按域（布局 / 归属 / 焦点 / 进度 / 停机）拆具体票，每票沿用既有的验证契约（`fmt` + `clippy` 双 feature + `--workspace --locked`，行为改变处附"旧代码上失败"的证据）。
