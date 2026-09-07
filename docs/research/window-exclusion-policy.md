# Spool 窗口排除策略与 DockDoor / AltTab 对照

核查日期：2026-09-06。范围：研究应用/窗口发现、窗口切换候选与平铺资格，不修改运行代码或用户配置。

后续实施说明（2026-09-06）：本文保留重构前快照。当前契约见[窗口规则](../WINDOW_POLICY.md)：用途偏好属于可编辑用户规则，核心只判断身份、结构和操作能力；Accessory 不等于默认 float。推荐表不是全部已实现，源码旧行号仅作研究时定位参考。

## 1. 结论边界

本次区分三个不同问题：

1. **是否跟踪**：是否为用户可识别的窗口保留身份、生命周期和可恢复状态。
2. **是否出现在某个导航视图中**：全局切换器、当前 Space 方向导航、应用内切换、bar 图标可以有不同筛选。
3. **是否平铺及允许哪些操作**：移动、缩放、激活、关闭分别有能力约束，不能由“已经被跟踪”统一推导。

第 3 项是独立的重要策略，而非准入规则的尾部条件；默认布局、真实操作能力和临时执行状态的平铺管理器对照见 [平铺资格策略](window-tiling-policy.md)。

这延续 [领域术语](../CONTEXT.md) 中 Tracked Window、Floating Window、Window Visibility 的区分。下文建议是待决策方向，不是已经批准的实现方案。

证据级别：

- **本机已观察**：前一轮查询中，KeepingYouAwake 设置页可通过 AX 读取为标准窗口，运行类型为 UIElement，Spool 状态无对应记录；安装包 `LSUIElement=true`，没有强制跟踪规则。
- **源码已确认**：当前 Spool 工作区和固定 DockDoor 快照存在下述判断/调用。
- **推导风险**：说明什么输入会命中判断，不等于已经用真实应用复现每个情形。
- **设计差异**：窗口切换器与平铺管理器目标不同，差异不能全部称为 bug。

Spool HEAD：`e2c4406f0c50509d3beae2cb066b4b8b0c25f209`；工作区有既有未提交更改，下面描述的是核查时工作区，**不是该提交的纯净快照**。源码定位可能随并行开发移动。

DockDoor 固定提交：`d2a8e2bb388c7664f28b7b844e2de60380ac8c31`，提交时间 `2026-09-05T11:19:10-07:00`，提交说明为更新 beta v1.40.1.0.3 appcast。本次分析上游源码，不声称对应用户安装版本，也没有运行 DockDoor 验证 KeepingYouAwake 实际入选。

AltTab 固定提交：`2f3c67739211f790b8f949f427ccb0575c8684a8`，提交时间 `2026-09-05T19:27:08Z`，版本 `11.6.0`。这次发布包含窗口跟踪重构，因此以下结论针对这个快照，不能套用到旧版本。本次没有运行 AltTab 做真实窗口验收。[A1]

## 2. Spool 当前的排除链

| 阶段 | 当前逻辑 | 为什么重要 |
| --- | --- | --- |
| 应用发现 | `activationPolicy == Regular` 才自动接受；指定 bundle 的 `track=true` 可放行 | Accessory 的普通设置窗口会在窗口级分类之前被挡住。[S1][S2] |
| AX 窗口构造 | 接受 `AXStandardWindow`；或 `AXWindow + AXFloatingWindow`；匹配 `track=true` 可绕过 | AXDialog 等默认不通过；属性读取错误也可能落入拒绝分支。[S3] |
| 启动窗口枚举 | 先从私有 WindowServer 查询得到过滤后的窗口 ID；结果为空时，在读取 AX 列表前返回错误 | 另一套低层谓词可以阻断初始发现；强制跟踪不等于绕过所有上游过滤。[S4] |
| 初始 tile/float | 规则 `floating=true` 或明确 `AXSize` 不可写则 float，其余默认 tile | 窗口类型筛选与平铺能力判断目前没有完整衔接。[S5] |
| 暂时 AX 不可用 | 标记 `WindowUnavailable`，暂停操作/焦点，保留实体和既有布局关系 | 是暂时不可操作，不是忽略或销毁；该区别应保留。[S6] |
| 隐藏/最小化 | `WindowVisibility` 保留状态，窗口移出 strip，并保存原 tiled 位置 | 合理停止布局，但不能据此认定它不应出现在全局切换器中。[S7] |
| 当前 Space 方向导航 | floating 候选需可见、属于当前 Space、与显示器相交 | 对方向导航合理，对“找回最小化/其他 Space 窗口”的全局切换器不够。[S8] |
| query / bar 投影 | query 主要由 strip 加可见 Space 的浮动窗口组成；bar 有另一套 native-membership 投影与只读 surface 图标候选 | UI 缺少记录不一定等于 ECS 从未跟踪；bar 有图标也不一定代表可操作的 tracked window。[S9] |

## 3. 需要调整或进一步验证的问题

### 3.1 应用激活策略不是窗口资格

**本机问题已确认。** `Process::is_observable()` 名字像是在判断 AX 是否可观察，实际只判断应用是否属于 Regular。Accessory 应用可以有用户主动打开的标准窗口，KeepingYouAwake 就是这个反例。[S1]

启动时被跳过的进程连对应 Process entity 都不创建；后续单纯打开设置页，不具有一个面向 Accessory 应用的稳定发现保证。新进程的 `ready()` 也会在同一 policy 检查上等待。[S1][S2]

**建议**：把 application policy 当作发现/扫描优先级和安全边界输入，不作为排除所有窗口的充分条件。允许检查 Accessory 的用户窗口，同时继续排除本进程装饰、菜单、popover、已知系统 surface。不要一口气将所有后台进程都当普通窗口应用。

### 3.2 AX role/subrole 白名单既有漏收，也有语义不对称

**源码已确认，具体应用影响待测。** `is_real()` 默认不接收 `AXDialog`、`AXSystemDialog` 等 subrole；其中有些可能是用户需要找回的独立对话框。与此同时，`AXStandardWindow` 分支没有同时要求 role 为 `AXWindow`，只有 floating 分支要求 role。[S3]

AltTab 当前也没有在 StandardWindow 分支额外要求 AXWindow role，所以这处不对称本身不构成错误证明；需要通过实际异常组合确定是否保留。明确的风险是分类覆盖范围和未知信息的处理，不能仅靠“与另一款软件不同”判错。[A3]

这不是建议接收所有 dialog：独立设置/工具窗口可作为候选，sheet 应与父窗口关联，菜单和临时 popup 一般不作为独立切换项。现有二元 `real/not real` 无法表达这些关系。

**建议**：以“独立可交互窗口、附属对话框、临时 UI、未知”区分，再决定跟踪/导航/平铺；不能把一个 subrole 当作三者统一结论。

### 3.3 临时读不到属性被混入策略拒绝

`is_real()` 把 role/subrole 的读取错误转成 `None`，最后返回 false。`is_forced_track()` 在 title 读取失败时直接返回 false。`window_list()` 还把 AX 列表错误变成空 Vec，并丢掉构造失败的项。[S3][S10]

**推导风险**：应用刚创建窗口或繁忙时，暂时未知与永久不合格在该入口没有明确区分，可能延迟发现或依赖其他事件重试。不能据此说 Spool 全部生命周期逻辑都会误删：较新的 `window_inventory()` 保留 `complete`，reconciliation 也已有 Suspend/Resume，这些是应该沿用的正确机制。[S6][S10]

**建议**：候选判断至少区分 `eligible`、`excluded(reason)`、`pending(reason)`；未知能力不应默认升级为安全可平铺，也不应直接永久排除。

### 3.4 “允许跟踪”与“适合平铺”仍然耦合过强

`AXFloatingWindow` 只让窗口通过构造，并不自动 float。进入默认布局后主要看规则和 `AXSize` 是否可写；未独立检查可移动能力，也没有用模态/附属关系决定默认布局。缩放能力查询失败时按非 fixed-size 继续配置策略，默认仍可能 tile。[S3][S5]

**建议**：工具窗口/独立设置页默认可导航但 float；不可缩放不应让它消失。可平铺需要更明确的证据，操作能力按项暴露。注意当前代码已在 retile 路径拒绝固定尺寸窗口，不能沿用旧笔记把它描述成始终可强制 retile 的缺口。[S5]

### 3.5 规则缺少独立排除语义与确定优先级

当前 `track` 是强制纳入开关：读取点只检查是否存在 `true`，`track=false` 不是主动排除。规则按 bundle 精确匹配和标题正则，保存在 HashMap；`floating()` 取第一个明确值，因此冲突规则没有可靠声明顺序。[S11]

**建议**：未来明确区分跟踪策略、导航显示策略、布局策略；规定确定的优先级/冲突行为，并支持必要的 role/subrole 等谓词。保留用户主动忽略的能力，但不要默认用“空标题、小窗口、非 Regular”代替真实的忽略意图。

### 3.6 上游 WindowServer 筛选会限制强制规则的覆盖范围

`find_existing_application_windows()` 先查询 `existing_application_window_list()`；其内部使用私有 tags/attributes 的 `found_valid_window()`，且 global list 为空便在 AX 枚举前返回。该谓词的第二个 OR 分支也不受第一个分支的 `parent_wid == 0` 限制，因此它并不是一个简单的统一 top-level 检查。[S4]

**推导风险**：不同发现入口具有不同门槛；相同窗口可能通过 AX role 规则，却在初始私有枚举路径中丢失。后续 AX observer/reconciliation 可能弥补，所以这里不是“永久无法发现”的既定结论。

**建议**：区分广义观测清单与策略候选。私有标记作为证据/去噪输入，避免一处不可解释的硬过滤屏蔽所有其他来源；保留 rejected/pending 原因。不要直接删除已有防 ghost surface 的处理。

### 3.7 现有导航/query 不是完整全局切换目录

隐藏/最小化会移出 strip；query 从 strip 和可见 Space 的可见浮动窗口构建，`WindowUnavailable` 也被 available 查询排除。没有显示在这些结果中，不能反推“窗口不存在”。bar 的投影已经与 query 不完全相同。[S6][S7][S9]

**设计差异**：当前 Space 的方向焦点导航可以只选可操作可见窗口；全局切换器则可以显示最小化/隐藏/其他 Space 的窗口，并在选择时执行恢复/跨 Space 激活。后者应是明确的新选择语义，不能悄悄把所有方向导航都改成跨桌面恢复。

## 4. DockDoor 实际如何选择切换窗口

### 4.1 先发现和缓存，再按使用场景生成列表

实际链路：SCK/AX 发现 → AX/CG 身份映射与候选检查 → 共享窗口缓存 → `getAllWindowsOfAllApps()` → `buildSwitcherWindows()` → 模式筛选、排序、分组、可选 app-only 项。[D1][D2]

切换器的最终列表没有 Regular-only 门槛；但**发现来源的覆盖范围不同**：

| 来源/阶段 | DockDoor 的实际行为 | 对 Accessory 的意义 |
| --- | --- | --- |
| 启动 AX seeder、全局 AX fallback、常规 AX observer | 只接受 Regular 应用 | 与 Spool 相似，不能靠这些入口完整发现独立 Accessory。[D1][D5] |
| 全局 SCK 路径 | owner 不要求 Regular，入库只排除 Prohibited；随后进行窗口级检查 | Accessory 有可达路径，不是整个应用一律排除。[D1] |
| 最终 switcher 列表 | 读取所有 PID 的共享窗口缓存，无额外 activationPolicy 检查 | 已成功入库的 Accessory 窗口可继续参与切换。[D2] |
| 无窗口应用兜底 | 只补 Regular 应用，并创建 `isWindowlessApp=true` 的合成项 | 不是实际窗口，也不能救回尚未发现的 Accessory 窗口。[D2] |
| Dock 悬停预览 | 从被命中的 Dock item 解析应用，读该应用缓存并定向刷新 | 与全局切换器不是同一入口；菜单栏应用不一定有可命中的 Dock item。[D6] |

因此，不能说 DockDoor “完全支持所有菜单栏应用窗口”，也不能说它“与 Spool 一样一律排除 Accessory”。

### 4.2 哪些窗口会进入候选

| 条件 | DockDoor 行为 | 可以借鉴 / 不应照抄 |
| --- | --- | --- |
| 标准窗口与独立 dialog | 通用 AX 分支接受 StandardWindow/Dialog，通常要求 normal level；level 未知也按正常层级继续，另有应用特例 | 比 Spool 仅 Standard/Floating 的枚举更丰富；仍不是完整语义分类。[D3] |
| 工具/浮动窗口 | 普通 floating-level dialog 可能被拒，用于排除提醒 toast；有 Adobe、scrcpy 等例外 | 防噪声有价值，但平铺管理器不能把“层级非零”统一解释为用户不需要的窗口。[D3] |
| SCK 候选 | owner 存在、onscreen、layer 0、默认至少 100×100；映射 AX 后，关闭/最小化按钮元素存在，或判别器通过即可继续 | 按钮检查只是存在，不是 enabled/可聚焦/可缩放证明；入口之间也不是统一谓词。[D1] |
| AX 候选 | ID/CG 映射、几何有效、CG alpha >0.01、状态/Space 检查；实际默认最小尺寸 100×50 | 与 SCK 100×100 不同；不能复制成 Spool 的统一小窗口排除阈值。尺寸过滤有用户关闭开关。[D3][D4] |
| 空标题 | 没有全局“标题必须非空”要求；可按 ID/几何匹配，部分应用特例要求标题 | 值得借鉴：未命名窗口不应仅凭空标题消失。[D3][D4] |
| 隐藏、最小化、全屏、其他 Space | AX 路和缓存保留逻辑区分这些状态；可进入共享缓存，随后按用户选项过滤 | 值得借鉴：目录完整性与特定导航视图分开。不是保证每种状态的新窗口都能成功被发现。[D3][D4] |
| 当前 Space 的 offscreen 普通窗 | 未最小化、非全屏、应用未隐藏时可能被判 ghost | 合理的组合去噪思路；不能拿 offscreen 单项直接删窗口。[D3] |
| 图片捕获失败 | SCK/AX 捕获用可失败结果，仍能缓存 image=nil 窗口；UI 可降级 compact | 身份/导航资格与图像可用性分开，适合借鉴。[D4][D9] |
| 用户排除 | 精确 bundle / 不区分大小写的应用全名；标题黑名单为不区分大小写的子串 | 可借鉴显式排除，但语法和匹配优先级需与 Spool 自己的规则一致。[D4][D7][D9] |

在该快照的默认设置下：switcher 包含隐藏/最小化窗口；仅当前 Space、仅当前显示器默认关闭；无窗口应用项默认开启。Dock 预览有独立的对应开关和单窗口应用开关，不能用 Dock 预览是否出现推断 switcher 是否入选。[D2][D6][D7]

### 4.3 DockDoor 本身的限制

1. **发现与录屏/图片开关存在耦合。** `shouldCaptureWindowImages()` 为 false 时跳过整个 SCK 枚举；Regular 还有常规 AX fallback，独立 Accessory 没有对应完整兜底。图片捕获单次失败与根本不运行 SCK 发现是两件事。[D1][D4]
2. **入口资格不统一。** SCK/AX 的尺寸阈值、level 检查、按钮绕过分支不同，同一窗口能否入库可能取决于发现路线。[D1][D3][D4]
3. **相当依赖应用特例与映射启发式。** 标题/几何回退可能误配；private remote token 暴力补齐有次数与退避边界，不保证找全。[D3][D8]
4. **已有缓存与新发现的门槛不同。** 对最小化/隐藏窗口这是必要保护，但静态源码不能证明所有状态/配置改变都即时收敛。[D4]
5. **app-only 项不是 window。** 无窗口兜底根据经过部分 switcher 过滤的列表检测，“没有列表项”不等于应用真的没有窗口；不能把它当作窗口跟踪完整性的证明。[D2]

对 KeepingYouAwake 的严谨结论：**有条件入选的源码路径存在，但本次没有证明它在 DockDoor 中实际出现。** 若依赖全局 SCK 路径首次发现，还需 SCK 可见性、layer/尺寸、AX 映射、过滤配置和权限共同满足；不能把这些条件概括为所有定向 AX 刷新入口都必需。可以参考其缓存与视图分层，不应宣传为一个已经验证的兼容性答案。[D1][D6]

## 5. AltTab 提供的更直接参考

### 5.1 发现范围与应用资格

这个快照的 AltTab **不是 Regular-only**。它监听 `NSWorkspace.runningApplications`，并从跨 Space 的 WindowServer 清单按 owner PID 补发现应用；`ApplicationAdmissionResolver` 拒绝 zombie，默认排除无用户交互证据的普通 XPC，但允许已知例外或 `attention` 证据放行。应用 AX handle 在 policy 不为 Prohibited 时建立。Regular 条件主要用于“没有窗口时是否生成应用占位项”，不等于真实窗口的准入条件。[A2]

`attention` 是它的焦点/交互事件管线提供的证据，不是“任何被点击的 surface 自动成为平铺窗口”。即使已经建立应用记录，窗口仍需自己的物理与语义判断；XPC 放行也不等于所有 XPC surface 均可切换。[A2][A3]

### 5.2 物理清单、准入、列表是不同层

源码将这些职责明确分开：

```text
WindowServer 跨 Space 清单 / 事件
    -> WindowSurfaceInventory：原始物理 surface 与父子关系
    -> WindowElementAcquisition：寻找 AX 语义句柄
    -> WindowAdmissionResolver：是否为独立切换目标
    -> 已跟踪 Window 状态
    -> WindowFilterResolver：某次切换列表是否显示
```

这是主路径的职责图，不表示只有一个串行入口。用户交互证据有先建立候选、再补 AX 的路径；后台 native tab 另有补发现路径。未解析的物理 surface 不直接占用正式窗口身份，父子 surface 的代表窗口解析限制在同一 PID，并防止环和缺失父记录被当作可信关系。[A3][A4]

**这一层次比“把另一款软件当前显示的列表照搬过来”更适合 Spool。** 拿走的是独立用户窗口的识别依据，不是窗口缩略图、切换器排序或仅当前 Space 的显示设置。

### 5.3 实际窗口准入规则

`WindowAdmissionResolver` 的结果是 `destination`、`represent(parent)`、`latent`、`reject`，不是一个 Bool。主要判断按以下顺序执行，较早的拒绝不能被后面的分支覆盖：[A3]

| 输入 | AltTab 结果 | 对 Spool 的意义 |
| --- | --- | --- |
| ID 无效 | reject | 身份有效性仍是前提 |
| 有 WindowServer parent ID | represent(parent)，不作为独立目标 | 借鉴父子关系，但不是仅检查 AXSheet 名称；关系缺失时不保证自动识别所有 sheet |
| `AXFloatingWindow` 或 `AXSystemDialog` subrole | reject，即使 main 或 attention 也不放行 | **不能照抄**：其中可能有 Spool 希望跟踪并 float 的独立工具窗 |
| 无 AX 语义的普通发现 | latent，等待补齐 | 未确定不等于已忽略 |
| 普通发现的 placement | level 0，或 fullscreen，或 AXMain 为真，才继续 | 非零 level 不等于一律拒绝；AXMain 也不是完整的可操作能力证明 |
| `AXWindow + AXMain` 或 `AXStandardWindow` | destination，无统一标题/最小尺寸要求 | 小尺寸、空标题不应成为全局拒绝条件；Standard 分支未另外检查 role |
| `AXDialog` | 有标题则 destination，无标题通常 latent | 前面的 main/attention 分支可接受无标题 dialog，不是所有无标题 dialog 都被拒 |
| 自定义 AXWindow root | 非空标题且至少 100×50 可成为 destination，否则继续区分 latent/reject | 尺寸是这一类的证据，不是所有用户窗口的共同阈值 |
| attention 路径 | 仍受 ID、parent、auxiliary subrole、placement 限制；语义缺失可暂收，明确非窗口 role 拒绝 | 用户交互可增强证据，但不能覆盖已知反证 |

100×50 还用于普通 AX 语义获取的预筛：无 parent 的 normal-level surface 可进入，其他 level 需满足尺寸。因此“小型非零 level 的 main 窗口”即使在已有语义时可被准入，也不保证从普通获取入口找到。**准入谓词通过不等于所有发现路线都可达。**[A3][A4]

此 resolver 没有 app/bundle 参数，不含 DockDoor 那套按应用名放行窗口的表；XPC 应用例外与用户显示例外在其他层。已核对上游测试中标准 1×1 窗口、无标题 dialog、parent 优先于 attention、浮动 subrole 强拒绝等输入，但没有运行这些测试。[A2][A3][A8]

### 5.4 发现不等于一次 AXWindows 枚举

AltTab 从私有 `CGSCopyWindowsWithOptionsAndTags` 的跨 Space ID 清单与 `SLSWindowQueryWindows` 批量查询获得 owner、bounds、parent、level、alpha、tags 等物理事实。AX 获取先合并应用发布的 windows、focused、main 窗口，再按 window ID 匹配；其他 Space 的缺项可用 private remote-token 枚举补齐。一次 inventory 按 PID 合并获取，单次遍历有时间预算，失败按窗口集合版本限次，不保证每次找全。[A4][A5]

特别值得借鉴的是 `WindowElementAcquisition.Outcome`：

- `found`：已找到可信的 AX 元素。
- `absent`：在该路线的语义下，应用成功回答，但未列出目标。
- `noAnswer`：应用未回应，或有界搜索没有找全，不能作为窗口已关闭的证据。

`absent` 也不是任何跨 Space 搜索失败的同义词：remote-token 搜索耗尽返回 `noAnswer`。Spool 的 `WindowUnavailable` 已体现相似保护，应将这种“不确定与否定分开”的原则延伸到新窗口分类，而不是只在已有窗口失联时使用。[A5][S6][S10]

### 5.5 最终切换列表另行过滤

`WindowFilterResolver.shouldShow` 消费已构造的窗口状态，而非重新执行 role/subrole/尺寸分类。共同过滤包括 phantom、用户隐藏例外、应用范围、隐藏应用显示选项；真实窗口再根据 fullscreen、minimized、Space、显示器和 inactive native tab 设置筛选。后面还可选每应用一个代表项并应用搜索。这里“不显示”通常是改变 `shouldShowTheUser`，不是删除跟踪身份。[A7]

该快照默认显示 minimized/hidden/fullscreen，覆盖全部 Space/显示器，native tabs 合为一个窗口；windowless 应用项是占位，而非证明应用有一个真正窗口。这些默认值是产品偏好，不应成为 Spool 的全局准入条件。[A7]

用户 exception 的 bundle 采用大小写敏感前缀，标题采用大小写敏感子串，和 DockDoor 的匹配方式不同；其 `ignore` 控制快捷键抑制，`hide` 控制列表隐藏，不能按名字直接映射成 Spool 的 Ignored Window。[A7]

phantom 检测组合可见性、alpha、Space、minimized/hidden/tab 状态，不能简化成 offscreen 就删除。不同检测入口的豁免顺序也不相同，不宣称隐藏或最小化窗口对所有 ghost 判定都免疫。[A9]

### 5.6 复用实现时的成本

- **不是纯 AX 实现。** 批量 WindowServer 查询、私有事件与 tags 解码、remote-token 补齐都是该方案的一部分；参考产品语义不要求 Spool 原样搬入所有私有调用。[A4][A5]
- **录屏不是此发现主链的必需前置。** 该版本允许用户跳过录屏授权，以 `skipped` 状态通过启动检查；AX 授权仍需满足。不要把“无缩略图”变成窗口消失，也不要把这个事实扩大成无需任何权限。[A6]
- **并发模型不能直接移植。** AltTab 将批量私有查询和外部 AX IPC 调度到后台，再回主线程更新模型；Spool 当前桥接规则不同，采纳识别规则不等于采纳它的线程安排。[A4][A5]
- **窗口切换资格不是完整控制能力。** 只有物理/交互证据的候选不应自动获得移动、缩放、关闭权限。Spool 必须逐项验证可信目标及 backend 能力；AX 不可用也不应被宣称为一切私有激活路线均不存在。

## 6. 对“用两者发现逻辑决定纳入管理”的判断

**赞同，前提是这里的“纳入管理”指 Spool 的 Tracked Window，而不是 Tiled Window。** 按 [现有术语](../CONTEXT.md)，float 仍然是被跟踪的窗口。这不是要否定用户的方向，而是要精确选择上游逻辑中的哪一层。

与之并列的平铺策略应参考 yabai 等平铺窗口管理器，单独决定“默认是否应该平铺”与“当前是否能执行布局”，不能只依赖窗口切换器的候选判定。详细比较与能力/用途/状态分层见 [平铺资格研究](window-tiling-policy.md)。

建议的产品准则：

> 用户有理由独立找到并切回的持久应用窗口，应进入 Spool 的跟踪候选；不能因为它不适合平铺、应用没有 Dock 图标或当前不可见而直接排除。

“持久”指它代表一个可返回的交互目标，不应实现成简单的存活毫秒数阈值。独立设置页/工具页与父窗口 sheet、菜单、tooltip 的区别需要窗口关系和状态证据。

| 可以采纳的上游逻辑 | 应归属 Spool 的决策 | 不应混入的条件 |
| --- | --- | --- |
| 物理 surface、AX 语义、独立父子关系、交互证据、已知噪声特征 | 窗口发现与跟踪资格 | 是否可缩放、是否有缩略图 |
| 最小化、应用隐藏、其他 Space、搜索与用户显示偏好 | 某个导航视图的候选 | 不能因此销毁跟踪身份 |
| 实际移动/缩放能力、用户布局规则（用途偏好也属于规则） | float / tile / 可用操作 | 不能反向排除只适合 float 的窗口 |

优先参考 **AltTab 的准入分层与证据模型**，以 **DockDoor 的多入口和应用兼容性案例**补充测试。不是把两个项目的通过结果做简单 OR：某个项目误收 overlay，另一个项目漏收工具页，机械并集无法辨别两者。也不采用交集，否则只会加重漏收。尤其 AltTab 明确拒绝的 floating subrole，需要按 Spool 自己的独立工具窗需求重新判断。[A3][D1][D3]

建议最终让分类结果可解释，例如 `tracked: independent-settings`、`floating: resize-unsupported`、`pending: ax-no-answer`、`ignored: attached-transient-surface`。这些是建议的诊断语义，不是要求立即引入特定字符串或复杂的新状态框架。

KeepingYouAwake 设置页应成为回归样例：跟踪，采用模板偏好或确认能力受限时 float；菜单不独立跟踪。判断不依赖它是否出现在当前 AltTab/DockDoor 列表。本次未实测两款切换器对它的最终结果。

## 7. 哪些现有边界应保留

- `WindowUnavailable` 与销毁分开；不能为了切换列表完整而向失效 AX 对象持续写入。[S6]
- 隐藏/最小化时停止平铺与尺寸写入；但可保留目录中的身份与恢复动作。[S7]
- 系统/临时 surface 与普通用户窗口分开。bar 的只读 presentation 候选不能作为生命周期或可操作窗口的权威依据。[S9]
- 固定尺寸默认 float，而不是不断发出无法满足的缩放请求。[S5]
- 空标题、窗口小、截图暂时不可用应作为组合证据或降级信号，而不是单独的全局排除条件。具体阈值需实测，不照抄其他项目。

## 8. 待决策的行为表

以下是建议验收语义，不是当前全部已实现：

| 情形 | 跟踪/目录 | 全局切换 | 当前 Space 方向导航 | 默认布局 |
| --- | --- | --- | --- | --- |
| Regular 标准文档窗口 | 是 | 是 | 当前可见且可操作时 | 能力允许则 tile |
| Accessory 的独立设置窗口 | 是 | 是 | 当前可见且可操作时 | float |
| 独立工具窗口/独立 dialog | 按实际独立交互性判断 | 通常是 | 按焦点/父子语义 | float |
| attached sheet | 与父窗口关联 | 不默认重复为独立项 | 激活时导向正确的 modal target | 不独立 tile |
| 菜单、tooltip、普通 popover | 通常不作为独立 tracked window | 否 | 否 | ignore |
| 最小化/隐藏应用的窗口 | 保留 | 可显示并恢复 | 默认否 | 暂停布局，恢复原策略 |
| 其他 Space 的窗口 | 保留 | 可按配置显示并切换 | 当前 Space 默认否 | 所属 Space 的布局 |
| 标题暂时为空/AX 元数据暂时失败 | pending 或保留已知身份 | 可用旧记录/占位，按能力禁用动作 | 未确认时不操作 | 不贸然改分类 |
| 只有 WindowServer surface，无可信 AX 目标 | 观测记录或有证据的候选 | 有可信激活路线才允许导航，不假定其他操作可用 | 按已验证能力 | 无真实几何能力时不 tile |
| 用户明确忽略 | 按明确策略 | 按导航策略 | 否或按配置 | 不 tile |

## 9. 建议顺序与回归场景

建议按以下顺序讨论，不先重写全部窗口管理层：

1. 明确“跟踪、导航、平铺”三个独立决策及拒绝/等待原因；同一观测身份可以服务多个视图。
2. 补足 Accessory 的用户窗口发现，并建立 Regular/Accessory 的启动前后行为测试。
3. 完善独立 dialog、sheet、工具窗的关系与能力分类，而不是扩大一个 subrole 白名单后默认全部 tile。
4. 增加完整窗口目录/全局切换投影的语义；保留当前 Space 方向导航的局部性。
5. 最后统一明确的用户规则与确定优先级，替换不能解释原因的散落过滤。

特别不建议：把 DockDoor 的 SCK 发现依赖、两款项目的最小尺寸门槛或应用特例表原样搬进 Spool。借鉴职责分层、保存失败原因并建立兼容性样例，比复制全部判别条件更有价值。

建议后续实现前先固定测试输入与期望：

1. Spool 启动前/后打开 KeepingYouAwake 设置页，窗口均能进入候选并默认 float，菜单本身不进入。
2. 独立 AXDialog、附属 sheet、可缩放 floating panel 分别验证，不用一个“非标准窗口”样例替代。
3. 窗口创建后 title/subrole/可写性前几次查询失败，恢复后能收敛；不误判销毁，不提前写 frame。
4. 两条相互冲突的规则在多次启动、重载后保持同一结果；显式忽略与强制跟踪冲突有规定。
5. 隐藏/最小化/非活动 Space 窗口仍可保留全局目录，但不会突然被当前方向键激活。
6. AX 清单与 WindowServer 候选不一致时，记录来源和原因；私有标签变更不导致静默永久遗漏。
7. 截图失败、没有录屏权限与无法观察窗口身份分别测试，不能绑定为同一个排除结果。
8. 在“仅当前 Space”“隐藏最小化”“搜索无匹配”等导航过滤下，跟踪目录不被误删。
9. 用户切入此前未接受的独立窗口、父子窗口改绑、后台 native tab 与真实独立窗口分别验证，避免误收或重复布局。

除前一轮 KeepingYouAwake 的只读观察外，本次没有运行真实窗口切换实验、修改权限或执行 Spool 回归测试。静态风险需要以上测试才能升级为具体修复结论。

## Spool 来源

- **[S1] 应用策略**：[process.rs](../../src/manager/process.rs)，`is_observable:252`、`ready:389`。
- **[S2] 发现入口**：[systems.rs](../../src/ecs/systems.rs)，启动 `gather_initial_processes` 中 Regular/force 检查约 850 起，运行时 `process.ready()` 约 412 起。
- **[S3] 窗口构造与规则放行**：[windows.rs](../../src/manager/windows.rs)，`new_with_config:312`、`is_forced_track:362`、`is_real:388`。
- **[S4] 私有清单过滤**：[manager.rs](../../src/manager.rs)，`find_existing_application_windows:686`、`space_window_list_for_connection:952`、`found_valid_window:1041`。
- **[S5] 初始布局/retile**：[triggers.rs](../../src/ecs/triggers.rs)，`window_is_fixed_size:119`、retile 固定尺寸保护约 996、`apply_window_defaults` 中 fixed_size 约 1583；[ecs.rs](../../src/ecs.rs) 的 `WindowProperties::floating:877`。
- **[S6] 暂时不可用**：[reconcile.rs](../../src/ecs/reconcile.rs)，`suspend_window:1080`；[params.rs](../../src/ecs/params.rs)，`AvailableWindows:259`。
- **[S7] 隐藏/最小化与 strip**：[triggers.rs](../../src/ecs/triggers.rs)，visibility 事件约 677 起、`window_visibility_trigger:905`。
- **[S8] 当前 Space 导航**：[commands.rs](../../src/commands.rs)，`visible_floating_entities:280`。
- **[S9] 展示投影**：[state.rs](../../src/ecs/state.rs)，query 提取约 892 起、window set 约 700 起；[bar/state.rs](../../src/bar/state.rs)，`project_windows:129`、`window_record:200`；[manager.rs](../../src/manager.rs) 的 presentation 与 lifecycle inventory 约 904-936。
- **[S10] AX 两类清单入口**：[app.rs](../../src/manager/app.rs)，`window_inventory:278` 与 `window_list:323`。
- **[S11] 配置表达能力**：[config.rs](../../src/config.rs)，`find_window_properties:193`、`should_force_track_process:213`、HashMap 存储约 638、`WindowParams:798`；[ecs.rs](../../src/ecs.rs) 的 `WindowProperties::floating:877`。
- Apple role 名称依据：本机 macOS SDK 26.4 的 `ApplicationServices.framework/Frameworks/HIServices.framework/Headers/AXRoleConstants.h`，`AXSheet`、`AXMenu`、`AXPopover`、`AXDialog`、`AXSystemDialog`、`AXFloatingWindow`；角色名称本身不规定 Spool 的产品策略。

## DockDoor 固定提交来源

- **[D1] 发现路径与 Accessory/SCK 入口**：[WindowUtil.swift:1041](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/Window%20Management/WindowUtil.swift#L1041)、[SCK gates:1155](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/Window%20Management/WindowUtil.swift#L1155)、[image discovery gate:301](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/Window%20Management/WindowUtil.swift#L301)。
- **[D2] 最终切换器及 app-only**：[KeybindHelper.swift:129](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/KeybindHelper.swift#L129)、[WindowUtil.swift:747](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/Window%20Management/WindowUtil.swift#L747)。
- **[D3] 候选判别、几何、应用特例、ghost/Space 状态**：[WindowDiscoveryShared.swift:136](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/Window%20Management/WindowDiscoveryShared.swift#L136)、[predicate:181](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/Window%20Management/WindowDiscoveryShared.swift#L181)、[CG/Space:426](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/Window%20Management/WindowDiscoveryShared.swift#L426)、[validity:575](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/Window%20Management/WindowDiscoveryShared.swift#L575)。
- **[D4] 身份映射、AX 入库、截图可失败及缓存保留**：[WindowUtil.swift:682](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/Window%20Management/WindowUtil.swift#L682)、[capture:1247](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/Window%20Management/WindowUtil.swift#L1247)、[AX candidates:1342](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/Window%20Management/WindowUtil.swift#L1342)、[purify:1526](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/Window%20Management/WindowUtil.swift#L1526)。
- **[D5] Regular-only AX seeder/observer**：[WindowSeeder.swift:5](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/Window%20Management/WindowSeeder.swift#L5)、[WindowManipulationObservers.swift:91](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/Window%20Management/WindowManipulationObservers.swift#L91)。
- **[D6] Dock 悬停与定向刷新**：[DockObserver.swift:334](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/DockObserver.swift#L334)、[WindowUtil.swift:919](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/Window%20Management/WindowUtil.swift#L919)。
- **[D7] 默认设置**：[consts.swift:116](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/consts.swift#L116)、[consts.swift:149](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/consts.swift#L149)、[filters:268](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/consts.swift#L268)。
- **[D8] AX/private ID 映射与有界补齐**：[AXUIElement.swift:51](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Extensions/AXUIElement.swift#L51)、[AXUIElement.swift:117](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Extensions/AXUIElement.swift#L117)。
- **[D9] 无图降级与应用名匹配**：[WindowPreviewHoverContainer.swift:1149](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Views/Hover%20Window/WindowPreviewHoverContainer.swift#L1149)、[WindowUtil.swift:309](https://github.com/ejbills/DockDoor/blob/d2a8e2bb388c7664f28b7b844e2de60380ac8c31/DockDoor/Utilities/Window%20Management/WindowUtil.swift#L309)。

## AltTab 固定提交来源

- **[A1] 版本快照**：[release commit](https://github.com/lwouis/alt-tab-macos/commit/2f3c67739211f790b8f949f427ccb0575c8684a8)、[changelog.md](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/changelog.md#L1)。
- **[A2] 应用发现与准入**：[RunningApplicationsEvents.swift:12](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/events/RunningApplicationsEvents.swift#L12)、[ApplicationState.swift:14](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/switcher/state/ApplicationState.swift#L14)、[ApplicationDiscriminator.swift:2](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/switcher/state/ApplicationDiscriminator.swift#L2)、[Application.swift:145](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/switcher/state/Application.swift#L145)、[Applications.swift:1177](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/switcher/state/Applications.swift#L1177)。
- **[A3] 窗口准入及真实调用**：[WindowAdmissionResolver.swift:5](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/switcher/state/WindowAdmissionResolver.swift#L5)、[resolve:103](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/switcher/state/WindowAdmissionResolver.swift#L103)、[Windows.swift:544](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/switcher/state/Windows.swift#L544)、[attention candidate:630](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/switcher/state/Windows.swift#L630)。
- **[A4] 物理清单、私有发现及调度**：[WindowSurfaceInventory.swift:5](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/windowserver/WindowSurfaceInventory.swift#L5)、[Applications.swift:181](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/switcher/state/Applications.swift#L181)、[WindowServerQuery.swift:7](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/windowserver/WindowServerQuery.swift#L7)、[WindowServerEvents.swift:48](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/events/WindowServerEvents.swift#L48)。
- **[A5] AX 获取与失败分类**：[WindowElementAcquisition.swift:11](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/windowserver/WindowElementAcquisition.swift#L11)、[PublishedWindows.swift:15](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/windowserver/PublishedWindows.swift#L15)、[AXUIElement.swift:210](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/macos/api-wrappers/AXUIElement.swift#L210)。
- **[A6] 权限与图片降级**：[SystemPermissions.swift:70](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/macos/SystemPermissions.swift#L70)、[recording skipped:140](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/macos/SystemPermissions.swift#L140)、[WindowThumbnails.swift:7](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/switcher/state/WindowThumbnails.swift#L7)。
- **[A7] 最终显示过滤及默认选项**：[WindowFilterResolver.swift:14](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/switcher/state/WindowFilterResolver.swift#L14)、[Windows.swift:118](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/switcher/state/Windows.swift#L118)、[ExceptionMatcher.swift:38](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/switcher/state/ExceptionMatcher.swift#L38)、[Preferences.swift:54](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/preferences/Preferences.swift#L54)。
- **[A8] 上游测试输入**：[WindowAdmissionResolverTests.swift:20](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/switcher/state/WindowAdmissionResolverTests.swift#L20)、[WindowFilterResolverTests.swift:68](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/switcher/state/WindowFilterResolverTests.swift#L68)。
- **[A9] Phantom 判定**：[PhantomWindowDetector.swift:38](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/switcher/state/PhantomWindowDetector.swift#L38)、[Window.swift:177](https://github.com/lwouis/alt-tab-macos/blob/2f3c67739211f790b8f949f427ccb0575c8684a8/src/switcher/state/Window.swift#L177)。
