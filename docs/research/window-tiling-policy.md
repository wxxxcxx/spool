# Spool 平铺资格策略：yabai / AeroSpace / Rift 对照

核查日期：2026-09-06。范围：窗口已经进入跟踪候选后，如何判断默认 tile/float、当前是否允许布局，以及用户规则如何覆盖。仅研究与文档更新，不修改运行代码、配置、权限或窗口状态。

后续实施说明（2026-09-06）：本文保留研究时的 Spool 快照和上游事实，当前契约见[窗口规则](../WINDOW_POLICY.md)。用途偏好不进入核心启发式，改为可编辑默认 Lua 规则；推荐的“默认 float”以采用相应用户规则或存在明确能力限制为前提。尚未部署或进行真实桌面验收。

## 1. 与窗口准入分开

本文与 [窗口排除策略](window-exclusion-policy.md) 互补：

- **跟踪资格**参考 AltTab / DockDoor 的独立用户窗口识别，不以能否平铺为前提。
- **平铺策略**参考平铺窗口管理器，但不能把它们名为 `managed` 的变量直接翻译成 Spool 的 Tracked Window。
- **当前执行资格**回答此刻是否可以写入几何，不应反向改写窗口身份或用户布局意图。

按 [领域术语](../CONTEXT.md)，Floating Window 仍是 Tracked Window。KeepingYouAwake 设置页应是“跟踪，但默认 float”的验收样例，而非“平铺失败后忽略”。

源码快照：

| 项目 | 固定版本/范围 |
| --- | --- |
| Spool | HEAD `e2c4406f0c50509d3beae2cb066b4b8b0c25f209` 加核查时既有未提交修改，非纯净提交快照 |
| yabai | `dd845723416f5fe92af49fad5ebab00369e07edd`，提交时间 `2026-06-14T14:40:04+02:00` |
| AeroSpace | `39e519044725694635712c739df9ca40ae78c5d1`，提交时间 `2026-09-05T23:23:21Z`，main 快照 |
| Rift | `beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1`，提交时间 `2026-09-04T15:11:01Z` |

固定快照用于可复核比较，不声称为每个项目的最新发布版。源码/测试阅读不是实际应用兼容性或私有 API 的本机成功认证。

## 2. 当前 Spool 的依据与缺口

| 当前实现 | 已有价值 | 待补的策略边界 |
| --- | --- | --- |
| `AXSize` 明确不可写则默认 float | 防止固定尺寸窗口占用 strip；已有相应 mock 测试 | 可缩放不等于可移动，也不等于适合当前布局。[S1][S2] |
| 规则 `floating=true` 则 float；否则通常 tile | 已能由用户选择浮动 | `AXFloatingWindow` 只影响跟踪准入，未自动成为浮动分类依据。[S1][S3] |
| AXSize 查询失败不会作为固定尺寸证明 | 没有把错误直接解释为不可缩放 | 当前随后保留配置策略，默认可继续 tile；需要独立的能力未知/等待执行语义。[S1] |
| 全屏默认处理延期；固定尺寸 retile 有保护 | 已有重要的运行时边界 | 初始分类、恢复、规则重算、手动 retile 应使用一致的能力与策略语义。[S1] |
| AX 写后读回实际 frame | 能发现失败或应用尺寸约束造成的不一致 | 应把实际结果用于有界收敛；不能把 setter 成功等同于目标尺寸已实现。[S4] |

固定尺寸测试覆盖默认路径不发出 frame 写入，不代表“浮动窗口永远不能移动”，也不是所有带 grid 规则或显式操作的路径都已经被该测试覆盖。[S2]

## 3. yabai：跟踪、资格、平铺状态分别判断

### 3.1 它的 managed 在这里主要表示 tile

yabai 先尝试观察并保存窗口，再判断 root 窗口资格和 tile/float；AXUnknown 或观察失败仍可能提前退出，不能把源码中“跟踪所有窗口”的注释当作无条件保证。其文档明确把 `manage` 描述为 tile 与 float 的选择。[Y1][Y5]

窗口资格接受 root 上的 AXWindow + Standard/Floating/Dialog，或符合条件的强制规则。root 也不只是 WindowServer parent 为零：构造时还会参考 AXParent 是否指向应用。自动平铺则更严格，默认要求标准窗口、normal level、可移动。[Y1][Y2]

### 3.2 默认浮动不是单一的 can-resize 判断

在初始 eligible root 窗口、未被隐藏/最小化/全屏或强制规则提前分流的路径上，下列任一条件会设置 floating：[Y1]

| 条件 | 解释 |
| --- | --- |
| sticky | 不作为普通单一 Space 平铺目标 |
| 不能移动 | 自动布局无法安置它 |
| 不是标准窗口 | dialog / floating 等可被跟踪，但默认不自动平铺 |
| 不是 normal level | 避免把层级特殊的工具/HUD 等当普通布局目标 |
| 不能缩放且尺寸偏小 | `width <= 500` **或** `height <= 500`；不是“两边都小”，也不是所有不可缩放窗口都 float |

因此，一个大于 500×500、可移动的不可缩放标准窗口，可能仍通过默认平铺路径；普通 float-to-tile 检查也没有统一要求可缩放。**这是 yabai 的取舍，不是建议 Spool 照抄 500 阈值。** 对任意调宽调高的 strip，能移动但不能满足目标大小仍会产生约束冲突。[Y1][Y2][Y3]

### 3.3 能力读数、规则与临时状态

- 可移动/缩放分别来源于 `AXPosition` / `AXSize` 的 settable 查询，失败被折叠为 false，并缓存为 flags；最小化/恢复、全屏状态变化等路径会重查。不能照搬成 Spool 的永久能力判决。[Y2][Y4]
- `should_manage_window` 先排除非 root、已浮动、sticky、minimized、隐藏应用；后面才允许 `manage=on` 绕过 standard/level/move 的默认条件。强制规则并非绕过所有上游边界，也不保证几何写入会成功。[Y1][Y3]
- 全屏退入退出现由其他事件路径移出/恢复布局，不能只阅读一个 `should_manage_window` 函数就声称它覆盖全部状态。[Y4]
- 多条规则按数组顺序合并，后面明确设置的 `manage` 覆盖前面的；对非标准 role/subrole 的 `manage=on`，还需要相应匹配字段，不能认为仅指定 app 就能强制所有窗口。规则新增与对现有窗口 `--apply` 是不同操作。[Y1][Y5]

可借鉴的是：**类型/用途启发式 + 操作能力 + 用户覆盖 + 运行状态分开**。不宜复制的部分包括尺寸魔数、错误当 false、缓存能力的具体刷新时机，以及允许规则绕过移动能力的取舍。

## 4. AeroSpace：用途启发式不是能力检测

### 4.1 初始类型与最终布局分开

真实调用链先把窗口分成 popup、dialog、window，再分别绑定到 popup 容器、工作区浮动容器、tiling tree。popup 判定优先；它不是普通 float。因此不能只摘走 `isDialogHeuristic` 而忽略其前置窗口识别。[AS1][AS2]

默认 dialog/float 启发式包括：非 Standard subrole、部分应用的特殊窗口、Firefox 的最小化按钮状态，以及多数应用的全屏按钮缺失/未启用。存在 qutebrowser、终端、编辑器等应用例外，不能写成“没有全屏按钮的窗口全部 float”。这里读的是 fullscreen button，不是 zoom/maximize button，也不直接检查通用 AXModal。[AS1]

重要的源码测试样例：**System Settings 同时列出 `AXPosition`、`AXSize` 可写，但期望分类仍为 dialog**。这直接说明“能移动、缩放”与“默认应该平铺”不是同一问题。该结论来自上游保存的 AX fixture 和分类测试，不是本次对系统设置的现场测试。[AS3]

### 4.2 它没有提供强几何能力保证

核查快照中，`AXUIElementIsAttributeSettable` 的调用在 debug dump，而不在生产分类或 frame 写入的预检路径。实际 frame setter 尝试 size、position、size；上层忽略 setter 的 success bool，失败也不会在这条路径自动切为 float。因此 **AeroSpace 的用途启发式适合参考，但不能替代 Spool 的能力/约束检测和写后读回**。[AS4]

属性读取失败通常变为 nil，nil 又可能命中“按钮未启用”“非 Standard”等分支；另一个获取不到窗口 AX 元素的入口会回退为 window。这些不同入口的失败语义也不宜原样继承。[AS1][AS4]

### 4.3 规则是有序动作，不是永久能力标签

初始分类之后才自动调用 `on-window-detected`。默认首个匹配回调执行后停止，即使 run 命令失败；`check-further-callbacks=true` 才继续，后续实际执行的布局命令可以改变之前的结果。不是“更具体规则自动优先”。[AS5]

`layout tiling` 可以将已浮动窗口强制放进 tiling tree，绕过默认 dialog 判定，但不保证目标 frame 可实现；自动回调不会救回仍被归为 popup 的对象，layout 命令本身也拒绝 popup 及某些 native 临时状态。普通刷新不会持续重跑所有窗口的 dialog 判定；全屏/隐藏/最小化返回时保留之前的 tiling/floating 归属。[AS2][AS5][AS6]

可借鉴的是用途启发式、可覆盖的默认值、明确回调顺序、临时状态返回时保留选择。不宜复制的是按钮信号的可靠性假定、大量应用例外、nil 的混合语义，以及把“已进树”当成“几何已完成”。

## 5. Rift：允许带约束的窗口参与平铺

### 5.1 manage、floating、resizable 不是同一个值

Rift 保留 `is_manageable` 启发式与 `manage_override`，布局准入要求未最小化，再应用显式 override 或默认结果。默认结果要求 standard、root、非 sticky，并根据可得 WindowServer 信息检查 layer/level；`is_resizable` 不在该准入条件里，未知 level 也不单独拒绝。[RF1]

`AXFloatingWindow` 默认不是 standard，单独写 `floating=true` 不会强制它通过管理准入；已成功注册但启发式不通过的窗口，需要 `manage=true` 才能进一步选择 floating 或 tiled。反过来 `manage=false` 也不是“作为被管理的浮动窗口”，但不等于内部跟踪记录立刻删除。更早的注册失败、Popover/Menu 等排除不能由后续规则救回。[RF1][RF4]

### 5.2 能力与约束的实际用途

AXSize 的 settable 查询失败在发现路径中回退为 true；`can_move()` 有封装但在核查源码中无调用，不能把它写成已执行的平铺条件。最终 AX frame 写入有读回路径，但 setter 返回值被丢弃；中间动画写入又是另一种处理方式，因此仍不等于统一的成功保证。[RF2]

Rift 确实将私有 WindowServer min/max 约束传给布局器：`SLSWindowIteratorGetConstraints` 返回全零数据时再试 `SLSPackagesGetWindowConstraints`。这两次调用的错误返回值未被检查，所以是“全零回退”，不是可靠的错误分类或私有 resize 接口。[RF3]

固定尺寸处理具有实际布局调用链，而非仅定义了数据结构：[RF3]

- 每轴的正数 min/max 相等时锁定该轴；整体不可缩放时可用已观测的正数宽高锁定。
- 浮动状态与约束分别处理；未浮动的固定尺寸窗口仍可加入布局树。
- 不同布局消费约束的方法不同。Traditional 会忽略部分可缩放叶子的普通 min 值以保护布局比例；BSP/Scrolling 使用约束分配和目标尺寸钳制。
- 共享求解器在固定段总和超出可用空间时，甚至会缩小数学上的 fixed 段。这不能使真实固定尺寸窗口接受缩小，因此不能从“算出了 frame”推断“真实布局一定无重叠”。

Rift 值得参考的是**布局感知的约束模型**，不是“固定尺寸可以任意 tile”。私有约束的可用性、更新时机和实际写回效果仍需目标应用测试。

### 5.3 规则来源、优先级与重算时机

规则选择匹配字段最多者，同分取配置中较早者；只使用获胜规则的完整 action，不合并其他规则。因此，更具体规则省略 `manage`，不会继承较宽规则的 `manage=true`。[RF4]

它还区分规则浮动与非规则浮动：没有新的浮动规则时，原先非规则浮动可保留；原先由规则导致的浮动则可能在规则不再要求浮动时回到 tile。`floating=false` 不是无条件覆盖手动浮动的命令。[RF4]

配置重载会重建规则引擎，但核查路径没有立即遍历所有现有窗口重新分类。重评发生于创建/发现、部分 Space 激活协调及可选标题变化；标题重应用默认关闭。优先级之外，**何时重算、是否保留手动选择**也必须成为 Spool 的显式契约。[RF5]

## 6. 三种实现不能简单合并

| 问题 | yabai | AeroSpace | Rift |
| --- | --- | --- | --- |
| 默认平铺依据 | 标准类型、normal level、移动能力等；初始固定尺寸结合大小 | 先 popup/dialog/window 启发式，dialog 默认 float | standard/root/层级等准入；floating 与尺寸约束另算 |
| 不可缩放窗口 | 小于等于任一 500 边长时默认 float；较大者未一律排除 | 不以 AXSize settable 为分类条件 | 可继续 tiled，以观测尺寸/轴约束分配 |
| 能力查询失败 | settable 错误转 false | 生产分类不查 settable；其他属性 nil 会影响启发式 | resize 错误转 true |
| 多规则冲突 | 后面的明确字段覆盖前面 | 默认首个匹配后停止；可配置继续执行 | 更具体者胜，同分较早者胜，效果不合并 |
| 强制 tile 的含义 | 放宽部分管理条件，不保证操作成功 | 改变树归属，不证明 frame 可达 | 放宽启发式准入，仍受前置发现与状态边界限制 |

上表分别由前述固定源码支持。[Y1][Y2][Y5][AS1][AS4][AS5][RF1][RF2][RF3][RF4]

结论：不能从三者挑一个 `isResizable` 或 `shouldManage` 函数作为全部答案。Spool 应单独规定**窗口用途的默认值、真实操作能力、当前布局能否满足约束，以及临时执行状态**。参考成熟实现是为了获得输入和反例，不是把所有启发式做 AND/OR。

## 7. 对 Spool 的建议

以下是待决策设计，不是已实施接口，也不要求立即增加大量 marker。

### 7.1 不用一个布尔值承担所有职责

至少区分三个结果：

| 结果 | 典型信息 | 不应混淆 |
| --- | --- | --- |
| 默认/显式布局意图 | tile 或 float；来自用户选择、规则或默认分类 | 最小化不意味着意图变成 float |
| 有效操作能力 | move / resize 的 yes、no、unknown；已知尺寸约束与证据来源 | AX 报错不等于 no；AX settable 也不是实际尺寸可达的保证 |
| 当前布局决策 | 参与布局、保持浮动、暂缓执行；附原因 | 不因暂缓而从全局窗口目录消失 |

能力应描述当前有效 backend 能做什么，而不是与某个 AX 字段永远绑定。关于 AX/私有读取、真实 resize 与视觉变换的区别，沿用 [能力矩阵](macos-window-capability-matrix.md)；不能用视觉缩放伪装真实平铺能力。

还应保留“属性声明不可写”与“经正常操作观测证实受限”的证据差别。应用可能错误报告 AX 属性；明确的用户兼容性覆盖可要求重新验证或选择另一条已验证的 backend 路线，但不能将真正不可达的目标反复强写。

### 7.2 默认规则的推荐次序

1. 已知附属 sheet/临时 surface 不独立进入 strip；处理好父子及 modal 关系。
2. 全屏、最小化、隐藏、Space 归属待确认、操作句柄暂不可用时，暂停相应执行并保留原布局意图。非活动 Space 可保留布局状态，不等于必须在其不可见时写几何。
3. 用户明确 float 优先于自动 tile；手动选择和配置规则之间的优先级要有确定定义，不能被一次普通元数据刷新覆盖。
4. 用户要求 tile 可覆盖“dialog/工具窗通常浮动”等启发式，但不能制造底层能力。确认无法移动或无法满足当前布局约束时，保留要求及拒绝原因，不持续发出注定失败的写入。
5. 工具/dialog/设置页默认 float 是可编辑用户规则，不是核心用途分类。模板提供低优先级偏好；未采用时，可移动可缩放的独立窗口默认尝试 tile。不凭 Settings 标题或 Accessory policy 猜测用途。
6. 能力未知的新窗口先等待/保留候选；已有窗口保留布局意图并暂停相关写入，通过有界重试重新确认。避免暂时失败后永久 float，也避免未确认就立即平铺。

规则匹配、能力采集和最后写入应分开。匹配逻辑适合纯函数和构造数据测试；AX/WindowServer 采集继续留在 Spool 平台/manager 边界，ECS 消费已解释的事实与结果。

### 7.3 平铺适合性最终取决于布局约束

“可缩放”并不足以说明窗口能放进任何格子：最小尺寸、最大尺寸、固定比例、单轴锁定、应用钳制都会影响实际落位。反过来，固定尺寸也不代表理论上不能排布：专门支持固定 item 的布局可以只移动它，不发 resize。

**对当前 Spool 的保守选择**是保留固定尺寸默认 float，不照搬 yabai 的“大固定窗口仍可 tile”。将来若支持固定高度/宽度或带约束的 item，应先定义 strip 如何分配空间、如何避免重叠，再允许这一类参与布局。不能只移除固定尺寸保护。[S1][S2]

不要在发现窗口时为分类偷偷试改尺寸。可先读能力/约束，在正常且已允许的布局操作中读回结果，以观测修正约束并有界重试；真正的兼容性探测另行授权。

## 8. 建议验收场景

| 输入 | 跟踪 | 推荐布局结果 |
| --- | --- | --- |
| KeepingYouAwake 独立设置页 | 是 | 默认 float；不能以禁用 zoom 按钮单独证明固定尺寸 |
| 标准文档窗、move/resize 可用 | 是 | 默认 tile |
| 可移动可缩放的独立设置/dialog | 是 | 采用用途规则则 float；核心不猜用途，无该规则时可 tile |
| 标准窗、不能移动但可以缩放 | 是 | 不自动 tile，保留不可安置原因 |
| 标准窗、不能缩放，小尺寸或大尺寸 | 是 | 当前 Spool 均默认 float，区别于 yabai 的尺寸阈值 |
| 独立 AXDialog / AXFloatingWindow，操作能力均可用 | 是，取决于独立窗口准入 | 默认 float；允许明确规则改 tile，并继续验证布局约束 |
| 新窗口的 AXSize 前几次返回错误，后恢复正常 | pending/保留 | 暂缓布局后收敛，不把错误写死为 float 或假定支持 |
| 已 tiled 窗口进入最小化/全屏再恢复 | 保留 | 暂停并恢复原意图，不重复占位或误做默认分类 |
| 多 Space sticky 窗口 | 保留 | 默认 float，未定义多 Space 布局归属前不重复放进多个 strip |
| 用户手动 float 后标题/可写性发生变化 | 保留 | 不因普通重采样覆盖显式选择 |
| 用户强制 tile，但目标尺寸被应用最小尺寸钳制 | 保留 | 调整为可行布局或报告冲突，不无限重试相同目标 |
| 多个锁定尺寸窗口超出可用空间 | 保留 | 明确选择滚动、换列或浮动等策略，不把数学缩小当真实成功 |
| 两条相互冲突的布局规则 | 保留 | 重启/重载结果确定，诊断能解释获胜规则 |

源码阅读和上游 mock/fixture 核查已进行；本文没有执行这些项目的测试、重启 daemon 或移动真实窗口。上述表是后续实现的验收输入，不是已全部通过的结果。

## 来源

### Spool

- **[S1] 默认与重新入列**：[triggers.rs](../../src/ecs/triggers.rs)，`window_is_fixed_size:119`、retile 约 996、创建时约 1385、`apply_window_defaults:1537`、初始化结束策略约 1793。
- **[S2] 固定尺寸测试**：[tiling.rs](../../src/tests/tiling.rs)，`fixed_size_window_floats_instead_of_disturbing_the_tiled_strip:507`。
- **[S3] 规则与窗口准入**：[ecs.rs](../../src/ecs.rs)，`WindowProperties::floating:877`；[windows.rs](../../src/manager/windows.rs)，`is_real:388`、`is_resizable:698`。
- **[S4] 几何写后读回**：[windows.rs](../../src/manager/windows.rs)，`observe_geometry_write_result:140`、`observe_geometry_write:569`、geometry setters 约 712 起。

### yabai

- **[Y1] 跟踪、规则、默认分类与平铺资格**：[window_manager.c:19](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window_manager.c#L19)、[rule matching:90](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window_manager.c#L90)、[rule application:171](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window_manager.c#L171)、[should_manage:272](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window_manager.c#L272)、[initial classification:1438](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window_manager.c#L1438)。
- **[Y2] 能力、阈值、root、role 与 level**：[window.c:773](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window.c#L773)、[undersized:810](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window.c#L810)、[root and type:1031](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window.c#L1031)、[create:1093](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window.c#L1093)。
- **[Y3] 显式浮动切换**：[window_manager.c:2181](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window_manager.c#L2181)。
- **[Y4] 生命周期与能力刷新**：[event_loop.c:725](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/event_loop.c#L725)、[minimize:829](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/event_loop.c#L829)、[deminimize:873](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/event_loop.c#L873)。
- **[Y5] 规则顺序与公开语义**：[rule.c:63](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/rule.c#L63)、[yabai.asciidoc:620](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/doc/yabai.asciidoc#L620)。

### AeroSpace

- **[AS1] 窗口类型启发式**：[AxUiElementWindowType.swift:3](https://github.com/nikitabobko/AeroSpace/blob/39e519044725694635712c739df9ca40ae78c5d1/Sources/AppBundle/model/AxUiElementWindowType.swift#L3)、[dialog:21](https://github.com/nikitabobko/AeroSpace/blob/39e519044725694635712c739df9ca40ae78c5d1/Sources/AppBundle/model/AxUiElementWindowType.swift#L21)、[admission:101](https://github.com/nikitabobko/AeroSpace/blob/39e519044725694635712c739df9ca40ae78c5d1/Sources/AppBundle/model/AxUiElementWindowType.swift#L101)。
- **[AS2] 类型绑定与显式布局命令**：[MacWindow.swift:204](https://github.com/nikitabobko/AeroSpace/blob/39e519044725694635712c739df9ca40ae78c5d1/Sources/AppBundle/tree/MacWindow.swift#L204)、[LayoutCommand.swift:8](https://github.com/nikitabobko/AeroSpace/blob/39e519044725694635712c739df9ca40ae78c5d1/Sources/AppBundle/command/impl/LayoutCommand.swift#L8)。
- **[AS3] 几何可写但仍为 dialog 的测试输入**：[system_settings.json5:93](https://github.com/nikitabobko/AeroSpace/blob/39e519044725694635712c739df9ca40ae78c5d1/axDumps/system_settings.json5#L93)、[AxUiElementWindowTypeTest.swift:5](https://github.com/nikitabobko/AeroSpace/blob/39e519044725694635712c739df9ca40ae78c5d1/Sources/AppBundleTests/AxUiElementWindowTypeTest.swift#L5)。
- **[AS4] 只在调试查询 settable 与实际写入**：[dumpAxRecursive.swift:16](https://github.com/nikitabobko/AeroSpace/blob/39e519044725694635712c739df9ca40ae78c5d1/Sources/AppBundle/util/dumpAxRecursive.swift#L16)、[accessibility.swift:334](https://github.com/nikitabobko/AeroSpace/blob/39e519044725694635712c739df9ca40ae78c5d1/Sources/AppBundle/util/accessibility.swift#L334)、[MacApp.swift:197](https://github.com/nikitabobko/AeroSpace/blob/39e519044725694635712c739df9ca40ae78c5d1/Sources/AppBundle/tree/MacApp.swift#L197)、[setFrame:411](https://github.com/nikitabobko/AeroSpace/blob/39e519044725694635712c739df9ca40ae78c5d1/Sources/AppBundle/tree/MacApp.swift#L411)。
- **[AS5] 检测回调与规则顺序**：[MacWindow.swift:243](https://github.com/nikitabobko/AeroSpace/blob/39e519044725694635712c739df9ca40ae78c5d1/Sources/AppBundle/tree/MacWindow.swift#L243)、[parseOnWindowDetected.swift:3](https://github.com/nikitabobko/AeroSpace/blob/39e519044725694635712c739df9ca40ae78c5d1/Sources/AppBundle/config/parseOnWindowDetected.swift#L3)、[guide.adoc:537](https://github.com/nikitabobko/AeroSpace/blob/39e519044725694635712c739df9ca40ae78c5d1/docs/guide.adoc#L537)。
- **[AS6] 原布局归属恢复**：[Window.swift:4](https://github.com/nikitabobko/AeroSpace/blob/39e519044725694635712c739df9ca40ae78c5d1/Sources/AppBundle/tree/Window.swift#L4)、[normalizeLayoutReason.swift:1](https://github.com/nikitabobko/AeroSpace/blob/39e519044725694635712c739df9ca40ae78c5d1/Sources/AppBundle/normalizeLayoutReason.swift#L1)、[closedWindowsCache.swift:53](https://github.com/nikitabobko/AeroSpace/blob/39e519044725694635712c739df9ca40ae78c5d1/Sources/AppBundle/tree/frozen/closedWindowsCache.swift#L53)。

### Rift

- **[RF1] 准入与发现**：[model/reactor.rs:99](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/model/reactor.rs#L99)、[reactor/utils.rs:14](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/actor/reactor/utils.rs#L14)、[sys/app.rs:360](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/app.rs#L360)、[registration:1582](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/actor/app.rs#L1582)。
- **[RF2] 能力与写入结果**：[axuielement.rs:278](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/axuielement.rs#L278)、[capability snapshot:360](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/app.rs#L360)、[animation writes:628](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/actor/app.rs#L628)、[frame and readback:812](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/actor/app.rs#L812)、[read errors:1787](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/actor/app.rs#L1787)。
- **[RF3] 私有约束、布局传递、求解器及测试**：[window_server.rs:208](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/window_server.rs#L208)、[constraints caller:591](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/window_server.rs#L591)、[engine.rs:1452](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/layout_engine/engine.rs#L1452)、[axis constraints:11](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/layout_engine/systems.rs#L11)、[traditional:2463](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/layout_engine/systems/traditional.rs#L2463)、[bsp:480](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/layout_engine/systems/bsp.rs#L480)、[scrolling:965](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/layout_engine/systems/scrolling.rs#L965)、[solver:12](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/layout_engine/systems/constraints.rs#L12)、[fixed-width tests:2019](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/layout_engine/systems/scrolling.rs#L2019)。
- **[RF4] 规则胜出与浮动来源**：[app_rules.rs:181](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/model/app_rules.rs#L181)、[floating effect:38](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/model/app_rules.rs#L38)、[virtual_workspace.rs:1048](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/model/virtual_workspace.rs#L1048)、[rule fields:66](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/common/config.rs#L66)。
- **[RF5] 重载与重评时机**：[reactor/events/command.rs:198](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/actor/reactor/events/command.rs#L198)、[engine config update:501](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/layout_engine/engine.rs#L501)、[Space reconciliation:341](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/actor/reactor/events/window_discovery.rs#L341)、[title event:2631](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/actor/reactor.rs#L2631)、[title reapply default:128](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/common/config.rs#L128)。
