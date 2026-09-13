# 暂时失联与确认销毁的意图寿命

Id: 04
Type: grilling
Label: wayfinder:grilling
Status: resolved
Assignee: none
Parent: [Spool 声明式状态模型与迁移边界](../map.md)
Blocked by: none

## Question

窗口或 Space 暂时不可观测、确认销毁时，哪些意图保留？

## Answer

三位专家一致，用户明确确认：暂时不可观测保留意图并暂停相关效果，确认仍是同一对象后继续；确认销毁终止对象绑定意图和在途操作，迟到结果不能复活目标。

同应用重开、同标题或同原生 ID 不自动继承旧实例意图；独立配置规则及仍存活列的偏好保留。Space 删除不等于窗口删除，存活窗口按重新确认的原生归属协调。空列寿命、销毁证据与合并次序留待后续票。

## Resolution basis

用户在当前建图讨论中明确确认；详见原任务上下文。相关专家票的共同意见已在 Answer 中标明，范围票包含用户后续修正。

