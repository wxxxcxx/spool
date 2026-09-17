# 窗口准入与平铺规则

决策日期：2026-09-06。本次重构的行为契约，不代表已部署或完成真实桌面验收。

## 路线

保留现有 AX 操作和 WindowServer/Native Spaces 观察后端，统一策略，不增加注入、SIP 调整或另一套私有控制后端。

- 参考 AltTab/DockDoor 的“发现、身份、视图过滤分离”，不照搬切换器的最终显示列表。
- 参考 yabai 等项目区分 tracked 与 tiled；当前 strip 需要移动和双轴缩放，不采用“大固定窗口也平铺”的尺寸阈值。
- **窗口用途不是核心分类依据。** “设置页、工具窗通常浮动”属于可编辑用户规则；可写的设置窗不因名称或用途被核心禁止平铺。
- 结构事实与能力限制不能通过偏好覆盖。菜单不是独立窗口；`floating=false` 不会让不可写的 AX 属性变得可写。

研究依据见[准入比较](research/window-exclusion-policy.md)和[平铺比较](research/window-tiling-policy.md)。研究保留重构前快照，当前实现以本文为准。

## 独立决策

| 问题 | 当前处理 | 不应混淆 |
| --- | --- | --- |
| 应用是否可能有窗口 | Regular、Accessory；明确 bundle 的 `track=true` 可扩大进程观察范围 | Accessory 不等于默认 float |
| 是否跟踪具体窗口 | 有效 AX 窗口 ID、role/subrole、已知父关系、准入规则 | 无截图、不能 resize、不可见不等于应忽略 |
| 默认布局意图 | 既有恢复语义、用户规则；兜底准入默认 float，其余无偏好时尝试 tile | Settings 标题不是核心用途分类 |
| 此刻能否执行 | backend 能力，加已有可用性、全屏、可见性和 Space 迁移保护 | AX 报错不等于永久不支持 |

Floating Window 仍是 Tracked Window，参与现有查询和聚焦流程，不占 strip 栏位。窗口身份与导航视图应否显示它仍是两个问题。

方向、首尾、编号和上一/下一平铺窗口导航共享 `Windows::navigable_strip` 投影：过滤不可用、隐藏和浮动项，保留 Stack/Tabs 结构。原始 `LayoutStrip` 中为恢复保留的旧身份不能阻塞导航，也不能为了修复导航而直接删除。浮动导航仍使用当前 Space 的可见浮动候选。`spool::navigation=debug` 记录焦点快照、原始成员、投影成员及方向导航目标，便于对比命令所见状态。

新平铺窗口默认插入所属 Space 当前焦点所在列之后。若原生焦点已先切到尚未入列的新窗口，则使用该 Space 最近的有效平铺焦点；只有没有有效锚点时才追加到末尾。显式 `index` 与已应用的会话恢复位置保留优先级，不从其他 Space 借用锚点。浮动窗口不参与列插入。

## 准入契约

1. AX 必须给出有效窗口 ID。物理 surface 本身不自动产生可控制窗口。
2. `track=false` 排除匹配窗口。`track=true` 可放行非标准 subrole，但不能放行非 `AXWindow` 元素或已确认附属于另一个窗口的条目。
3. 默认接收 `AXWindow` 下的 `AXStandardWindow`、`AXFloatingWindow`、`AXDialog`、`AXSystemDialog`，**并且**要求窗口样证据：关闭或最小化按钮至少存在一个，或 AXPosition 与 AXSize 都明确可写（无边框但可移动可缩放的窗口）。独立 dialog 不再仅因 subrole 被排除，也不因此在核心里默认 float。
3.1. **subrole 单独不构成窗口**（2026-09-17）。macOS 自己的面板也会回答这四个 subrole 之一：控制中心的面板（`com.apple.controlcenter`，CG 层 22，656×967）就是如此，它没有关闭/最小化按钮、也不可移动不可缩放，却被旧判定直接 `Track` 并当成一个浮动窗口管理。现在窗口样证据对**所有** subrole 路径生效：明确缺失 → 排除；读取失败 → 暂缓（失败不等于没有）。显式 `track=true` 仍然单独决定。标题**不作为**证据——控制中心的面板本身就有标题。
4. 已确认 AXParent role 为 `AXWindow`、`AXSheet`、`AXDrawer` 时，不另建独立跟踪项。父属性不可用不等于已证明是子窗口：以 AX 窗口列表为基础，不把缺失父属性作为全局否决条件。
5. role/subrole 读取失败与实际返回非标准值不同。元数据或必要规则匹配尚未确定时暂缓准入；应用 inventory 保留原始身份供核对，不把它当作窗口销毁。

### 非标准窗口兜底（2026-09-08）

在上述标准准入之外，非标准 subrole 的 `AXWindow` 可通过通用证据纳入，不按 Finder bundle、标题或 `Quick Look` 字符串开特例：父级明确为 `AXApplication`，AX 位置有限、尺寸有限且为正，WindowServer 当前屏幕列表中存在同 ID 的正常层或原生浮动层（layer 0 / `kCGFloatingWindowLevel`）、正 alpha surface，并且 AX 关闭或最小化按钮至少存在一个。按钮不存在与通信失败分别记为否定和未知；缺少必要证据则暂缓，不猜测可交互性。

兜底窗口携带默认浮动偏好，参与既有查询、Bar、焦点与 overlay 目标选择，不占 strip 栏位。显式 `floating=false` 或手动平铺可覆盖此偏好，但不能绕过移动/缩放能力。`track=false`、非窗口控件、已知附属窗口仍不能通过按钮兜底放行；显式 `track=true` 保留原有语义。

AXWindows 枚举与分步补充发现共享判定；**窗口 chrome（关闭/最小化按钮）与移动/缩放能力对每个候选都读取**（判定需要它们区分窗口与系统表面），其余附加属性仅对需要兜底的候选读取，每步至多一次同步 AX 调用。屏幕 surface 只用于新兜底身份的准入，不是窗口存活规则：已跟踪窗口后续隐藏、最小化或短暂丢失属性时，仍通过原始 AX 身份进行生命周期对账。首次发现时不在屏幕上的非标准窗口需等到可见后再纳入。

这不是 DockDoor 最终显示过滤器的完整移植，也没有修改 overlay 的原生层级；未聚焦预览仍可见时的遮挡须另做真实桌面验收。

原生 Space 枚举另补充独立浮动 surface：候选必须已经由对应 Space 的原生列表返回、无原生父窗口、有窗口属性与浮动标签，再由 CoreGraphics 确认浮动层、正 alpha 和有效身份。此步骤只补足旧普通窗口标签过滤漏掉的成员，不直接授予跟踪资格，也不猜测当前 Space。每次 Space 枚举有此类候选时至多增加一次 CoreGraphics 列表读取。2026-09-08 现场 Quick Look 的 `parent=0, attributes=0x2, tags=0x1000c2802` 属于 Space 1，但旧过滤返回 false。

现场验收还覆盖关闭和重新打开：窗口 325 关闭后从可见状态移除，重开后以 `floating=true` 再次投影。焦点可能先于准入到达，因此此前记录为 Untracked 的同 ID 后续出现 tracked entity 时必须重验；对账不选择不可用的旧实例，并优先匹配当前确认的实例。已确认 Finder 原生焦点、Spool 焦点和 overlay 目标均能指向 325。这里验证的是目标选择和生命周期，不等于所有多显示器、后台浮动预览遮挡或动画场景均已验收。

AX 列表和私有 WindowServer 列表提供互补发现证据。后者为空或失败不再否决前者；补找窗口按 ID 差集计算，不用数量相同代替身份相同。已启动但暂时无窗口的应用保留观察，后续由通知和 inventory reconciliation 发现窗口，不再因五秒内没有窗口而清理应用。

**应用可观察不等于已经就绪。** 初始扫描和后续启动统一经过 `Process::ready()`：等待 `isFinishedLaunching`，再检查可观察策略或显式强制规则。初始扫描中未就绪的进程转为待启动状态，保持观察就绪变化，但不提前创建 AX Application。不会按辅助进程名字建立隐藏黑名单，也不会因此排除已就绪的菜单栏应用。

## 能力契约

| 输入 | 无显式 float 时的结果 |
| --- | --- |
| AXPosition、AXSize 均明确 settable | 可以尝试 tile |
| AXPosition 明确不可写 | float，原因 `NotMovable` |
| AXSize 明确不可写 | float，原因 `NotResizable`，不区分大小窗口 |
| 没有明确不支持，但至少一个能力读取失败 | defer，保留身份并有界重试 |
| 用户规则 `floating=true` | float，无须证明 tile 能力 |

settable 不保证任意尺寸可达。明确返回 attribute unsupported 也归为当前 backend 不支持，而非通信失败。写入继续经过现有几何事务和读回机制，处理应用钳制、部分成功与有界重试；不会为了分类偷偷试改尺寸。

初始未知状态复用 `WindowDefaultsPending`：暂以 Floating 投影留在 strip 外，快速尝试后冷却重试，不把未知结果提交成永久默认浮动。手动 tile 时未知则保留 `RetilePending`，五秒后重试；再次切换会取消待完成的请求。明确不支持时拒绝本次 tile，保持 float，用户可在应用状态改变后重试。

创建、默认布局、重新平铺使用同一能力函数。浮动切换命令不再直接插入 strip，取消最小化等既有重新平铺入口也经过检查。手动 tile 可覆盖默认浮动偏好，不能绕过物理能力。

浮动 `grid` 需要完整几何操作：能力未知时等待，明确受限时不执行 grid，保留当前 frame。固定窗口也不执行浮动切换时的装饰性缩放。

## 控制目标身份（2026-09-15）

几何提交按窗口的「原生控制目标」判定，与窗口准入是两个问题。`WindowApi::represented_window_id` 读取窗口自身子元素中的唯一直接 `AXTabGroup`，再解析其 `AXWindow` 归属：

| 枚举结果 | 提交行为 |
| --- | --- |
| 唯一 `AXTabGroup`，归属指向另一个窗口 | 拒绝写入，保留「不猜测未知控制目标」保护 |
| 子元素表可读，其中没有 tab group | 该窗口自己就是控制目标，正常提交 |
| **子元素表读不出来** | 同样按「没有直接 anchor」处理，记住结论并只 WARN 一次 |
| 可读但存在多个直接 `AXTabGroup` | 歧义，仍然拒绝而不是猜测 |

读取失败不是「另一个窗口拥有该元素」的证据。旧行为把两者等同，于是 AX 子元素表读不出来的窗口其几何写入被永久挂起；现在只有「读成功且归属确实指向别的窗口」才继续拒绝。结论与成功结果一样只结算一次，因此既不会每帧重试失败读取，也不会反复刷日志。

现场证据（2026-09-15，Telegram 12.8 / `ru.keepcoder.Telegram`，窗口元素）：

- 该元素在 `AXUIElementCopyAttributeNames` 中声明支持 `AXChildren` 与 `AXChildrenInNavigationOrder`，但两者每次都返回 `kAXErrorFailure`（`-25200`），耗时 1–2 ms。不是超时：超时对应 `kAXErrorCannotComplete`（`-25204`），本机同一会话中其他进程会返回它。也不是客户端差异：Spool CLI、独立 C 探针与 System Events 结论一致（System Events 看到该窗口 0 个 UI element）。同一元素上 `AXRole`/`AXTitle`/`AXPosition`/`AXSize`/`AXFrame` 均 0.1 ms 成功，真正不支持的属性正确返回 `kAXErrorAttributeUnsupported`（`-25205`）。
- 修复前的表现：`desired` 正确、`presented` 停在动画中途、`observed` 停在旧位置，`attempts=0`、无 `WindowFrameMotion`；显式 `window reconcile` 与新的列宽意图都不能推动该窗口，同一 Space 的其他窗口全部正常移动。这与「缓存只在窗口诞生时读到一次成功结果」一致：诞生瞬间读成功的新窗口此后一直可写，而已经完整运行后才被接管（典型情形是 daemon 重启）的窗口永远读不到成功结果。

可观测性：提交因控制目标未解析而被扣留时，`commit_window_frames` 输出一次 WARN（窗口 ID、entity、真实 AX 错误）；`window inspect --source spool` 的 `state.blockers.commit_suspended` 表示该窗口当前处于提交挂起。`geometry.realization` 的 attempts/active/blocked/confirmed 是目标轮次状态，不替代这两者。

此决定不放宽写入资格：身份、可用性、Space 可见性、全屏与 Space 迁移保护不变，原生写入失败仍走既有有界重试与挂起。验收状态：本次现场中该窗口修复后可随重排、reveal 与列序变化一起移动；daemon 重启后重新接管一个已完整运行的同类窗口是否同样可写，需要下一次重启后现场确认。

**残留风险（刻意接受）。** 「读不出来 → 该窗口自己就是控制目标」是 fail-open 方向。若某个窗口的原生 chrome 实际由另一个窗口的 tab group 拥有，而那次枚举恰巧失败，Spool 会照常提交，把几何写到它自己跟踪的元素上，而正确目标本该是别处的元素。选这个方向是因为反向的代价更大：旧行为把这类窗口的几何写入**永久挂起**（现场证据见上），而 fail-closed 只能等下一次成功读或原生事件才可能解除。风险面限定为「枚举失败 **且** 实际归属别处」同时成立：

- 读成功且归属指向另一个窗口 → 仍拒绝（上表第一行）；
- 读成功且存在多个直接 `AXTabGroup` → 仍按歧义拒绝（上表第四行）；
- 但注意可观测性的边界：上面那条 WARN 只在提交被**扣留**时输出，因此 fail-open 方向真的写错元素时，日志里不会有这条痕迹；诊断只能看到该窗口「一直正常提交」。

若将来要收紧这个方向（未采纳，需要自己的证据与现场复现）：对枚举失败的窗口降级为只读不写、并按窗口记住该状态直到出现一次成功枚举；或在枚举失败时读取 `AXChildrenInNavigationOrder`、`AXParent` 等其他归属线索作为第二证据。两者都会把「读失败即挂起」的代价重新引入，所以应先拿到误写方向的现场证据再动手。

## 用户规则

`windows` 保持命名表。四个可选匹配条件是 AND：`title` 正则、`bundle_id`、`role`、`subrole` 精确匹配。省略条件表示不限制该属性，不再要求 `title=".*"`。真实空标题可以匹配 `^$`；暂时读取失败不能冒充空标题。其他条件已经确定不匹配时，不必等待无关元数据。

Adapter 把明确不存在的 bundle ID、AXTitle 的 no-value/unsupported 转为空值；真正的 AX 通信错误仍是未知。无 bundle 的应用不会因为系统设置的 bundle 规则而永久等待。

规则按 `priority` 降序排列，默认 `0`，同级按规则名的 Rust 字符串顺序升序。**每个字段取第一个明确值**，包括 `false`；不是整条规则覆盖，也没有隐含的“更具体者优先”。`bindings_passthrough` 沿用累积行为。

```lua
spool.setup { windows = {
  default_dialog = {
    subrole = "AXDialog", floating = true, priority = -100,
  },
  my_dialog = {
    bundle_id = "example.editor", title = "^Settings$",
    floating = false, priority = 10,
  },
  ignore_overlay = {
    bundle_id = "example.editor", title = "^Overlay$",
    track = false, priority = 20,
  },
} }
```

[default.lua](../config/default.lua) 提供低优先级浮动偏好：三个 dialog/floating subrole，以及 System Settings、KeepingYouAwake 两个明确 bundle。没有通用 Settings 标题猜测，也没有 Accessory 全局浮动规则。用户可删除、修改或覆盖。

`Config::default()` 不注入这些用途偏好。已有 `init.lua` 不被升级重写，也不自动合并新模板，已有用户按需采用。因此，没有采用模板偏好且能力可用的设置窗口可以正常 tile。

## 生效与迁移

- 准入规则作用于新发现、尚未跟踪的窗口。新加 `track=false` 不会即时撤销已有身份或伪造销毁事件，需重新创建窗口或下次启动才按新准入规则处理。
- `floating/index/width/grid` 保留初始默认值语义；待完成事务使用新配置，不因每次元数据刷新覆盖手动选择或重排桌面。其他动态配置沿用原有更新机制。
- 手动 tile 不把默认 `floating=true` 当作永久禁令。已完成的选择不被普通重采样重置。
- 延迟手动 tile 以完整、唯一的原生 Space 归属为准；切换当前 Space 不改变目标。几何使用目标 Space 所属显示器的可用区域，没有活动显示器时也可恢复到已知目标。
- 启动恢复保留既有优先级：可匹配的已保存平铺布局优先于初始偏好，但不绕过已知移动/缩放限制。未知能力可能延迟到恢复宽限期之后，不能保证恢复旧位置。
- 恢复期间临时无法确认归属时，匹配候选按窗口 incarnation 保留初始默认事务，避免浮动偏好提前消耗恢复资格；用户随后明确选择浮动仍然有效。超过恢复宽限期后不继续保留这一阻塞。
- 旧冲突规则没有可靠顺序；现在顺序确定。若此前依赖偶然结果，需要显式设置 `priority`。
- 旧 `track=true` 可把非窗口 role 强行包装成窗口；现在收紧该绕过。真正使用非 AXWindow 根元素的应用需要经证据验证的 adapter 兼容路径，不能依靠全局强制开关。

## AX 通知能力（2026-09-07）

通知支持与窗口准入、移动/缩放能力独立。`AXObserverAddNotification` 返回 `-25207`（`kAXErrorNotificationUnsupported`）表示目标 AX 元素不支持该通知，不是辅助功能未授权，也不意味着窗口必须被排除。

注册策略见 [app.rs](../src/manager/app.rs)：

| 返回结果 | 处理 |
| --- | --- |
| 成功、already registered | 记录已完成，不重复注册 |
| notification unsupported (`-25207`) | 记录该目标不支持；仅首次输出 DEBUG，不进入错误重试列表 |
| cannot complete (`-25204`)、API disabled (`-25211`) | 立即停止本批注册，保留已完成项与真实错误码，由调用方处理应用级退避 |
| 其他失败 | 保留真实错误码和重试机会，不伪装成统一的 PermissionDenied |

按应用观察器/具体窗口 incarnation 和通知名称保存结果；一项失败不能导致已成功或明确不支持的其他项整批重试。取消观察时清理对应记录，窗口 ID 复用不会继承旧实例的结论。`observe() == Ok(true)` 表示没有待重试项，不保证所有请求的通知都可用；即使全不支持，也继续依靠 inventory 和状态 reconciliation 跟踪窗口。

此次错误的原因是旧注册逻辑把所有非成功结果都当成可重试失败；扩大 Accessory 观察范围后会涉及更多不支持这些通知的目标。测试覆盖全不支持、部分失败恢复、实例隔离、取消后重注册及 ERROR 日志不再刷屏。该修正不新增权限、不移除跟踪资格，也不自动部署或重启 daemon。

### 应用级通信故障与退避

`-25204`（`kAXErrorCannotComplete`）表示消息通信失败或目标忙碌、无响应，不能据此认定缺少授权，也不能缓存成永久不支持。与 `-25207` 的通知能力结论分开处理。

- 注册发生通信/权限失败时中止剩余通知，包含“已有部分通知成功”的情况；错误交给上层，避免每项 ERROR 再叠加一次应用 WARN。
- `WindowStateSync` 按 Application Entity 保存失败状态。注册或 AXWindows inventory 返回 `-25204` / `-25211` 后，等待 **1、2、4、8、16、30 秒**，持续失败时最多每 30 秒探测一次。实际探测发生在到期后的下一次对账，事件风暴不绕过冷却。
- 当轮注册通信失败，不继续读取同一应用的 AXWindows。冷却期间跳过该应用在 reconciliation 中的注册、inventory、焦点和 frame AX 查询。其他应用保持正常频率；这不是所有用户命令和平台调用的全局熔断器。
- 对账中的同一连续故障、同一错误码只首次 WARN，带 PID、应用名、bundle ID 和真实错误。后续失败记 DEBUG，错误码变化重新 WARN。成功读取 inventory 后清除退避并记恢复 DEBUG，下次故障从一秒开始。启动路径的一次性诊断不属于这项日志去重范围。
- 进程存活与 WindowServer 观察继续执行。AX 失败或冷却不能当成窗口销毁证据，保留既有身份和布局；物理 surface 缺失仍按原有暂不可用流程处理。明确进程退出可立即清理，不必等待冷却结束。退避随 Entity 清理，复用 PID 的新应用不继承旧故障。

2026-09-07 的只读现场样本：日志 PID `80780` 对应 `com.apple.WebKit.Networking`，应用名为“飞牛同步 Networking”，`activationPolicy=Accessory`、`isFinishedLaunching=false`。原有进程初始化路径只检查可观察策略，没有经过后续启动路径的 `ready()`；随后通知批次继续访问失败端点，心跳又立即重试，形成持续错误。这个样本说明 Accessory 不是 AX 可用性的保证，不代表所有 WebKit helper 或所有 Accessory 应用都应排除。

错误定义核对自本机 Xcode SDK 的 `HIServices.framework/Headers/AXError.h`（`kAXErrorCannotComplete`、`kAXErrorNotificationUnsupported`、`kAXErrorAPIDisabled`）。Apple 网页本次未返回可用正文；真实桌面修复效果仍须重建并经授权部署后验证。

## 边界与验收

- 不是完整重写 AltTab 目录，没有增加截图权限、注入、私有 AX token 遍历或新私有 API。
- 扩大应用观察范围和增加父关系读取会增加 AX 工作量；本次未做大量应用并行运行的真实开销测量。
- 不新增 sheet/modal 聚焦代理或完整父子模型；父属性缺失的兼容性需真实应用验证。
- 不支持固定尺寸 item、单轴约束布局求解，也不把视觉缩放当作真实 resize。
- 未实现 sticky 多 Space 布局归属、实时撤销已跟踪窗口的准入规则，或所有动作的能力菜单。
- 原因用于内部决策和日志，未扩展公开 query schema。待定初始窗口的 `floating` 投影不等于已完成默认决策。
- 本次仅代码、mock 与静态验证；KeepingYouAwake、System Settings、第三方 dialog、多显示器与真实全屏切换需授权后的桌面验收。禁用 zoom 按钮不是 fixed-size 证据。

## 实现入口

- [window_policy.rs](../src/window_policy.rs)：不依赖 AppKit/ECS 的准入与能力决策。
- [config.rs](../src/config.rs)：匹配、未知值和确定顺序。
- [windows.rs](../src/manager/windows.rs)、[process.rs](../src/manager/process.rs)：平台属性和应用观察。
- [triggers.rs](../src/ecs/triggers.rs)、[defaults.rs](../src/ecs/defaults.rs)：初始事务、浮动和重试。
- [策略回归测试](../src/tests/window_policy.rs)：用途无关、规则覆盖、能力拒绝、失败恢复和无窗口应用生命周期。
