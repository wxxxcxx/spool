# 窗口身份：编号复用、NSWindow.windowNumber 与 CFHash(AXUIElement)

调研日期：2026-09-16。本机 macOS 26.6.2 (25G83)，单显示器，一个登录会话。
对应票：[窗口唯一标识的核实](../wayfinding/declarative-state/issues/27-window-identity-verification.md)。
探针：[scripts/probe-window-identity.m](../../scripts/probe-window-identity.m)（只创建/销毁自己的小无边框窗口，无任何写操作或跨应用操作）。

## 结论摘要

- **一手文档只保证"当前用户会话内唯一"，不说寿命与复用**（[1]、[2]、[H]）。`NSWindow.windowNumber` 的文档同样不讲复用（[3]）。
- **实机没有复现复用**：同一登录会话内 4 个进程、275 次"创建→销毁→再创建"循环、3 轮 4 窗口批量、"释放中间槽位"三种场景，编号**单调递增、无一重复**（48264…48614）。代码注释里"WindowServer 会复用整数 ID"这一断言**没有一手出处，也没有现场证据支持**；但也没有任何文档保证它不会发生。
- **`NSWindow.windowNumber` 与 `kCGWindowNumber` 在实机上是同一个数值**（全部配对样本相等），尽管 Apple 文档明确写"isn't the same as the global window number assigned by the window server"（[3]）。→ 二者不能当作两条独立证据互相佐证。
- **`CFHash(AXUIElement)` 不是对象地址，而是基于底层元素身份的取值哈希**：同一 pid 的两次 `AXUIElementCreateApplication` 是不同对象（不同指针）却 `CFEqual=1` 且 `CFHash` 相等；不同 pid 不等且哈希不同。窗口元素是否同样如此**未验证**（本机进程无辅助功能信任）。
- **本次未完成的另一半**：`AXIdentifier` 是否自动反映 `NSWindow.identifier`、同一窗口两条 AX 路径的元素是否 `CFHash` 相等、`_AXUIElementGetWindow` 是否等于该窗口的 `kCGWindowNumber`——都需要辅助功能权限，见"未验证"。

## 核实方式

运行时探针（本机、单会话、只读）加一手文档/头文件核对。探针编译与运行：

```sh
clang -fobjc-arc -O0 -Wall -framework AppKit -framework ApplicationServices \
  -o /tmp/probe-window-identity scripts/probe-window-identity.m
/tmp/probe-window-identity 100          # 第二个可选参数 prompt-trust 触发系统权限提示
```

## 一、`kCGWindowNumber` 在窗口销毁后是否被复用

### 文档

- Apple 文档（[1]，页内原文）：*"The value for this key is a `CFNumber` type … that contains the window ID. The window ID is unique within the current user session."*
- 本机 SDK `CGWindow.h`（[H]）：*"The window ID, a unique value within the user session representing the window."*
- 两处都只声明唯一性，**不声明寿命，也不声明复用行为**。

### 实机观测

| 场景 | 观测结果 |
| --- | --- |
| 同进程顺序 100 次"创建→销毁→再创建" | 100 个互不相同的编号，0 次重复；编号连续递增 48496→48595 |
| 4 窗口批量 ×3 轮（每轮全部关闭后再开下一轮） | 48596–48599 / 48600–48603 / 48604–48607：跨轮也不复用 |
| 创建 4 个后只关闭中间一个 | 被释放的 48609 未被复用；新窗口拿到 48612（下一个编号） |
| 跨进程（4 个进程依次运行） | 48264…48322 → 48323…48461 → 48462…48494 → 48496…48614：前一个进程整体退出、其窗口全部销毁后，编号仍继续递增 |

汇总：同一登录会话内 275 次同进程创建/销毁 + 12 次批量创建 + 一次释放中间槽位，**0 次复用**；本会话已观察到的编号范围是 48264…48614（连续、无空洞被重新填回）。

### 结论与后果

- **未复现不等于保证**。会话编号空间有限，WindowServer/登录重建、编号空间耗尽、更长的会话未被覆盖，而文档没有给出任何承诺。因此本次观测**不足以**把"编号在同一会话内不复用"当作事实写进证据模型。
- 因此**维持现行谨慎策略**：窗口编号仍必须由 pid/bundle（以及 AX 属性或几何）佐证才能建立候选绑定，绝不单独充当身份；跨登出/重启本来就无文档保证。
- 但代码里"`WindowServer` 会复用整数 ID"的断言属于**过度断言**：它既没有一手出处，也与本次现场观测不符。正确表述是"复用未被观测到、且无文档保证，故编号仍需佐证"。

## 二、`NSWindow.windowNumber` 与 `kCGWindowNumber` 的关系

### 文档

- `NSWindow.windowNumber`（[3]）：摘要 *"The window number of the window's window device."*；讨论 *"Each window device in an application is given a unique window number—note that this isn't the same as the global window number assigned by the window server."*，以及 *"If the window doesn't have a window device, the value of this property is equal to or less than `0`."*
- `kCGWindowNumber` 的唯一性范围是"当前用户会话"（[1]、[2]）。文档把两者描述成不同概念（应用内 window device 编号 vs window server 的全局编号）。

### 实机观测

每次只创建一个窗口，用 `CGWindowListCopyWindowInfo(kCGWindowListOptionAll)` 的集合差取出该窗口的 `kCGWindowNumber`，再与该 `NSWindow` 的 `windowNumber` 比对：

```
PAIR phase=sequential index=0 ns_windowNumber=48496 cg_windowNumber=48496
...
TARGET ns_windowNumber=48613 anonymous_ns_windowNumber=48614
```

本机 macOS 26.6.2 上，普通 AppKit 窗口的两个数值**全部相等**（120+ 样本，含设置与未设置 `identifier` 的窗口，含顺序、批量、部分关闭三种场景）。

### 结论与后果

- 文档的"不同"是概念与所有权口径（应用内唯一 vs 会话内唯一），不是数值不同；在本机实现上它们是同一个整数。
- Spool 目前不读 `NSWindow.windowNumber`（只有 overlay 用 `NSWindow`），所以现在没有"两条独立证据"的问题。记录此条是为了防止今后把二者当作互相独立的佐证。

## 三、`CFHash(AXUIElement)` 到底哈希什么

### 文档

- `AXUIElement.h`（[H]）只说 `AXUIElementRef` 是 `CFTypeRef`，可与 `CFRetain` / `CFRelease` / `CFEqual` 等 Core Foundation 多态函数一起使用；**没有一句关于 `CFHash` 语义或元素等价判定的说明**。
- Apple 文档页 `AXUIElement`（[4]）只有摘要 "A structure used to refer to an accessibility object." 与一段概述，没有等价/哈希语义。
- `kAXIdentifierAttribute`（[5]）在文档里**只有符号本身**（macOS 10.7+，无 Discussion 文本）：不说明谁提供它、是否稳定、是否唯一。头文件（[H]）也只是一行 `#define kAXIdentifierAttribute CFSTR("AXIdentifier")`，归在 "UI element identification attributes" 下。
- `NSWindow.identifier` / `NSUserInterfaceItemIdentification`（[6]）的文档说明它是**应用内**用于状态恢复的唯一标识：*"Identifiers are used during window restoration operations to uniquely identify the windows of the application."*；恢复要求"每个窗口配置唯一 identifier、frame autosave name 与 restoration class"（[7]）。这是应用自己维护的恢复标识，不是窗口服务器身份。

### 实机观测（不需要权限的那一半）

```
AX_OBJECT first=47434252224 second=47434252080 other=47434251984 system_wide=47434251408
AX_OBJECT_SAME_PID equal=1 same_hash=1 first_hash=1634788537 second_hash=1634788537
AX_OBJECT_OTHER_PID equal=0 same_hash=0
AX_OBJECT_HASH_IS_POINTER first=0 second=0 system=0
AX_OBJECT_UNTRUSTED_READ error=-25208 value=(none)
```

- 同一个 pid 的两次 `AXUIElementCreateApplication` 返回**不同对象**（指针 47434252224 vs 47434252080），但 `CFEqual=1`、`CFHash` 相等（1634788537）且哈希不等于指针 ⇒ `AXUIElement` **实现的是取值哈希**，取自底层元素身份，而不是对象地址。
- 不同 pid（`other`）⇒ `CFEqual=0`、哈希不同。
- 未受信任的进程读取自己的 `kAXWindowsAttribute` 得到 `-25208` = `kAXErrorNotImplemented`（[H] `AXError.h`），没有拿到值。

### 对 `WindowIncarnation` 的意义与仍未验证的部分

`WindowIncarnation` 是 `CFHash(element)`（`src/manager/windows.rs`）。上面这一半说明"两次拿到同一元素的包装对象会哈希相同"这件事**在应用级元素上成立**，因此"包装对象不同 ⇒ 哈希必不同"的假阴性风险比原先设想的小。但**窗口级**元素是否同样如此仍未验证：同一窗口经 `kAXWindowsAttribute`、`kAXFocusedWindowAttribute`、`AXUIElementCopyElementAtPosition` 三条路径取得的元素是否 `CFEqual`/`CFHash` 相等，需要辅助功能权限才能测。

## 四、对 Spool 证据阶梯的意义

| 证据 | 状态 | 在恢复中的用法 |
| --- | --- | --- |
| `kCGWindowNumber` | 会话内唯一（文档 [1][2]）；本会话未见复用（实机）；寿命与复用无文档 | 同一登录会话内最强的单条**提示**，但必须由 pid/bundle（必要时再叠加 AX 属性/几何）佐证；跨登出无效 |
| `pid` + `bundle_id` | 文档级事实 | 应用级连续性必要条件，不区分同一应用的多个窗口，不构成身份 |
| `AXIdentifier` | 文档只有符号（[5]）；是否由 AppKit 从 `NSWindow.identifier` 填充**未验证** | 若验证可用，才可能升为主证据，并需要"应用未提供时"的回落阶梯 |
| `NSWindow.identifier` / 应用状态恢复 | 应用自己实现的**应用内**恢复标识（[6][7]） | 只能经 `AXIdentifier` 间接观察；不是服务器身份 |
| 标题/AX role/几何 | 会变化 | 佐证，不是身份 |
| `CFHash(AXUIElement)` | 取值哈希（实机，应用级元素） | 现有 `incarnation` 用法保持；窗口级路径等价性未验证，不能假定"两条路径必然相等" |

## 五、未验证与边界

- 只在本机、单会话、单显示器、普通 AppKit 窗口上观测；未覆盖其他实现创建的窗口（SkyLight/私有层窗口、无窗口设备的窗口）、原生 tab 成员、以及编号空间耗尽或极长会话。
- 未做跨登录/重启观测；编号跨登录本就无文档保证。
- **未验证**：窗口元素经不同 AX 路径的 `CFHash` 等价性；`AXIdentifier` 是否反映 `NSWindow.identifier`；`_AXUIElementGetWindow` 返回值是否等于该窗口的 `kCGWindowNumber`。这三项都需要给探针进程（其所属应用）辅助功能权限后重跑 `probe-window-identity`；`--prompt-trust` 只触发系统提示，授权后需重启探针。
- 本调查不改动任何运行期行为；`incarnation` 语义若需要变，另开票。

## Primary Sources

- [1]: https://developer.apple.com/documentation/coregraphics/kcgwindownumber
- [2]: https://developer.apple.com/documentation/coregraphics/required-window-list-keys
- [3]: https://developer.apple.com/documentation/appkit/nswindow/windownumber
- [4]: https://developer.apple.com/documentation/applicationservices/axuielement
- [5]: https://developer.apple.com/documentation/applicationservices/kaxidentifierattribute
- [6]: https://developer.apple.com/documentation/appkit/nsuserinterfaceitemidentification/identifier
- [7]: https://developer.apple.com/documentation/appkit/restoring-your-app-s-state-with-appkit
- [H]: #sdk-header-evidence
- [P]: ../../scripts/probe-window-identity.m — 探针源码；下文引自 2026-09-16 在本机的四次运行输出（`/tmp/probe-window-identity-2026-09-16.txt` 为其中一次）

### SDK Header Evidence

Apple macOS 26.6 SDK（`MacOSX.sdk`），`xcrun --show-sdk-path` 指向的 Xcode SDK：

- `CoreGraphics.framework/Headers/CGWindow.h`：`kCGWindowNumber` 的注释（"a unique value within the user session"）。
- `AppKit.framework/Headers/NSWindow.h`：`windowNumber`、`windowNumbersWithOptions:` 声明（细节在文档页 [3]）。
- `AppKit.framework/Headers/NSUserInterfaceItemIdentification.h`：`identifier` 的协议注释（应用内唯一、供恢复使用）。
- `AppKit.framework/Headers/NSWindowRestoration.h`：按 identifier 恢复窗口的协议。
- `ApplicationServices.framework/Frameworks/HIServices.framework/Headers/AXAttributeConstants.h`：`kAXIdentifierAttribute` 定义（"UI element identification attributes"）。
- `ApplicationServices.framework/Frameworks/HIServices.framework/Headers/AXUIElement.h`：`AXUIElementRef` 是 CFTypeRef、可用 CF 多态函数；无哈希/等价语义说明。
- `ApplicationServices.framework/Frameworks/HIServices.framework/Headers/AXError.h`：错误码（`-25208` = `kAXErrorNotImplemented`、`-25211` = `kAXErrorAPIDisabled`）。
