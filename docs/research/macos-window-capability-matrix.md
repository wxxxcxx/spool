# macOS 窗口管理能力与可用实现路径

核查日期：2026-09-05。用途：为后续窗口管理器选型和实现提供能力清单，不预先决定 AX 或私有 API 优先。

本文以**其他应用的窗口**为主要对象，自有窗口单独列出。收录有 Apple 接口依据或现有项目实际调用证据的路线；不把导出的私有符号、未调用封装、理论注入方案当作已验证通用能力。这里的“可用”表示有接口契约或可复核实现，**不表示已在本机实测成功，也不保证所有 macOS 小版本、所有应用都支持**。

## 1. 阅读方式与证据范围

### 路线标记

| 标记 | 路线 | 权限与边界 |
| --- | --- | --- |
| AX | Accessibility 跨进程属性、动作、通知 | 辅助功能信任；受目标应用的 AX 实现、响应时间和状态约束。[A1] |
| CG | 公开 CoreGraphics 窗口信息 API  | 用于当前图形会话的窗口信息；不是通用远程写接口，部分字段可缺失。[A3] |
| OWN | 自有 `NSWindow` / `NSPanel` | 控制本进程持有的窗口；不能拿第三方窗口 ID 直接获得可操作的 `NSWindow`。[A4] |
| PRIVATE | 普通进程调用 SkyLight/CGS/SLS 等私有接口 | 不等于 Dock 注入，也不自动要求关闭 SIP；具体符号、权限、对象所有权及系统版本分别约束。[R2][Y2] |
| EVENT | 合成键鼠或手势 | 普通 `CGEventPost` 与未公开手势协议须分开；事件发送/监听权限分别检测，事件投递不等于目标操作完成。[A5][R4] |
| SA | yabai scripting addition，在 Dock 中执行 | 需要对应 SIP 放宽、加载权限和版本兼容；不能仅凭 `root` 或辅助功能授权假定具备。[Y5][Y6] |
| SCRIPT | 目标应用自己的 Apple Events/脚本字典 | 仅适用于目标应用实际公开的命令和属性，按 Automation 授权配置；不是所有应用通用。[A6][A7] |
| CAPTURE | ScreenCaptureKit 捕获内容后自行呈现 | 用于截图、缩略图、视觉代理；遵守录屏授权/系统选择流程，不直接控制原窗口。[A8] |

证据标记：**公开契约**是 Apple 文档/SDK；**实现证据**是固定提交中的调用链；**条件路线**表示还依赖版本、权限、能力探测或目标应用支持。所有私有实现均需目标系统验收，本文没有“本机运行已通过”项。

### 固定快照

- Rift：`beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1`，提交时间 `2026-09-04T11:11:01-04:00`。
- yabai：`dd845723416f5fe92af49fad5ebab00369e07edd`，提交时间 `2026-06-14T14:40:04+02:00`。
- Apple SDK：本机 Xcode 所选 macOS SDK **26.4**；在线文档在核查日读取。SDK 中有一个接口，不代表运行系统也具有相同接口。
- 源码永久链接列于文末。Wiki 与 Apple 在线文档未固定历史版本；本次没有关闭 SIP、授予权限、注入、切换桌面或操作真实窗口。

## 2. 查询、识别与几何读取

| 能力 | 可用实现路径 | 适用性与限制 | 证据 |
| --- | --- | --- | --- |
| 枚举某应用的窗口 | AX：应用元素的 `AXWindows` | 获得可继续请求属性/动作的 AX 元素；不能把暂时缺失或查询错误当作窗口已销毁。 | [A1][R1] |
| 枚举会话窗口、获得 ID/PID | CG：`CGWindowListCopyWindowInfo`，取 `kCGWindowNumber` / `kCGWindowOwnerPID` | 是 WindowServer 信息，不是 AX 对象列表；会包含辅助 surface，需分类。非 Quartz GUI 会话可返回 NULL。 | [A3] |
| 更细的窗口清单/树/属性查询 | PRIVATE：`SLSCopyWindowsWithOptionsAndTags`、`SLSWindowQueryWindows`、iterator | Rift 实际使用；可读父窗口、层级、alpha 等。查询失败的空结果不能无条件当作不存在。 | [R2] |
| AX 元素映射到窗口 ID | PRIVATE：`_AXUIElementGetWindow` | Rift/yabai 的具体桥接路线；不是公开 AX API，失败时需保留未知映射状态。不能用标题匹配冒充精确 ID。 | [R2][Y1] |
| 读取位置和尺寸，公开 AX 路线 | AX：分别读 `AXPosition` 与 `AXSize` | Apple SDK 有定义；单位为 points。位置是左上角全局坐标；两次读取不是原子几何快照。 | [A2] |
| 读取位置和尺寸，扩展 AX 路线 | AX 属性字符串 `AXFrame` | Rift 的 `frame()` 实际使用；本机公开 `AXAttributeConstants.h` 不定义它，不能把它当成每个 AX 元素必备的公开属性。 | [R1][A2] |
| 批量读取位置和尺寸 | CG：窗口字典的 `kCGWindowBounds` | 不需要先得到 AX 元素；是信息快照，不是写接口。窗口/字段缺失需保留未知状态。 | [A3] |
| 按窗口 ID 直接读几何 | PRIVATE：`SLSGetWindowBounds`；亦有 `CGSGetWindowBounds` 路线 | yabai 动画代码实际调用前者。Rift 的后者仅有未调用封装，不能作为它的业务路径证据。 | [Y3][R1] |
| 批量/事件路径读取几何 | PRIVATE：`SLSWindowIteratorGetBounds` | Rift `get_window/get_windows` 及窗口移动/缩放通知处理中实际调用；写后 AX 回读是另一条路径。 | [R2][R3] |
| 判断能否移动/缩放 | AX：`AXUIElementIsAttributeSettable(AXPosition/AXSize)` | 只表示当前对象报告属性可写；不是应用一定接受任意 frame 的保证。错误应与“明确不可写”区分。 | [A1][R1] |
| 读取尺寸约束 | PRIVATE：`SLSWindowIteratorGetConstraints`，全零时尝试 `SLSPackagesGetWindowConstraints` | Rift 有实际实现；私有结果不能当作所有应用的完整布局约束。公开 AX 没有本文已核实的统一 min/max-size 属性路线。 | [R2] |
| 读取角色、标题、模态状态 | AX：`AXRole` / `AXSubrole` / `AXTitle` / `AXModal`；标题也可来自 CG `kCGWindowName` | 类型描述不等于平铺策略；CG 标题为可选字段，隐私/系统条件可能限制信息，不能用缺标题认定窗口不存在。 | [A2][A3] |
| 读取前后顺序、层级、透明度 | CG：列表顺序、`kCGWindowLayer` / `kCGWindowAlpha`；PRIVATE：iterator level/alpha | “排在屏幕上”不等于无遮挡；layer/alpha 不直接给出键盘焦点，也不提供写入权限。 | [A3][R2] |
| 位置命中窗口/元素 | AX：`AXUIElementCopyElementAtPosition`；PRIVATE：`SLSFindWindowAndOwner` | AX 可能返回窗口内的子元素；私有路线返回窗口/owner。命中结果不是点击或激活成功证明。 | [A1][R2] |

**关于 `CGSGetWindowBounds` 的更正：**此前把它描述为 Rift 的快速业务读取路径过于宽泛。在固定快照中，`fframe()` 只有定义，没有调用者；实际私有读取走 `SLSWindowIteratorGetBounds`。`CGSGetWindowBounds` 不能被写成 Rift 已启用的默认路径或 AX 失败回退。[R1][R2][R3]

## 3. 移动窗口与真实缩放

### 移动窗口

| 路线 | 实现 | 是否需目标应用处理 AX 请求 | 限制与使用条件 |
| --- | --- | --- | --- |
| AX | `AXUIElementSetAttributeValue(AXPosition, CGPoint)` | 是 | 通用主路径；检查可写性，处理超时/失效，并读回实际位置。Rift/yabai 都有业务调用。[A1][R1][Y1] |
| SA | Dock 内 `SLSMoveWindowWithGroup`，随后 `SLSReassociateWindowsSpacesByGeometry` | 这条移动调用不经过 AX setter | yabai 鼠标拖动先尝试 SA，通信失败再走 AX；不是任何普通进程都能照搬该调用。移动可能改变 Space 关联，需检查结果。[Y3][Y4] |
| SCRIPT | 目标应用脚本字典暴露的 window `bounds` | 不经过 System Events 的 AX UI 脚本，但仍需目标应用处理命令 | Terminal 字典存在读写 `bounds`；不同应用需单独适配，不能覆盖全部窗口。[A6][A7] |
| EVENT | 合成标题栏拖动；支持时用系统/应用移动命令 | 不要求调用 AX setter，但目标 UI 必须响应 | 需可靠命中、前台/可见性和正确坐标；受布局、遮挡、用户同时输入影响。属于交互路线，不是后台绝对位置 setter。[A5][A6] |
| OWN | `NSWindow.setFrameOrigin` / `setFrame` | 否 | 仅本进程窗口；自有 bar/overlay 优先使用该类接口。[A4] |

普通进程中发现 `SLSMoveWindow`/`SLSMoveWindowWithGroup` 符号，不足以证明可移动任意第三方窗口。本文只把有调用上下文证据的 SA 路线列为外部窗口的私有移动实现；其他上下文应先做独立能力验证。

### 调整真实尺寸 / 设置整个 frame

| 路线 | 实现 | 限制与代价 |
| --- | --- | --- |
| AX | 写 `AXSize`；完整 frame 由位置、尺寸写入组合 | Rift/yabai 实际有 size → position → size 顺序；不是原子事务。窗口最小尺寸、应用内部布局和临时状态仍可影响结果。[R1][Y1] |
| SCRIPT | 应用支持的 `bounds` 或尺寸属性 | 适合目标应用明确的集成；命令语义和坐标约定按脚本字典核对，仍由应用执行。[A7] |
| EVENT | 合成边缘/角落拖动，或调用应用的缩放菜单命令 | 遵守原 UI 约束；需要可见目标、稳定命中及完成检查，难以作为低干扰高频布局后端。[A5][A6] |
| OWN | 自有 `NSWindow.setFrame` / 内容尺寸 API | 可控性强，但不适用于第三方窗口；AppKit 尺寸约束有方法级差异。[A4] |

**没有在本次证据中找到“普通第三方应用通用、无需 AX/应用配合、真实重排内容”的私有 resize 替代品。**这不是数学上的不可能结论，而是当前选型不能依赖的能力空缺。yabai 的 SA `SLSSetWindowTransform` 是视觉缩放，不是让应用按新尺寸重排内容；它的常规真实 resize 仍写 `AXSize`。[Y1][Y3]

跨显示器移动属于几何操作，但跨原生 Space 的 membership 是下一节的不同能力，不能仅以目标 frame 或函数名中包含 `space` 推断完成。

## 4. 焦点、激活、关闭、最小化与全屏

| 能力 | 可用路线与接口 | 限制 |
| --- | --- | --- |
| 激活应用 | 公开 `NSRunningApplication.activate(...)` | 是应用级请求，不保证某一个窗口成为 key；需要接着观察焦点状态。[A9] |
| 置前某窗口 | AX `AXRaise` | 与键盘焦点、长期层级、应用激活不是同义词；动作要查支持情况。本机实测：对后台应用的窗口调用会改变该应用自身的 `AXFocusedWindow`/`AXMainWindow`（前台应用不变），并可能把该窗口短暂置于前台应用窗口之上，见 [sls-order-window-research.md](sls-order-window-research.md)。[A1][A2] |
| 聚焦具体窗口 | 应用激活 + 支持的 AX `AXMain`/焦点属性/raise 组合；PRIVATE `_SLPSSetFrontProcessWithOptions` + `SLPSPostEventRecordTo` | 公开组合需逐项检测，不是保证成功的单一 API。Rift/yabai 私有焦点路径有实际调用，仍结合 AX raise/状态检查。[R1][R2][Y1] |
| 聚焦但不 raise | PRIVATE：yabai 的 process/event 路径 | 有独立实现，含针对应用的延迟 workaround；不能推导所有应用都支持焦点与层级完全解耦。[Y1] |
| 读取当前焦点 | AX：系统/应用 focused/main 属性；PRIVATE `SLPSGetKeyFocusProcess` + connection/window 查询 | `AXMain` 不等于 key focus。Rift 私有查询是另一观测源，失败仍可能无结果。[A2][R2] |
| 关闭窗口 | AX：取 `AXCloseButton` 后执行 `AXPress`；SCRIPT 应用 close；EVENT Command-W | 尊重应用的保存确认和阻止关闭行为；Command-W 可能关闭 tab，不一定是 window。不要替换成杀进程。[A2][A6][A7][R1] |
| 最小化/恢复 | AX `AXMinimized`；或支持的 minimize 按钮、应用脚本；OWN `miniaturize` / `deminiaturize` | 只支持具备该能力的元素；最小化与隐藏、移出屏幕是不同状态。[A2][A4][A7] |
| 隐藏/显示整个应用 | 公开 `NSRunningApplication.hide/unhide`；应用 AX `AXHidden` | 应用级，不是只隐藏其中一扇窗口。[A2][A9] |
| 原生全屏 | 支持时读取/写入应用暴露的 `AXFullScreen` 类扩展属性；公开按钮元素 `AXFullScreenButton` + `AXPress`；应用全屏菜单/快捷键；OWN `toggleFullScreen` | 扩展 AX 字符串不可假设所有应用/拼写都一致；需实际枚举/检测。按钮是元素引用。进入系统全屏是异步状态变更，不等于把 frame 填满屏幕。[A2][A4][Y8] |
| 窗口 zoom / 占满可用区域 | `AXZoomButton` + `AXPress`；或 AX 设工作区 frame | zoom 由应用解释，不保证等于最大化；直接设 frame 也不等于 native fullscreen 或系统平铺状态。[A2][R1] |

对于 `AXUIElementPerformAction`，Apple 特别说明超时可能只是目标尚未返回，不一定代表动作没执行。因此关闭、最小化、全屏切换等动作超时后，应先读状态，不要盲目重复 toggle。[A1]

## 5. 原生 Spaces 与自定义工作区

| 能力 | 可用路径 | 限制与证据 |
| --- | --- | --- |
| 查询原生 Space 拓扑/当前 Space | PRIVATE `CGSCopyManagedDisplaySpaces` / `SLSCopyManagedDisplaySpaces`、`CGSManagedDisplayGetCurrentSpace` 等 | Rift/yabai 有使用；名称相近不表示所有 SDK/系统导出都一致。CG 的 `kCGWindowWorkspace` 已在本机 SDK 标记 10.8 起不再支持，不能作为现代拓扑接口。[R5][Y2][A3] |
| 查询窗口所属 Space(s) | PRIVATE `SLSCopySpacesForWindows` | 可能有多个 ID/过渡态；读取与写入权限分开。[R2] |
| 将第三方窗口移到现有原生 Space | PRIVATE bridge：`SLSBridgedMoveWindowsToManagedSpaceOperation` + `SLSPerformAsynchronousBridgedWindowManagementOperation` | yabai 首先按符号可用性选此分支；没有经过 Dock SA。提交异步操作不等于窗口已迁移。[Y2] |
| 同上，旧系统分支 | PRIVATE 普通 connection 调用 `SLSMoveWindowsToManagedSpace` | yabai 仅在不命中特定 workaround 版本时采用；不能把旧实现当作所有新版本可用的通用接口。[Y2][Y7] |
| 同上，Dock 路线 | SA 在 Dock 内调用 `SLSMoveWindowsToManagedSpace` | 在需要 workaround 的分支被尝试；要求 SA 可用。[Y2][Y3] |
| 同上，兼容回退 | PRIVATE `SLSSpaceSetCompatID` → `SLSSetWindowListWorkspace` → 清理 compat ID | yabai 在特定分支 SA 通信失败时使用；属于修改系统兼容状态的 workaround，必须有清理和实际 membership 校验，不作为默认安全等价替代。[Y2] |
| 切换到原生 Space | EVENT 系统已配置的桌面快捷键；PRIVATE EVENT 合成 Dock 手势；SA 操作 Dock 的 Space 对象 | 手势路线通常是相对切换，受显示器、动画、Mission Control 状态影响；由 Space ID 推算步数也不等于原子按 ID 激活。yabai 不同入口的回退并不一致。[A5][R4][Y2][Y3] |
| 创建/删除/移动/重排整个 Space | SA：Dock 内部 add/remove/move routines | yabai 有实际实现，依赖内部对象/函数定位；非 user Space、最后一个 user Space、动画/Mission Control 等有额外限制。不能从“移动窗口到 Space”推导这组能力。[Y2][Y3] |
| 通过系统 UI 管理桌面 | EVENT / System Events 操作 Mission Control 已暴露的按钮、拖放 | 是条件 UI 自动化路线，不是稳定拓扑 API；本文未完成指定 OS/UI 结构的适配验收，不将其列为可直接启用的后台拓扑后端。[A6] |
| 自定义工作区 | 管理器保存自己的 membership，使用 AX 将窗口停放到屏幕边缘/屏幕外并恢复 | Rift 有实现；这不是原生 Space。多显示器排列、窗口最小可见边界、应用自行置前都需处理。[R6] |
| 自有 overlay 跟随/跨 Space | OWN `collectionBehavior`；PRIVATE 对自有窗口分配 Space | 不授予修改第三方窗口行为的权限。`canJoinAllSpaces`、全屏 auxiliary、Stage Manager 角色分别配置。[A4] |

yabai 快照的旧路径 workaround 判定覆盖 macOS 12.7+、13.6+、14.5+ 与 major >=15，但 **bridge 分支先于该判定**。这是一份源码路由条件，不是这些版本的成功认证，也不能据此推断 bridge 首次可用的具体版本。[Y2][Y7]

## 6. 层级、透明度、阴影、鼠标穿透与动画

| 能力 | 可用路径 | 必须保留的限制 |
| --- | --- | --- |
| 相对前后排序 | AX raise；SA `SLSOrderWindow` / group ordering；OWN AppKit order 方法 | raise、relative order、numeric level、key focus 是四个不同维度。`SLSOrderWindow` 只在 Dock 注入路线可用：普通进程对第三方窗口返回 `1000`，对自有窗口的 `above/below` 是**返回 `0` 的静默 no-op**（macOS 26.6.2 实测，仅 `order=0/2` 的隐藏/恢复生效），见 [sls-order-window-research.md](sls-order-window-research.md)。[A2][A4][Y3] |
| 第三方窗口层级 | SA：yabai 使用 `SLSSetWindowSubLevel(..., CGWindowLevelForKey(...))` | 这是在 Dock 上下文中的具体实现，不要误写成普通进程任意调用 `SLSSetWindowLevel` 即可。[Y3] |
| 第三方窗口 alpha | SA `SLSSetWindowAlpha`，渐变由循环更新 | 不等于背景透明或鼠标穿透；当前所核对 yabai 路径没有无 SA 的等价回退。[Y3] |
| 第三方窗口亮度/dim | PRIVATE `SLSSetWindowListBrightness` 有 Rift 客户端示例调用 | **示例证据**，不是 Rift 核心默认功能，也不是 alpha。未在本机实测，需单独验证目标窗口及系统能力。[R7] |
| 第三方窗口阴影 | SA 修改 window tag | yabai 有具体 enable/disable 实现；不能据自有 overlay 的 setShadow 推导外部窗口可写。[Y3] |
| 第三方窗口 sticky | SA 设置/清除 window tag bit 11 | 指包含该窗口的显示器上的 Spaces，不是复制窗口到全部显示器；要分别验收全屏、跨屏行为。[Y3][Y6] |
| 自有窗口外观/层级/穿透 | OWN `alphaValue`、`backgroundColor`、`hasShadow`、`level`、`ignoresMouseEvents`；也有私有自有 CGS window 实现 | 公开 AppKit 路线已有对应能力。私有封装接受一个 window ID，不是可修改任意第三方窗口的证明。[A4][R8] |
| 让任意第三方窗口鼠标穿透 | 本次未核实通用且有业务调用的路线 | 不把私有 tags 的猜测列为已具备功能。自己的 overlay 可穿透，原应用窗口能否穿透是另一个问题。 |
| 真实 frame 动画 | AX 分帧写位置/尺寸，合并过期帧 | Rift 实际采用；仍受 AX 响应和目标应用约束。批量消息不等于系统原子事务。[R1] |
| 代理动画 | yabai 捕获/创建代理 + 私有变换/切换显示，同时 AX 提交真实 frame | 动画配置路径检查相关 SIP 条件与录屏访问；视觉流畅不表示真实 resize 绕过 AX。[Y3][Y9] |
| 视觉缩放/画中画 | SA `SLSSetWindowTransform` | 改变合成结果，不请求应用按新尺寸布局。需恢复 transform；不能当作突破最小尺寸的真正 resize。[Y3] |
| 窗口预览、自有动画代理 | CAPTURE：ScreenCaptureKit + 自有绘制/动画 | 是可用图像来源；接管原窗口可见性、避免双影、正确转发输入还需其他能力。不是开箱即用的原窗口控制后端。[A8] |

## 7. 事件与状态同步

| 能力 | 可用路径 | 决策限制 |
| --- | --- | --- |
| 窗口创建、移动、缩放、焦点等 | AX `AXObserverCreate` / `AXObserverAddNotification`，注册 run-loop source | 不同应用/元素对通知支持不同；订阅成功与状态已同步不是同义词。应保留重新枚举与状态读取。[A1][R1] |
| WindowServer 事件 | PRIVATE `SLSRegisterConnectionNotifyProc` / `SLSRequestNotificationsForWindows` | Rift 有事件 actor；移动/缩放会查询几何，部分 Space/membership 事件直接转发。应按事件类型处理，并结合重新读取/协调确认最终状态；事件 ID、注册范围与版本相关。[R3][R9] |
| 窗口失踪判定 | CG/PRIVATE 清单 + AX 应用清单交叉核对 | 工程策略：查询失败、AX 失效、offscreen、最小化、销毁应分开，不用某一次 empty list 执行永久删除。 |
| 鼠标/键盘监听 | CG event tap | 按事件类型与 tap 模式检查权限；监听权限不是合成权限，且不是窗口变化通知。[A5] |
| 写后确认 | AX/CG/PRIVATE 重新查询 actual frame、focus、membership | 工程策略：区分“请求发出”“transport 返回”“状态符合目标”。超时和异步 bridge 尤其需要这一层。[A1][Y2][Y5] |

## 8. 看似不同、实际上没有增加权限的路线

- **System Events UI scripting** 使用 Accessibility 框架，不是避开 AX 限制的替代后端。[A6]
- **目标应用自己的 AppleScript 字典** 才是另一条应用协作路线；例如 Terminal 的 window `bounds`。不能因为某个应用支持就推广到全部应用。[A7]
- **调用 Rift/yabai 等工具的 CLI/IPC** 可以复用实现和版本适配，但继承该工具的 API、权限、行为限制；命令成功不保证 OS 效果完成。[Y5]
- **自有 NSWindow 操作** 不等于第三方窗口控制。**截屏后绘制**不等于修改原窗口。**transform** 不等于真实 resize。[A4][A8][Y3]
- **AX 前缀**不必然表示公开接口：`_AXUIElementGetWindow` 是私有桥接；动态字符串 `AXFrame`、全屏扩展属性也应单独标注支持级别。[A2][R1][R2]
- **root、私有 framework 链接、一个导出的函数名**均不是任意窗口写权限的证据。未核实的任意进程注入、替换 WindowServer、内核驱动等不纳入本次“可选后端”。[Y5]

## 9. 权限、维护与性能成本

| 项目 | 选型时应计入的成本 |
| --- | --- |
| AX 信任 | 使用 `AXIsProcessTrustedWithOptions` 检查；对象不支持、进程未实现、元素失效、通信超时均是独立错误。[A1] |
| AX 超时 | `AXUIElementSetMessagingTimeout` 对 system-wide 元素设置的是本客户端全局 timeout；对其他对象设置仅影响该对象。不要假定对 application 元素设置一次就覆盖所有 window 元素。[A1] |
| Event 权限 | `CGPreflightListenEventAccess` 与 `CGPreflightPostEventAccess` 分别检查；不能把启动时某一个权限通过当成所有路线均可用。[A5] |
| Automation 权限 | `AEDeterminePermissionToAutomateTarget` 按运行中的目标应用检查 Apple Events 权限；可能等待用户授权，Apple 明确要求避免在主线程调用该检查。[A7] |
| 录屏 | 捕获内容/代理动画单独考虑录屏授权和系统选择流程；不能把基本几何查询一概说成必须录屏，也不能把几何可读当成像素可捕获。[A3][A8][Y9] |
| SA 安全与安装 | 官方 Wiki 按架构/系统要求部分 SIP 放宽；Apple Silicon 还涉及 arm64e 配置。源码 loader 检查 root、payload、注入及握手能力。本文不提供或执行修改保护的命令。[Y5][Y6] |
| 私有版本维护 | 对符号/类/selector 做能力探测，对结果做语义验证；Dock 注入还存在二进制定位、内部对象、架构差异。符号或 capability bit 可用不等于每次操作成功。[Y2][Y3][Y5] |
| 分发 | Mac App Store 的公开 API 与沙箱要求会限制上述方案；这不自动等于站外 Developer ID 签名不可能。公证、权限、签名身份需另行验收。[A10] |
| 性能 | 私有查询可能减少应用 AX 往返，但本文无 benchmark，不承诺固定延迟或整体更快。对真实布局要分别测排队、调用、应用处理、读回、稳定收敛时间。 |
| 稳定性 | AX 依赖目标应用；私有路线依赖 OS 内部行为；SA 增加 Dock 注入故障面。三者没有可凭 API 名称得出的统一可靠性排序。 |

## 10. 后续决策清单

以下是决策维度，不是已批准的 Spool 改造方案：

1. **是否仅控制第三方窗口，还是还包括自有 bar/overlay？** 自有窗口优先从 OWN 能力中选，不与第三方权限混算。
2. **真实 resize 是否为硬要求？** 若是，当前证据仍需 AX 或目标应用配合；不能用 transform 替代验收。
3. **能否接受私有 API，但保持 SIP 不变？** 可评估私有查询、焦点、bridge membership、手势等；逐项建立探测与回退，不做一个全局“私有 API 可用”布尔值。
4. **是否必须程序化创建/删除/重排原生 Spaces，或修改第三方 alpha/阴影/sticky？** 若是，评估 SA 的收益是否足够覆盖安全、部署和系统更新成本。
5. **原生 Spaces 还是自定义工作区？** 后者减少原生拓扑控制需求，但承担停放、多显示器和应用自行置前的语义成本。
6. **是否只服务少数明确应用？** 可评估应用自己的 SCRIPT；通用窗口管理器不能依赖每个应用都有脚本字典。
7. **分发目标是什么？** App Store、站外个人工具、受控机器部署有不同的可接受边界。

候选路线组合：

| 组合 | 能覆盖的目标 | 明确牺牲 |
| --- | --- | --- |
| 公开接口为主：AX + CG + OWN | 常规移动/缩放/按钮动作、信息清单、自有 overlay | 缺乏本文已核实的公开原生 Space-ID 写接口及外部窗口任意外观控制。 |
| 混合无注入：上述 + PRIVATE + 必要 EVENT | 更丰富查询、焦点、条件原生 Space 迁移/切换 | 私有版本适配；不保证原生拓扑和所有外观能力，也没有全面消除 AX。 |
| 可选 SA 增强 | 补充 Dock 管理的拓扑、层级、外观及代理动画 | SIP/安装/注入/版本成本；真实 resize 仍不能自动摆脱 AX。 |

## 11. 从“有实现”升级到“本项目可用”的验收

每条拟采用路线记录：`OS build + CPU 架构 + TCC/SA 状态 + 目标应用版本 + 操作前状态 + 返回结果 + 操作后状态 + 收敛时间`。

- 覆盖普通窗口、固定尺寸、sheet/dialog、最小化、隐藏应用、原生全屏、非活动 Space、双屏不同排列。
- 读取：比较 AX 和 WindowServer 几何，说明坐标原点、points/像素转换、阴影/装饰边界差异。
- 移动/缩放：确认内容真实布局与最终 frame；测应用不响应、连续请求、用户同时拖动、部分成功及 timeout。
- 焦点：同时检查前台应用、具体 key/焦点窗口和层级；不能只看 AXMain 或 raise 返回。
- Spaces：操作后读 membership 和 active Space；测试动画中、切显示器、目标 Space 已消失，以及 bridge 提交后尚未完成。
- 外观/动画：测原窗口与代理的对应、鼠标命中、结束恢复、崩溃后的残留效果。
- 失败策略：超时先观测，不盲目重复非幂等动作；缺能力返回明确 unavailable，不能静默宣称成功。

本次仅完成接口与源码核对，没有执行以上行为测试；没有为上述候选组合替用户作最终选择。

## 12. 可复核来源

### Apple

本机 SDK 根目录：`/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk`。下面的头文件路径均相对此目录。

- **[A1] AX 接口与错误/权限/通知**：`System/Library/Frameworks/ApplicationServices.framework/Frameworks/HIServices.framework/Headers/AXUIElement.h`，重点 55-74、187-248、316-340、387-402、579 起；[Apple Accessibility 文档](https://developer.apple.com/documentation/applicationservices/axuielement)。
- **[A2] AX 属性与动作定义**：同上 `Headers/AXAttributeConstants.h`，重点 478 起、609-641、805-1006；`Headers/AXActionConstants.h` 的 `kAXPressAction`、`kAXRaiseAction`。Rift/yabai 使用的动态扩展字符串另以项目源码为依据。
- **[A3] CG 窗口列表**：`System/Library/Frameworks/CoreGraphics.framework/Headers/CGWindow.h`，重点 42-178；[Required Keys](https://developer.apple.com/documentation/coregraphics/required-window-list-keys)、[Optional Keys](https://developer.apple.com/documentation/coregraphics/optional-window-list-keys)。其中 SDK 对 `kCGWindowWorkspace` 的 deprecated/no longer supported 声明不可忽略。
- **[A4] 自有窗口**：[NSWindow](https://developer.apple.com/documentation/appkit/nswindow)、[CollectionBehavior](https://developer.apple.com/documentation/appkit/nswindow/collectionbehavior-swift.struct)、[minSize](https://developer.apple.com/documentation/appkit/nswindow/minsize)；本机 `System/Library/Frameworks/AppKit.framework/Headers/NSWindow.h`。
- **[A5] 事件与权限**：本机 `System/Library/Frameworks/CoreGraphics.framework/Headers/CGEvent.h`，重点 `CGEventTapCreate`、`CGEventPost`、398-408；[CGEvent](https://developer.apple.com/documentation/coregraphics/cgevent)。
- **[A6] UI 脚本与 AX 的关系**：[Apple Mac Automation Scripting Guide: Automating the User Interface](https://developer.apple.com/library/archive/documentation/LanguagesUtilities/Conceptual/MacAutomationScriptingGuide/AutomatetheUserInterface.html)。该文是归档文档，用于接口机制，不用于当前系统设置 UI 位置。
- **[A7] 应用脚本的具体例子与权限**：本机 `/System/Applications/Utilities/Terminal.app/Contents/Resources/Terminal.sdef`，window 的 `bounds` 在 220-222 行，`miniaturized` 在 232 起；SDK `System/Library/Frameworks/CoreServices.framework/Frameworks/AE.framework/Headers/AppleEvents.h:568-601` 的 `AEDeterminePermissionToAutomateTarget`；[Apple Scripting Dictionary 指南](https://developer.apple.com/library/archive/documentation/LanguagesUtilities/Conceptual/MacAutomationScriptingGuide/NavigateaScriptingDictionary.html)。没有执行脚本或请求授权。
- **[A8] 捕获而非控制**：[ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit)、[macOS capture](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-in-macos)。本次读取官方 `.md` 正文；部署时按 macOS 目标版本核对具体捕获 API 和授权方式。
- **[A9] 应用激活和隐藏**：[NSRunningApplication](https://developer.apple.com/documentation/appkit/nsrunningapplication)。
- **[A10] 分发边界**：[App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/)，2.5.1、2.4.5(i)。

### Rift 固定提交

- **[R1] AX 封装与真实调用**：[axuielement.rs:165](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/axuielement.rs#L165)、[app.rs:629](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/actor/app.rs#L629)、[app.rs:793](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/actor/app.rs#L793)。
- **[R2] 私有清单、几何、约束、Space membership、焦点**：[window_server.rs:136](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/window_server.rs#L136)、[bounds:194](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/window_server.rs#L194)、[membership:412](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/window_server.rs#L412)、[focus:797](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/window_server.rs#L797)。
- **[R3] 私有事件中的实际几何读取**：[window_notify.rs:261](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/actor/window_notify.rs#L261)。
- **[R4] 无注入手势切换**：[space_switch.rs:170](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/space_switch.rs#L170)。源码包含版本选择，不等于各版本已经实测。
- **[R5] Space 拓扑**：[screen.rs:622](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/screen.rs#L622)。
- **[R6] 虚拟工作区停放**：[engine.rs:2357](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/layout_engine/engine.rs#L2357)、[app.rs:935](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/actor/app.rs#L935)、[README](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/README.md)。
- **[R7] 亮度示例**：[dimmer.rs:161](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/crates/rift-client/examples/dimmer.rs#L161)。
- **[R8] 自有私有窗口封装**：[cgs_window.rs:90](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/cgs_window.rs#L90)。
- **[R9] WindowServer 通知订阅**：[window_notify.rs:54](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/window_notify.rs#L54)。

### yabai 固定提交

- **[Y1] AX 几何与私有焦点**：[window_manager.c:415](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window_manager.c#L415)、[frame:729](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window_manager.c#L729)、[focus:1293](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window_manager.c#L1293)。
- **[Y2] Space 路由与回退**：[space_manager.c:665](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/space_manager.c#L665)、[focus:925](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/space_manager.c#L925)、[topology:1037](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/space_manager.c#L1037)。
- **[Y3] Dock payload 与动画**：[payload.m:460](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/payload.m#L460)、[scale/move:595](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/payload.m#L595)、[appearance/order:650](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/payload.m#L650)、[proxy:515](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window_manager.c#L515)。
- **[Y4] 拖动先 SA 后 AX**：[event_loop.c:1251](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/event_loop.c#L1251)。
- **[Y5] SA 加载、握手与通信结果**：[sa.m:270](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/sa.m#L270)、[load/send:369](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/sa.m#L369)、[loader.m:129](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/loader.m#L129)。
- **[Y6] 官方 SIP 要求**：[Disabling System Integrity Protection](https://github.com/asmvik/yabai/wiki/Disabling-System-Integrity-Protection)，核查日读取；Wiki 未固定 revision。
- **[Y7] 版本 workaround**：[workspace.m:17](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/workspace.m#L17)。
- **[Y8] 全屏 AX 扩展**：[window.h:4](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window.h#L4) 定义 `AXFullScreen`；[window.c:830](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window.c#L830) 的 `window_is_fullscreen` 读取；[window_manager.c:2296](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window_manager.c#L2296) 的 `window_manager_toggle_window_native_fullscreen` 先聚焦并等待所属 Space 激活，再写全屏属性并等待转换。
- **[Y9] 动画配置权限检查**：[message.c:1297](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/message.c#L1297)。
