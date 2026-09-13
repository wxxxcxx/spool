# 首个完整迁移切片及后续次序怎样验收

Id: 10
Type: grilling
Label: wayfinder:grilling
Status: resolved
Assignee: none
Parent: [Spool 声明式状态模型与迁移边界](../map.md)
Blocked by: 05, 06, 07, 08, 09, 16, 17

## Question

选择首个可实施切片、需要替换的旧控制路径、原子状态编辑/渲染/效果/诊断/保存端到端验收，以及后续焦点和Space归属迁移次序。区分本地mock验证和未来明确授权的真实桌面验收。

## Answer

三位专家支持首片、替换范围和下列共同动画协议，按用户授权接受。后来拆出的政策分歧均已由用户逐项裁决；本路线已定稿，不代表现有代码已实现。

### 首个完整切片

不可见Space的列宽编辑：显式ColumnId/WidthIntent -> 原子接纳 -> 有效槽宽与frame派生 -> 有界呈现 -> 资格门禁与原生确认 -> 诊断 -> 原始意图保存及隔离候选读取。

绝对逻辑点与比例均覆盖；未知viewport比例接纳后阻塞派生。所有tiled-column width输入和writer同次迁移，包括配置、初始化、命令、脚本、结构编辑、外部接纳与纯导入；不能加一个字段后保留首成员/最大成员frame的第二权威。浮动窗口独立宽度不扩大进首片，但进入平铺时必须经过新列政策。

### 动画与纠正共同协议

- 正常动画中间帧不逐帧消耗最终目标纠正预算、不作为尺寸约束证据；所有帧仍检查实例、相关版本、执行资格和暂停状态。
- 同一初始目标的正常动画流必须有限；整数插值停滞/异常速率不得无限发帧，不通过重启动画刷新预算。
- 任一中间写失败或未知冲突立即停止该流。后续许可纠正直接提交最新最终有效目标，不重播全段动画。
- 调整轮次是语义尝试；position/size等底层调用分别记录结果但不各算一个独立意图。实际开始原生调整即占一次，整段动画和终点共用该次，终点不再重复扣数；观察已达目标时不为计数再补写。
- 用户确认动画中途失败仍占用该次，后续许可尝试直接提交最终目标，见 [动画在终点前失败如何计入尝试预算](19-animation-budget.md)；不得逐帧扣预算或允许失败无限免费重播。

### 替换入口

| 当前入口 | 首片职责 |
|---|---|
| [layout.rs](../../../../src/ecs/layout.rs) | 显式列身份/宽意图、槽宽求解；替换最大成员和首成员frame两种宽度来源 |
| [admission.rs](../../../../src/commands/admission.rs)、[targeted.rs](../../../../src/commands/targeted.rs)、[layout_edit.rs](../../../../src/commands/layout_edit.rs) | tiled宽度与排列入口改领域转移；把可见性、当前平台写资格移到effects；不粗暴移除身份/结构校验。`space_layout.rs` 的剩余操作已移入状态域处理器并删除该文件 |
| [layout_ops.rs](../../../../src/ecs/layout_ops.rs)、[commands.rs](../../../../src/commands.rs) | ratio、预设、全宽恢复及结构宽度影响统一进入新writer；恢复原始变体和值，不从受限frame算回比例 |
| [triggers.rs](../../../../src/ecs/triggers.rs)、[window_geometry.rs](../../../../src/ecs/window_geometry.rs) | 初始化走已定来源政策；resize verifier/外部几何只通过证据政策改约束或提交领域转移 |
| [window_frame.rs](../../../../src/ecs/window_frame.rs)、[systems.rs](../../../../src/ecs/systems.rs)、[reconcile.rs](../../../../src/ecs/reconcile.rs) | 几何派生、呈现、提交、读回分权；移除Bounds/ResizeMarker/WidthRatio作为tiled宽度第二写者的旁路 |
| [layout_snapshot.rs](../../../../src/ecs/layout_snapshot.rs) | ordinal/列及窗口身份绑定、延迟结构编辑的版本核验；它不是磁盘保存器 |
| [state.rs](../../../../src/ecs/state.rs)、[restore.rs](../../../../src/ecs/restore.rs) | 新格式意图保存、候选隔离、純导入；旧自动恢复writer退出新模型 |
| [inspection/spool.rs](../../../../src/inspection/spool.rs) | 目标、有效值、原生观察投影、差异、阻塞和接纳/保存版本；独立native来源仍只报告现实证据 |

### 验收矩阵

| 情境 | 必须证明 |
|---|---|
| 后台列宽800，随后900 | 立即接纳最新版本；无聚焦/切Space/原生几何写；可执行后仅协调900 |
| 比例但viewport未知 | 保留比例并明确派生阻塞，不借其他Space几何 |
| 原始800、受限有效1000 | 槽宽、后续列位置和frame使用同一有效解，padding一次转换；保存仍800，约束解除恢复最新原意图 |
| A在途后接纳B | A不完成B、不恢复A意图；新鲜实际副作用仍被观察 |
| 成员换序、暂缺、约束变更 | ColumnId和原意图不被改变；约束重算不冒充用户编辑 |
| 新列、拆列、加入列 | 按最终已决初始化/继承政策，无frame偷渡；接纳ordinal后重排不换目标 |
| 外部采样期间新命令 | 候选提交前版本门禁拒绝旧编辑；未知冲突不被称为用户操作 |
| 正常动画超过三帧 | 不因帧数用尽纠正预算；有界结束，迟到旧帧提交前被拒绝 |
| 动画中途写失败 | 后续正常帧停止，已消耗一次、最多剩2次；许可重试只发最终目标，正常帧与终点不重复扣数 |
| 约束交集空 | 不擅改结构、不发依赖未知宽度的半套目标；真实部分完成仍可观测 |
| 耗尽后噪声、同值命令、时间推进 | 不补预算、不无限专用轮询；正常被动观察可确认后来的实际达成 |
| 保存乱序、失败、读取候选 | 不退盘，失败仍dirty；candidate-read不改意图不发effects |
| 纯导入可信mock映射 | 无部分非法提交、不覆盖本会话新编辑；不宣称证明跨进程绑定 |
| 原生tabs、主线程和Lua | 只控独立外框，平台主线程，Lua handler仍不阻塞事件泵 |

复用现有mock Bevy/WindowManager测试及window_frame_architecture、command_dispatch、layout_ops、session_restore、native_move/display/focus相关测试。实现后执行cargo fmt、cargo check、相关cargo test、cargo clippy；本轮仅改规划文档，不把未运行测试当通过。

真实桌面另行明确授权后验证：后台不激活、可见时实际收敛、约束与回声、原生tab边界、动画连续及失败停止。mock不能证明这些原生行为，本轮未运行桌面动作。

### 后续次序与实施门槛

列宽完整切片 -> 其他排列/stack高度意图与同样的事务投影 -> Space偏好/全局激活/实际焦点分离与让权 -> 目标归属及双strip事务、原生归属确认与非幂等Space操作协议。原生生命周期发生于每个阶段，不能推迟首片的列身份和拆分处理。

首片政策已全部定案：普通新列采纳初始逻辑宽度（2026-09-13用户纠正，见问题18）、Space拆分宽度、150ms防抖稳定读回、几何含首次共3次、250ms/1s/5s只读检查及动画中途失败计一次，必须覆盖首片验收。焦点不自动重试也已确认，后续焦点迁移须遵守。参数化测试覆盖协议边界，同时验证用户确定的默认。

## Votes

- migration_architecture：支持首片及次序；动画初次推进即预占预算，中途失败也计一次。
- migration_platform：支持首片与动画失败停流/直接终点纠正；强调现有writer全切换，未明确中途失败预占口径。
- migration_testing：支持首片及次序；中途失败未提交终点不扣终点预算；提醒初始化和现存Space拆分不能跳过。

没有剩余未能表述的问题；动画口径及其他分歧均已在具体票中获得用户裁决。规划路线定稿，地图关闭，实现及原生验收尚未开始。
