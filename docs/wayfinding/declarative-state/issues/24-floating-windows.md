# 浮动窗口的完整迁移切片

Id: 24
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [Spool 声明式状态模型与迁移边界](../map.md)
Blocked by: none

## Question

浮动窗口的几何与参与方式，迁移到声明式模型意味着什么？本票只做只读核对与切片规格整理，不重新表决已有政策；实现前需要用户逐项裁决（见文末）。

已定政策（直接适用，不再询问）：[状态归属与迁移规格](../spec.md)、[意图、焦点记忆与原生事实分别由谁写入](05-state-ownership.md)、[外部操作如何取代旧意图](02-external-policy.md)、[接纳证据](06-evidence.md)、[意图寿命](04-lifecycle.md)、[列宽表示与求解](17-layout-intent.md)、[stack 高度](20-stack-height.md)、[归属的不变量与修复](23-space-membership.md)。

## Answer

### 当前证据（只读核对，基线 `99df0aa`）

1. **浮动几何没有保留意图。** 浮动窗口的编辑（`window center` / `move` / `resize` / `grow width` / `shrink width` / `snap`）都以 `Windows::requested_frame` / `moving_frame` 为基值，即"当前 desired/moving frame"，然后发布 `RepositionMarker` / `ResizeMarker`（[targeted.rs](../../../../src/commands/targeted.rs) 的浮动分支）。意图隐含在"上一次写入"里：没有可重复表达的原始目标，也没有"原始意图 vs 有效目标"的分层。
2. **外部手势对浮动窗口直接覆盖期望。** [window_geometry.rs](../../../../src/ecs/window_geometry.rs) 的浮动分支注释写着"floating windows have no neighbours to disturb, so their ECS projection remains live instead of waiting for the quiet period"：把观测帧**直接**写进 `Position` / `Bounds` / `DesiredWindowFrame` / `PresentedWindowFrame`。同一函数里平铺窗口要过 `explicit_gesture` / `prior_fulfilled` / `settling` 判定（"stable geometry alone does not prove user intent"）。也就是说：**浮动的观测就是期望**，与声明式模型的"意图与观测分离"正好相反。
3. **浮动窗口不进布局投影。** [layout.rs](../../../../src/ecs/layout.rs) 的 `position_layout_windows` 只为有 strip context 的实体建立投影（`strip_contexts.get(&entity) else { continue }`），浮动窗口被整体跳过 —— 没有几何求解、没有约束投影、没有"有效目标"这一步，它们的期望帧只会被写入者设置。
4. **保存不含浮动几何。** [state.rs](../../../../src/ecs/state.rs) 的 `SavedWindow { window_id, pid, bundle_id }` 只有身份；v6 保存的是列宽与 stack 高度意图、stack item 身份与成员提示。浮动窗口的位置与尺寸不进保存。
5. **归属已经迁完**（23 号票）：浮动窗口有 `DeclaredSpace`（观测优先、记忆兜底、修复不写原生、未知不等于缺席）。
6. **焦点侧已基本覆盖**：`FocusCoordinator` 有每 Space 的 `last_floating` 与浮动层导航，聚焦切片按"确认归属"接纳浮动窗口。
7. **可复用的既有设施**：`clamp_origin_to_viewport`、`checked_actual_display_bounds`、`Display::update_geometry` 的新鲜度检查、`exit_restore` 的"限制到可用显示器"逻辑、以及 23 号票确立的"不变量 + 修复（只改状态、不写原生）"模式。
8. **命令面缺可保留的表达**：今天无法表达"把这个浮动窗口放回主屏左上角"之类的目标，只能"以当前帧为基值再挪一点"；因此连续微调、显示器变化后的恢复、重启后的位置都无处安放。

### 状态与接口（若裁决为"保留意图"）

- **新增浮动几何意图**：每 tracked 浮动窗口一份原始 frame（位置 + 尺寸）与 revision，形如列宽/高度的"原始意图"。它属于窗口，不属于 strip。
- **不变量**：尺寸为正，且与某台当前已知显示器有正交集（或明确标记未解析）。
- **修复**（外部事实使其失效时，只改状态、不写原生）：① 夹到最近/原显示器的可用视口 → ② 原显示器不在时迁到主显示器并保持相对位置 → ③ 无法判定时标记未解析，不猜。
- **观测的角色**：外部拖拽/缩放按既有政策**接管**并写回意图（02/06）；`ObservedWindowFrame` 只用于呈现、命中与诊断，不再直接写 `DesiredWindowFrame`。
- **效果层**：浮动窗口的期望帧**由状态派生**（多一步"有效目标"投影），与平铺窗口同构；效果失败/受阻沿用既有有界协调。
- **唯一 writer**：浮动几何领域转移（编辑）与修复（外部事实）。今天的 `window_geometry.rs` 浮动直接写入不再是 writer。

### 实施顺序（若裁决通过）

1. **状态层**：意图 + 不变量 + 修复 + 诊断（声明/有效/观测/修复历史与原因）。不动原生写。
2. **接纳层**：把 `targeted.rs` 的浮动编辑改为写意图；命令语义不变（仍是"相对当前帧挪一点"），但结果落在意图上而不是覆写期望。
3. **效果层**：`position_layout_windows` 为浮动窗口补一步"意图 → 有效目标"投影；`window_geometry.rs` 的浮动分支改为"观测 → 按外部政策写回意图"。
4. **保存与导入**：浮动几何进入新格式，候选隔离，明确不跨重启自动应用。
5. **移除清单**：`window_geometry.rs` 的"浮动直接写 desired"路径；`targeted.rs` 浮动分支的"以当前帧为基值"隐含权威。

### 验收矩阵（草案）

- 外部显示器分辨率/可用区域变化 → 意图保留、有效目标被夹取、诊断可查、零原生写入（除非随后按状态实现）。
- 显示器移除 → 按修复顺序迁移或标记未解析；窗口不消失、不被静默搬走。
- 外部拖拽/缩放 → 意图被观测接管（写回），呈现与命中检测立即可信；连续内部编辑与外部操作交替不产生震荡。
- Space 切换不改动浮动窗口的位置与尺寸（位置是窗口属性）。
- 连续内部微调（十次 `grow width`）后，意图仍是可重复表达的原始值 + 派生有效目标，不是十次覆写的产物。
- 保存 → 候选隔离读取：浮动几何作为候选存在、不自动应用、不与身份自动绑定。
- 平铺行为与既有回归不变（全量绿）。

### 边界

- 不做：跨重启自动恢复（另立恢复地图）、真实桌面验收（需另行授权）、平铺窗口的行为改动、每 Space 一份浮动位置（首片按每窗口一份）、浮动窗口的"自动布局"或吸附到其他窗口。

## 需要用户裁决

1. **浮动几何是保留的意图，还是"观测即状态"？**（倾向：保留意图，与尺寸/排列一致；观测在无明确意图或外部操作时接管。）
2. **粒度**：位置 + 尺寸都保留？每窗口一份（首片）还是每 Space 一份？（倾向：位置+尺寸、每窗口一份。）
3. **修复顺序**：夹到原显示器可用视口 → 原显示器不在则迁到主显示器保持相对位置 → 无法判定标记未解析；是否接受？（倾向：接受，且修复不写原生。）
4. **命令面**：浮动编辑改为写意图（相对当前帧的语义不变）？（倾向：是。）
5. **保存**：浮动几何进新格式、候选隔离、不自动应用？（倾向：进。）
6. **Space 语义**：位置与尺寸是否为窗口属性、不随 Space 变化？（倾向：是。）

## Resolution

用户于 2026-09-15 确认按票中六项倾向执行（保留意图、位置+尺寸、每窗口一份、修复顺序且不写原生、命令写意图、进保存、位置为窗口属性）。裁决记录在此；实现未开始。

## Implementation follow-up

### 已实施：帧权威的横切切换（2026-09-15）

- **新模块 `src/ecs/floating_geometry.rs`**：`FloatingGeometry { frame, anchor, unresolved, repairs }`（修复历史保留最近 4 次）与 `derive_floating_frames`（注册在 `layout.rs` 的 Update 链、紧接 legacy marker 适配器之后）。
- **三个写入者都改为"陈述意图"**：浮动编辑命令（Move / Snap / Center / Maximize / Resize / SetWidth，另经 `TargetedWindow` 的显式目标路径）、外部手势观察（`window_geometry.rs` 的浮动分支）、以及既有的观测采纳路径（`reconcile.rs` 里原 `update_floating_intent`，现改名 `adopt_floating_frame`，并在采纳处写入意图）。第一次被看见的浮动窗口由投影从其当前帧初始化意图。
- **投影即约束派生**：`derive_floating_frames` 从意图派生 `Position` / `Bounds` / `DesiredWindowFrame`（并在需要时插入 `WindowFrameMotion`，让修复可见而不是跳变）；**不写原生**。派生受"投影版本"闸门约束（`revision`/`derived`），因此同一帧内更早发生的观测采纳不会被同一帧内的旧意图覆盖——这是 4 个既有测试逼出来的修正。
- **修复**：`clamped_to_viewport`（所属显示器仍在，帧被夹进其可用视口）、`display_gone`（所属显示器已消失，迁到当前显示器并**保持相对它原来所在显示器的偏移**）、`unresolved`（显示器清单读不出来时保留意图、不派生——未知不是缺席）。
- **意图寿命**：窗口被平铺时意图休眠但保留（`With<Floating>` 过滤），再次浮动时回到保留的帧（有测试）。

### 与票中"移除清单"的偏差（已记录）

没有删除 marker 路径与手势的直接吸附，票中那两条"移除"因此被**取代**：

- `reposition_entity`/`resize_entity` 的 marker 仍是**同一批命令内累积 pending 几何**的机制（`moving_frame` 读 pending；"两次移动累积"是既有语义，撤掉会让 `command_batch_floating_moves_accumulate_pending_geometry` 等测试失败），也是帧动画的入口；
- 外部拖拽的直接吸附必须保留：否则拖动会经动画跟随，手感变差。

权威已经转移：投影在每次意图 revision 变化时从意图重派生帧，所以**意图是保留的目标**，marker/吸附只是效果管道。这一点与列宽片"替换所有 writer"的目标部分一致（权威替换完成，管道保留）。

### 与 issue 03 的一处刻意差异

03 号票的约束模型是"保留原始意图 + 派生有效目标"；本片按已裁决的修复顺序，把**越界夹取记为修复**（`clamped_to_viewport`，意图被改写为夹取后的帧）。若将来更希望保留原始帧、只写有效目标，改动很小：把该分支改成只写派生结果、不动意图。

### 已实施：保存与候选隔离（第 4 步，2026-09-15）

- `SpoolState` 新增 `floating: Vec<SavedFloatingWindow>`（`window_id` / `pid` / `bundle_id` + `frame`），字段带 `#[serde(default)]`：**不升版本**，因此旧文件仍可加载（升版会按"旧格式被忽略"丢弃用户已有的列宽/高度意图），新文件多一个字段。
- 抓取：`SpoolState::extract` 与三条保存路径（变更捕获、周期保存、退出保存）都带上浮动窗口；`window inspect` 的 `capture_current_intent` 同样。
- 候选隔离：浮动帧随 `SpoolState` 进入 `RestoreCandidates`，**没有任何路径自动应用或自动绑定身份**；跨重启恢复本身仍在地图的 Out of scope。
- 测试：往返（序列化→反序列化后帧保留）+ 旧文件（无该字段）仍可加载并得到空列表。
- 文档：`CONFIGURATION.md` 的 Layout Intent Persistence 段落写明保存内容与"字段是新增而非新格式"。

## 原计划（保留作对照）

第一步是**帧权威的横切切换**：浮动窗口的期望帧目前有多个写入者（编辑命令经 `RepositionMarker`/`ResizeMarker`、外部手势直接写 `DesiredWindowFrame`、而 `position_layout_windows` 跳过浮动窗口），迁移要求这些写入者与"意图 → 有效目标"派生在同一次改动内一起切换，否则会互相覆盖；按仓库对列宽片的先例（"所有输入和 writer 同次迁移"），它必须在一次连贯、验证过的改动里落地，而不是分次留下双重权威。第 4 步（保存与候选隔离）可单独成一次改动。
