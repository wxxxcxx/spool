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

用户在逐项解释后明确同意：没有明确设置或匹配规则的新列，默认继承配置宽度。此为用户裁决，不是专家一致；历史投票保留在Votes中。

新列默认建立InheritConfig宽度意图，而不是把应用打开时的窗口宽度采纳为Absolute。明确创建宽度或匹配规则按既定输入政策提供初始值；加入现有列或按已决拆分政策创建子列不重新套用普通新列默认。

仍继承配置的列跟随当前默认，显式调整后的列保留自己的模式和值，即使其数值恰好等于默认也不自动变回继承。清除覆盖后重新继承配置。原生尺寸无法达到默认时只改变约束下有效目标，不将读回写成原始意图。

## Acceptance

配置默认800、应用初始1100的新列意图为继承配置800；默认改900后仍继承的列更新，已显式设800的列保持800；应用最小1000时有效目标受限但继承模式不被覆盖。
