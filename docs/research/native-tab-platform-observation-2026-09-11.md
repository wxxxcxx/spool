# Native Tabs: 一个标签组作为一个布局窗口

调研日期：2026-09-11。本地代码基线：`9cbf7882ba0664ff6a6ec75186611f65cfcd5bf0` 加本轮开始前已有的未提交改动。Apple 部分来自官方文档与本机 macOS 26.4 SDK。下方“结论”至“验证与后续验收”保留初始调研记录，其 detector/config 描述属于修改前状态；最新实现和实机证据以本节为准。尚未部署或重启 daemon。

## 实施与实机观测更新

实现边界：布局只接收普通窗口，不再构造原生 tab 组；移除
`detect_tabbed_windows` 和 `disable_native_tabs`。旧 `Column::Tabs` /
`StackItem::Tabs` 数据及操作暂留兼容。平台层保留唯一的直接 `AXTabGroup`
对象，通过其 `AXWindow` owner 解析当前控制目标，不读取 tab 内容、顺序或
选择状态。完整应用清单确认后，原 ECS entity 更换控制目标而不增加布局列。
没有已保留 chrome 时，只有唯一新旧 publication 转换、同物理矩形、同用户
Space、旧 ID 仍属原进程且不再呈现的联合证据才允许启动阶段身份接续。
多个候选、不完整清单、关闭的旧 ID 和单纯屏外状态不能触发这种接续。

只读探针：`examples/native_window_probe/main.m`。编译和执行：

```sh
xcrun clang -fobjc-arc -Wall -Wextra -framework Cocoa -framework ApplicationServices examples/native_window_probe/main.m -o target/native-window-probe
target/native-window-probe com.apple.finder
target/native-window-probe pid:820
target/native-window-probe com.apple.finder 20
```

最后一个参数保留 AX chrome 句柄一段时间，结束后读取其 owner；用于用户手动
切换 tab 的对照。仅采集窗口/控件身份、角色、几何和 WindowServer 元数据，
不读取终端内容，不输入命令。PID 和以下 ID 只适用于本次观测。

| 实机场景 | 观测结果 |
| --- | --- |
| Finder 原有4个 tab | 只公开1个标准 AXWindow，当前 ID5944；后台 CG 身份仍存在。 |
| Finder 临时 a/b/c 窗口切换 | 当前 root6281、6300、6313 随选择变化；共享 AXTabGroup hash1701802403 不变。 |
| Finder 拆出 c | c6313 成为独立无标签栏窗口；a/b 的共享 chrome 保留在原窗口，公开窗口数增加1。 |
| Finder 保留 chrome 后 b→a | 同一个保留对象的 AXWindow owner 从6300变成6281，证明无需重读后台 root 的 children。 |
| Ghostty PID820，用户切到第二个 tab | 公开 root 从2309变成5066，共享 chrome hash1668246403 不变；仍只有1个 AXWindow、3个 tab。 |
| Ghostty PID22449 | 同时存在另一个 Ghostty 实例；root6267、chrome1668267680、3个 tab，切换期间未变化。 |

hash 仅用于诊断显示；生产代码保留 AX 对象本身，不以 hash 当持久身份。
Ghostty 界面自动操作受 Computer Use 的终端应用限制；切换由用户完成，
探针只读观察。此前“Ghostty 未运行”的结论错误，原因是仅凭单实例查找推断。

新增 `src/tests/independent_windows.rs` 覆盖启动后台对象过滤、同实体身份
接续、无创建事件切换、拆出新窗口、独立同尺寸窗口、关闭/不完整/多候选保护、
合并不重复控制目标，以及后台身份无位置写入。实机验证的是平台观测关系，
不是新 daemon 的端到端验收；部署后仍需验证新增/关闭/拆合、Bar、一列稳定性
及跨显示器/Space 行为。Ghostty 拆合尚未实机验证。

最终代码验证：`cargo test --locked --workspace --quiet -- --test-threads=1`
通过，主程序936 passed、2 ignored，其余工作区 crate 分别20、6、91 passed；
`cargo clippy --locked --all-targets -- -D warnings`、`cargo fmt -- --check`
和 `git diff --check` 通过。IPC 测试经独立授权在沙盒外执行。
临时 Finder a/b/c 窗口已关闭，原有 Finder 窗口和 Ghostty 会话未关闭。

## 结论

### 后续 Finder 闪缩回归（2026-09-11）

用户运行新版后，Finder 仍重复占列并在后台闪缩。只读 AX 快照只有一个
标准窗口，daemon 查询却同时包含5944和5465。Finder 的 AXWindows 还返回
无 WindowServer ID 的桌面 AXScrollArea；此前该元素使整个 inventory
长期 `complete=false`，阻止身份接续和后台对象退出布局。这是先前 mock
未覆盖的真实输入形状，现通过 role 分类排除明确的非窗口，真正的窗口 ID
或 role 读取错误仍使清单不完整。

使用 `spool subscribe --raw --json` 采集约7.744秒得到347条事件：

- 5944和5465各23条 `window_destroyed`，来源均为 SpaceNotification；
  WindowServer 探针仍能看到这些对象，不能当作实际关闭。
- Finder 46条 AccessibilityUiElement 焦点复核及46条应用局部焦点解析。
- ChatGPT 47条 StateSync 焦点解析，公开焦点在43与空值间重复变化。
- Chrome 23条 AccessibilityWindow 复核与23条应用 reconciliation。

原始带采集时间的 JSONL 位于本机临时路径
`/private/tmp/spool-finder-flicker-20260911.jsonl`。这些是订阅诊断事件，
不是几何写入日志。PID29569的stdout/stderr指向 `/dev/ttys001`，未保存
当前运行日志；旧 launchd 服务日志不属于该进程，未作为因果证据使用。

复现测试另外确认两处反馈环缺口：后台 AX focus revalidation 会使全局
焦点失效；tiled stacking 会 raise 已指向另一个控制目标的旧 native root。
前者现只接受前台应用复核，后者现只 raise 当前有效控制目标。两个回归
均先失败后修复。此链路与订阅循环吻合，但完整的实机写入因果链及修复后
事件频率仍需重启到最终构建、保存当前日志并重新采样，不能以 mock 通过
代替闪缩实机验收。

用户随后澄清的目标是：**Spool 只识别和管理独立窗口的外框，不管理原生 tab 的数量、顺序、选中状态或内部排布。** 这不是要求 Spool 建立一个自己的 tab 组管理器。下文对当前 `Column::Tabs` 的分析是现状说明，不是最终设计要求。

**可以，而且布局上应该把一个 native tab group 当作一个普通窗口单元：三个 tab 只占一列。** 需要区分系统窗口身份与布局位置：AppKit 的 tab 成员是多个 `NSWindow` 对象，但这不要求 Spool 为每个成员分配一个布局位置，也不保证外部客户端在每种状态下都能枚举到相同的 AX/CG 身份。[3], [H], [S1]

当前 Spool 已有 `Column::Tabs` 和 `StackItem::Tabs`。`convert_to_tabs()` 会删除 follower 原有的布局项，将成员合并到 leader 的位置；焦点确认后把选中的 tab 放到组首。因此主要问题是**可靠地发现和维护分组，而不是布局结构不能表达一组一列**。[S1], [S3]

这不能通过开启 `disable_native_tabs` 解决。该配置默认是 `false`，设为 `true` 会跳过自动合组，保留独立列。[S6]

## 当前识别链路

1. 应用 AX inventory 或窗口创建事件提供窗口候选，`spawn_window_trigger()` 按窗口身份创建独立 ECS entity。这是跟踪身份，不是最终物理窗口分组。[S4]
2. `detect_tabbed_windows()` 只为 `Added<Window>` 建立检测任务。它从当时的 **active layout strip** 收集同应用候选，保存扣除 Spool padding 后的观测矩形快照；候选列表在此次任务内不刷新。[S2]
3. 检测每 50 ms 重试，最长约 1 秒。新成员必须在当前 active strip 且出现在 onscreen 列表；旧候选必须不在 onscreen 列表，但其 ID 和 PID 仍在整个会话的窗口清单中。[S2], [S5]
4. 同应用、同 strip、尺寸误差不超过 1，且位置误差不超过 1 时合组；位置不匹配时，还允许尺寸匹配、旧矩形越过 active display 边界的候选。这是几何和可见性启发式，没有读取系统提供的 native-tab group ID。[S2]
5. 合组成功调用 `convert_to_tabs()`；没有成功时，普通 placement 路径仍可给各身份分配独立列。[S1], [S2], [S4]

`Update` 的相关顺序是 defaults、defaults frame commit、tab detection、普通 placement。因此“系统刚发现三个身份”和“布局最后只保留一个位置”是不同阶段，不能把创建 ECS entity 本身理解为必然多占一列。[S7]

## 已有改动与剩余缺口

工作区在本轮开始前已经包含以下修复：保存几何候选快照、处理 padding 差异、等待 WindowServer 可见性延迟，以及普通布局/动画/漂移纠正不再分别写入 inactive tab 的几何。现有写入保护针对已识别的 tiled tab group；defaults 与跨显示器 transfer 有独立路径，不能扩大为“所有操作永远只写一个 ID”。这些是既有改动，不是本次调研实现。[S2], [S8]

以下是从代码控制流确认的覆盖边界，**不是对用户当前三列现象的实机归因**：

| 场景 | 当前边界 | 影响 |
| --- | --- | --- |
| 启动时批量发现已存在的 tab | 若捕获候选时布局中还没有这些成员，快照候选为空；重试不重建候选。 | 存在无法补成一组的路径。 |
| 多显示器或非当前 Space | 候选和待识别成员限于 active strip，兜底几何使用 active display。 | 并不覆盖所有已知 Space 或所有可见显示器。 |
| 旧窗口合并为 tab，或把 tab 拖出 | 检测入口只由新增 ECS `Window` 触发；没有在这个 detector 内持续校验既有组成员关系。 | OS 若保留既有身份，这些变化本身不会重启检测；拆组也需要额外观测。 |
| 观测延迟超过 1 秒或暂时查询失败 | 任务到期后删除，未看到持续重试或全量分组重建入口。 | 已分配的独立列可能保留。 |
| 普通窗口恰好同应用、同尺寸、旧窗口不在 onscreen | 即使验证 ID 仍存活，也只是排除了关闭的一种情况。 | 仍缺少确定的 tab 关系，不能无条件依赖矩形合组。 |

以上入口、时间限制和过滤条件见 [S2]；普通 placement 与启动顺序见 [S4], [S7]。没有检查当前安装的 daemon 是否由这份工作区构建，也没有检查用户实际配置。

## 建议的行为契约

这是按用户澄清调整后的建议，不是已完成的功能。**布局和操作层可以不理解 tab；发现和平台适配层仍必须区分独立窗口与不应单独平铺的系统对象。** 简单删除 detector、随后将全部 AX 对象各自入列，并不能达到这个目标。[S2], [S4]

- **一个独立窗口一个位置。** 布局只管理外框、滚动位置、窗口间导航，不消费 tab 数量或内部结构。
- **内部变化不改变布局。** 应用切换、增删或排列 tab 不应触发 Spool 插列。若系统随之更换可操作对象，平台层需要维护同一个布局窗口的控制目标连续性，而不是向布局报告新的独立窗口。
- **不要求完整枚举后台 tab。** 发现层可以保留为身份连续性和生命周期确认所必需的元数据，但不把后台对象作为独立布局项；tab 切换、内容和排序仍由应用负责。
- **只响应独立窗口的增减。** 拖出 tab 后确实出现新的独立窗口，才新增位置；合并后不再存在多个独立窗口，才回收多余位置。这是窗口生命周期的观测，不是替应用执行合并或拆分。
- **不把 onscreen 当作是否参与布局的总开关。** 滚出屏幕、其他 Space、最小化和真正的 inactive tab 必须区分；查询失败保留不确定性，不等同于关闭。[S5], [9], [10], [11]

更符合这一目标的架构是 `AX / WindowServer 对象 -> 独立窗口识别与控制目标解析 -> 普通布局窗口`，而不是让布局层理解 tab 成员并调度它们。当前 `Column::Tabs` 可以解释现状，也可能用于迁移兼容，但不是用户要求保留的最终抽象。要先验证独立窗口识别和身份连续性，再决定如何移除布局中的 tab 专用语义，不能把删除 detector 本身当作修复。

AX tab controls 或其他 backend 证据可以作为发现层的待验证补充，但下面的公共 API 调研没有发现能直接返回所有应用“独立可管理窗口”的通用接口，也没有找到完整的跨进程 native-tab group 查询契约。因此可以确定管理边界，却不能承诺仅靠现有 AX 枚举、一个 onscreen 过滤条件或完全不处理系统身份差异就能可靠实现。

Bar 是另一个展示边界：当前按组占一个横向位置，但仍可绘制多个叠放的成员图标。按澄清后的边界，它也应消费同一个普通窗口投影，而不是把后台 tab 展示为独立窗口。这不是当前三列问题的根因。[S9]

## 验证与后续验收

本轮执行并通过：

- `cargo test --locked tabs:: -- --test-threads=1`：12 passed，928 filtered out。
- `cargo fmt -- --check`。
- `cargo check --locked`。
- `cargo clippy --locked --all-targets -- -D warnings`。

现有测试覆盖新 tab 合为同列、padding 差异、可见性延迟、关闭后保留列、inactive tab 几何保护，以及三成员组只通过 selected member 隐藏/恢复。**这不是全量测试，也不是对真实 AppKit 行为的验证。**[S10]

修复识别覆盖后，至少需要以下回归与实机验收：启动前已有三个 tab 仍只有一列；新增/切换/关闭 tab 不改变原列位置；合并既有窗口收为一列；拖出成员才增加列；第二显示器和 Space 切换后关系正确；普通同尺寸窗口、滚动屏外窗口、AX 无响应不得误并或消失。用户当前场景的具体应用、AX/CG 观测序列和运行版本仍待确认。

## Public API Evidence

The following platform research uses current Apple documentation fetched directly via its `.md` endpoints and Apple's installed macOS 26.4 SDK headers. It establishes API contracts, not observed behavior in the user's application.

## Documented AppKit Model

- **`NSWindow.tabbedWindows` (10.12+):** an array of windows displayed as tabs, but **nil when the window is not showing a tab bar**. The SDK calls it a wrapper over `tabGroup.windows` with that visibility condition. Nil is therefore not a documented test for the absence of a tab group. [1], [H]
- **`NSWindow.tabGroup` (10.13+):** the window's tab-group object; the SDK says it is created lazily on demand. **`NSWindowTabGroup.windows`** contains the member `NSWindow` objects in visual, leading-to-trailing tab order and supports KVO. The SDK describes one virtual tabbed window made from this group of windows. Group-object availability alone is not a documented multi-member test. [2], [3], [H]
- **`selectedWindow`:** the group's selected/frontmost member. Setting it changes the selected tab; assigning a nonmember raises an exception. It supports KVO. This is group-local selection, not a promise that the application or window has global keyboard focus. [4]
- **`tabbingIdentifier`:** the SDK describes matching values as allowing windows to tab together. Equality establishes eligibility, not proof of current membership; actual membership is represented by `tabGroup.windows`. [3], [H]

## Objects Versus Window IDs

**Documented:** native AppKit tab members are separate `NSWindow` objects, not merely content tabs inside one `NSWindow`. That object-level model does **not** establish one independently enumerable, persistent WindowServer ID per member in every tab state. [3], [H]

Apple documents `NSWindow.windowNumber` as a window-device number, unique within the application, explicitly distinguishing it from the server's global window number; it can be nonpositive when no window device exists. Separately, `kCGWindowNumber` identifies a server window uniquely within the current user session. These sources do not specify a tab member's server-device lifetime or ID behavior across selection, merging, and detaching. Do not turn common runtime mappings into an unconditional identity guarantee. [5], [6]

## What a Public External Client Can Observe

| Surface | Documented contract | Boundary |
| --- | --- | --- |
| AppKit tab properties | Membership, tab order, and selected member on actual AppKit objects. | These declarations are not a remote lookup protocol from another process's AX element or CG ID. [2], [3], [4], [H] |
| Application `AXWindows` | `accessibilityWindows()` returns all the application's windows; SDK definitions specify an array of accessibility UI elements and connect it to `AXWindows`. | This is an accessibility representation, not an enumeration of Objective-C objects or a native-tab membership mapping. Neither source specifies a separate top-level AX element for every inactive native tab. [7], [H] |
| `AXTabs` / `AXTabGroup` | Public accessibility vocabulary exists for tab controls; `accessibilityTabs()` returns a tab view's tab accessibility elements. | Generic tab UI is not a documented `NSWindowTabGroup` identity, membership, or member-to-CG-ID bridge. [8], [H] |
| `CGWindowListCopyWindowInfo` | Dictionaries describing server windows in the current user session. `.optionAll` includes on- and offscreen windows; `.optionOnScreenOnly` returns onscreen windows front-to-back. | Neither enumeration describes native-tab membership or selection. `.optionAll` broadens server-window coverage, not the contract to include every AppKit object. [9], [10], [11] |

AX is interprocess messaging: the public header documents unsupported attributes, missing values, invalid elements, incomplete messaging, and incomplete accessibility implementations; it also defines client trust checks. An unsuccessful read is not proof that a window or tab does not exist. The reviewed public AX declarations provide a PID lookup but no general AX-window-to-`CGWindowID` conversion or native-tab-group query. This last statement is a **bounded API-surface finding**, not a claim that app-specific or private mechanisms are impossible. [H]

CG dictionaries have required identity, owner PID, bounds, layer, and other fields; window name and onscreen status are optional keys. The documented key lists have no native-tab group ID, member array, or selected-tab field. A CG entry is consequently not synonymous with a manageable document window. The listing can return an empty array for no matches, or null outside a GUI session/without WindowServer. [9], [12], [13]

## Inference and Unverified Behavior

**Conclusion from the reviewed contracts:** public AX plus CGWindowList does not establish reliable, application-independent enumeration and grouping of all native tabs. Accessible tab controls may support app-specific observation, but complete membership, selection, and ID continuity require additional evidence; the AppKit in-process model alone cannot supply it. [2], [3], [4], [7], [8], [H]

Matching owner, frame, size, titles, or an onscreen/offscreen transition is **heuristic evidence**, not a documented native-tab relationship. Nor does disappearance from one list establish destruction. The exact inactive-member entries/IDs, startup snapshots, merge/detach transitions, and Space/display coverage remain **unverified here**. No claim of a fix or live acceptance is made. [5], [9], [10], [11], [12], [13]

## Primary Sources

[1]: https://developer.apple.com/documentation/appkit/nswindow/tabbedwindows.md
[2]: https://developer.apple.com/documentation/appkit/nswindow/tabgroup.md
[3]: https://developer.apple.com/documentation/appkit/nswindowtabgroup/windows.md
[4]: https://developer.apple.com/documentation/appkit/nswindowtabgroup/selectedwindow.md
[5]: https://developer.apple.com/documentation/appkit/nswindow/windownumber.md
[6]: https://developer.apple.com/documentation/coregraphics/kcgwindownumber.md
[7]: https://developer.apple.com/documentation/appkit/nsaccessibilityprotocol/accessibilitywindows().md
[8]: https://developer.apple.com/documentation/appkit/nsaccessibilityprotocol/accessibilitytabs().md
[9]: https://developer.apple.com/documentation/coregraphics/cgwindowlistcopywindowinfo(_:_:).md
[10]: https://developer.apple.com/documentation/coregraphics/cgwindowlistoption/optionall.md
[11]: https://developer.apple.com/documentation/coregraphics/cgwindowlistoption/optiononscreenonly.md
[12]: https://developer.apple.com/documentation/coregraphics/required-window-list-keys.md
[13]: https://developer.apple.com/documentation/coregraphics/optional-window-list-keys.md
[H]: #sdk-header-evidence

The numbered citations link to the Apple pages fetched for this note. Header evidence supplements those pages, rather than substituting inferred runtime behavior for their contracts.

**Unavailable / unknown:** attempted standalone Apple web pages for `kAXWindowsAttribute`, `AXUIElement`, and `AXUIElementCopyAttributeValue` returned HTTP 404; their evidence here comes from installed Apple SDK headers. Whether a particular application's AX tab controls link every inactive native member to its group and CG ID remains unknown. No universal API-absence proof or runtime linkage test was attempted.

### SDK Header Evidence

Apple macOS 26.4 SDK, under `/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk/System/Library/Frameworks/`:

- `AppKit.framework/Headers/NSWindow.h`: `tabbingIdentifier`, `tabbedWindows`, `tabGroup` comments.
- `AppKit.framework/Headers/NSWindowTabGroup.h`: `windows`, `selectedWindow`, and membership operations.
- `AppKit.framework/Headers/NSAccessibilityProtocols.h` and `NSAccessibilityConstants.h`: application windows, tab-view elements, and tab-group role.
- `ApplicationServices.framework/Frameworks/HIServices.framework/Headers/AXAttributeConstants.h`: `kAXWindowsAttribute`, `kAXTabsAttribute`.
- `ApplicationServices.framework/Frameworks/HIServices.framework/Headers/AXUIElement.h`: public client API declarations, attribute reads, errors, trust, and `AXUIElementGetPid`.
- `CoreGraphics.framework/Headers/CGWindow.h`: `CGWindowID`, required/optional dictionary keys, and window-list options.

## Local Code Sources

[S1]: ../../src/ecs/layout.rs#L702
[S2]: ../../src/ecs/systems.rs#L1977
[S3]: ../../src/ecs/triggers.rs#L477
[S4]: ../../src/ecs/triggers.rs#L1498
[S5]: ../../src/manager.rs#L850
[S6]: ../CONFIGURATION.md#1-global-options-options
[S7]: ../../src/ecs.rs#L199
[S8]: ../../src/ecs/systems.rs#L1660
[S9]: ../../src/bar/layout.rs#L556
[S10]: ../../src/tests/tabs.rs#L57

Additional local evidence: application inventory in `src/manager/app.rs:267`, ordinary placement in `src/ecs/triggers.rs:1915`, inactive-tab drift protection in `src/ecs/reconcile.rs:877`, and external geometry observation in `src/ecs/window_geometry.rs:198`. Line numbers describe this reviewed working-tree snapshot and may drift with later edits.
