# 退役落盘与恢复线，启动按规则重建 state

Id: 03
Type: task
Label: wayfinder:task
Status: open
Assignee: none
Parent: [运行期状态与实现对账](../map.md)
Blocked by: none

## Question

[ADR 0010](../../../adr/0010-runtime-state-rebuilt-from-rules.md) 决定 retained state 只在运行期存在：重启由"规则 → 观测 → 默认"重建，跨会话偏好由**规则**表达，兜底接受丢失。本票删掉落盘与恢复线，并把启动重建接上。

## 决策沿用（ADR 0010，已确认，不再询问）

- state 只在运行期存在；不写盘、不读候选、没有导入缝、没有 `spool session restore`。
- 重建表（规则 / 观测 / 默认）与"接受丢失"清单见 ADR 0010。
- 规则字段可以一张票一张票加；本票先按兜底落地，不新增规则字段。

## What to build

1. 删除：`state.json` 读写（`StatePersistence`、`SpoolState` 的 capture/commit/version、`StateFilePath`）、保存系统（周期保存、退出保存、抓取）、`RestoreCandidates`、`restore.rs` 的绑定与导入缝、`Action::RestoreIntents` 与 wire 类型、`spool session restore` 及其文档、相关检查项与诊断字段。
2. 启动重建：新生窗口/新列按"规则 → 观测 → 默认"建立 retained state（列宽、高度、浮动帧、声明归属、每 Space 焦点），移除所有"候选冻结/导入窗口/基线"概念。
3. 同步文档与票面：`ARCHITECTURE.md`（资源表、§6 意图持久化整段）、`CONFIGURATION.md`（Layout Intent Persistence 段）、`CONTEXT.md`（删除 `Restore Candidate` 术语；`Retained State` 保留）、`README.md` 若有提及；24 号票的保存半、26/28/29 号票整体、25 号票的恢复裁决加带日期反转说明；本仓库地图（`declarative-state/map.md`）指向本图。
4. 删除 27 号票中"编号复用 → 候选绑定"的依赖叙述（编号复用本身保持 `open`，理由改为运行期实例判定）。

## 验收矩阵

- 仓库中不再存在 `state.json` 的读写路径、候选/导入/`session restore` 的任何入口（含 CLI 帮助与文档）。
- 冷启动（无任何磁盘状态）后：规则命中的窗口按规则建立意图；未命中的按观测（窗口自身位置/所在 Space）与默认建立。
- 运行期编辑照常生效；进程结束后无任何磁盘痕迹（除配置与日志）。
- 重启后行为符合重建表：运行时拉宽的列、手调的高度、精确浮动位置、每 Space 焦点记忆**不恢复**（除非规则表达）。
- 文档漂移检查：`docs/**/*.md` 相对链接 0 失效；不存在仍宣称持久化/恢复的段落。
- 既有测试按新现实调整：删除保存/恢复相关测试，新增"重建表"测试。

## 边界

- 不做：规则字段本身（04 票）、对齐机制（01 票）与准入闸门（02 票）——本票与它们相互独立。
- 不迁移旧 `state.json` 数据（ADR 0007：开发期不要求旧格式兼容）。

## 验证契约

同 01 票；本票涉及文档面广，额外做一次全仓 `state.json`/`RestoreCandidates`/`session restore` 引用扫描，确保无残留。
