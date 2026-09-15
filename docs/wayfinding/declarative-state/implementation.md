# 声明式列宽、排列/stack高度与焦点实施记录

日期：2026-09-13。用户在规划地图的19项决策完成后授权实现。

## 已实施范围

已落实地图中第10票定义的两个切片。**列宽首片**：后台 Space 列宽编辑从接纳、派生、动画、原生协调，到诊断和原始意图保存。**排列/stack高度片**：同 Space 排列与 stack 高度意图从接纳、投影、效果版本门、外部几何接纳，到 Lua 快照、诊断和 v6 保存。后续焦点激活切片见本文末；目标Space归属事务仍是下一阶段，不把这些切片称为整个窗口管理器已完成声明式迁移。

### 列宽首片

- `ColumnId` 与 `WidthIntent` 明确列身份及原始宽度：继承配置、逻辑点、viewport比例。结构版本与意图版本分开；同值写入不制造新版本。整列移动保留身份，拆列复制原始意图并分配独立身份。
- 配置、规则、初始化、命令、Lua、结构编辑和外部接纳统一写列意图。删除 `WidthRatio` 和从成员 frame 决定列宽的旧旁路。明确宽度规则优先；普通新列一次性采纳接纳时逻辑宽度为Absolute，配置预设不再覆盖窗口初始宽度（用户2026-09-13纠正，见问题18）。
- 后台或暂时不可观测的保留窗口可接纳列宽；真正的身份失效与原生归属事务仍拒绝不安全编辑。命令成功表示接纳，原生执行另行检查资格。
- 纯求解器在相同逻辑槽宽口径求约束交集；原始800可派生有效1000，保存仍800。viewport未知或约束冲突时阻塞相关整条投影，不猜测其他Space尺寸，不输出依赖未知宽度的半套目标。
- `FrameTransition` 封装连续重定向、插值和有限结束。Desired、Presented、Observed 保持不同职责与独立ECS变更检测。正常动画整段只占一次语义尝试；中途失败停流，后续尝试直达最新终点，含首次最多3次，时间推进不续费。只读检查按250ms、1s、5s执行。
- 外部几何按窗口150ms尾部防抖，新事件延后采样；静默后重新读取。无手势变化仅在先前目标已满足、无约束未满足且相关意图/派生上下文仍有效时接纳。旧候选不能覆盖更新后的命令或配置。
- Lua快照保留窗口实例、列身份和意图版本；同一同步计划内部结构变化会刷新自身绑定，过期外部结果不能误改替代列。查询的 `width_ratio` 来自原始意图，绝对宽度在viewport未知时为null，浮动转平铺的纯预测也不伪造配置比例。
- v6保存原始列意图，接纳、保存、dirty分别报告；单调版本防止旧快照退盘，写入失败保留dirty，临时文件同步后原子发布。读取只生成隔离候选；纯导入要求明确可信映射和冻结初始基线，不能覆盖本会话后续编辑。
- 删除旧自动恢复writer、旧恢复配置和快照迁移命令。退出时恢复本次启动前窗口位置是独立功能，继续保留；其显示器就绪依赖现在显式位于启动快照模块。

### 排列/stack高度片

- 高度意图属于稳定 `StackItemId`，与独立窗口或native-tab外框组一一对应；每个item持一份正有限原始权重，默认1，不从任何frame初始化。1:1为等份，2:1为相对份额。
- 同项移动、重排、tab代表改变、暂缺保持身份与权重。真正拆分分配新身份并复制原意图；合并采用明确目标项语义。`sync_height_items` 按cohort成员重新关联元数据，拆分与复用都得到独立身份。
- 纯派生按权重分配，沿用既有stack最低200逻辑点的布局政策并确定性补齐整数余数；这不是原生最小尺寸证据。单项填满viewport且不受stack最低高度限制。容纳不下所有参与项时阻塞依赖投影，不删成员、不反写权重。
- 投影区分“保留项”与“参与项”：`StackItemId` 层面新增 `StackProjectionParticipant` 概念，只有当前可用且未被ordered-out的成员所在item参与投影、占用高度、参与最低高度计算，并可作为外部编辑的donor或recipient。保留但未参与的item保身份与原始权重，重新可用时恢复其份额。
- Grow/Shrink保留配置的逻辑点步长，从原始归一化份额和已知viewport计算；未知viewport拒绝需要计算的相对编辑，等分与显式权重不需要viewport。
- `adopt_height_for` / `adopt_height_from_previous_for` / `resize_height_for` 只在参与cohort内编辑：被排除item保持精确原始权重，被编辑cohort保持精确权重单位总和。权重单位按 `share * units` 精确重算，消除了旧实现中 `share * scaled_total * maximum` 在cohort最大值不为1时把单位总数翻倍的缺陷。
- 外部几何接纳按参与cohort定位目标与前一个参与item：下边缘拖动只改目标份额，上边缘拖动由前一个**参与**item单独让出，绝不由夹在中间但已ordered-out的保留item让出。
- 同Space排列、stack/unstack、等分及高度编辑接纳与原生可执行资格分离。保留身份可在后台编辑；失效实例、全屏条目、受影响成员的原生归属事务仍拒绝。后台边缘移动不能隐式跨显示器或聚焦。
- Lua延迟结构编辑绑定结构和项高度版本；同步计划内自身操作刷新自身绑定。新意图废弃旧动画协调及防抖候选上下文。
- 移除 `refresh_workspace_window_sizes` 中最后的平铺尺寸直写，该函数随之改名为 `reconcile_workspace_refresh`：viewport变化不再对保留平铺窗口排队 `ResizeMarker`，目标frame只由声明式宽/高意图派生。该查询也不再可变读取 `Bounds`。浮动窗口的视口内重新定位与外部inventory对账保留。
- 诊断的高度投影改用与运行时相同的参与策略（`effective_stack_heights_for`），并在有效投影中带上item ID；`height_items` 仍完整显示全部保留item及其原始权重。已ordered-out的兄弟项不再虚占有效高度，也不制造假的最小高度阻塞。
- 保存升级到v6嵌套结构，包含列类型、嵌套item及原始高度权重。成员改为“每个保留成员一个slot”：`SavedMember { hint: Option<SavedWindow> }`。无法在保存时解析身份的保留成员写成显式 `hint: null` slot，既不伪造身份也不静默缩短成员列表。旧格式忽略，读取仍是隔离候选，纯可信映射导入不能覆盖晚到高度或结构编辑，不新增真实跨daemon绑定。

## 实现入口

| 职责 | 代码 |
|---|---|
| 列身份、意图、求解与结构维护 | [layout.rs](../../../src/ecs/layout.rs)、[layout_intent.rs](../../../src/ecs/layout_intent.rs) |
| Stack item身份、原始权重、参与cohort编辑与派生 | [height_intent.rs](../../../src/ecs/height_intent.rs) |
| 状态接纳与CLI | [layout_edit.rs](../../../src/commands/layout_edit.rs)、[column_width.rs](../../../src/commands/column_width.rs)、[argv.rs](../../../crates/shared_types/src/argv.rs) |
| 脚本身份门禁 | [layout_ops.rs](../../../src/ecs/layout_ops.rs)、[layout_snapshot.rs](../../../src/ecs/layout_snapshot.rs) |
| 动画与协调 | [window_frame.rs](../../../src/ecs/window_frame.rs)、[reconcile.rs](../../../src/ecs/reconcile.rs)、[systems.rs](../../../src/ecs/systems.rs) |
| 外部几何接纳 | [window_geometry.rs](../../../src/ecs/window_geometry.rs) |
| 保存与隔离导入 | [state.rs](../../../src/ecs/state.rs)、[restore.rs](../../../src/ecs/restore.rs) |
| 诊断 | [inspection/spool.rs](../../../src/inspection/spool.rs) |
| 首片端到端测试 | [column_intent.rs](../../../src/tests/column_intent.rs) |
| 高度端到端测试 | [height_intent.rs](../../../src/tests/height_intent.rs) |

## 实施中的专家复核

列宽首片由三位专家分别复核领域、动画/外部接纳、保存/诊断。高度片由领域、执行、验证三位专家复核；最终复核发现“外部接纳仍按全部保留item计算”与“保留成员无身份提示时被静默丢弃”两处缺口，分别以参与cohort编辑API和显式memberslot修复。

Balance的继承语义经过三位专家一致确认：若参考列继承规则0.75，其他列继承默认0.25，Balance将参考配置来源的原始0.75复制为各列显式意图，使各列真正同宽；显式Absolute/Ratio保留原始变体和值。这里不复制各自解释不同的InheritConfig标记，也不从受约束frame反算宽度。这是显式平衡命令的行为，不改变普通继承配置及拆列原始意图复制政策。

高度片按用户“三个独立专家”政策处理新设计问题。宽度单位守恒与成员slot表示问题上两位独立专家给出不同实现（计数伴随字段 vs 显式slot），按ADR 0007“开发期不需要旧格式/旧接口兼容”选择显式slot并升版到v6；分歧已在交接记录中登记，随后用户指示不再使用三专家策略，最终按显式slot定稿。

## 验证

自动验证结果（列宽首片，历史）：

- `cargo test --workspace`：1181项通过，0失败，2项原有忽略；其中主程序1052项，共享类型96项，其余crate及文档测试33项。

自动验证结果（列宽+高度两片合并后）：

- `cargo test --workspace`：1209项通过，0失败，2项原有忽略；其中主程序1080项，shared-types 96项，其余crate及文档测试33项。
- `cargo fmt --all --check`：通过。
- `cargo check --no-default-features`：通过。该检查暴露 `Windows::layout_transition_pending` 被lua门控而状态接纳路径无条件调用的既有缺口，已改为共享门控。
- `cargo clippy --workspace --all-targets -- -D warnings`：通过。
- `git diff --check` 与实施文档本地链接检查：通过。

重点回归覆盖：后台800→900期间零原生写入/聚焦/切Space、显示后实现900；未知viewport；约束解除恢复原意图；无手势受限frame不反向覆盖；身份/拆分；动画失败预算；保存乱序/失败；候选隔离与晚到导入；后台stack与最后项高度只实现最新意图；容纳不足阻塞且不删成员；无viewport的Equalize只改原始意图；隐藏中间项时上/下边缘拖动只作用于参与cohort且保留下边缘外成员的原始权重与cohort权重单位总和；诊断中已ordered-out项保原始权重但不占有效高度；无身份提示成员保存为显式slot并可往返。

## 明确边界

- 没有安装、重启daemon或操作真实桌面。mock通过只证明模型和调用协议；真实macOS动画、约束、回声及原生tab边界仍需单独验收。
- 两个切片都没有实现跨daemon身份自动绑定或实际重启恢复。旧格式直接忽略，旧配置直接拒绝。成员slot只保留“该item期望几个成员”与顺序，不保留已丢失的身份，也不授权任何自动绑定。
- 可信宽度约束求解与适配接口已实现并经mock验证，但尚无原生硬宽度区间采集器。原生拒写不能被冒充为最小/最大宽度证据；无法证明的能力仍保持未知，按有界协调处理。
- stack最低200逻辑点是布局政策，不是采集到的原生最小高度证据。
- 浮动窗口独立几何及焦点/归属全面迁移不在已完成的切片内。

## 焦点激活切片

用户在焦点规划整理后明确要求继续实现；未启用子代理。依据[焦点偏好、激活请求与实际焦点的完整迁移切片](issues/21-focus-activation.md)，落实：

- 激活版本与观测resolution generation独立；请求绑定entity/window ID/PID/incarnation/Space，终态保留诊断。unknown不清掉请求，旧采样可以接纳事实但不能完成新激活。
- 每Space偏好、逻辑选择、确认历史分开；`space prefer-focus <Space ID> --window <窗口 ID>`只编辑状态。浮动窗口可按确认归属接纳，保留平铺项允许后台/暂缺编辑。非法或歧义身份拒绝。
- 聚焦observer只接纳，由同一个主线程协调器处理即时与延期执行。首次原生调用前消耗一次尝试；部分提交失败保留失败标记，只读确认，不自动重发。AppKit底层activate/key/raise错误向上返回。
- 可用性、Mission Control、可见归属不足表现为blocked并报告原因；可见性资格失败只在新拓扑generation后复查，不逐帧查询。已确认命令携带本次原生Space资格，避免用旧strip覆盖当前原生归属或重复消耗资格读。
- 原生复验250ms/1s/5s后停止专用读；两次同一完整身份样本至少相隔250ms才可让权。完整应用inventory、前台/焦点复核、可见成员归属以及Space事务/Mission Control门禁共同取证。unknown或不同竞争事实打断稳定候选；旧候选不能结束新请求。
- 未确认的被替换尝试保留为自身迟到效果嫌疑，不用于让权。让权是未实现终态，不能把B当A成功，也不能在资格恢复时复活A。
- 临时AX失联保留目标，后续窗口操作不回退误操作旧窗口；确认销毁或换实例才结束对应请求。主动隐藏等已有显式失效路径仍保持原有取消政策。
- 自动恢复不覆盖已有pending请求，不为同一未确认或让权目标补预算；源Space不可见时不隐式激活，move-follow只在原生目的归属确认后提交。
- session/Space诊断新增实际焦点、激活结果/预算/失败/阻塞和每Space偏好/选择。CLI、配置与Lua action共享接纳路径。IPC升级至7（包括握手探针）；磁盘布局仍v6，没有焦点跨daemon恢复。

验证回归包含：未知保留、旧采样/旧竞争隔离、稳定让权与unknown打断、Space选择隔离、后台偏好零effects、Mission Control延期、同请求零重试/新明确输入新预算、真实mock提交失败后复验让权、专用读预算耗尽、原生move-follow及现有布局交互。

### 本片验证

最终代码树验证：

- `cargo test --workspace -- --test-threads=1`：1222通过、0失败、2项原有忽略（主程序1093、shared-types 96、其余33）。
- `cargo fmt --all --check`、`cargo check`、`cargo check --no-default-features`、`cargo clippy --workspace --all-targets -- -D warnings`全部通过。
- `git diff --check`通过；docs本地Markdown链接检查无失效。
- 新增未知保留回归先在旧代码上失败，再验证修复。协议升版后同步了握手探针版本及拒绝消息测试，最终完整IPC往返验证通过。

此前两片1209项记录为历史；最终验证后仅填写文档结果，未再修改代码。

### 本片实际边界

没有安装、daemon重启、推送或真实桌面操作。时序阈值是实现默认，不是原生稳定性证明。自身迟到效果的排除采用保守的会话内实例记录，无法排除的竞争会保持未确认；不能宣称已获得macOS输入来源或完全因果识别。目标Space归属的声明式事务与完整跨重启恢复仍未实现。

## 归属切片（声明式目标 Space 归属）

用户于 2026-09-15 裁决的模型不是"保留目标、条件允许时实现"，而是**不变量 + 修复**：归属是状态的一部分，必须始终指向一个当前存在的用户 Space；编辑要么当场可尝试、要么当场拒绝；外部事件触发修复，修复只改状态、不写原生。四小片按 [issue 23](issues/23-space-membership.md) 落地。

### 已实施

- **拒绝原因具体化**（`b91b9a5`）：归属路径不再混装 `native_precondition_failed`，拆为 `target_space_unavailable`（无此用户 Space）、`fullscreen_space`、`space_not_visible`（目的显示器当前未显示，属"现在不行"）、`capability_unavailable`、`native_move_pending`、`window_unavailable`、`layout_not_found`、`ineligible_layout_entry`、`snapshot_binding_stale`、`topology_unresolved`（读取失败不是缺席）。`Rejection::TargetSpaceUnavailable` 不再有生产者，已删除。
- **最近一次尝试可查**（`b91b9a5`）：只读诊断 `SpaceMoveAttempt`（`in_flight` / `confirmed` / `timed_out` / `retired` / `refused(code)`），`window inspect --source spool` 的 window row 暴露为 `membership.attempt`。
- **声明归属与修复**（`b1ff96a`）：`DeclaredSpace { target, observed, repairs }`（修复历史保留最近 4 次）与 `reconcile_declared_space`（调度在 `Last`：销毁处理 → 事务协调 → 声明协调）。触发器：目标 Space 被销毁/合并（`target_space_destroyed`）、变原生全屏（`target_space_fullscreen`）、外部移动/合并（`membership_changed`）、未确认尝试（`attempt_unconfirmed`）、成员实例退休（`member_retired`）；显示器断开而目标仍存在、以及读取不可读时**不修**（未知不是缺席）。实例退休时声明随实体消失，替换实例不继承历史。
- **被接纳的编辑写入声明**（`7d2f040`）：整列/关联成员的每一个都立即声明目标 Space；这是作者转移而非修复，不进修复历史。**在途尝试拥有实现权**：带 `NativeMoveOwner` 时只更新 `observed`、不修复，否则观测会把刚被接纳的声明按回去。
- **未确认的尝试带原因修复**（`b6b8b2b`）：2 秒确认窗口过后事务释放所有权，声明协调据尝试记录把声明修复回观测并记 `attempt_unconfirmed`（`Retired` → `member_retired`）；不自动重试（测试断言原生意图计数不变）。超时因此不再只剩一条日志。
- **跨显示器可见性门**（`ad33d31`）：结论**保留**并写明理由 —— `move-to-display` 的语义是"放到那块屏幕上我能看到的地方"（transfer 用目的显示器可用视口暂存 frame 并在确认后呈现于此），因此没有唯一可见 Space 的目的显示器答 `space_not_visible`，而不是替用户挑一个 Space；把窗口送到另一屏的隐藏 Space 走 `move-to-space <id>`，不受此门约束。

### 本片验证

最终代码树验证（本机，Apple M4，并行套件）：

- `cargo test -p spool`：1164通过、0失败、2项原有忽略。`cargo test --workspace --locked`：主程序1164、shared-types 96、local-ipc 24，其余 6/4，全绿。
- `cargo test -p spool --no-default-features`：1021通过、0失败。
- `cargo fmt --all --check`、`cargo clippy -p spool --all-targets`（默认与 `--no-default-features -- -D warnings`）全部通过。
- `docs/**/*.md` 相对链接检查 0 失效。
- 行为改变处均有"先在旧代码上失败"的证据：拒绝原因（旧代码得到 `native_precondition_failed` / `TargetSpaceUnavailable`）、在途不被按回（移除跳过时声明被拉回源 Space：`left: Some(2) right: Some(3)`）、未确认原因（移除推导时得到 `membership_changed`）。新状态类型的测试在旧代码上无法编译（`cannot find type DeclaredSpace`），这是新状态的固有形态。

### 本片实际边界

- 没有安装、daemon 重启或真实桌面操作：mock 通过只证明模型与调用协议，真实 macOS Space 动画、约束、回声与原生 tab 边界仍需另行授权的现场验收。
- 跨重启恢复归属仍未实现：声明归属是会话内状态，新格式保存与候选隔离没有覆盖它（Out of scope，见 [地图](map.md)）。
- 声明归属只覆盖已跟踪窗口；未解决的原生表面（无 AX 身份）不进入该状态。
- `SpaceMoveAttempt` 与 `DeclaredSpace` 都不驱动行为：前者是只读诊断，后者由修复与**被接纳的编辑**写、被效果层读取的目标；没有任何路径用它们反向覆盖观测。
