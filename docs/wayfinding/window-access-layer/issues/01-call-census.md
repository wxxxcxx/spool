# 调用普查：把 AX/CG 的失败按类别与应用计数

Id: 01
Type: task
Label: wayfinder:task
Status: claimed
Assignee: none
Parent: [窗口访问层](../map.md)
Blocked by: none

## Question

用户要建"稳定的窗口读写 API"，但第一步要量：失败到底是哪一类、哪个应用、多高的比率。今天最有价值的失败（按钮/几何读失败）在 `Option` 处被压成 `None`，连日志都没有，所以先要一个**只读的调用普查**。

## 已实施（2026-09-17）

- `src/manager/ax_census.rs`：按 `(source, operation, subject, app)` 计数，**成功与失败都记**（于是有失败率），结局分类 `no_value / unsupported / unresponsive / invalidated / not_permitted / illegal_argument / not_implemented / failure / malformed`；每 5 分钟一条 `INFO` 累计汇总，每次调用**结果原样透传**（零行为改变）。
- 接入点：`AXRole`/`AXSubrole`/`AXTitle`/`AXParent`/`AXFullScreen`/`AXPosition`/`AXSize`/`AXCloseButton`/`AXMinimizeButton`/`AXWindows`、`AXPosition.settable`/`AXSize.settable`、`AXPosition`/`AXSize` 写、应用观察者注册、`CGWindowListCopyWindowInfo`（含缺字段）。
- 采集与判读标准写在 [AX 与 CG 调用可靠性](../../../research/ax-reliability-2026-09-17.md)；聚合脚本 `scripts/aggregate-ax-census.sh`。
- 单元测试：错误码分类（含未知码归 `failure`）、上下文标签不伪造应用、结果透传且成功/失败都被计数。
- 验证：`cargo test --workspace --locked` = 1173 / 24 / 96 / 6 / 4；`--no-default-features` = 1031；fmt 与两套 clippy `-D warnings` 通过。

## 待办（数据）

- 用户按采集方案跑一次（需要重启一次守护进程、前台运行并落盘），把最后一次 `ax_census` 汇总贴回来。
- 已知未接入的调用点列在研究的"尚未接入"表里，判读时按未覆盖处理。

## 边界

- 不改变任何判定或写入行为；不加 CLI/线协议出口（若要，另开票）。
- 不记录标题等窗口内容，只记属性名、错误码、窗口号、pid、bundle id。
