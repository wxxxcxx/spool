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

## 已实施（2026-09-16）

- **退役**：`state.json` 读写（`StatePersistence`/`StateFilePath`/`SpoolState` 及其 `Saved*` 类型/`INTENT_STATE_VERSION`）、三条保存路径（周期、退出、抓取）与 `IntentCapture`、`src/ecs/restore.rs` 整个模块（候选 + 可信绑定 + 导入缝）、`Action::RestoreIntents` 与全部 wire 类型（含 `FocusRole`）、`spool session restore` 及其文档、诊断里的修订号三字段、`window inspect` 的 intent 抓取。启动不再读任何文件，也不再注入 `StatePersistence`/`RestoreCandidates`。
- **随删除一并消失的辅助 API**（只服务落盘/导入）：`FocusCoordinator::focus_memory`/`import_focus_memory` 与 `FocusMemory`、`DeclaredSpace::declare`、harness 的临时 state 文件、`all_strips` 抓取。
- **启动重建**：即既有行为——列宽由规则 `width` 或"新列一次性采纳接纳宽度"，浮动帧由规则 `floating`/`grid` 或窗口自身位置，声明归属由原生观测，焦点由 macOS 当前焦点/第一个可用窗口，stack 高度默认等分，列序由发现顺序（`index` 可影响）。这次没有新增代码，重建表因此是"确认过的现状"而不是新机制（见 ADR 0010 的表）。
- **文档**：`README.md` 与 `docs/CONFIGURATION.md` 的持久化小节改写为"布局意图与重启"（含重建表与接受丢失清单）；`ARCHITECTURE.md` 的资源表、模块表、§6 意图持久化整段与测试清单同步；`CONTEXT.md` 删除 `Restore Candidate` 术语；24/25/26/28/29 号票各加带日期的反转说明，`declarative-state` 地图与实施记录指向本图。
- **一处顺带修好的既有缺陷**：删除时发现 `admission.rs` 的 `Action::Layout(plan)` 分支丢了 `#[cfg(feature = "lua")]`（被删除脚本的范围吞掉），无默认特性构建会失败——已恢复。
- 验证：`cargo test --workspace --locked` 上主程序 1168 通过 / 2 原有忽略（净减 43 条：随保存与恢复测试整批删除），`--no-default-features` 1026；`fmt` 与两套 clippy `-D warnings` 通过；docs 相对链接 0 失效。

### 顺带修正：对齐门改为"按编辑"而非"按确认历史"

删掉落盘后，stack 高度对齐的既有测试暴露了一个真缺陷：高度门当时比较**列级** `height_revision`，而该修订被列内任意 item 的权重编辑共用，于是**没被编辑的相邻 item 也会对齐**，并且会拿邻居尚未实现的权重当"其余项"来算。改为按**本次 bind 是否带来更新的编辑**判定（`last_bound_intent`，宽度看列宽修订、高度看**该项自己的**修订），并放在回合之外，使回合被丢弃重建（启动默认帧、Space 重分配）也不会把"未编辑"误判成"编辑未实现"。宽度对齐的既有测试与新高度测试都通过；证据：把高度门改回列级修订时 `a_constrained_height_edit_aligns_to_what_the_window_shows` 失败于 `the authored weight follows the displayed split: 4 vs 1`。

## 边界

- 不做：规则字段本身（04 票）、对齐机制（01 票）与准入闸门（02 票）——本票与它们相互独立。
- 不迁移旧 `state.json` 数据（ADR 0007：开发期不要求旧格式兼容）。

## 验证契约

同 01 票；本票涉及文档面广，额外做一次全仓 `state.json`/`RestoreCandidates`/`session restore` 引用扫描，确保无残留。
