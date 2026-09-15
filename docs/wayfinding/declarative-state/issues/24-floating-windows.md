# 浮动窗口的完整迁移切片

Id: 24
Type: task
Label: wayfinder:task
Status: open
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

待用户逐项裁决后填写；裁决前不实施。

## Implementation follow-up

未实施。
