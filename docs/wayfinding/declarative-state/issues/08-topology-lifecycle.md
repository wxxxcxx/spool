# 空列清理、Space 删除与窗口再归属如何保留布局

Id: 08
Type: grilling
Label: wayfinder:grilling
Status: resolved
Assignee: none
Parent: [Spool 声明式状态模型与迁移边界](../map.md)
Blocked by: 04

## Question

最后一个窗口销毁后是否删除空列？Space 删除导致存活窗口重归属时，源分组/顺序/宽度如何与目的布局合并？何种生命周期证据允许清理，如何隔离原生 ID 重用与迟到事件？

## Answer

共同部分获三位专家支持，按用户授权采用。拆分组宽度的分歧已拆为独立子票，不在本票宣布一致。

- 最后成员确认销毁后删除空列；暂缺、隐藏、最小化或归属待确认不触发空列清理。独立规则保留；不默认建立持久空槽。
- Space 删除后先确认存活窗口真实目的归属。目标既有相对顺序不动，源组按原布局顺序追加；完整落到同一目的地的组保留可表达结构和逻辑宽度。
- 分裂的非空成员投影保持组内相对顺序和仍可表达的结构，以新身份插入。宽度取值见 [Space 拆分后的子列是否继承宽度意图](11-split-width.md)。
- 同一批次多源合并使用删除前确认的 Space 次序和源列次序，不使用通知到达或 HashMap 遍历顺序；重复观测不得重复插入。
- 归属通知分批到达时保留可重算的暂存计划，不能过早把完整组当拆分组；身份未知或归属重叠时保持待定。
- 窗口退休依据是可靠绑定 incarnation 的销毁证据，或覆盖相应对象且完整有效的 AX/WindowServer 身份审计。单一整数 ID、读取失败、缺失次数、超时不能代替证据。Space 退休使用完整拓扑和过渡确认，不能以窗口清单代替。
- 对象销毁使旧焦点目标失效，先接纳 OS 新确认焦点；新的明确聚焦命令仍可成为新意图，不按旧布局邻居自动抢焦点。

## Votes

- topology_domain：共同规则支持；拆分子列不继承源专属宽度。
- topology_platform：共同规则支持；拆分子列复制原始宽度意图，各自独立。
- topology_interaction：共同规则支持；拆分子列复制原始宽度意图，各自独立。

## Evidence

[Lifecycle audit](../../../../src/ecs/reconcile.rs)、[incarnation destruction](../../../../src/ecs/triggers.rs)、[membership evidence](../../../../src/ecs/topology.rs)、[layout transfer](../../../../src/ecs/workspace.rs)。现有代码的框架是证据入口，不等于新原始宽度模型已经实现。
