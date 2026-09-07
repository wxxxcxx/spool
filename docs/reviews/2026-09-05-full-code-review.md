# Spool 全量 Code Review 与重构建议

日期：2026-09-05。审查基线：`495c2ffd421b4224961e9bc24bded70cf0e0d0fd`。

## 范围与结论边界

- 用户明确选择当前代码全量审查，不使用历史提交 diff。Standards 与 Spec 由独立审查者检查，再由主审查者核实关键发现和补充测试；两轴分别报告，不合并评分。
- 深入检查：显示器事件、Display/Native Space 拓扑、窗口 membership、布局坐标、Desired/Presented/Observed、普通提交与重试、启动恢复及相关测试。
- 辅助检查：Lua worker/查询缓存/热重载、Bar 状态与生命周期、IPC、持久化。辅助检查深度低于窗口管理主路径，不能据此宣称这些模块没有其他缺陷。
- 规范来源：`AGENTS.md`、`docs/ARCHITECTURE.md`、`docs/CONTEXT.md`、ADR 0001；功能契约补充 `docs/SCRIPTING.md`、`docs/CONFIGURATION.md`、`docs/BAR.md`。没有提供单一 issue/PRD，缺少 `docs/agents/issue-tracker.md`；本次不擅自初始化 issue tracker，按当前仓库契约审查。
- 原工作区生产代码与既有测试未修改。审查期间出现的未跟踪 `docs/research/` 不属于本次工作，未改动。回归探针位于独立的 `/tmp/spool-review-495c2ff` 源码快照。
- 以下 mock 重现证明具体代码路径存在缺陷，不等于已确认用户真实桌面上每一次错位都由它们引起。未重启 daemon、操作用户窗口或进行实体显示器插拔。

## Standards

保留该轴审查者的发现顺序。S1-S5 是规范/合约偏离，S6 是设计判断，不是工具可强制的 lint 违规。

### S1 [P2] 后台 AX discovery 违反仓库线程约束

规范：`docs/ARCHITECTURE.md:105` 要求与 Accessibility 的交互发生在主线程。

`src/ecs/systems.rs:283` 将 `bruteforce_windows` 放入 `AsyncComputeTaskPool`；`src/manager.rs:1151` 在该任务内创建 AX element，随后构造窗口包装对象并返回到 ECS。这与当前声明的线程封闭模型不一致。尚未证明它导致真实崩溃，不能把它当成已复现的插拔根因。

建议：明确平台 Adapter 的线程契约。若保留现有主线程规范，采用主线程拥有、每 tick 有工作预算的增量 discovery Interface；不要把整段同步暴力扫描直接移到事件泵中。若后台 AX 探测是有意例外，先验证例外范围、跨线程对象寿命并更新规范，而不是继续依靠隐含约定。

### S2 [P1] 普通窗口几何存在竞争写入者

规范：`docs/ARCHITECTURE.md:107-108` / ADR 0001 声明单向投影，普通提交只消费 Presented。

`src/ecs.rs:247` 把位置 verifier 放在普通 commit 之后；`src/ecs/systems.rs:1456` 却直接写 Desired 的 origin。正常 retile 在 `src/ecs/triggers.rs:1080` 安装该 verifier，因此这不是仅限退出恢复的特殊写入。另有 defaults 直接写 frame（`src/ecs/triggers.rs:1646`）。

**已用 mock 重现：**同一 PostUpdate 中，Presented 的 X 为 `36`，实际窗口被 verifier 写到最终 X=`200`。下一帧动画仍从 Presented 继续，具备跳变/回拉条件。

建议：defaults、verifier、drift recovery 只提交 intent/retry 请求，由一个 frame-commit Module 执行。特殊 owner 必须显式交接。`src/ecs/reconcile.rs:167,802` 的直接有界重试及读回覆盖 Presented 也应统一，但本次不把该重试本身另算一个已重现行为缺陷。

### S3 [P2] Display 的真实观测绕过平台 Interface

规范：`AGENTS.md:18` 要求 ECS 通过 manager/platform 包装访问 macOS。

`src/ecs/display.rs:422-450` 在 ECS observer 内直接读取 `NSScreen`、safe area、visible frame。主线程保护存在，但 mock manager 无法替换这一观测过程；当前测试通过插入 DockPosition 等方式模拟结果，没有覆盖真实观测转换。

建议：在平台 Adapter 中产生纯数据 `DisplayProperties`，ECS 仅应用它。测试应覆盖带偏移、负坐标、菜单栏和 Dock 的输入，而不直接跳到最终 marker。

### S4 [P2] 旧 Lua callback 可以覆盖新 runtime 的 handler 状态

契约：`docs/SCRIPTING.md:112-118` 描述成功热重载替换当前配置/runtime。

`src/lua/worker.rs:443-447` 发布新 runtime 的 handler 状态，但 reload 前已经挂起的 task 仍持有旧 runtime；它结束时 `src/lua/worker.rs:478-479` 再次向共享 `has_handlers` 写入旧值。旧脚本只有 bind、新脚本新增 on handler 时，旧 bind 的迟到完成可把新值改回 false。`src/lua.rs:90` 会据此停止转发事件。

这是已核实的静态执行路径，本次未新增并运行 Lua 重现测试。

建议：用 runtime generation 限制状态发布；旧 task 可以按明确策略完成自己的动作，但不能发布新 runtime 的能力状态。

### S5 [P2] Lua 查询缓存的寿命由所有重叠 callback 决定

契约：`docs/SCRIPTING.md:206` 表述为按 callback 按需获取状态；`docs/CONTEXT.md` 定义 State Snapshot 为当前状态的时点投影。

`src/lua/world.rs:138` 直接返回已有缓存，直到 `src/lua/world.rs:272` 的全局 `in_flight` 归零才清除。callback A 持有一次读取并持续挂起时，之后其他帧的 callback B/C 仍可能拿到 A 的旧布局；连续重叠任务可以让缓存长期不失效。reload 也共享同一个 DispatchWorld。

这是静态缓存寿命缺陷，本次未运行专门 Lua 重现。单次 callback 内快照一致性本身不是问题，问题是新 callback 意外继承旧批次。

建议：明确 batch/callback snapshot generation。只合并同代在途读取，不把“所有 callback 都结束”当成帧边界。

### S6 [P2，设计判断] 拓扑策略重复且修复动作不一致

属于 Duplicated Code / Shotgun Surgery 判断，不是仅因文件长而建议拆分。

`src/ecs/display.rs:356` 与 `src/ecs/native_space.rs:400` 都会创建或重新挂接 LayoutStrip：前者处理 Timeout，后者处理 Position、可见与激活状态；`src/ecs/workspace.rs:1014` 还存在第三条 orphan 修复路径。一次拓扑变化的知识分布在多个 Module，且每条路径更新的字段不同。

建议：一个 topology reconciliation Module 独占 strip 创建、重挂接、失效及恢复策略；各 ECS system 消费同代结果，不分别重新读取 OS 再决定 ownership。

## Spec

以下六项均有新增 mock 回归重现。先保留独立 Spec 审查的发现顺序，再列主审查者的显示器专项补充。S2 的 verifier 重现计入 Standards，不在此重复计数。

### B1 [P1] 短暂缺失的 Space 回来后仍被冻结

需求：`docs/ARCHITECTURE.md:106`，macOS 是 topology/membership 的事实来源；当前观测恢复后应能够恢复投影。

路径：`src/ecs/workspace.rs:438` 把一次 Space 缺失认定为待销毁，冻结窗口；随后 ID 再次出现时，`src/ecs/native_space.rs:429` 跳过 pending strip，`src/ecs/workspace.rs:604` 又把该 Space 排除在 membership 恢复候选外。不存在“撤销误判”的路径。

重现：不发 SpaceDestroyed，短暂从拓扑中移除一个非当前 Space，再恢复原 ID 和 membership；运行多次 heartbeat 后，`retained=true, tombstoned=true, frozen=true`。

修复方向：区分 `SuspectedMissing` 与已确认 destruction；恢复原 ID 时可取消失效、保留列结构并解除 geometry suspension。显式销毁事件与暂时缺失观测不应无条件进入同一不可逆状态。

### B2 [P1] 原生跨 Space 移动尚未确认时仍写源显示器坐标

需求：`docs/ARCHITECTURE.md:108` 明确要求 Space reassignment 暂停或取代普通几何写入。

路径：`src/ecs/native_space.rs:231-241` 先提交 native move，只记录 PendingMove，没有取得窗口 geometry ownership 或暂停 motion。命令在 PreUpdate 处理，普通 commit 在 PostUpdate，membership transaction 在 Last 才确认（`src/ecs/workspace.rs:87-93`）。

重现：窗口带未结束动画，native move 已被接受，模拟 OS 把窗口搬到目标显示器。确认之前运行真实 PostUpdate，窗口从 `(1024,20)` 被写回 `(0,20)`，frame write 次数 `1 -> 2`。

修复方向：提交前为全部成员取得 transaction generation 和暂停令牌，确认目标 membership 后重新计算 target 再恢复写入。失败、超时、成员关闭、目标消失都要明确释放/重新观测；不能只在成功路径删除 PendingMove。

### B3 [P2] 启动恢复用保存的 Space 覆盖真实 membership

需求：`docs/ARCHITECTURE.md:106` 的 split ownership，保存状态只能恢复 Spool 布局，不能凭空宣布原生 membership 已改变。

路径：`src/ecs/restore.rs:186` 直接使用 saved Space；`src/ecs/restore.rs:426-439` 校验 Space 是否存在及 display 映射，却没有按当前每个窗口的 membership 过滤。`finish_setup` 虽做一次真实 membership 对齐，但在其结束时才触发 RestoreWindowState（`src/ecs/systems.rs:384-385`），恢复又会覆盖刚对齐的结果。平时 detect_moved_windows 依赖新激活 Space，未覆盖长期 inactive 目的地。

重现：保存的 Space 为 `2`，窗口实际处于仍存在但 inactive 的 Space `3`。启动并运行 50 帧，OS membership 仍为 `3`，LayoutStrip owner 却为 `[2]`。

修复方向：restore 复用实时 membership projection；只在确认的当前 Space 内恢复顺序/列结构。若将来需要恢复原生 Space 归属，应作为显式、可失败的 OS transaction，不能直接改 layout 宣布成功。

### B4 [P1] 显示器 ID 不变、原点改变时布局仍使用旧全局坐标

需求：每显示器独立布局（`README.md:22-24`）及 `docs/CONTEXT.md` 的 Desired Window Frame 定义。

路径：`src/ecs/display.rs:344` 更新 Display bounds，但 `src/ecs/display.rs:369` 只在 parent 不同时修复 strip；`src/ecs/native_space.rs:444` 同样如此。`src/ecs/layout.rs:207-215` 只标记 strip 改变。纯 origin 平移、尺寸不变时，relative_positions 没变，Bounds/LayoutPosition 不会因此触发新的全局坐标投影；strip Position 仍在旧位置。

重现：同一个 display ID 从 `y=0` 移至 `y=-768`，同时模拟 OS 已平移窗口。消耗布局帧并使五秒 refresh deadline 到期后，Desired 与实际 Y 都为 `20`，应为 `-748`。旧坐标被 drift retry 重新写入。

修复方向：把 display geometry revision 与 parent/membership 变化分开；origin、尺寸、usable viewport 任一改变，都必须重新求解全局 frame，并取消旧 geometry generation 的写入。测试应包含只改 X、只改 Y、主屏切换及负坐标。

### B5 [P1] 一块屏幕的 Space 查询失败会被当作拔屏，且不会自动补回

需求：`docs/ARCHITECTURE.md:106` 的 OS-authoritative reconciliation。查询失败不是物理 removal 证据。

路径：`src/manager.rs:612-619` 用 filter_map 丢弃 Space 查询失败的显示器；`src/ecs/display.rs:175` 只保护“结果全空”，非空但不完整的结果进入 `src/ecs/display.rs:210-215`，删除缺失的 Display。扫描恢复后，native-space heartbeat 只遍历已有 ECS displays（`src/ecs/native_space.rs:545`），而 display reconciliation 没有普通 heartbeat，只由列出的 OS 事件触发。orphan 修复也要求目标 Display entity 已存在。

重现：双屏，仅一次副屏 topology query 失败；之后所有 query 都成功，不再提供新的 OS 配置事件。运行 50 帧后 Display entity 仍只有 `1` 个，预期 `2` 个。

修复方向：Display inventory 和 per-display Space read 分开表达，保留成功/失败/完整性及 generation。失败时保留最后可信 projection，dirty topology 持续重试，不能把错误压扁成 Vec 中的缺席。

### B6 [P2] 唤醒刷新把合法 floating 窗口强行移到左上角

需求：`docs/CONTEXT.md` 定义 Floating Window 不占平铺 strip；显示器恢复应保留仍合法的用户几何，而非无条件重置位置。

路径：`src/ecs/display.rs:241-243` 在唤醒后安排 RefreshWindowSizes；`src/ecs/workspace.rs:1126-1134` 将该 Space 内所有 floating 窗口 reposition 到 viewport.min，没有检查窗口是否已经可见。

重现：一个完整位于屏幕内、400x300 的 floating 窗口在 `(220,160)`；相同拓扑唤醒，令五秒 refresh deadline 到期后，窗口被移至 `(0,20)`。多个 floating 窗口会被安排到同一原点。

修复方向：优先保留合法 observed frame；只有不可访问时才按目标 viewport 做最小位移修正，尺寸约束与位置修复分别处理。

## 验证记录

原工作区：

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过 |
| `cargo check -p spool --no-default-features --locked` | 通过 |
| `cargo test --workspace --locked -- --test-threads=1` | 沙箱内 6 个 socket/IPC 测试被 EPERM 阻止；获准沙箱外重跑后通过 |
| 测试计数 | 主程序 494 passed / 2 ignored；local_ipc 11、lua 6、shared_types 47 passed；合计 558 passed / 2 ignored |

独立快照未修改 production implementation。仅在以下测试文件加入 probe：

- `/tmp/spool-review-495c2ff/src/tests/display.rs`
- `/tmp/spool-review-495c2ff/src/tests/interaction.rs`
- `/tmp/spool-review-495c2ff/src/tests/session_restore.rs`

运行目录 `/tmp/spool-review-495c2ff`：

```sh
cargo test -p spool --locked review_ -- --test-threads=1 --skip bar::
```

结果：**7 failed / 0 passed**，实际测试执行约 0.55 秒，不含编译。`--skip bar::` 用于排除名称含 `preview_` 的已有 Bar 测试。七个探针在完整集合中重复运行后仍全部失败，失败断言与上述症状一致；没有修改实现来制造失败。

| Probe | 对应发现 | 断言结果 |
| --- | --- | --- |
| `review_position_verifier_does_not_overwrite_the_presented_frame` | S2 | 实际 X=200，Presented X=36 |
| `review_resurrected_space_clears_omission_tombstone_and_window_freeze` | B1 | 拓扑恢复但 tombstone/freeze 仍在 |
| `review_native_space_move_suspends_source_frame_before_membership_confirmation` | B2 | 目标 X=1024 被源 X=0 覆盖 |
| `review_restore_obeys_current_inactive_space_membership` | B3 | OS=3，layout owner=[2] |
| `review_same_display_new_origin_rebases_window_frames` | B4 | Desired/actual Y=(20,20)，预期=(-748,-748) |
| `review_partial_display_scan_recovers_without_another_os_event` | B5 | Display 数=1，预期=2 |
| `review_wake_preserves_valid_floating_window_position` | B6 | (220,160) 被改为 (0,20) |

完整输出：`/tmp/spool-review-495c2ff/probes.log`。随报告保存的 `2026-09-05-repro-sources.tar.gz` 仅含三个测试源文件与日志，不含 production 代码改动或构建产物。

测试边界：display partial-scan probe 单独运行 production reconcile system，以保证一次错误注入落在指定消费者；随后用真实 harness schedules 观察恢复。move/verifier probe 构造可达的待提交状态并执行真实 PostUpdate。五秒 RefreshWindowSizes 使用 Instant，不随 harness 的 Bevy 虚拟时间走，因此相关 probe 显式将该 deadline 设为过去；没有等待或操作真实桌面。

## 重构方案

不建议重写 Bevy、不建议先按文件大小机械拆分，也不建议仅增加更多 timeout。现有 Desired/Presented/Observed 分层方向可以保留，重点是让实际写入路径遵守它。

### 阶段一：先保留失败证据，修复最高风险路径

先把七个 probe 规范化为正式回归测试；补同 ID 仅 X 平移、三屏排列、短暂缺失后回归、移动中目标屏幕消失等组合。每个修复单独提交，先确认对应测试由红变绿，再跑已有套件。

最先处理 B4、B5：它们与多显示器坐标错乱直接相关。随后 B1/B2 处理拓扑误判和写入竞争，再处理 B3/B6 与 S2。这个实施依赖顺序不改变两个审查轴的评分或原报告顺序。

### 阶段二：统一 topology observation 与 reconciliation

建立一个有明确 Interface 的 `TopologyReconciler` Module，而不是增加一个只转发现有函数的中间层：

- 输入为一次收集的 `TopologyObservation`：物理 displays、每显示器 Space 读取结果、visible Space、window memberships、geometry revision、完整性和 generation。
- 平台 Adapter 负责读取并报告错误；mock Adapter 可编程返回缺失、乱序、重复和延迟观测。
- 明确区分 `Observed`、`Unavailable`、`SuspectedMissing`、`ConfirmedRemoved`。这四类不是相同的空集合。
- 输出纯 `ReconcilePlan`：spawn/reparent/rebase/suspend/resume/retire；一个 apply 阶段更新 ECS。display.rs、native_space.rs、workspace.rs 不再各自决定 strip 生命周期。
- 同一轮的 layout、Bar、query 与持久化使用同代可信快照。记录每次变更的原因，重试 dirty observation 直到恢复，不依赖新 OS 通知碰巧到达。

验收：一个 Space 恰好一个 strip；暂时不可观测不破坏列顺序；删除需有证据；原点变化必然重新生成 Desired；同一观测重复应用无附加 mutation。

### 阶段三：统一 membership transaction 与 frame writer

将恢复、显式移动、系统迁移共同使用的 membership 决策归入一个 Module。`NativeMoveTransaction` 保存窗口身份、源/目标 Space、generation、期限和暂停令牌；列移动保留完整成员结构。

几何输出固定为：

```text
Confirmed topology + Layout State -> Desired
Desired + animation state -> Presented
Presented + valid ownership/generation -> platform commit -> Observed
Observed drift -> retry request, not a second frame writer
```

建立消费 `FrameIntent`、返回 `CommitOutcome` 的 `FrameCommitter` Interface。普通 defaults、verification 和 reconciliation 都向它提交请求，保留已有重试预算、cooldown 与应用尺寸约束。全屏、Space reassignment、外部手势及退出恢复通过显式 ownership 转移处理，而非各自维护一套不一致的过滤条件。

把 strip 的“显示器全局原点”与“用户滚动 offset”分开存储。DisplayGeometry revision 改变时，从局部 layout 加新 origin 求解全部相关 frame，不要求 Bounds 或相对 LayoutPosition 恰好发生变化。

验收：过期 topology/transaction generation 的 frame 绝不提交；Presented 是唯一普通写入输入；重试不把 Observed 反写为逻辑布局；失败/超时后 owner 能确定释放或重新收敛。

### 阶段四：统一时钟、补回放与 Lua generation

- 将影响行为的 Instant deadline 纳入可注入时钟，统一测试时间推进；保留真实运行时单调时钟语义。
- 在现有 TestHarness 上增加事件 trace replay：原生事实与通知独立注入，不需要另建测试框架。覆盖同帧顺序变化、多帧延迟、部分查询失败和通知丢失。
- 对实际平台保留必要观测字段：display/Space ID、topology revision、frame owner/generation、Desired/Presented/Observed、失败原因。避免日志记录窗口标题等无关敏感信息。
- Lua runtime publication 与 query batches 各自显式 generation，补旧 callback 跨 reload、跨帧重叠 callback 的回归测试。
- 最后进行获授权的真实桌面验收：双/三屏插拔、主屏改变、睡眠唤醒、全屏退出、动画中迁移。mock 通过是必要条件，不是最终验收。

## 汇总

Standards：6 项（其中 1 项设计判断），该轴最严重问题为 S2 普通几何竞争写入，已重现。Spec：6 项，B1/B2/B4/B5 均为 P1；六项均有 mock 重现。两轴不合并总分，不把静态 Lua/线程风险宣称为用户桌面故障的已确认根因。
