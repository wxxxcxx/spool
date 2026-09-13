# 焦点偏好、激活请求与实际焦点的完整迁移切片

Id: 21
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [Spool 声明式状态模型与迁移边界](../map.md)
Blocked by: none

## Question

对照已裁决的焦点归属、让权和单次尝试政策，确定下一切片的代码替换边界、状态协议与验收场景，不重新表决已有政策。

## Answer

本轮为本地代码核对与切片规格整理，沿用[状态归属](05-state-ownership.md)、[外部焦点让权](13-focus-yield.md)和[聚焦不自动重试](15-focus-retry.md)的已有裁决；没有新专家投票，也不声称用户逐项确认了下列实现建议。下一实现以完整焦点切片为单位，目标 Space 归属及双 strip 事务仍按[迁移次序](10-migration.md)另片推进。

### 当前证据与必须替换的行为

基线为本地提交 `bd94944`，本轮只读核对代码，未重跑历史测试。

- [focus.rs](../../../../src/ecs/focus.rs) 的 `FocusRequest` 仅存 entity；`FocusCoordinator::request` 与 `begin_resolution` 共用 generation。`Tracked`、`Untracked`、`Unresolved` 均直接清空 requested，不能区分命中目标、稳定竞争或证据未知。需要独立激活版本与观测采样版本，不能把任意观测结果当激活终态。
- `navigation_entity(workspace)` 优先取全局 requested，再取 Space 历史。请求本身未携带 Space 作用域；应由每 Space 逻辑选择回答导航，避免将全局请求当每 Space 偏好。调用者可能另有过滤，本结论不宣称已复现跨 Space 用户故障。
- `focus_window_trigger` 在 observer 内记录请求并立即调用平台；需拆为接纳、资格检查、一次尝试、只读确认。`Explicit/Automatic` 来源必须保留，自动恢复不能为已尝试请求重新发放预算。
- [systems.rs](../../../../src/ecs/systems.rs) 的 `retry_front_switch` 重试的是 `focused_window_id()` 读取，已有定时探测与超时。保留读侧恢复能力，不因名称含 retry 就删除；它不能再以 Unresolved 抹除独立激活记录。
- [manager/windows.rs](../../../../src/manager/windows.rs) 的 `focus_with_raise` / `focus_without_raise` 返回 unit，当前无法用调用返回值证明原生成功。提交结果需明确已尝试/失败/未知；即使底层调用成功，也只表示提交，完成仍需新鲜观测。

### 状态与接口

扩展现有焦点领域模块，不添加第二套竞争 writer。采用 woz-domain-modeling 与 woz-codebase-design 的术语及小接口原则；具体 Rust 名称由实现确定。

1. 每 Space 分别保存偏好、最新逻辑导航选择、已确认历史（any/tiled/floating）。后台偏好编辑仅更新状态；连续导航从该 Space 最新选择计算，不等待 macOS。历史和 FocusedMarker 仍仅由确认观测推进。
2. Session 最新激活记录包含独立版本、实例绑定（entity、window ID、incarnation）、目标 Space、来源、是否已尝试和结果。接纳新明确输入产生新版本；重复通知和同一请求重送不产生新预算。
3. 接纳入口验证身份和语义，返回版本或拒绝；暂时不可执行仅阻塞首次效果，不删除有效意图。效果入口重新验证最新版本、实例、原生归属/过渡与能力，在首次平台调用前占用唯一一次语义尝试。
4. 观测入口保留 tracked/untracked/unknown 事实及采样关联；原生事实可变化，不能因此修改激活版本。旧效果的新鲜事实可以更新现实，但不能完成或终止新激活请求。
5. 诊断快照只读暴露偏好、逻辑选择、实际焦点、激活版本/目标/来源/已尝试/阻塞或终态；不得将 accepted 或平台调用返回当 focused。

### 协调协议

- 尚未尝试：不可执行时 blocked；资格满足后仅对最新有效请求尝试一次。目标已由可靠新鲜证据确认时直接完成，不为了计数补写。
- 已尝试：随后只读。未知、暂缺、超时保留未确认与原因，不重发；专用采样预算耗尽后停止专用轮询，正常被动观测仍可确认。
- 目标确认：记录完成。竞争候选成立须符合[外部焦点让权](13-focus-yield.md)全部证据门槛，再检查激活版本；成立则终态让权，不将竞争窗口写成命令来源。
- 新明确请求：替换旧激活，旧候选不能结束新请求。终态不会因 Space/权限/可用性恢复重启。
- 暂时 AX 不可用不等同销毁；确认销毁或实例替换终止对应请求并清理对应偏好/历史，不清除无关目标。
- 自动 Space 恢复与 move-follow 必须携带产生它的激活/事务关联及来源；重复激活通知不制造新请求，旧 follow 不覆盖新的明确输入。真正的新 Space 激活事件与旧请求的再次执行必须分开建模。

### 实施顺序与移除清单

| 步骤 | 入口与完成要求 |
|---|---|
| 领域及回归 | focus.rs 拆开采样版本与激活版本；经统一入口测试状态、单次预算及终态 |
| 接纳与导航 | commands.rs、commands/targeted.rs、ecs/params.rs、Lua 与 FocusWindow 调用者统一接纳；每 Space 选择不再借全局 requested |
| 平台执行 | focus_window_trigger 退出直接执行；主线程协调提交，manager 接口保留真实失败/未知证据；不引入 ECS 内直接 FFI |
| 观测与生命周期 | triggers.rs、systems.rs、reconcile.rs、native_space.rs 的观察/恢复/follow 路径对齐独立版本和来源；删除清空请求的旧旁路 |
| 诊断及文档 | inspection/spool.rs 及相关共享协议报告状态与结果；wire 变更按仓库版本规则处理，native 来源不回退 daemon |

完整迁移必须盘点所有 focus_with_raise/focus_without_raise、focus_entity/restore_focus_entity 调用者。单独 raise 的层叠政策仍归现有模块，但不得偷偷承载激活重试；不以理论上的 raise 中性替代原生证据。持久化完整焦点/激活及跨 daemon 恢复不在本片，Session 激活不会从磁盘重放。

### 验收矩阵

| 情境 | 必须证明 |
|---|---|
| 后台 Space 偏好 B→C | 保留 C；零聚焦、零 Space 切换，实际焦点不变 |
| 连续导航 A→B→C，B 未确认 | 下一步从 B 计算；实际焦点和历史不乐观推进 |
| 两个 Space/显示器 | 逻辑选择各自隔离，全局实际键盘焦点仍唯一 |
| 首次资格受阻后恢复 | 恢复后只尝试最新有效目标一次 |
| 首次失败、超时、重复通知、权限恢复 | 同一请求不再写；新明确按键可获得新一次尝试 |
| unknown 或仅前台应用 | 保留未确认，不让权，不清除激活记录 |
| 请求 A 后稳定 tracked/untracked B | 满足全部证据门槛才让权；不算 A 完成 |
| B 候选期间新请求 C | 旧候选不得终止 C；旧版本事实不能冒充 C 完成 |
| 请求前 B 样本、过渡、迟到自身效果 | 不误判为稳定外部竞争 |
| 让权后 Space 或 AX 恢复 | A 不复活；自动恢复不能替旧 A 重发 |
| 目标销毁/ID复用/暂缺 | 销毁或换实例终止，暂缺保留；无关请求不受影响 |
| move-follow 与新明确聚焦竞争 | 旧 follow 不覆盖新输入，事务身份保护保持 |
| 读回预算耗尽后被动目标确认 | 无新增写入及专用轮询，仍可更新实际事实和合法完成 |
| 诊断、主线程、Lua | accepted/attempted/confirmed/yielded 可区分，handler 不进入主线程 |

先以 mock 时钟和平台调用日志覆盖上述协议，再执行 fmt、check、相关与全 workspace tests、严格 Clippy、no-default-features。原生验证须另行授权，不能以 mock 推断真实桌面行为。

### 实现前需具体化的证据参数

现有裁决确定了“新鲜且稳定复验”，没有确定焦点专属的采样间隔/总期限，也没有证明迟到自身效果的原生因果识别能力。实现先核对现有 FocusObservation/事务关联字段，采用可测试的有界读侧策略；不得把几何的150ms直接宣称成已定焦点政策。无法排除自身效果或身份不完整时保留未确认，不能猜测让权。若必须改变已定交互语义，再提出具体场景供用户裁决。

## Resolution

完成本地代码差距盘点与下一切片规格；已有领域裁决未改。未实施焦点代码、未运行新测试、未安装/重启/操作桌面。证据参数的实现核对要求已显式保留，不能将本票视为原生因果识别已解决。


## Implementation follow-up

用户随后明确要求继续，进入代码实现。焦点版本、实例绑定、状态接纳、一次效果、只读复验、诊断和后台偏好命令已接线；验证结果与实际边界集中记录于[实施记录](../implementation.md)。本票以上代码差距描述保留为实施前基线。

只读复验选用250ms/1s/5s共三次，两个竞争样本至少相隔250ms；这是实现默认，不是用户另行裁决或原生稳定性证据。完整应用身份inventory、前台/焦点复核、可见原生归属及已知过渡门禁共同构成接纳证据。被新请求替换而未确认完成的旧尝试目标保留为自身迟到效果嫌疑，不用于让权；证据不足保持未确认。

暂时AX失联不再取消请求后回退操作旧窗口。自动恢复也不再隐式激活已不可见的源Space；move-follow仍只在目的归属和可见性确认后获得执行资格。
