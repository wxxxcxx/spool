# 对齐机制：结局三分类与采纳显示值

Id: 01
Type: task
Label: wayfinder:task
Status: open
Assignee: none
Parent: [运行期状态与实现对账](../map.md)
Blocked by: none

## Question

[ADR 0011](../../../adr/0011-realization-outcomes-and-alignment.md) 决定了结局三分类与对齐语义。本票把它实现出来，**先接在今天就存在的失败源上**（平台拒绝写入、读回在宽限期内始终不匹配），不动准入闸门——那是 02 票。

## 决策沿用（ADR 0011，已确认，不再询问）

- 结局三分类：**实现成功（含受限实现）/ 明确拒绝 / 暂不可判定**。明确拒绝 = 平台回答"这次请求对这个实例无效"，或目标被证实不复存在（确认销毁、实例被替换、Space/列已不存在）。
- 明确拒绝 → **对齐**：读显示的稳定值，写回意图字段；不恢复旧值。
- 采纳前提（缺一不可）：① 该字段仍是这次编辑的最新版本；② 观测新鲜、稳定、非动画中途；③ 不属于"暂不可判定"的成因。缺证据则保持第 3 类（等待），不猜。
- 宽限期 = 既有只读检查序列 250ms / 1s / 5s；到期时窗口已稳定显示不同值 → 第 2 类并对齐；观测不可读或仍在动 → **延长**宽限期，不采纳。
- 逐域换算：浮动帧、声明 Space 恒等；列宽反算回**逻辑槽宽**并**保持变体**（比例仍是比例，按当前 viewport 重算；继承变为 Absolute；viewport 未知则不采纳）；stack 权重按观测高度成比例归一；焦点记忆没有显示对应物，不参与。
- 对齐只改**作者意图字段**，不改派生目标；修正后派生层自然重算。
- 诊断只在内存，有界（与 23/24 号票的 `repairs` 同构），`window inspect --source spool` 可见：字段、原意图值、采纳值、原因/错误码、时间。

## What to build

1. **结局模型**：一处（不是每个 recipe 各写一套）判定某个实现请求的结局；至少区分"平台明确拒绝这次请求"与"暂不可判定"（`kAXErrorCannotComplete`、权限类、`NotImplemented`、读不到、Mission Control、初始化、原生移动在途、窗口暂缺、拓扑未解析、约束无解 → 第 3 类；`IllegalArgument`/`AttributeUnsupported`/`ActionUnsupported`、确认销毁、实例替换、目标 Space/列消失 → 第 2 类；语义含糊的 `kAXErrorFailure` 归第 3 类）。
2. **对齐入口**：给定（字段身份、意图版本、目标值），在采纳前提满足时写入显示值；版本过期则丢弃该失败事件。
3. **宽限期接线**：复用既有 `FRAME_CHECKPOINTS`；到期判定需要"稳定且不等于目标"的观测。
4. **逐域接入**，按此顺序：列宽 → stack 高度 → 浮动帧 → 声明归属（声明归属多为恒等，可复用既有修复路径的判定）。
5. **诊断**：对齐后留下的有界记录（含未对齐的原因），并在 `window inspect --source spool` 暴露。

## 验收矩阵

- 平台明确拒绝写入 → 该字段被对齐为屏幕上的稳定值，并留下一条记录（原值/采纳值/错误码）。
- 同一字段在失败后又被更新编辑改过 → **不对齐**，新编辑自己收敛（state 不被旧失败拉回）。
- 观测不可读、或窗口仍在动画 → **不对齐**，保持等待与未确认诊断。
- 宽限期到期且观测稳定、且不等于派生的有效目标 → 对齐发生。
- 受限实现（约束导致显示的派生目标 ≠ 原始意图，但派生目标已达成）→ **不对齐**（这是成功，不是失败）。
- 列宽对齐：比例意图仍是比例（按当前 viewport 重算）；继承意图变为绝对；viewport 未知时不采纳。
- stack 对齐：按观测高度成比例归一，且被最小高度夹住的项与显示自洽。
- 连续编辑（连按"加 50"）→ 实现器只追最新目标，期间旧请求的失败不触碰状态。
- 导出：对齐后 `state == 显示`（同一字段比较），且诊断记录有界。

## 已实施（2026-09-16，第一增量：机制 + 列宽域）

- **结局模型**（`src/ecs/alignment.rs`）：`refusal_is_definitive(code)` 把 macOS 错误码分为“这次请求无效”（`IllegalArgument`/`AttributeUnsupported`/`ActionUnsupported` → 明确拒绝）与其余“暂不可判定”（`CannotComplete`/权限类/`NotImplemented`/含糊的 `Failure`）。`FrameWriteRefused { code }` 把提交点的拒绝带给拥有 strip 的对账层。
- **对齐记录**：`RealizationAlignments` 资源，每窗口最多 4 条，含字段（目前 `ColumnWidth { column, prior, adopted }`）、原因（`NotRealizedWithinGrace` / `WriteRefused { code }`）与时间；窗口销毁时清理；`window inspect --source spool` 的 window row 新增 `alignment.records`。
- **换算**：`aligned_width_intent` 保持变体——`Absolute`/`InheritConfig` → `Absolute(显示宽)`，`ViewportRatio` → 按当前 viewport 重算比例，viewport 未知或宽 ≤ 0 时**不采纳**。`LayoutStrip::viewport_width()` 为此开放。
- **两条路径**：提交失败且错误码是明确拒绝 → **立即对齐**；回合已 exhausted 且**已经过最后一个只读检查点**（宽限期 = 250ms/1s/5s 全序列）且观测稳定 → 对齐。
- **作者编辑门（本次实施中发现的关键约束）**：只在“这回合绑定的宽度意图版本尚未被证实实现过”时对齐（`WindowStateSync::unrealized_edit`，经 `realized_intent` 跟踪）。否则**外部漂移**与**内部修正**会把 authored 宽度一路拖走——那正是既有测试护住的“有界重试不得变成无界循环”。最初版本没有这道门，四个既有测试立刻失败（多出一次尝试 / 目标被写回）；补门后它们全部恢复原断言。
- **不再无限 blocked**：回合结束后不再每帧重插 `WindowFrameCorrection` 标记（只在回合仍活时插）。

### 本增量验证

- `cargo test --workspace --locked`：主程序 1203 通过 / 2 原有忽略（本次 +13：7 条纯函数单测 + 改写 1 条 + 新增 1 条 + 其余为本票机制）；local-ipc 24、shared-types 96、其余 6/4；`--no-default-features` 1061 通过。
- `cargo fmt --all --check`、`cargo clippy -p spool --all-targets`（默认与 `--no-default-features --locked -- -D warnings`）通过。
- **旧代码上失败的证据**：把两条对齐分支禁用后重跑——`a_constrained_width_edit_aligns_to_what_the_window_shows` 与 `a_refused_width_write_aligns_to_what_the_window_shows_immediately` 均失败于 `left: IVec2(640, 748) right: IVec2(400, 748)`（左＝未实现的编辑目标，右＝屏幕实际值），正是本票要消除的“意图永久覆盖显示”。
- 被反转的既有断言（已按 ADR 0011 改写并注明）：`constrained_resize_animation_preserves_tiled_bounds_intent` → `a_constrained_width_edit_aligns_to_what_the_window_shows`。

## 已实施（2026-09-16，第二增量：stack 高度域）

- **换算**：`aligned_item_weight(others_weight, observed_height, viewport_height)`——投影按 `viewport * w_i / Σw` 分配，所以解出**只有失败项**的权重：`w = S·h/(V−h)`（`S` 为其余项权重和）。只动失败项、其余项保持权重与占比，显示上的分配因此被原样复现；`None`（无余量、高度不可表示、无 viewport）时不采纳。
- **门**：`unrealized_height_edit` 与宽度同构，只是比较意图元组里的**高度修订**（`intent.3`）。
- **接入**：一个窗口的回合结束后，宽度与高度各自按各自的门对齐（`align_unrealized_fields`）；单 item 列（占满视口）不参与。
- **记录**：`AlignedField::StackItemHeight { item, prior, adopted }`，诊断里为 `kind: "stack_item_height"`。
- **测试**：`a_constrained_height_edit_aligns_to_what_the_window_shows`（两个窗口 stack、两端都 constrain、把某项权重改成 4 倍 → 宽限期后权重回到显示对应的值，派生高度等于显示高度，一条记录）。禁用高度分支时该测试失败于 `the authored weight follows the displayed split: 4 vs 1`。

### 本增量验证

- `cargo test --workspace --locked`：主程序 1206 通过（+3：2 条纯函数单测 + 1 条集成）；`--no-default-features` 1064。`fmt` 与两套 `clippy -D warnings` 通过。

## 声明归属域：已经是对齐行为（无需新代码）

查证结论：`DeclaredSpace` 的既有修复路径本来就采纳观测值——被接受的移动先声明目标（`declare_space_membership`），2 秒确认窗口过期后按观测修复（`attempt_unconfirmed`；观测已移到别处则 `membership_changed`）。而**被拒绝的移动根本不会声明目标**（拒绝发生在 `perform_native_space_intent` 之前，声明只在 `Ok`/`fully_reconciled` 路径写），所以“拒绝 → 对齐”在这个域没有对象可对齐。ADR 0011 的语义在此域已成立，只是原因词汇是领域自己的；本票不改，记录在案。

## 尚未实施（本票剩余）

- **浮动帧**：需要先决定一件事（见下）。
- “稳定”判据的进一步强化（目前是“上次观测 == 本次观测”＋已排除动画与手势）。
- `Center` 的 warp 与准入闸门属于 02 票；真实桌面验收未做。

### 已裁决（2026-09-16）：浮动窗口 = 3+2，已实施

用户选择 **3+2**：不给浮动窗口加对账回合（它的帧权威本来就是观测），只把"被明确拒绝的浮动移动"留成一条**纯诊断**。

- 依据（查证）：浮动分支本来就每帧把观测写回意图（`adopt_floating_frame` + `set_floating_frame`，条件 `can_adopt_frame`），所以"失败 → 状态跟随显示"在下一帧自动成立，连宽限期都不需要；而"意图随观测走"意味着**漂移就是意图**，对齐门在此域没有意义。
- 实现：提交点给出的明确拒绝在浮动分支不再被丢弃，而是写成 `FloatingMoveRefused { code, at }`（每窗口一条，仅诊断、不驱动行为）；窗口重新平铺并收敛时清除；`window inspect --source spool` 的 floating 组新增 `refused` 字段。
- **测试**：`a_refused_floating_move_is_kept_as_a_diagnostic`（浮动窗口 + 编码拒绝 → 组件带该 code、屏幕不动、被拒的目标不残留为意图）。去掉诊断写入后该测试失败于 `the refusal is recorded for diagnosis`。
- 验证：workspace 1207 / 24 / 96 / 6 / 4，`--no-default-features` 1065，fmt 与两套 clippy `-D warnings` 通过。

### 当时的三个选项（保留记录）

平铺窗口的对账回合（`FrameConvergence`：attempts/checkpoints/blocked/realized_intent）在浮动窗口上是**被移除**的（`sync.frame_convergence.remove(&entity)`），浮动帧由观测采纳（手势/外部移动）驱动。于是“某个浮动窗口的移动命令失败”今天没有任何记录，对齐门也无从判断。三个选项：

1. 给浮动窗口同样的回合（统一模型，代价是浮动路径多一套状态）；
2. 只给“命令驱动的浮动移动”加一个轻量标记（不引入完整回合）；
3. 认定浮动帧的权威本来就是观测（意图随观测走），因此**不需要**对齐——把手势采纳视为它们唯一的对齐路径。

我倾向 **3**（浮动帧今天的行为已经是“状态跟随显示”，再叠一层对齐会与手势采纳竞争），但这是你的事实判断，票里保持开放。
- 逐域采纳前提里“观测新鲜稳定”的强化：目前用“上一次观测 == 本次观测”＋`can_adopt_frame`（已排除动画/手势/过渡），未做 `settling` 之外的时间长度要求。
- `Center` 的 warp 与准入闸门属于 02 票。
- 未做真实桌面验收。

## 边界

- 不做：准入闸门（02 票）、落盘与恢复线的退役（03 票）、规则字段（04 票）、真实桌面验收、原生约束采集器。
- mock 通过只证明模型与调用协议；"overview 期间写几何是否有害"等原生问题不在本票。

## 验证契约

`cargo fmt --all --check`；`cargo clippy -p spool --all-targets` 与 `--no-default-features --locked -- -D warnings`；`cargo test -p spool`；切片边界跑 `cargo test --workspace --locked`。行为改变处附"在旧代码上失败"的证据。
