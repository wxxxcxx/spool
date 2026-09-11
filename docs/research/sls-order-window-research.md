# macOS 私有 `SLSOrderWindow`：签名、ABI、跨进程权限与实测结论

核查日期：2026-09-11。用途：回答“`SLSOrderWindow` 到底是什么、窗口管理器能否在不改变焦点的前提下用它重排窗口层级”。

本文遵守与 [macos-window-capability-matrix.md](macos-window-capability-matrix.md) 相同的证据分级：**公开契约**=Apple 文档/SDK；**Mach-O 证据**=本机 dyld shared cache 中的导出符号与反汇编；**实现证据**=固定提交中的第三方源码；**本机实测**=本文在核查机上实际运行探针得到的返回值与观测量；**未验证**=只在博客/gist/论坛出现、本次未能复核的说法。所有“本机实测”结论只对应 macOS 26.6.2 (25G83) / arm64e，不外推到其他版本。

## 1. 结论摘要

一句话：**在未注入、SIP 未放宽的普通进程里，`SLSOrderWindow` 对“别人的窗口”会被 WindowServer 拒绝（返回 `1000`），对“自己的窗口”会被接受（返回 `0`）但观测不到任何层级变化；本次没有找到任何在普通进程内用它重排窗口（无论自有还是第三方）的可行路径。**

| 问题 | 结论 | 证据 |
| --- | --- | --- |
| 精确签名 | `CGError SLSOrderWindow(int cid, uint32_t wid, int order, uint32_t relativeToWid)` | [M1] 反汇编；yabai [Y1]、Rift [R1] 头声明一致 |
| 返回值类型 | `CGError`（`int32`），`0=kCGErrorSuccess`、`1000=kCGErrorFailure`、`1001=kCGErrorIllegalArgument` | [M2][M3]；本机实测复现 0/1000/1001 |
| order 取值 | `-1=below`、`0=out`、`1=above`、`2=in`（隐藏后恢复）；其他值返回 `1001` | [H1] 枚举；[M1] `2` 的处理分支；本机实测 `2` 恢复可见、`3` 返回 `1001` |
| 非 owner 进程重排第三方窗口 | **被拒绝**：`place=1/-1`、`relativeTo=0/对方窗口` 全部返回 `1000`，层级与焦点均无变化 | 本机实测（第 4 节） |
| 自有窗口重排 | **接受但无效**：返回 `0`，`order=0/2` 的隐藏/恢复有效，但 `above/below` 相对排序观测不到变化；公共 `-[NSWindow orderWindow:relativeTo:]` 在同一测量下有效 | 本机实测（第 4 节） |
| 是否改变焦点/前台 | 对第三方窗口调用被拒绝，前台应用与目标应用 AX focused/main 窗口全程不变；不能用它“既排序又不动焦点”，因为排序本身不成立 | 本机实测（第 4 节） |
| 可行的等价物 | yabai 经 Dock scripting addition（注入）调用同一函数；文档明确该路线要求放宽 SIP，Spool 不可用 | [Y2][Y3][Y5] |
| 不改变焦点的可行排序 | 本次验证可用的只有：自有窗口的公共 AppKit 排序；第三方窗口的 `AXRaise`（会改变目标应用自身 key/main 窗口） | [A4]；本机实测 |

## 2. 精确 ABI

### 2.1 签名与参数

`SLSOrderWindow` 是 SkyLight 的 C 导出符号，头文件不存在于任何 SDK 中；签名来自调用方声明与反汇编互证。

```c
extern CGError SLSOrderWindow(int cid, uint32_t wid, int order, uint32_t relativeToWid);
```

- `cid`（`int`）：调用方连接 ID，通常取 `SLSMainConnectionID()`。[M1][Y1][R1]
- `wid`（`uint32_t`）：被排序窗口的 WindowServer window ID。[M1]
- `order`（`int`）：相对放置模式，见 2.2。[H1][M1]
- `relativeToWid`（`uint32_t`）：参照窗口 ID；`0` 表示在该 level 内相对全体窗口放到最前/最后。[A4 对应语义][M1]

反汇编层面的证据：`_SLSOrderWindow`（`dyld_info -exports` 文件偏移 `0x329D78`）只做寄存器整理后跳到 `_SLSOrderWindowList(cid, &wid, &order, &relativeToWid, 1)`；`w1,cid` 存入栈槽、`w3/w2`（relativeToWid、order）成对存栈，随后 `x1=栈槽(&wid)`、`x2=sp+0x10(&order)`、`x3=sp+0xc(&relativeToWid)`、`w4=1`。这与上述参数顺序一致。[M1]

> **已核实**：导出符号名与偏移可由 `dyld_info -exports` 复核（见第 2.3 节与 [M2]）；但**不能**从反汇编断言“参数名”本身，`wid`/`order`/`relativeToWid` 的语义由调用方声明与实测共同确定。

### 2.2 `order` 的整数值

私有枚举（CGSInternal 头文件，被多项目引用）：

```c
typedef enum {
    kCGSOrderBelow = -1,
    kCGSOrderOut,   /* 0: hides the window */
    kCGSOrderAbove, /* 1 */
    kCGSOrderIn     /* 2: shows the window */
} CGSWindowOrderingMode;
```

- [H1] 该枚举存在于 `NUIKit/CGSInternal` 的 `CGSWindow.h`（Alacatia Labs 2007-2008 头文件整理，Robert Widmann 2015-2016 更新），同一文件给出 `CGSOrderWindow(CGSConnectionID, CGWindowID, CGSWindowOrderingMode, CGWindowID)`。**这是第三方头文件整理，不是 Apple 头**；标为 `[第三方头]`，但与反汇编和实测一致。
- [M1] 反汇编显示 `_SLSOrderWindowList` 对 `order` 数组做了 `==2 → 改成 1（above）+ relativeToWid=0` 的重写，正对应 `kCGSOrderIn` 的“显示”语义。**该改写位于 List 变体；`SLSOrderWindow` 单个包装把 `2` 原样传入**，本机实测 `order=2` 在 `order=0` 隐藏后能把窗口恢复可见。
- 本机实测：`order=3` 返回 `1001`（`kCGErrorIllegalArgument`），说明取值有校验。

### 2.3 变体与相关符号（本机 macOS 26.6.2 arm64e 导出表）

以下均为 `dyld_info -exports` 在 `/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight` 上得到的实际导出行 [M2]：

| 符号 | 备注 |
| --- | --- |
| `_SLSOrderWindow` | 单窗口包装，等价于 `SLSOrderWindowList(..., 1)` |
| `_SLSOrderWindowList` | 数组：`(cid, const uint32_t *wids, const int *orders, const uint32_t *relativeTo, uint32_t count)`；内部对每个 wid 调 `__CGSOrderWindow` [M1] |
| `_SLSOrderWindowListWithOperation` / `_SLSOrderWindowListWithGroups` | 数组 + 组/操作形式；`WithGroup` 走 `_SLSWindowBridgedOrder` 与 `__CGSOrderWindowListWithGroups` [M1] |
| `_SLSOrderWindowWithGroup` | 单窗口 + group 形式，转发到 `SLSOrderWindowListWithGroups` [M1] |
| `_SLSTransactionOrderWindow` / `_SLSTransactionOrderWindowGroup` / `_SLSTransactionOrderWindowGroupFrontConditionally` / `_SLSTransactionSafeOrderWindowGroup` | 事务版本；yabai 用 `SLSTransactionOrderWindowGroup` 做 proxy 动画 [Y4] |
| `_SLSOrderFrontConditionally`、`_SLSSetFrontWindow`、`_SLSSetIgnoreAsFrontWindow` | 与“置前/前置窗口”相关但不等于相对排序 |
| `_SLSSetWindowLevel`、`_SLSGetWindowLevel`、`_SLSSetWindowSubLevel`、`_SLSSetWindowLevelForGroup`、`_SLSSetWindowLevelsForActivation` | 数值 level；`SLSSetWindowLevel` 走 `_objc_msgSend$setWindowLevel:` 后直接发 `mach_msg`，**其返回语义不可靠**（见 2.4） |
| `_SLSSetWindowSubLevel` | yabai 在 Dock 内用 `CGWindowLevelForKey(layer)` 调用它 [Y4] |
| `_SLSSetDeferOrdering`、`_SLSDoDeferredOrdering`、`_SLSBlockWindowOrdering` | 延迟/阻塞排序控制；本机实测 `SLSDoDeferredOrdering` 并未让排序生效 |

**没有** `SLSPillowTalk*` 符号；`_SLSSetFrontProcessWithInfo` 是焦点/前置路径（`SLPSSetFrontProcessWithInfo` 系列），与排序无关。[M2]

注意：本机 `dyld_info` 明确报告 x86_64 slice 不存在（纯 arm64e 系统），因此上面的符号表只覆盖 arm64e。[M2]

### 2.4 返回值为何“看着成功其实没做”

`SLSOrderWindow` → `SLSOrderWindowList` → `__CGSOrderWindow`：后者只发一条 `mach_msg`（msgh_id `0x30004003`）到 WindowServer，把 reply 里的结果作为 `CGError` 返回；返回值有真实语义（`0`/`1000`/`1001` 都能复现）。[M1]

对比 `SLSSetWindowLevel`：它先写本地映射对象的 level 缓存，再发一条**不等待结果**的 `mach_msg`，随后无条件 `mov w22, #0` 返回 `0`。也就是说该函数的返回值不携带服务器裁决。[M1] → 不能把“`SLSSetWindowLevel` 返回 0”当成权限通过的证据。（这一条来自反汇编，本轮**没有**对第三方窗口做 `SLSSetWindowLevel` 的独立实测；不要把它当成“已实测第三方 level 写入会成功/失败”。）

## 3. 归属与权限约束

### 3.1 本机实测：非 owner 进程被拒

在被注入的 WindowServer 语义里，“owner”是创建窗口的连接。`SLSGetWindowOwner(cid, wid, &outOwnerCid)` 可验证这一点，本机实测：

```
main_cid=741611 own_wid=8127 owner_cid=741611 getOwner_result=0 match=1
foreign_wid=5465 owner_cid=512259 result=0 match=0
```

即自有窗口的 owner connection == `SLSMainConnectionID()`，第三方 Finder 窗口属于另一个连接（`512259`）。因此 cid 参数本身没有传错。`CGSSetWindowOwner` / `SLSSetWindowOwner` 在当前系统**不导出**（`dyld_info` 无此符号）[M2]，所以不存在“先改 owner 再排序”的普通进程路径。[M2]

对第三方窗口的调用结果（第 4 节表格）全部是 `1000`；这既不是查询语法错误（同进程自有窗口返回 `0`），也不是“无需变更所以跳过”（请求构成真实前后关系变化时同样 `1000`）。

### 3.2 yabai 为什么“能用”

yabai 在普通 daemon 里**从不**直接调用 `SLSOrderWindow`；全仓库只有两类位置：

1. `src/misc/extern.h` 的声明与 `src/view.c` / `src/event_loop.c` 中针对**自有 feedback window** 的调用（`g_connection` + `feedback_window.id`，`relativeTo` 为受管 window id）。[Y1]
2. Dock 内 scripting addition：`src/osax/payload.m` 的 `do_window_order()` 执行 `SLSOrderWindow(SLSMainConnectionID(), a_wid, order, b_wid)`，其中 `a_wid` 是**第三方窗口**；daemon 侧经 socket 发 `SA_OPCODE_WINDOW_ORDER`，对应命令是 `window --raise` / `window --lower` / 插入 stack、toggle scratchpad 等。[Y2]

关键点：

- yabai 官方 wiki 明确：需要（部分）关闭 SIP 才能把 scripting addition 注入 `Dock.app`，而“它拥有到 WindowServer 的唯一连接”，并以此实现“control window layers”等能力。[Y5]
- `scripting_addition_order_window()` 的布尔返回值只表示 socket 往返是否成功（`scripting_addition_send_bytes` 里 `recv` 到一个 dummy 字节即 `true`），**不表示 WindowServer 接受了排序**；payload 内部也直接丢弃 `SLSOrderWindow` 的 `CGError`。[Y2]
- 因此，“yabai 用 `SLSOrderWindow` 成功重排第三方窗口”这一说法，其成立前提是 **Dock 注入**，不是普通进程可复制的调用方式。这与本机实测（普通进程被拒）一致，而不是矛盾。

**未验证**：网上（Stack Overflow、issue 讨论）存在“在 AppKit 应用里直接调 CGPrivate 排序函数即可控制其他应用窗口层级/焦点”的说法。本次抓取该页被反爬拦截（HTTP 403），未复核其代码与前提，故不采信；本文不把它列为证据。[U1]

### 3.3 版本说明

- 本机 macOS 26.6.2 (25G83) arm64e 上，`SLSOrderWindow` 及全部相关符号仍导出。[M2]
- 本次**没有**在 macOS 14/15 上运行探针，因此“14/15 上普通进程能否重排第三方窗口”保持未知。yabai 快照中的版本判定（含 `workspace_is_macos_sequoia()` / `workspace_is_macos_tahoe()`）用于验证与通知路径，不构成排序能力证据。[Y6]
- `kCGSOrderOut=0` 的含义意味着：把第三方窗口当参数时若真的“成功”，最坏结果是把别人的窗口从屏幕移除。探针因此从不以 `order=0` 作用于第三方窗口。

## 4. 本机实测（macOS 26.6.2 / 25G83 / arm64e）

探针源码：仓库内 `examples/native_window_probe/order_focus.m`（另有更早的最小探针 `order.m`）。探针进程以 accessory 激活策略启动并调用 `finishLaunching`，再让 run loop 转若干次——否则自有窗口根本不会进入 WindowServer 的 on-screen 列表，探针也就无法观测自己的调用。`dlopen` 解析 `SLSMainConnectionID`、`SLSOrderWindow`；`SLSGetWindowOwner` 见第 3.1 节。测量方式：自有窗口在 `CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly)` 中的前后 rank，外加公共 `-[NSWindow orderWindow:relativeTo:]` 的对照——对照证明同一对窗口的 rank 确实会随排序变化，所以“rank 不变”不是测量方法失灵。表末 `AXRaise` 行需要 `PROBE_AX_RAISE=1` 才会执行，因为它会改目标应用自身的 key/main 窗口。

| 调用 | 目标 | result | 观测到的层级变化 | 前台应用 | 目标应用 AX focused/main |
| --- | --- | --- | --- | --- | --- |
| **对照** `-[NSWindow orderWindow:NSWindowAbove relativeTo:]` | 自有 | — | **有**（A rank 0 / B rank 1，相对调用前反转） | 不变 | 不适用 |
| `own: B above A` | 自有 | `0` | 无（B 仍 rank 1、A 仍 rank 0） | 不变 | 不适用 |
| `own: A above B` | 自有 | `0` | 无 | 不变 | 不适用 |
| `own: A to front (rel=0)` | 自有 | `0` | 无 | 不变 | 不适用 |
| `own: order=0`（out） | 自有 | `0` | **有**：rank 0 → 不在 on-screen 列表 | 不变 | 不适用 |
| `own: order=2`（in） | 自有 | `0` | **有**：恢复在 on-screen 列表，rank 0 | 不变 | 不适用 |
| `own: order=3`（非法值） | 自有 | `1001` | 无 | 不变 | 不适用 |
| `own above foreign`（Finder 窗口作参照） | 自有 | `0` | 未单独验证（自有窗口 rank 未变） | 不变 | 不适用 |
| `foreign above own` | Finder | `1000` | 无（rank 3→3） | Chrome 不变 | 5465→5465 |
| `foreign below own` | Finder | `1000` | 无 | Chrome 不变 | 5465→5465 |
| `foreign above foreign`（已是该顺序） | Finder | `1000` | 无 | Chrome 不变 | 5465→5465 |
| `foreign above foreign`（**真实变化**） | Finder | `1000` | 无（rank 4→4） | Chrome 不变 | 5465→5465 |
| `AXRaise`（公开 AX 对照，opt-in） | Finder | 成功路径 | **有**（rank 3→1，短时间内置于前台应用窗口之上） | Chrome 不变 | focused/main **7315→5465** |

`AXRaise` 两列来自当日两次独立运行：rank 变化在 21:27 的 `order_focus` 运行中观测，key/main 变化由单独的 `ax_key_effect` 探针测得（`order_focus.m` 的默认路径不会执行它）。`order=0/2/3` 行已并入已提交探针，可原样复现。

附带观测：

- `order=0`（out）与 `order=2`（in）在自有窗口上**真的生效**，而 `above/below` 不生效；这说明调用确实到达 WindowServer，只是在普通连接下相对排序被静默忽略。`3`/`-2` 返回 `1001`，说明取值有校验。[M1]
- `SLSTransactionCreate` + `SLSTransactionOrderWindowGroup` 的自有窗口尝试返回值为垃圾值（探针未正确初始化其返回/声明），且同样观测不到重排；此路线**不确定**，不能据此认为事务路径可用或不可用。
- `CGWindowListCopyWindowInfo` 的裸列表顺序在本次对照中能反映公共 AppKit 排序（对照行），但对 `SLSOrderWindow` 的 `above/below` 不敏感——两者共同表明“无变化”是私有调用的性质，而不是列表不更新。Rift 另有“该列表顺序并非在所有情况下可靠”的警告 [R7]；因此本文不以裸列表 rank 作为唯一依据，另有 on-screen 成员变化（`order=0/2`）与 AppKit 对照两条交叉验证。
- 若要做“窗口 A 是否在 B 之上”的单点判定，可用 `kCGWindowListOptionOnScreenBelowWindow`（Apple 的 z-order 语义，见 [A12]）；本轮未使用该选项。

### 4.1 这组实测推翻了什么

- 推翻“普通进程持自己的 connection 就能重排第三方窗口”：本机为 `1000`，且 AX focused/main 与前台应用全程不变。
- 推翻“私有 `SLSOrderWindow` 是 `AXRaise` 之外更干净的无焦点排序手段”：在自有窗口上它是静默 no-op，在第三方窗口上直接失败。
- 修正“`order` 参数含义不清”：`-1/0/1/2` 的含义有头文件 + 反汇编 + 实测三方互证；`0` 是 `out`（隐藏），非常容易误用成“置底”。

## 5. GitHub 调研

| 项目（固定提交） | 是否使用 | 位置与调用 | 能得出什么 |
| --- | --- | --- | --- |
| [asmvik/yabai](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd) `dd84572` | **是** | 声明 [extern.h:33](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/misc/extern.h#L33)；自有 feedback window [view.c:42](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/view.c#L42)、[view.c:114](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/view.c#L114)、[event_loop.c:949](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/event_loop.c#L949)；第三方窗口只经 SA [payload.m:568](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/payload.m#L568)、[payload.m:856](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/payload.m#L856) | 第三方排序 = Dock 注入路线；daemon 直连只作用于自有窗口 |
| [acsandmann/rift](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1) `beeac0e` | **是**（仅自有窗口） | 绑定 [skylight.rs:528](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/skylight.rs#L528)；`order_above/below/out` [cgs_window.rs:242](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/cgs_window.rs#L242)；调用方为自有 UI [stack_line.rs:191](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/ui/stack_line.rs#L191)、[mission_control.rs:1425](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/ui/mission_control.rs#L1425) | 注释明确标 `kCGSOrderAbove=1` / `kCGSOrderBelow=-1` / `kCGSOrderOut=0`；全部落在自有窗口 |
| [lwouis/alt-tab-macos](https://github.com/lwouis/alt-tab-macos) | **否** | `src/macos/api-wrappers/SkyLight.framework.swift` 只绑定查询/通知/`SLSSpaceSetFrontPSN` 等；排序/raise 用 `AXUIElementPerformAction(kAXRaiseAction)` [AXUIElement.swift:321](https://github.com/lwouis/alt-tab-macos/blob/master/src/macos/api-wrappers/AXUIElement.swift#L321)，自有面板用 `orderFrontRegardless` | 一个成熟的跨应用切换器选择 AXRaise，而不是私有排序 |
| [ejbills/WindowKit](https://github.com/ejbills/WindowKit) | **否** | `CapturedWindow.swift:161` 用 `axEl.performAction(kAXRaiseAction)`；`SkyOverlay.swift:139` 用自有窗口 `orderFrontRegardless` | 同上 |
| [Hammerspoon/hammerspoon](https://github.com/Hammerspoon/hammerspoon) | **否** | 全仓库（含 `extensions/window/`）无 `SLSOrderWindow` / `CGSOrderWindow` / `kCGSOrder*`；窗口层级走 public AX | 缺席也是证据：跨应用窗口层级不靠私有排序 |
| [rxhanson/Rectangle](https://github.com/rxhanson/Rectangle) | **否** | 同上，无任何排序私有符号 | 纯几何管理器不需要它 |
| [kasper/phoenix](https://github.com/kasper/phoenix) | **否** | 同上 | — |
| [nikitabobko/AeroSpace](https://github.com/nikitabobko/AeroSpace) | **否** | 现快照（`c33cf4a` 远端 master）全仓库无 `SLSOrderWindow`；私有 API 集中在 `Sources/PrivateApi/include/private.h`，只做窗口信息读取 | i3 风格平铺器不依赖窗口排序 API |
| [NUIKit/CGSInternal](https://github.com/NUIKit/CGSInternal) | 头文件来源 | `CGSWindow.h` 定义 `CGSWindowOrderingMode` 与 `CGSOrderWindow` | 唯一的枚举出处（第三方整理，非 Apple 头） |

> 提交口径：yabai 与 Rift 使用仓库文档既有的固定 SHA；其他项目按分支抓取（`master` / 默认分支），克隆日期 2026-09-11，未固定 SHA，因此这些“缺席”结论只代表当日快照。抓取方式为 `git clone --depth 1` + 全树 grep（`SLSOrderWindow|CGSOrderWindow|kCGSOrder`）。

## 6. 对 `SLSOrderWindow` 的逐项判定（第三方窗口）

| 问题 | 判定 | 依据 | 强度 |
| --- | --- | --- | --- |
| (a) 是否改变堆叠/遮挡？ | **否**（本机）；因为调用先被拒绝，根本未生效 | 本机实测：真实变化请求返回 `1000`，`OnScreenBelowWindow` 关系不变 | 已证实（仅 macOS 26.6.2，普通非注入进程） |
| (b) 是否改变 key window？ | **否**（未发生任何变化）；无法判断“若被接受会怎样” | 本机实测：目标应用 `AXFocusedWindow` 与 `AXMainWindow` 全程 5465→5465 | 已证实为“当前路径下不改变” |
| (c) 是否激活/前置宿主应用？ | **否** | 本机实测：前台应用全程为 Chrome，目标为 Finder 时不变 | 已证实 |
| (d) 跨进程能否工作？ | **不能**（普通进程、SIP 开启）；Dock 注入路线（yabai SA）在文档意义上“可用”，但本机未复测 | `1000` 拒绝 + yabai 完全绕道 SA + wiki 的 SIP 要求 | 拒绝为已证实；SA 路线的成功为第三方实现证据，非本机实测 |
| 自有窗口是否可用来做无焦点排序？ | **不可观测**：返回 `0` 但相对排序无变化（out/in 正常） | 本机实测 + AppKit 对照 + `SLSGetWindowOwner` 确认 cid 正确 | 已证实（仅本机版本/进程类型） |
| 事务/组变体是否可行？ | **未知**：`SLSTransactionOrderWindowGroup` 返回垃圾值且无观测变化 | 本机实测（探针未正确初始化返回值） | 未验证，不得作为结论 |

## 7. 与现有能力矩阵的对账

- 矩阵第 6 节“相对前后排序”行当前写：`AX raise；SA SLSOrderWindow / group ordering；OWN AppKit order 方法`，证据 `[A2][A4][Y3]`。**本轮实测要求改写为**：`SLSOrderWindow` 只应标注为 **SA（Dock 注入）依赖**；普通进程对第三方窗口被 WindowServer 拒绝，对自有窗口则是静默 no-op（本机 macOS 26.6.2）。矩阵“不预先假设私有 API 优先”的立场得到支持，但该行原先没有区分 SA 与普通连接，容易被读成普通进程可用。
- 矩阵第 4 节“置前某窗口 / AX `AXRaise`”行：本轮实测确认 `AXRaise` 在**不改变前台应用**的情况下改变了第三方窗口的 z-order（rank 4→3），这与 [A2]（`AXRaise` 与 key focus/应用激活不是同义词）一致；但 Spool 自己的 `docs/research/native-tab-platform-observation-2026-09-11.md` 已记录 `raise_without_focus`（底层即 `AXRaise`）仍可造成 Finder 激活态闪动，因此“不改变前台应用”不等于“应用级零副作用”。
- 矩阵第 6 节“第三方窗口层级”行（SA `SLSSetWindowSubLevel(..., CGWindowLevelForKey(...))`）与本轮 `SLSSetWindowLevel` 反汇编结论一致：**数值 level 的返回值不可信**，且这些层级写入在 yabai 中同样位于 Dock 上下文。[M1][Y4]
- 矩阵第 12 节的 [A2][A4][Y3] 引用清单无需删除，但第 118 行需要补上本文 [M1]/[M2] 级别的证据，否则读者无法区分“符号存在”“SA 可用”“普通进程可用”三件事。

## 8. Spool 适用性

Spool 现状（未修改任何源码）：

- `src/manager/skylight.rs` 链接 `SkyLight.framework`，已绑定 `SLSMainConnectionID`、`SLSMoveWindowsToManagedSpace`、`SLSRequestNotificationsForWindows`、`SLSGetWindowBounds`（已注释）、`_SLPSSetFrontProcessWithOptions`、`SLPSPostEventRecordTo` 等；**没有**绑定 `SLSOrderWindow`。
- `src/manager.rs:70-81` 注释已正确声明 `SLSMoveWindowsToManagedSpace` 只用于自有窗口、无需 Dock 注入/关闭 SIP——本轮实测（owner cid 匹配）与该注释一致。
- 现有一致的前后排序路径是 `src/ecs/focus/stacking.rs` → `WindowApi::raise_without_focus()`（`src/manager/windows.rs:1040`）→ `AXUIElementPerformAction(kAXRaiseAction)`。它走的是公开 AX，而不是私有 CGS 排序。
- `TiledStackingState.last_requested` 的注释（“This records requests, not proof that WindowServer accepted the order.”）与本轮实测高度相关：`AXRaise` 至少能被后续几何观测校验，而 `SLSOrderWindow` 的 `0` 返回值在本机是**不可作为成功证据**的。

结论：**不建议**把 `SLSOrderWindow` 作为 Spool 的排序后端。理由：(1) 第三方窗口在普通进程被 `1000` 拒绝；(2) 自有窗口上它在本机是 no-op，而 Spool 对自有窗口用公共 AppKit 更可靠；(3) 唯一已知可行的第三方路线（Dock 注入）与 Spool 的“不要求关闭 SIP/不注入”的架构边界冲突。

`examples/native_window_probe/order_focus.m` 是正确的下一步探针，但它已经把本机上的问题回答到这个程度；要把它变成结论，只缺跨版本/跨进程形态的复测。

### 8.1 后续实验应测什么（把未知变成事实）

1. **版本矩阵**：在 macOS 14 / 15 上运行 `order_focus.m` 的同一组调用，记录第三方窗口的 `CGError` 是否仍为 `1000`。这是判断“这是 26 的新限制还是长期行为”的唯一方式。
2. **进程形态**：以带 bundle 的普通前台 App（而非 accessory 探针）重复自有窗口 `above/below` 用例，确认 no-op 是否与激活策略/AppKit 生命周期有关；同时用 `NSWindow` 的公共排序做对照。
3. **测量保真**：所有“排序是否生效”的判定改用 `kCGWindowListOptionOnScreenBelowWindow` 或屏幕像素/截图对照，不要用 `CGWindowListCopyWindowInfo` 裸列表顺序（本机已观察到其不可靠）。
4. **SA 对照（可选，需另一台机器）**：在已注入 Dock 的 yabai 环境里对同一对第三方窗口复测 `1000`/成功，以确认“Dock 连接”确实是能力差异的来源，而不是版本差异。
5. **焦点副作用**：若未来发现任何可用的排序路径，必须同时记录前台应用、目标应用 `AXFocusedWindow`/`AXMainWindow`、以及 key window 的外观（红绿灯）三类观测，避免重演 `raise_without_focus` 的“方法名与副作用不符”。
6. **不要**用 `order=0` 对第三方窗口做实验：其语义是 `out`（隐藏）。

## 9. 来源

### Mach-O / 反汇编证据（本机）

- **[M1] `SLSOrderWindow` 及其变体反汇编**：`/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight`，`dyld_info -disassemble -arch arm64e`。**导出符号的文件偏移**（2026-09-11 用 `dyld_info -exports` 复核）：`_SLSOrderWindow 0x329D78`、`_SLSOrderWindowList 0x329A8C`、`_SLSOrderWindowListWithGroups 0x46A980`、`_SLSOrderWindowListWithOperation 0x329DDC`、`_SLSOrderWindowWithGroup 0x46AA84`、`_SLSSetWindowSubLevel 0x32A488`、`_SLSSetWindowLevel 0x32A274`、`_SLSTransactionOrderWindow 0xD9650`、`_SLSTransactionOrderWindowGroup 0xD98AC`、`_SLSGetWindowOwner 0x32BBC8`。`__CGSOrderWindow` **不在导出表**中，只能由反汇编到达；本文对它的描述来自 `SLSOrderWindowList` 的目标地址，未经独立验证，标为**未验证**。
- **[M2] 导出符号表**：同文件 `dyld_info -exports -arch arm64e`；x86_64 slice 在本机不存在（`dyld_info` 报 `does not contain specified arch(s)`）。符号数量与清单见第 2.3 节。
- **[M3] `CGError` 枚举**：本机 SDK `System/Library/Frameworks/CoreGraphics.framework/Headers/CGError.h:17-27`（`kCGErrorSuccess=0`、`kCGErrorFailure=1000`、`kCGErrorIllegalArgument=1001`）。

### 第三方头文件（非 Apple）

- **[H1] `CGSWindowOrderingMode` 与 `CGSOrderWindow`**：[NUIKit/CGSInternal `CGSWindow.h`](https://github.com/NUIKit/CGSInternal/blob/master/CGSWindow.h)（Alacatia Labs 原始整理，Robert Widmann 2015-2016 更新；亦见 iterm2 内嵌副本 [GitLab `gnachman/iterm2`](https://gitlab.com/gnachman/iterm2/-/blob/b79f3fb2653eb0a038bf1db4f7552fcd121d0056/CGSInternal/CGSWindow.h)）。**不是 Apple 头文件**，仅作枚举来源与交叉印证；与 [M1] 反汇编和本机实测一致。

### Apple 文档（公开契约）

- **[A4] 公共 AppKit 排序语义**：[`-[NSWindow orderWindow:relativeTo:]`](https://developer.apple.com/documentation/appkit/nswindow/order(_:relativeto:))——“Repositions the window's window device in the window server's screen list”；`above` 紧随 `otherWin` 之前、`below` 紧随其后、`out` 移出屏幕列表；`otherWin=0` 表示在该 level 内相对全体窗口置前/置后。同页引用的 [`orderFront`](https://developer.apple.com/documentation/appkit/nswindow/orderfront(_:)) / [`orderFrontRegardless`](https://developer.apple.com/documentation/appkit/nswindow/orderfrontregardless()) 明确“不改变 key window 或 main window”，这是公共 API 里“只排序不动焦点”的正式表述。
- **[A11] `CGWindowLevelKey` / `CGWindowLevelForKey`**：本机 SDK `CoreGraphics.framework/Headers/CGWindowLevel.h:23-52`（`kCGBaseWindowLevelKey=0`、`kCGNormalWindowLevelKey`、`kCGFloatingWindowLevelKey` …；`CGWindowLevelForKey` 为公开函数）。
- **[A12] z-order 列表语义**：`CGWindowListCopyWindowInfo` 的 `kCGWindowListOptionOnScreenBelowWindow` 选项（[CGWindow.h](https://developer.apple.com/documentation/coregraphics/cgwindowlistoption)）；本轮以 `OnScreenBelowWindow` 作为层级关系测量依据。

### yabai 固定提交 `dd845723416f5fe92af49fad5ebab00369e07edd`

- **[Y1] 声明与自有窗口调用**：[`src/misc/extern.h:33`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/misc/extern.h#L33)、[`src/view.c:42`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/view.c#L42)、[`src/view.c:114`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/view.c#L114)、[`src/event_loop.c:949`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/event_loop.c#L949)。
- **[Y2] SA 数据传输与 opcode**：[`src/sa.m:422`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/sa.m#L422)（返回值仅代表 socket 往返）、[`src/sa.m:576`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/sa.m#L576)、[`src/osax/common.h:42`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/common.h#L42)、[`src/message.c:2379/2394`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/message.c#L2379)（`window --raise/--lower`）。
- **[Y3] Dock 注入**：[`src/osax/loader.m:13`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/loader.m#L13)（`payload_path` 指向 `/Library/ScriptingAdditions/yabai.osax/...`）、[`loader.m:133`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/loader.m#L133)（定位 Dock.app pid）。
- **[Y4] SA payload 中的排序与层级**：[`src/osax/payload.m:56`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/payload.m#L56)、[`payload.m:67`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/payload.m#L67)、[`payload.m:758`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/payload.m#L758)、[`payload.m:827`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/payload.m#L827)、[`payload.m:856-868`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/payload.m#L856)（`do_window_order`，返回值被丢弃）。
- **[Y5] SIP 要求（官方 wiki）**：[Disabling System Integrity Protection](https://github.com/asmvik/yabai/wiki/Disabling-System-Integrity-Protection)：“…inject a scripting addition into Dock.app, which owns the sole connection to the macOS window server… control window layers…”。Wiki 未固定 revision，核查日读取。
- **[Y6] 版本判定辅助**：[`src/workspace.m:17`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/workspace.m#L17)（`workspace_is_macos_sequoia` / `tahoe`），仅用于通知与兼容分支，不证明排序能力。

### Rift 固定提交 `beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1`

- **[R1] 绑定与自有窗口封装**：[`src/sys/skylight.rs:528`](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/skylight.rs#L528)、[`src/sys/cgs_window.rs:242`](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/cgs_window.rs#L242)（`order_above`/`order_below`/`order_out`，注释给出 `1/-1/0`）。
- **[R7] 列表顺序不可靠的同源警告**：[`src/sys/window_server.rs:486`](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/window_server.rs#L486)（“cgwindowlistcopywindowinfo does not appear to order windows properly”）。这与本机观测一致，是本文改用 `OnScreenBelowWindow` 的原因。

### 其他项目（分支快照，2026-09-11 抓取）

- **[O1] alt-tab-macos**：[`AXUIElement.swift:321`](https://github.com/lwouis/alt-tab-macos/blob/master/src/macos/api-wrappers/AXUIElement.swift#L321)（`kAXRaiseAction`）；全树无排序私有符号。
- **[O2] WindowKit**：[`CapturedWindow.swift:161`](https://github.com/ejbills/WindowKit/blob/main/Sources/WindowKit/Models/CapturedWindow.swift#L161)、[`SkyOverlay.swift:139`](https://github.com/ejbills/WindowKit/blob/main/Sources/WindowKit/Overlay/SkyOverlay.swift#L139)。
- **[O3] Hammerspoon / Rectangle / phoenix / AeroSpace**：全树无 `SLSOrderWindow`、`CGSOrderWindow`、`kCGSOrder*`；AeroSpace 私有接口集中在 `Sources/PrivateApi/include/private.h`，均为读取类。

### 未验证

- **[U1] “AppKit 进程直接调 CGPrivate 排序函数即可控制其他应用窗口层级/焦点”**：见于 Stack Overflow 讨论 [Controlling level and focus of windows other apps with CGPrivate functions](https://stackoverflow.com/questions/34738375/controlling-level-and-focus-of-windows-other-apps-with-cgprivate-functions)，本次抓取被反爬拦截（403），未能核对代码、返回值与前提。**不作为证据**。
