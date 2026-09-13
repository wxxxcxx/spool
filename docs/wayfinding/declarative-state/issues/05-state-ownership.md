# 意图、焦点记忆与原生事实分别由谁写入

Id: 05
Type: grilling
Label: wayfinder:grilling
Status: resolved
Assignee: none
Parent: [Spool 声明式状态模型与迁移边界](../map.md)
Blocked by: 01, 02, 03, 04

## Question

确定领域状态归属和唯一写入者：LayoutStrip、每 Space 焦点记忆/偏好、Session 激活意图、实际焦点、实际/目标 Space 归属、几何派生及协调元数据如何分离？配置/命令/脚本/恢复/外部接纳是否共用显式状态转移入口？该设计必须保持 Bevy ECS、主线程平台适配和独立 native 观察。
 
## Answer

三位专家均支持下表。按用户“专家一致直接继续”授权接受；不是用户逐项亲自确认。唯一 writer 指受控领域模块，可以由内部有序 ECS 系统组成，不要求巨型 reducer、巨型 Resource 或复制整个 World。

| 状态 | 归属 | 唯一写入职责 |
|---|---|---|
| 列、栈、标签结构、逻辑尺寸 | 每 Space 的 LayoutStrip | 布局领域转移 |
| 偏好焦点、逻辑导航选择 | 每 Space，包含浮动窗口 | 焦点领域转移 |
| 已确认焦点历史 | 每 Space，any/tiled/floating | 有效焦点观测的接纳流程 |
| 最新全局激活意图 | Session | 激活领域转移 |
| 实际焦点（含 untracked/unknown） | Session 的独立观测状态 | 原生焦点观测接纳器 |
| 目标 Space 归属 | 窗口意图 | 归属领域转移 |
| 实际归属、Space 拓扑与可见性 | 原生观测 | 原生观察接纳器 |
| 有效布局及最终几何目标 | 派生状态 | 布局求解模块 |
| 呈现几何和动画 | 呈现状态 | 呈现模块 |
| 在途尝试、阻塞、重试与完成证据 | 各效果领域 | 协调模块 |

### 不变量与接口

- 配置、命令、脚本、恢复、外部接纳先转换为显式领域转移，共用校验和编辑语义；原生通知不能直接当意图。
- 窗口目标归属为单一权威；strip 中的排列成员是其结构索引，二者与源/目的布局修改在同一领域事务中验证并提交。浮动窗口也有目标归属，但不进入 tiled strip。
- 目标布局与真实归属允许不同。呈现、导航、命中检测必须使用当前已确认适用条件，不能提前把目标归属显示为已实现。
- 连续导航从最新已接纳的逻辑选择计算；确认历史只在原生确认后推进。后台 Space 偏好不产生全局激活请求；多显示器也只有一个实际键盘焦点。
- 接口形状为“接纳领域转移 -> 接纳版本或拒绝”“派生有效布局”“协调当前意图/观测”；最终 Rust 命名属于实现细节。通过受限 SystemParam/领域访问器控制写权限，跨 Space 编辑先全量验证再提交。
- 保留主线程平台 adapter 和 Lua worker 的 Send 数据/查询 round-trip，不能为了统一入口运行任意 Lua handler 于主线程。
- 现有 DesiredWindowFrame 是最终几何派生，不是所有原始意图的容器。实现时可明确替换/改名为有效几何目标，原始列宽保存在领域意图；当前文档不得声称源码已迁移。

### 验收

后台焦点偏好编辑不聚焦/切 Space；移动请求接纳后可同时检查新目标与旧实际归属；连续导航不等待原生确认但实际焦点不乐观推进；一次跨 Space 编辑不出现双重布局所有权。

## Votes

- ownership_domain：六项赞成；确认历史与逻辑导航分离，目标归属与排列事务一致。
- ownership_ecs：六项赞成；Session 是作用域而非巨型可写资源，唯一 writer 是模块责任。
- ownership_platform：六项赞成；原生 membership 不提前修改，多屏不制造多个真实焦点。另提出 Space create/delete 非幂等操作的处理，尚需另票投票，未包含在本票共识中。

## Evidence

- [LayoutStrip](../../../../src/ecs/layout.rs)
- [FocusCoordinator](../../../../src/ecs/focus.rs)
- [Native Space](../../../../src/ecs/native_space.rs)
- [Geometry pipeline](../../../../src/ecs/window_frame.rs)
- [Lua worker](../../../../src/lua/worker.rs)
