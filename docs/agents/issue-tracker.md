# Issue tracker: Local Markdown

本项目 wayfinding 采用本地 Markdown；按 AGENTS.md 的文档位置要求，将技能模板的 .scratch 路径改为 docs/wayfinding/。不发布外部 issue。

## Wayfinding operations

- Map: docs/wayfinding/<effort>/map.md，Label: wayfinder:map。
- 子票: issues/NN-title.md；Title 为一级标题，Type/Label/Status/Assignee/Parent 元数据。Status 为 open、claimed、resolved。
- 依赖: Blocked by: NN, NN；无依赖写 none。Markdown 无原生依赖关系，使用此回退约定。
- Frontier: 开放、未认领、所有 blocker 已 resolved 的子票，按编号排序。
- Claim: 工作前设 Status: claimed 与 Assignee: codex；争议等待用户时设 open、Assignee: none，保留具体待确认项。
- Resolve: ## Answer 记录结论及依据，## Votes 记录三个独立专家的票与差异，Status: resolved；map 的 Decisions so far 只加名称链接与一句摘要。
- 依赖在票创建后的第二遍接线；重新编辑前读取当前文件，保留并发更改。决策只存其票；规格与地图通过名称链接引用。

