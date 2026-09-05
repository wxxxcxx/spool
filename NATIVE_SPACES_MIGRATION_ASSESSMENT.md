# Spool 移除虚拟 workspace、改用 macOS 原生 Spaces 的能力与迁移评估

> 调研日期：2026-08-27  
> 验证环境：macOS 26.5.2 (25F84)，Xcode macOS 26.4 SDK  
> 范围：评估能力边界与迁移风险；本文不包含实现，也没有修改现有源码。

## 结论

可以移除 Spool 的虚拟 workspace，并让每个原生 Space 对应一条
`LayoutStrip`。这样能够保留 Spool 最核心的价值：用户用 macOS 切换 Space，
Spool 在当前 Space 内维护滚动平铺布局。

这只会删除“把整条非活动 virtual row 停放到屏幕外”的机制。滚动长条内部仍会有
离屏列，macOS 仍可能把完全离屏的窗口挪到其他显示器，因此现有
`sliver_width`/`sliver_height` workaround 仍然需要。

但“使用原生 Spaces”不等于“获得一个 Apple 公开支持的 workspace 后端”。
如果只使用公开 API，Spool **不能**可靠完成以下现有功能：

- 枚举原生 Spaces、取得每个显示器的活动 Space ID；
- 按编号或 ID 选择 Space；
- 创建、删除、重排、重命名 Space；
- 查询任意第三方窗口的 Space 归属；
- 将任意第三方窗口发送到指定 Space，包括“发送但不跟随”；
- 按稳定 Space 身份恢复窗口归属。

这些能力只能来自 SkyLight/CGS 私有接口、驱动 Dock 的 Mission Control
辅助功能界面，或向 Dock 注入脚本扩展。后两者的维护和安全成本已经接近
yabai，而不是普通 AX 窗口管理器。

因此推荐：

1. 第一阶段只做 **observe-only 原生 Space 模式**：Space 的创建、删除、切换和
   窗口跨 Space 移动由用户/macOS 完成；Spool 只观察系统状态并管理每个当前
   可见 Space 的布局。
2. 保留现有只读/监听型 SkyLight 适配层，因为没有它甚至无法获得每屏 Space
   ID；但让核心 ECS 不直接依赖私有字典结构和通知编号。
3. 把“选择 Space”和“发送窗口到 Space”放进一个可选、带能力探测的私有控制
   后端；不可用时明确返回 `unsupported`，不能静默退化成不确定的键盘模拟。
4. 不把 Dock 注入、关闭 SIP、自动创建/删除 Space 作为 Spool 默认路径。

## 证据等级与三档能力

本文区分三种完全不同的承诺等级。

| 档位 | 可以做什么 | 不能/不应承诺什么 | 风险 |
| --- | --- | --- | --- |
| A. 纯公开 API | 收到“Space 已变化”通知；用 AX 移动/缩放当前可访问窗口；控制 Spool 自己的 overlay 是否出现在全部 Spaces；用户通过 macOS UI/手势管理 Spaces | Space 列表、ID、每屏活动 Space、第三方窗口归属、跨 Space 发送、Space 生命周期控制 | 最低；可以走公开兼容路线 |
| B. Spool 现有只读/监听型私有接口 | 枚举显示器及其 `id64`；读取每屏当前 Space；区分普通/全屏 Space；按 Space 枚举窗口；监听私有 Space 创建/销毁事件 | 不主动切换/创建/销毁 Space，不改变窗口 Space 归属 | 中等；OS 更新可能改变符号、字典字段、事件编号，但修改面仍主要是状态适配层 |
| C. 完整私有控制 | 按 ID 切换 Space；把窗口送到 Space；创建/删除/重排/跨屏移动 Space；可进一步消除动画 | Apple 不保证兼容性；部分路径要操作 Dock UI，部分要 Dock 注入/部分关闭 SIP | 高；需要逐 OS/架构维护，默认不适合作为 Spool 的基础契约 |

### A. 纯公开 API 的精确边界

Apple 公开的
[`NSWorkspaceActiveSpaceDidChangeNotification`](https://developer.apple.com/documentation/appkit/nsworkspace/activespacedidchangenotification)
只说明发生了 Spaces 变化，通知没有 `userInfo`；公开 API 不返回新的 Space ID
或显示器。macOS 26.4 SDK 只在 `NSWorkspace.h:339` 声明了该通知。

公开 AppKit 能控制的是**应用自己的** `NSWindow`：

- [`isOnActiveSpace`](https://developer.apple.com/documentation/appkit/nswindow/isonactivespace)
  可判断自己的窗口是否关联当前 Space；
- [`canJoinAllSpaces`](https://developer.apple.com/documentation/appkit/nswindow/collectionbehavior-swift.struct/canjoinallspaces)
  可让 SpoolBar/overlay 出现在所有 Spaces；
- [`moveToActiveSpace`](https://developer.apple.com/documentation/appkit/nswindow/collectionbehavior-swift.struct/movetoactivespace)
  可让自己的窗口激活时移到活动 Space；
- `fullScreenAuxiliary` 可让自己的辅助窗口与全屏窗口共同显示。

这些选项见本机 SDK
`AppKit.framework/Headers/NSWindow.h:94-150,535-548`；它们不是控制其他进程
窗口的句柄。

AX 仍然适合窗口几何管理。公开头文件提供 `AXWindows`、`AXPosition`、
`AXSize`、`AXMinimized` 和 `AXRaise`，并要求调用方检查属性是否可写；写入仍可能
返回 `not implemented`、`cannot complete` 等错误。证据见
[`kAXPositionAttribute`](https://developer.apple.com/documentation/applicationservices/kaxpositionattribute)、
[`AXUIElementSetAttributeValue`](https://developer.apple.com/documentation/applicationservices/1460434-axuielementsetattributevalue)
以及本机 SDK `AXAttributeConstants.h:609-641,822-832,1004-1008`、
`AXUIElement.h:188-223,315-331`。公开 AX 属性/动作目录没有 Space ID、Space
归属、切换 Space 或 move-to-Space 语义。

旧 CoreGraphics 键
[`kCGWindowWorkspace`](https://developer.apple.com/documentation/coregraphics/kcgwindowworkspace)
不能作为替代方案：SDK `CoreGraphics.framework/Headers/CGWindow.h:101-107`
明确标记为 `API_DEPRECATED("No longer supported", macos(10.5,10.8))`。

对 macOS 26.4 SDK 的公开 framework headers 全量搜索也没有找到公开的
`CGS*Space`、`SLS*Space`、`CopySpaces`、`CreateSpace` 或 `RemoveSpace`
声明。这是对当前 SDK 的结论，不代表系统内部没有对应私有实现。

### B. Spool 已经使用的只读/监听型私有接口

Spool 当前已经链接私有 `SkyLight.framework`，并声明了
`SLSManagedDisplayGetCurrentSpace`、`SLSSpaceGetType`、
`SLSCopyManagedDisplaySpaces`、`SLSCopyWindowsWithOptionsAndTags` 等符号：
[`src/manager/skylight.rs:11`](./src/manager/skylight.rs#L11)、
[`src/manager/skylight.rs:128`](./src/manager/skylight.rs#L128)、
[`src/manager/skylight.rs:180`](./src/manager/skylight.rs#L180)、
[`src/manager/skylight.rs:222`](./src/manager/skylight.rs#L222)。

当前实现从 `SLSCopyManagedDisplaySpaces` 的私有字典读取
`Display Identifier`、`Spaces` 和 `id64`，并按显示器返回 Space 列表：
[`src/manager.rs:222`](./src/manager.rs#L222)。每屏活动 Space 则由
`SLSManagedDisplayGetCurrentSpace` 读取，`SLSSpaceGetType == 4` 用来识别原生
全屏 Space：[`src/manager.rs:384`](./src/manager.rs#L384)。

事件层同时使用：

- 公开的 `NSWorkspaceActiveSpaceDidChangeNotification`；
- SDK 未公开的 `NSWorkspaceActiveDisplayDidChangeNotification`：
  [`src/platform/workspace.rs:264`](./src/platform/workspace.rs#L264)；
- 私有 `SLSRegisterConnectionNotifyProc` 以及逆向得到的事件号 1327/1328/1329：
  [`src/platform/notify.rs:15`](./src/platform/notify.rs#L15)、
  [`src/platform/notify.rs:157`](./src/platform/notify.rs#L157)。

本机 SDK 也把相关二进制符号放在
`System/Library/PrivateFrameworks/SkyLight.framework/.../SkyLight.tbd`
而不是公开 headers 中。例如 `SLSCopyManagedDisplaySpaces`、
`SLSManagedDisplayGetCurrentSpace`、`SLSMoveWindowsToManagedSpace` 分别出现在
该 `.tbd` 的 337、542、555 行。

因此迁移到原生 Spaces **不会消除私有 API**。observe-only 路线的实际含义是：
继续承担现有私有“读取系统真值”的风险，但不新增改变系统 Space 状态的风险。

### C. 完整私有控制的实际做法

成熟实现证明了“技术上能做”，但也证明了其脆弱性。

Hammerspoon 的 [`hs.spaces`](https://www.hammerspoon.org/docs/hs.spaces.html)
明确将模块标为 experimental，并说明它混合私有 API 与 Accessibility hacks。
创建、删除、跳转 Space 都要打开 Mission Control，等待 Dock 的 AX 元素生成，
再程序化按按钮；存在无法完全消除的视觉反馈和可调的经验延迟。其文档还指出
后台 Space 的窗口 ID 未必能解析成 AX 窗口。

移动窗口方面，Hammerspoon 当前源码直接调用/兼容
`SLSMoveWindowsToManagedSpace`、`SLSSpaceSetCompatID` 和
`SLSSetWindowListWorkspace`：
[`extensions/spaces/libspaces.m:1056-1108`](https://github.com/Hammerspoon/hammerspoon/blob/master/extensions/spaces/libspaces.m#L1056-L1108)。
文档明确限制普通窗口只能从 user Space 移到另一个 user Space；全屏/平铺
Space 不是等价目标：
[`moveWindowToSpace`](https://www.hammerspoon.org/docs/hs.spaces.html#moveWindowToSpace)。

yabai 进一步展示了完整控制面的成本：

- Space/window 枚举依赖 `SLSCopyManagedDisplaySpaces` 和私有字典字段：
  [`space_manager.c`](https://github.com/asmvik/yabai/blob/master/src/space_manager.c#L2959-L3038)；
- macOS 26 路径会尝试私有 Objective-C 类
  `SLSBridgedMoveWindowsToManagedSpaceOperation`，再回退到其他私有调用或脚本
  扩展：[`space_manager.c`](https://github.com/asmvik/yabai/blob/master/src/space_manager.c#L3220-L3281)；
- 创建、销毁、重排、跨屏移动 Space 等命令仍在 yabai 文档中标记为需要
  partially disabled SIP：
  [`yabai.asciidoc`](https://github.com/asmvik/yabai/blob/master/doc/yabai.asciidoc)；
- yabai 在 2026-06 仍需为 macOS 26.6 更新 `add_space` 的 ARM64 字节模式：
  [commit `dd84572`](https://github.com/asmvik/yabai/commit/dd84572)。

Apple 官方建议只在开发低层组件时暂时关闭 SIP，并在测试后重新开启，因为关闭
会降低系统保护：
[`Disabling and Enabling System Integrity Protection`](https://developer.apple.com/documentation/security/disabling-and-enabling-system-integrity-protection)。
另外，Mac App Store 审核准则 2.5.1 要求只使用公开 API：
[`App Review Guidelines`](https://developer.apple.com/app-store/review/guidelines/#software-requirements)。

需要准确区分：截至 2026-08，yabai 的更新记录表明“聚焦 Space”和“移动窗口到
Space”在部分当前系统上可在 SIP 开启时工作；这不使相关接口变成公开 API，
也不意味着创建/销毁/移动整个 Space 已摆脱 Dock 脚本扩展。

## 原生 Spaces 对用户能做什么

Apple 当前
[`Work in multiple spaces on Mac`](https://support.apple.com/guide/mac-help/work-in-multiple-spaces-mh14112/mac)
确认用户可以：

- 在 Mission Control 创建最多 16 个 Spaces；
- 通过触控板/鼠标手势、`Control-Left/Right` 或 Mission Control 切换；
- 把窗口拖到屏幕边缘后移到相邻 Space；
- 在 Mission Control 把窗口拖到指定 Space；
- 把窗口拖到全屏 app 上创建 Split View；
- 在 Dock 里把**应用**分配到 All Desktops、This Desktop、某个显示器的当前
  Desktop，或 None；
- 删除 Space，macOS 会把其中窗口移到另一个 Space；
- 给不同 Spaces 使用不同壁纸。

Apple 没有说明删除后窗口的确切目标 Space。Dock 的 assignment 也是应用级，
不是 Spool 当前的逐窗口策略。

原生 Spaces 还原生整合全屏和 Split View。Apple 的
[`Mission Control guide`](https://support.apple.com/en-ie/guide/mac-help/mh35798/mac)
把普通桌面、全屏 app 和 Split View 都放进 Spaces bar；
[`Split View guide`](https://support.apple.com/en-ca/guide/mac-help/mchl4fbe2921/mac)
说明 Split View 会创建新的 desktop Space。

“最多 16 个 Spaces”在 Apple 当前页面中没有明确说明是全局还是每显示器上限。
设计容量时应把它作为未知项，在目标多屏配置上实测，不应假定是每屏 16 个。

## 多显示器语义

Apple 公开保证：开启 `Displays have separate Spaces` 后，每块显示器有自己的
Space 集合。公开 AppKit 只提供
[`NSScreen.screensHaveSeparateSpaces`](https://developer.apple.com/documentation/appkit/nsscreen/screenshaveseparatespaces)
这个布尔值；SDK `NSScreen.h:29-31` 特别说明它不代表当前一定有多个显示器或
多个 Spaces。

Apple 的
[`Desktop & Dock settings`](https://support.apple.com/guide/mac-help/change-desktop-dock-settings-mchlp1119/mac)
还说明：

- 该选项开启时 Dock 可出现在所有显示器；
- 其他显示器使用 Split View 和 Stage Manager 需要该选项；
- `Automatically rearrange Spaces based on most recent use` 可按 MRU 重排 Spaces；
- 激活应用时可自动跳到已有该应用窗口的 Space。

在第二块显示器进入 Mission Control 时，只显示那块显示器正在使用的窗口和
Spaces。可是公开 API 不提供“每屏 Space 列表/活动 ID”，也没有公开的
active-display-changed 通知。

这要求 Spool 在模型上分开：

- **focused display/space**：唯一一个接收键盘焦点的显示器和 Space；
- **visible space per display**：开启 separate Spaces 后，每块显示器各有一个
  当前可见 Space。

当前 `ActiveWorkspaceMarker` 是全局单例：新 marker 加入时会从所有其他
`LayoutStrip` 移除它，见
[`src/ecs/workspace.rs:825`](./src/ecs/workspace.rs#L825)。
`ActiveDisplay` 参数也依赖单个 `ActiveWorkspaceMarker` 和单个
`ActiveDisplayMarker`：
[`src/ecs/params.rs:82`](./src/ecs/params.rs#L82)。这个不变量无法直接表达
“显示器 A 的 Space 1 和显示器 B 的 Space 5 同时可见”。

推荐的新不变量是：

- 每个普通 native Space 恰好有一个 `LayoutStrip`；
- 每个已连接显示器恰好有一个 `VisibleNativeSpace`；
- 全局最多一个 `FocusedNativeSpace`/`FocusedDisplay`；
- `LayoutStrip` 是否可见与是否拥有键盘焦点是两个维度。

Space 变化通知不带显示器/ID，所以收到通知后应重扫**所有显示器**的活动
Space，比较前后快照，而不是只更新当前 menu-bar display。

## Spool 当前模型与契约的迁移面

### ECS 与布局

当前一条 `LayoutStrip` 由 `(native workspace id, virtual_index)` 标识：
[`src/ecs/layout.rs:294`](./src/ecs/layout.rs#L294)。同一原生 Space 可以有多条
row，`SelectedVirtualMarker` 记住该原生 Space 当前选中的 row；切换 row 只是在
ECS 内替换 marker，并移动窗口位置，不触发 macOS Space 切换：
[`src/ecs/workspace.rs:1110`](./src/ecs/workspace.rs#L1110)。

迁移后应删除 `virtual_index` 和 `SelectedVirtualMarker`，而不是把
`virtual_index` 永久固定为 0。目标模型是 `NativeSpace -> LayoutStrip` 一对一。
但建议先引入新模型并保持兼容读取，再清理字段，避免一次性修改所有系统。

原生 Space 切换是异步的。命令发出、动画开始、通知到达、活动 display/Space
更新、AX focus 回执与窗口几何稳定不会在同一 tick 完成。ECS 必须以重扫后的
系统快照为最终真值，并对 transition 设置 generation/timeout；不能在发出切换
命令时立即移动 `ActiveWorkspaceMarker`。

### 全屏 Space (`type == 4`)

Spool 已经用 `SLSSpaceGetType == 4` 识别全屏 Space，并创建
`NativeFullscreenMarker`/特殊 fullscreen strip：
[`src/manager.rs:395`](./src/manager.rs#L395)、
[`src/ecs/workspace.rs:449`](./src/ecs/workspace.rs#L449)。

迁移后要继续把它建模为 system-owned `SpaceKind::Fullscreen`，而不是普通可创建、
可删除、可重排的 workspace。它可以拥有临时的一条特殊 `LayoutStrip`，但：

- 不参与普通 Desktop 的稳定编号；
- 不接受普通 `send window`，除非后端明确证明目标兼容；
- 全屏退出后由系统销毁/转换，Spool 只能响应；
- Split View 的一个 Space 可能对应两个系统管理窗口，不能套用普通滚动 strip
  的全部操作。

### All Desktops / 粘性窗口

应用被分配到 All Desktops，或窗口使用 `canJoinAllSpaces` 时，一个窗口会在多个
Spaces 出现。私有实现的 `windowSpaces(window)` 也明确返回一个 ID 数组，而不是
单值：
[`Hammerspoon libspaces.m:1116-1158`](https://github.com/Hammerspoon/hammerspoon/blob/master/extensions/spaces/libspaces.m#L1116-L1158)。

这与当前“一个 window entity 只属于一条 `LayoutStrip`”冲突。推荐把多 Space
窗口标为 `Sticky`，只保留一份 ECS window entity，并默认不插入任何普通
`LayoutStrip`（或只作为当前 Space 的 floating projection）。绝不能为每个
Space 复制实体，否则 focus、关闭、tab/group 和持久化都会重复。

### 持久化状态

当前保存格式把多个 `SavedStrip { virtual_index, columns }` 放在同一个
`SavedWorkspace { workspace_id, active_virtual_index }` 下：
[`src/ecs/state.rs:55`](./src/ecs/state.rs#L55)、
[`src/ecs/state.rs:250`](./src/ecs/state.rs#L250)。

原生 Space 的私有 `id64` 没有公开的跨 Dock restart、重新登录、系统升级或
Space 重建稳定性承诺；ordinal 又会受 MRU、全屏和用户重排影响。因此 state v3
应区分：

- 会话内 native `space_id`（精确但不承诺长期稳定）；
- `display_uuid`、当时 ordinal、`SpaceKind`（仅恢复提示，不是可靠主键）；
- 布局和窗口身份（Spool 自己可稳定保存的部分）；
- 未匹配布局，宁可保留待恢复，也不要移动到猜测的 Space。

旧 state v2 必须只读迁移并保留备份；首次运行不能直接覆盖。当前恢复文档已经
采用“匹配不明确就跳过窗口”的保守策略，并且断开的显示器不创建占位状态：
[`CONFIGURATION.md:319`](./CONFIGURATION.md#L319)、
[`CONFIGURATION.md:345`](./CONFIGURATION.md#L345)。原生 Space 恢复应维持同样
的安全边界。

### query/subscribe 与旧版 SpoolBar

本节记录迁移前的 Swift 独立进程协议约束。该实现现已删除；内置 Rust Bar 直接从
Bevy world 投影结构化列，不再经过 query/subscribe 或 CLI。

当前 state document version 是 2，公开字段是 `native_workspace_id` 加
`virtual_workspace_number`，主数组名为 `virtual_workspaces`：
[`QUERY_AND_SUBSCRIBE_FORMAT.md:35`](./QUERY_AND_SUBSCRIBE_FORMAT.md#L35)、
[`QUERY_AND_SUBSCRIBE_FORMAT.md:152`](./QUERY_AND_SUBSCRIBE_FORMAT.md#L152)。

旧 SpoolBar 曾直接解码这些字段，并通过旧 workspace 命令操作；这些路径随
Swift 工程一并移除。

这不是内部重构，而是协议破坏。建议发布 state v3：

```text
spaces[] = {
  space_id, display_id, ordinal, kind,
  visible, focused, windows[]
}
```

过渡期可继续输出 deprecated `virtual_workspaces`，每个普通 native Space 合成
一条 row；但所有新控制命令必须以 v3 capability 为准。Bar 的 workspace label
仍然只能是 Spool 自己的别名，不能声称已经重命名 Mission Control 的 Desktop。

### CLI 与 Lua

当前 CLI/Lua 的 `workspace.select`、`move_window`、`add` 都直接映射到虚拟 row
操作：[`crates/lua/src/lib.rs:200`](./crates/lua/src/lib.rs#L200)。Lua 的纯
`WindowSet` 还把 `ws:view`、`ws:shift` 和 named scratchpad 的 stash workspace
作为同步、可提交的模型操作：
[`SCRIPTING.md:198`](./SCRIPTING.md#L198)、
[`SCRIPTING.md:246`](./SCRIPTING.md#L246)、
[`SCRIPTING.md:270`](./SCRIPTING.md#L270)。

原生迁移后：

- observe-only 后端应对 `view/shift/add` 返回明确的 capability error；
- 可选控制后端把返回的纯变更编译成“请求系统操作 -> 等通知 -> 重扫 ->
  对账”的异步事务，失败时不能让 ECS 假装成功；
- `send without follow` 需要真正的私有 window-to-Space 操作，公开键盘输入做不到；
- scratchpad 不能再默认依赖一个“永远不看”的 workspace 9；应另行设计最小化、
  隐藏或专用私有后端；
- 按 AGENTS.md 的线程约束，所有 AppKit/CoreGraphics/SkyLight 调用仍必须在主线程
  适配层执行，Lua worker 只能传递普通 `Send` intent。

## 迁移后最可能遇到的问题

| 问题 | 原因 | 建议处理 |
| --- | --- | --- |
| native ordinal 突然变化 | 用户开启了 MRU 自动重排；全屏/Split View 插入或退出；用户拖动重排 | 启动时检测并提示关闭 MRU；内部使用会话 Space ID，不把 ordinal 当持久主键 |
| 切换后短暂铺错 Space/显示器 | 通知不带 ID，原生动画与 AX focus/geometry 回执异步 | 全屏重扫所有显示器；debounce + generation；只在快照收敛后提交可见状态 |
| 多屏只更新了有焦点的显示器 | 当前 `ActiveWorkspaceMarker`/`ActiveDisplay` 是全局单例 | 引入每屏 `VisibleNativeSpace`，另保留全局 focused marker |
| 后台 Space 的布局无法刷新 | Apple 未保证 inactive Space 窗口会完整出现在 AX，或允许可靠写几何 | observe-only 只在 Space 变可见后铺；后台写入必须先做兼容性原型和失败回滚 |
| 激活窗口触发意外 Space 跳转 | “When switching to an application...” 系统选项；同一 app 跨多个 Spaces | 作为 preflight 配置说明；focus 系统必须容忍 SpaceChanged 紧跟 window focus |
| window 发送成功但 child/sheet 留下 | Window Server 的关联窗口、sheet、tab/group 不一定随单个 ID 同步 | 移动前收集 associated windows；操作后重新枚举并对账；不确定时拒绝 |
| 粘性窗口被重复平铺 | All Desktops 导致一个 window 属于多个 Space | 单实体 `Sticky` 模型；不复制到多个 strips |
| 全屏 Space 被当普通 Desktop | 私有 type 4 与普通 type 0 生命周期不同 | 显式 `SpaceKind`；禁止普通 create/reap/send 逻辑作用于 type 4 |
| 删除 Space 后布局错配 | Apple 只承诺窗口移到“另一个 Space”，目标未指定 | 收到销毁事件后全量重扫；按窗口身份重建，不猜目标 |
| 拔插显示器后 Space 身份/顺序错乱 | Apple 未公开保证 Space 如何重新归属 | display churn 期间暂停控制；稳定后按 display UUID + live Space snapshot 对账，未匹配状态保留 |
| 切换体验明显变慢 | 原生 Space 有系统动画；公开 API 没有禁用动画的 Space 开关 | 接受系统动画/Reduce Motion；不要把 Dock 注入作为默认“性能优化” |
| 私有符号在系统更新后失效 | SkyLight 无兼容承诺，私有字典和事件号来自逆向 | OS/架构 capability probe；适配层失败即降级 observe-only；不要让 daemon 启动失败 |
| 自动创建原生 Spaces 造成闪屏/超限 | Dock AX 必须打开 Mission Control；Apple 文档给出最多 16 个 Spaces | 默认不自动创建；用户显式操作；创建前 dry-run 和容量检查 |

## 现有虚拟 workspace 数据如何迁移

不能把每个 `virtual_index` 自动转成一个 native Space。假设一个显示器已有 `M`
个原生 Spaces，每个原生 Space 内有若干虚拟 rows，则需要的目标 Space 数是所有
rows 的总和，而不是最大 row 数；它可能超过 Apple 的 16-Space 上限，并且自动
创建、移动窗口、删除旧 Space 都进入完整私有控制档位。

建议提供三种显式策略，默认只启用第一种：

1. **安全折叠（默认）**：把同一 native Space 内的 rows 按 `virtual_index`
   顺序拼成一条 `LayoutStrip`，不改变任何窗口的 native Space 归属。保留 v2
   备份和迁移报告。代价是原来的任务分组消失，strip 会变宽。
2. **用户辅助映射**：用户先在 Mission Control 创建/排列目标 Spaces，迁移工具
   只生成 `row -> target native Space` 计划；逐组确认后移动窗口并对账。
3. **完整私有自动迁移**：Spool 创建目标 Space、移动窗口并跟随。仅适合可选实验
   后端，必须 dry-run、检查容量、保存回滚清单，并明确可能要求部分关闭 SIP。

无论选择哪种策略，都不能在普通升级启动时自动删除 native Space。Space 删除会
改变用户系统级状态，且 Apple 只保证其中窗口被移动到某个其他 Space。

## 推荐实施顺序

### 实施状态（2026-08-27）

迁移实现已完成，产品默认行为已经切换到原生 Space：

- `NativeSpace`、`SpaceKind` 和逐显示器 `VisibleNativeSpaceMarker` 是唯一 Space
  模型；每个原生 Space 恰好拥有一条 `LayoutStrip`；
- Space/display/wake 事件后只读重扫拓扑、可见 Space、type 和 Window Server
  成员，所有私有写操作后也以回读结果收敛，不做乐观 ECS 更新；
- state/query/subscribe 为 v3，只发布稳定 `space_id`；CLI、Lua WindowSet 和
  内置 Rust Bar 都以该 ID 寻址，旧虚拟 row 命令和配置已删除；
- `spool migrate-state` 默认 dry-run，`--apply` 先保存 v2 原文备份，再按 row
  顺序安全折叠为一 Space 一 strip；
- 默认严格 observe-only。只有显式设置
  `experimental_space_control = true` 且运行时探测成功时，窗口移动能力
  才会发布为 true；focus/create/delete 始终明确返回 capability error；
- 私有后端只使用运行时探测的 bridged window-management operation，不包含 Dock
  注入、旧兼容 Space ID、脚本模拟导航或关闭 SIP 的路径。

2026-08-27 当前机器只读抽样：macOS 26.5.2 (25F84)，TextEdit（AppKit）和
Google Chrome（Chromium）均可枚举标准 AX window。没有执行 position/size
写入；当前环境未找到明确的 Java/Catalyst 样本，也未在本次受限会话中取得完整
显示器拓扑。因此下列兼容性矩阵仍是发布前实机验收项，不被代码测试冒充为通过。

### 阶段 0：只读兼容性原型

**状态：原型与记录通道完成；跨应用/硬件矩阵持续验收。**

- 记录每屏 Space 列表、活动 Space、type、窗口成员和事件序列；不移动窗口。
- 覆盖 AppKit、Electron/Chromium、Java、Catalyst，普通/最小化/全屏/All
  Desktops 窗口。
- 实测 inactive Space 的 AX 枚举和几何写入，但原型默认只记录，不保留写入。
- 覆盖双屏 separate Spaces、MRU 开/关、sleep/wake、Dock restart、拔插显示器。

### 阶段 1：observe-only 原生模式

**状态：完成。**

- 引入 `NativeSpace`/`SpaceKind` 和每屏 visible marker。
- 建立一 native Space 一 `LayoutStrip`，先保留 v2 读取兼容。
- 用户用 macOS 切换；Spool 在目标 Space 可见并收敛后铺窗。
- state/query 发布 v3；Bar 保留一版 v2 输入解码，不再输出 v2 字段。
- 不提供 create/delete/send/view；调用时返回 capability error。

### 阶段 2：安全的数据迁移和协议收口

**状态：完成。**

- 提供 v2 dry-run、备份和默认安全折叠。
- 删除 `virtual_index`、`SelectedVirtualMarker`、virtual row commands/config。
- 更新 Lua/Bar，移除 deprecated v2 输出。

### 阶段 3：可选私有控制后端

**状态：在既定安全边界内完成。**

- 首先只实现 window-to-user-Space，操作后严格对账。
- 按 ID focus、create/delete 的可靠无注入实现未通过能力门槛，故明确不可用并
  降级为 observe-only，而不是模拟错误目标。
- reorder 与 Dock 注入不实现；SIP 保持开启。

## 必须实测、目前不能宣称的未知项

- 原生 `id64` 在 Dock restart、注销、重启、OS 升级后的稳定性；
- 多屏环境“最多 16 个 Spaces”的计数范围；
- macOS 26.5 上 inactive Space 的 AppKit/Electron/Java/Catalyst 窗口是否都能由
  `AXWindows` 枚举；
- 对 inactive Space 窗口设置 position/size 的实际结果及事件顺序；
- 删除 Space 后每个窗口的确切目标；
- 断开/重连显示器后 Space 的 ID、顺序和显示器归属；
- 不同焦点/鼠标位置下合成 `Control-Left/Right` 究竟作用于哪块显示器；
- 原生 transition 中间态会产生哪些 AX/SLS create/destroy/focus/move 事件；
- `SLSBridgedMoveWindowsToManagedSpaceOperation` 在 Spool 支持的每个 macOS/CPU
  组合上是否可用、是否需要额外 entitlement，以及失败时是否原子；
- associated windows、原生 tabs、sheet、popover 和 sticky 窗口跨 Space 移动
  的一致性。

这些是 Apple 公开文档没有保证的兼容性问题，必须以原型证据决定，不能从当前
Spool 或 yabai 在一台机器上的成功行为外推。

## 最终建议

移除 Spool virtual workspace 是可行的，前提是接受产品定位变化：

> Spool 管理每个原生 Space 内的滚动布局；macOS/用户拥有 Space 的生命周期与
> 导航。跨 Space 自动化属于可选私有能力，不是核心保证。

这条路线会消除整条 off-screen virtual row 停放引起的一类显示器重定位问题，并
让 Mission Control、系统手势、全屏/Split View 成为唯一 workspace 真值；它不会
消除滚动长条内部离屏列的 sliver workaround。代价是原生动画、更弱的可编程性，
以及仍然存在的只读 SkyLight 兼容风险。

如果目标是完整保留当前 `select/move/send/add/scratchpad/restore` 语义，那么这不
是一次“移除 workspace”的简化，而是把 Spool 升级成一套 yabai 级原生 Space
控制器。建议不要把这条高维护路线作为默认迁移目标。
