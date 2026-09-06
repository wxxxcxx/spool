# macOS NSWindow 行为属性速查

核对日期：2026-09-05。依据 Apple 官方在线 API 文档；网页检索工具返回空结果后，使用同站点 `.md` 正文及其 availability 元数据核对。本文是文档研究，不是跨 macOS 版本的运行时验证。

## 1. 适用范围

以下是应用对其持有的 **NSWindow / NSPanel 对象**设置的 AppKit 属性，不是拿任意其他进程的窗口 ID 就能调用的远程窗口管理协议。尤其不能把 `collectionBehavior` 当作给第三方窗口分配任意 Space 的 API：文档描述的是窗口参与系统管理的偏好，而非 Space ID 操作。[1][2]

对 Spool 的工程含义：这些 API 可用于自己的 bar、overlay、设置窗口；管理其他应用窗口的 AX / CoreGraphics 边界见第 7 节，私有 Space API 不在本文范围。AppKit 对象操作应留在主线程；`NSPanel` 官方声明为 `@MainActor`。[2]

## 2. styleMask：装饰、交互能力与窗口类型

`NSWindow.StyleMask` 是可组合位集，但并非每个标志适用于每种窗口。[3]

| 标志 | 官方语义 / 限制 |
| --- | --- |
| `borderless` | 不显示通常的外围装饰；不能据此推断可成为 key window，见焦点部分。 |
| `titled` / `closable` / `miniaturizable` | 分别显示标题栏、关闭按钮、最小化按钮。 |
| `resizable` | 允许用户调整大小；不要把它等同于远程 AX 可写性。 |
| `fullScreen` | 全屏样式状态；调用 `toggleFullScreen(_:)` 时系统自动切换该位。不要与“允许进入全屏”的 collection behavior 混淆。 |
| `fullSizeContentView` | 内容视图延伸至整个窗口，但仅对有标题栏的窗口生效；启用 layer backing，应使用 `contentLayoutRect` / `contentLayoutGuide` 避让标题栏与工具栏。macOS 10.10+。[4] |
| `utilityWindow` / `docModalWindow` | 面板 / 文档模态面板相关样式，文档指定 NSPanel 或其子类。 |
| `nonactivatingPanel` | NSPanel 或其子类不激活所属应用；不是普通 NSWindow 通用的“不抢焦点”开关。[5] |
| `hudWindow` | HUD 面板样式。 |

历史常量不是兼容性承诺：例如 `unifiedTitleAndToolbar` 官方明确为无效果，因为带工具栏的窗口已经统一采用该样式；旧名称还存在 deprecated 条目。[3]

## 3. collectionBehavior：按系统场景分组

官方强调这些设置是 **偏好**，并非所有标志适用于所有窗口管理技术；不要无差别 OR 全部标志。[1]

| 场景 | 标志 | 含义 |
| --- | --- | --- |
| Spaces | 默认行为 | 同时只出现在一个 Space。 |
| Spaces | `canJoinAllSpaces` | 可出现在所有 Spaces；不可由此推出能无条件覆盖任意全屏应用。 |
| Spaces | `moveToActiveSpace` | 窗口变为活动时，移动到当前 Space，而非切换到窗口所在 Space；不是指定目标 Space。 |
| Spaces / Mission Control | `managed` | 参与 Mission Control 与 Spaces。 |
| Spaces / Mission Control | `transient` | 在 Spaces 中浮动，在 Mission Control 中隐藏。 |
| Mission Control | `stationary` | 不受 Mission Control 影响，像桌面窗口一样保持可见且不移动。 |
| 全屏 | `fullScreenPrimary` | 可进入全屏；该成员 macOS 10.7+。[6] |
| 全屏 | `fullScreenAuxiliary` | 与全屏窗口显示在同一个 Space。 |
| 全屏 | `fullScreenNone` | 不支持全屏模式。 |
| 全屏分屏 | `fullScreenAllowsTiling` / `fullScreenDisallowsTiling` | 允许 / 禁止加入全屏 tile；不是通用桌面平铺算法。[7] |
| 窗口循环 | `participatesInCycle` / `ignoresCycle` | 控制是否参与 Cycle Through Windows 菜单命令。 |

全屏分屏仍有资格约束：默认允许符合条件的非 panel、非 sheet 窗口，例如可调整大小的窗口；不能独立全屏的窗口仍可能成为辅助 tile。`minFullScreenContentSize` 太大时，即便允许也可能无法加入。允许与禁止 tiling **同时设置会抛出异常**；`fullScreenAllowsTiling` 从 macOS 10.11 起可用。[7]

### Stage Manager 与全屏共用角色：macOS 13+

以下三个标志互斥，一个窗口至多选择一个；不能从 `CollectionBehavior` 类型本身的 10.5+ 推断新成员在旧系统可用。[1][8][9][10]

- `primary`：Stage Manager 与全屏的主窗口，适合文档、查看器窗口。
- `auxiliary`：辅助窗口，倾向与主窗口共同显示，适合设置、关于、工具面板。
- `canJoinAllApplications`：不参与 Stage Manager 布局；**满足资格条件时**可加入其他应用的全屏 Spaces，适合浮动窗口和系统 overlay。Apple 未在该页完整列出资格条件，因此不能承诺始终可见。

可以用更具体的全屏标志覆盖全屏角色，而保留 Stage Manager 角色。Apple 给出的例子包括 `primary | fullScreenAuxiliary`、`auxiliary | fullScreenNone`；对 `canJoinAllApplications`，文档说明可用 `fullScreenPrimary` 退出加入其他应用全屏 Spaces 的行为。[8][9][10]

## 4. level、key、main：三个不同维度

`level` 控制层级分组，高层级组位于低层级组前面；例如 floating 层在 normal 层之前。该属性文档不承诺窗口因此跨 Space、成为 key 或激活应用，不应把“置顶”与这些行为绑定。[11]

| 属性 | 用途与默认约束 |
| --- | --- |
| `isKeyWindow` / `isMainWindow` | 只读状态，分别查询应用的 key / main window，不是赋值开关。[12] |
| `canBecomeKey` | 只读资格；为 false 时，成为 key 的尝试被放弃。默认有标题栏或 resize bar 才为 true；自定义无边框窗口不能假定自动具备资格。[13] |
| `canBecomeMain` | 只读资格；默认要求可见、不是 NSPanel，并且有标题栏或调整大小机制。为 false 时，成为 main 的尝试被放弃。[14] |

因此应分开设计：应用是否激活、窗口是否接收键盘焦点、是否作为 main、是否置前。NSPanel 的按需 key 行为说明“不激活应用”与“不能取得键盘焦点”并不等价。[5][15]

## 5. 鼠标输入与 NSPanel

- `ignoresMouseEvents = true`：窗口对鼠标事件透明。它描述输入穿透，不是视觉透明，也不是全局事件监听开关。[16]
- `acceptsMouseMovedEvents`：是否接收并分发 mouse-moved 事件，默认 false；不是禁用全部鼠标输入。[17]
- `hidesOnDeactivate`：所属应用失活时是否移出屏幕；NSWindow 默认 false，NSPanel 默认 true。overlay 若需继续显示，应显式审视该项，而不只修改 level。[18]
- `NSPanel.isFloatingPanel`：面板是否浮动；**面板默认不浮在其他窗口上方**。Apple 推荐浮动面板用于小型、鼠标导向、需与普通窗口反复交互且失活时隐藏的工具面板。[19]
- `NSPanel.becomesKeyOnlyIfNeeded`：默认 false，即点击即可成为 key；true 表示仅需要键盘输入时成为 key。对于 nonactivating panel，文档进一步要求命中的 view 的 `needsPanelToBecomeKey` 返回 true，才能按此路径取得 key。[15]
- `NSPanel.worksWhenModal`：默认 false；true 时，在其他窗口运行模态 loop/session 期间仍可接收键盘和鼠标事件。这是 AppKit 模态事件行为，不是绕过其他应用或系统安全界面的权限。[20]

## 6. 实施边界与待验证项

**工程建议，不是已验证配方：** 自有纯展示 overlay 分别配置输入穿透、key/main 资格、层级、失活隐藏和 collection role；交互式面板则明确其键盘输入需求。不要将“NSPanel + floating”当作自动解决所有场景的组合。

按部署目标检查每个成员的 availability；部分旧属性页面仅列 `macOS: -`，本文不为其虚构引入版本。类型、子类、样式及全屏资格也会影响行为。至少在目标系统验证普通 Space、其他应用全屏、Stage Manager 开关、应用失活、鼠标点击与键盘输入；跨屏及动态修改样式的行为不在本次验证范围。

## 7. 远程窗口边界：AX 与 CoreGraphics

以下 AX 语义来自本机 Apple SDK，并核对了 Spool 的运行时探测调用。它们不能与自有 NSWindow 属性混为一套通用 getter/setter。

### Accessibility / AX

- `AXMain` 指主文档窗口，不一定是 key window。SDK 将 `AXMain`、`AXMinimized` 定义为可写，`AXModal` 为只读；这不是每个应用、每个元素都支持写入的保证。
- `AXCloseButton`、`AXZoomButton`、`AXMinimizeButton`、`AXFullScreenButton` 是只读的按钮元素引用，不是可赋值的关闭、缩放、最小化或全屏布尔开关。
- 按 `AXUIElement.h` 的接口要求，操作具体元素前使用 `AXUIElementIsAttributeSettable` 判断该属性当时是否可写。Spool 的 `src/manager/windows.rs:698` 附近在 `is_resizable()` 中对 `kAXSizeAttribute` 做了此探测；不能仅用 NSWindow 的 `resizable` 样式代替。
- `AXPosition` 是元素左上角的全局坐标：原点在含菜单栏屏幕的左上角，Y 向下，单位为 **points**；`AXSize` 也是 points，不是物理像素。

官方 SDK 来源（本机 `xcrun --show-sdk-path` 所指 SDK；版本变化后应重新核对）：

- `/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk/System/Library/Frameworks/ApplicationServices.framework/Frameworks/HIServices.framework/Headers/AXAttributeConstants.h`：坐标与尺寸约第 609-641 行，窗口属性约第 805-951 行。
- `/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk/System/Library/Frameworks/ApplicationServices.framework/Frameworks/HIServices.framework/Headers/AXUIElement.h`：`AXUIElementIsAttributeSettable` 声明及说明。

### CoreGraphics 窗口列表

Apple 将窗口信息字典字段分为两类，不能把 optional 当成必然存在：[21][22]

- **Required**：`kCGWindowNumber`、`kCGWindowStoreType`、`kCGWindowLayer`、`kCGWindowBounds`、`kCGWindowSharingState`、`kCGWindowAlpha`、`kCGWindowOwnerPID`、`kCGWindowMemoryUsage`。
- **Optional**：`kCGWindowWorkspace`、`kCGWindowOwnerName`、`kCGWindowName`、`kCGWindowIsOnscreen`、`kCGWindowBackingLocationVideoMemory`。

这里的 required 是对返回的信息字典的字段保证，不是保证能枚举所有窗口或获得所有内容的权限承诺。列表中的 layer、bounds、alpha 等是观察字段；它们的存在本身不提供修改远程 NSWindow 属性的接口。尤其不能从 optional 的 `kCGWindowWorkspace` 推导出现代原生 Space 的完整拓扑或可写控制能力；这些问题留给独立 AX/CG 研究。

## 8. 几何与外观

以下是自有窗口的常用属性与相关方法，完整成员索引见 NSWindow 文档。[23]

| 属性 / 方法 | 控制或描述的行为 |
| --- | --- |
| `frame` / `setFrame(...)` | 查询 / 设置整个窗口的位置与尺寸；不要与内容区域的尺寸混用。 |
| `minSize` / `maxSize` | 窗口外框的尺寸约束，包含标题栏；内容区另有 `contentMinSize` / `contentMaxSize`。 |
| `aspectRatio` / `resizeIncrements` | 调整尺寸时的宽高比例与步进约束。 |
| `isMovable` / `isMovableByWindowBackground` | 能否由用户移动、能否通过背景拖动；不是远程 AX 可写性的承诺。 |
| `alphaValue` | 整个窗口的透明度。[25] |
| `isOpaque` / `backgroundColor` | 是否不透明与背景颜色；不透明标志不是鼠标事件开关。[26] |
| `hasShadow` | 是否显示窗口阴影。 |
| `title` / `titleVisibility` / `titlebarAppearsTransparent` | 标题文本、标题是否显示、标题栏是否呈透明外观。 |

尺寸约束有方法级例外：Apple 明确说明 `minSize` 约束用户缩放及多数 `setFrame...` 方法，但 `setFrame(_:display:)` 与 `setFrame(_:display:animate:)` 不受该项约束；不要断言它是不可绕过的硬下限。[24]

## Apple 官方来源

以下是可浏览的正文 URL；本次实际读取对应 URL 后缀 `.md` 的官方版本。

1. [NSWindow.CollectionBehavior](https://developer.apple.com/documentation/appkit/nswindow/collectionbehavior-swift.struct)
2. [NSPanel](https://developer.apple.com/documentation/appkit/nspanel)
3. [NSWindow.StyleMask](https://developer.apple.com/documentation/appkit/nswindow/stylemask-swift.struct)
4. [fullSizeContentView](https://developer.apple.com/documentation/appkit/nswindow/stylemask-swift.struct/fullsizecontentview)
5. [nonactivatingPanel](https://developer.apple.com/documentation/appkit/nswindow/stylemask-swift.struct/nonactivatingpanel)
6. [fullScreenPrimary](https://developer.apple.com/documentation/appkit/nswindow/collectionbehavior-swift.struct/fullscreenprimary)
7. [fullScreenAllowsTiling](https://developer.apple.com/documentation/appkit/nswindow/collectionbehavior-swift.struct/fullscreenallowstiling)
8. [primary](https://developer.apple.com/documentation/appkit/nswindow/collectionbehavior-swift.struct/primary)
9. [auxiliary](https://developer.apple.com/documentation/appkit/nswindow/collectionbehavior-swift.struct/auxiliary)
10. [canJoinAllApplications](https://developer.apple.com/documentation/appkit/nswindow/collectionbehavior-swift.struct/canjoinallapplications)
11. [level](https://developer.apple.com/documentation/appkit/nswindow/level-swift.property)
12. [isKeyWindow（含 isMainWindow 关联条目）](https://developer.apple.com/documentation/appkit/nswindow/iskeywindow)
13. [canBecomeKey](https://developer.apple.com/documentation/appkit/nswindow/canbecomekey)
14. [canBecomeMain](https://developer.apple.com/documentation/appkit/nswindow/canbecomemain)
15. [becomesKeyOnlyIfNeeded](https://developer.apple.com/documentation/appkit/nspanel/becomeskeyonlyifneeded)
16. [ignoresMouseEvents](https://developer.apple.com/documentation/appkit/nswindow/ignoresmouseevents)
17. [acceptsMouseMovedEvents](https://developer.apple.com/documentation/appkit/nswindow/acceptsmousemovedevents)
18. [hidesOnDeactivate](https://developer.apple.com/documentation/appkit/nswindow/hidesondeactivate)
19. [isFloatingPanel](https://developer.apple.com/documentation/appkit/nspanel/isfloatingpanel)
20. [worksWhenModal](https://developer.apple.com/documentation/appkit/nspanel/workswhenmodal)
21. [Required Window List Keys（本次读取的官方 Markdown）](https://developer.apple.com/documentation/coregraphics/required-window-list-keys.md)
22. [Optional Window List Keys（本次读取的官方 Markdown）](https://developer.apple.com/documentation/coregraphics/optional-window-list-keys.md)
23. [NSWindow](https://developer.apple.com/documentation/appkit/nswindow)
24. [minSize](https://developer.apple.com/documentation/appkit/nswindow/minsize)
25. [alphaValue](https://developer.apple.com/documentation/appkit/nswindow/alphavalue)
26. [isOpaque](https://developer.apple.com/documentation/appkit/nswindow/isopaque)
