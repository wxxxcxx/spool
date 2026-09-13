# 列宽意图、列身份与约束求解怎样表示

Id: 17
Type: grilling
Label: wayfinder:grilling
Status: resolved
Assignee: none
Parent: [Spool 声明式状态模型与迁移边界](../map.md)
Blocked by: 03, 05, 06

## Question

现有Column宽度从窗口frame推导，新模型怎样显式存储列身份、绝对/比例宽度及原始值？ordinal如何接纳时绑定身份？不可见或viewport未知时如何保留比例意图？成员约束如何形成有效布局，交集为空时是否阻塞？

## Answer

表示与求解共同部分三方支持，按用户授权接受。新列默认初始化的差异另票，不伪报一致。

- 每列有运行期稳定ColumnId，重排不变、销毁不复用；结构revision与意图revision分开。ordinal只在接纳事务当前快照中解析为身份，执行不得重解释位置。
- WidthIntent明确为继承配置、绝对逻辑点或可用viewport比例，保存原始变体与值。逻辑宽度采用列布局槽宽，包含约定padding；原生外框约束先转换到相同槽宽口径，布局位置和成员frame共用同一派生宽度，padding只处理一次。
- 比例对该Space对应的最新有效可布局viewport求值；viewport未知仍可接纳合法比例但阻塞派生，不借另一个Space的旧frame代替。绝对值、比例必须正且有限；算术溢出是拒绝/派生失败，不暗中截断意图。
- 对可信独立宽度区间，L为成员最小值上界、U为成员最大值下界；L<=U时取最接近请求的可行值，否则保留阻塞，不自动拆列/float。离散或宽高耦合约束不能假装成宽区间，超出模型能力时明确阻塞。
- 未知能力不贡献虚假硬约束，但在身份与执行资格允许时可有界尝试原目标；一次成功只证明该次结果，失败不外推普遍最小尺寸。暂缺成员保留身份，过期约束不能永久当事实。
- 一列无法生成可信目标时，不向依赖该宽度的后续布局发出半套计划；原生效果本身非原子，必须分别观察部分完成并重新协调。
- native tabs只管理一个独立外框；应用内部tab不成为约束成员或写入目标。现有legacy布局表示可替换，不为旧兼容保留控制权。
- 整列移动保留身份与意图，加入现有列采用目的列语义；拆分产生新身份，其宽度按 [Space 拆分后的子列是否继承宽度意图](11-split-width.md) 的用户裁决处理。首次初始化见 [新列默认宽度来自配置还是接纳时观测](18-column-initialization.md)。

## Votes

- layout_domain：表示与求解支持；默认继承配置，首片不引入一次观测初始化。
- layout_ecs：表示与求解支持；允许无配置时一次观测初始化，记录ObservedInitial来源。
- layout_platform：表示与求解支持；规则优先，其余一次可信观测初始化；指出现有最大成员宽和首成员宽两种来源都须替换。

## Evidence and acceptance

[Column width and projection](../../../../src/ecs/layout.rs)、[resize feedback](../../../../src/ecs/triggers.rs)、[targeted size](../../../../src/commands/targeted.rs)。验证padding转换、不可见比例接纳、成员顺序/暂缺不改变宽意图、约束交集空不改结构、约束变化不写意图revision、ordinal延迟执行不换对象。
