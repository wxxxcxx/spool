# Space 拆分后的子列是否继承宽度意图

Id: 11
Type: grilling
Label: wayfinder:grilling
Status: resolved
Assignee: none
Parent: [Spool 声明式状态模型与迁移边界](../map.md)
Blocked by: 08

## Question

删除 Space 后，源列成员实际落入不同 Space，非空子列是否复制源列的原始宽度意图？

## Votes

- topology_domain：不继承，旧组专属宽度随旧组身份终止；新组按初始化规则取值。
- topology_platform：继承宽度意图值，但建立新组身份，彼此独立，目的约束只影响有效宽度。
- topology_interaction：继承，拓扑变化不代表用户重设宽度；不复制旧像素结果、约束缓存、在途状态或共享引用。

## Answer

用户在逐项解释后明确同意：拆开后保留原来的宽度，之后各自独立调整。此项由用户裁决，不是专家一致或多数票自动通过。

原列宽度意图为900，拆到两个Space后，各非空子列复制该原始宽度意图并独立演化；修改其中一个不影响另一个。复制原始意图的模式和值，不复制旧受限有效宽度、像素读回、约束缓存或在途请求。各子列仍有独立新身份，由目的上下文重新派生有效目标。

该规则适用于首片运行时已有的Space删除/拆分情形，实施不能推迟到后续归属迁移才处理。三位专家的历史分歧保留在Votes中。
