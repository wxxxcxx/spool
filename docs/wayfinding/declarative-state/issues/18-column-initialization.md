# 新列默认宽度来自配置还是接纳时观测

Id: 18
Type: grilling
Label: wayfinder:grilling
Status: resolved
Assignee: none
Parent: [Spool 声明式状态模型与迁移边界](../map.md)
Blocked by: 17

## Question

新列没有显式创建宽度或匹配规则时，继承当前配置默认，还是一次性采纳接纳时的可信逻辑宽度为Absolute？

## Votes

- layout_domain：默认InheritConfig；不在首片引入观测初始化，避免隐含Observed到Intent。
- layout_ecs：允许一次观测初始化并明确ObservedInitial来源，此后不再随观察变化。
- layout_platform：明确规则优先，其余可一次采纳可信初始逻辑宽度。

## Answer

2026-09-13用户根据实际启动行为明确纠正：窗口出现时多宽，就给它创建多宽的新列。本裁决覆盖此前“普通新列继承配置默认”的记录；上方历史投票不代表当前政策。

没有明确创建宽度或匹配宽度规则的普通新列，一次性采纳接纳时窗口的有效逻辑宽度为Absolute。逻辑宽度包含窗口水平padding，因此投影后实际窗口不会因padding缩窄。初始化保持revision 0，后续观测不持续改写意图。

显式宽度规则优先，仍使用InheritConfig跟随匹配规则；明确命令、恢复绑定、加入已有列和拆分继承不重新套用普通新列初始化。配置预设是手动调整的档位以及显式继承的默认值，不再自动将普通新列缩到首个档位。清除覆盖后可以显式回到配置继承。

## Acceptance

- 配置首个档位256、窗口初始900：启动后列与实际窗口保持900。
- 运行中新出现700宽窗口：新列保持700；重载默认档位不改写已有Absolute。
- 匹配宽度规则0.75：规则仍优先；拆列复制来源意图，不重新读回初始化。
- 最小宽度约束只影响有效目标，不把受限读回写成原始意图。
