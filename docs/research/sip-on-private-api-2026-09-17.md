# SIP-on 下的私有 SkyLight / CGS / SLS / AX 接口：能替代多少 AX

核查日期：2026-09-17。用途：回答“在 SIP 保持开启、不注入 Dock、不新增权限的前提下，积极使用私有 SkyLight / CGS / SLS / AX 私有接口，能让 Spool 哪些目前依赖 AX 的能力变好，哪些明确拿不到”——也就是把 AX 从“唯一的观察源兼写通道”降级为“只是写通道”的可行范围。

证据分级沿用 [sls-order-window-research.md](sls-order-window-research.md) 与 [macos-window-capability-matrix.md](macos-window-capability-matrix.md)：**公开契约**（Apple 文档 / SDK 头文件 / SDK 内的私有框架 `.tbd` 桩）、**Mach-O 证据**（本机 dyld shared cache 中的导出符号与反汇编）、**实现证据**（固定提交的第三方源码）、**本机实测**（本文探针在本核查机上实际运行得到的返回值与观测量）、**未验证**。所有“本机实测”只对应 macOS 26.6.2 (25G83) / arm64，不外推到其他版本。

上一轮任务的假设是“本机 SkyLight 只在 dyld shared cache 里、没有可解析的二进制，因此很可能取不到 Mach-O 证据”。**该假设不成立**：本机 `/usr/bin/dyld_info` 可以直接读取 shared cache 中的镜像，`-exports` 与 `-disassemble` 都成功（见 §3）。因此本文的 ABI 结论有 Mach-O 证据支撑，而不是只靠第三方头声明。仍然**没有** `dyld_shared_cache_util` / `ipsw` / `jtool2` / `radare2`；`nm` 也仍然读不了 shared cache（这些不影响 dyld_info 的可用性）。

## 1. 结论摘要

一句话：**读侧可以基本摆脱 AX——窗口几何、level、alpha、Space membership、显示器归属、尺寸约束包都能用 id 寻址的私有调用跨进程取得，而且比 CG 列表更完整；写侧一条都没拿到——层级、移动、alpha/阴影/sticky、真实 resize、原生 Space 拓扑在 SIP-on 非注入进程里全部不可写。**

三档计数：**A（真的能减少 AX 依赖）8 条，B（只减少 AX 读 / 只提高可信度）4 条，C（完全拿不到）5 条。**

| 档 | 能力 | 一句结论 | 证据 |
| --- | --- | --- | --- |
| **A1** | 窗口几何读 | `SLSWindowIteratorGetBounds` / `SLSGetWindowBounds` 对第三方窗口按 ID 可读，值与 CG `kCGWindowBounds` 逐点一致；最小化、`orderOut`、非当前 Space 的窗口仍然返回有效几何 | [M1][M2][L1][L3][L4][L5][L9] |
| **A2** | 尺寸约束读 | iterator 的约束包给出**应用声明的 min/max**（外框坐标 = AppKit content min/max + 标题栏/chrome），跨进程可读，正是 `layout_intent.rs` 缺的数据源 | [M2][R1][L5][L7] |
| **A3** | level / alpha 读 | `SLSGetWindowLevel` / `SLSWindowIteratorGetLevel`/`GetAlpha` 对第三方窗口可读，与 CG `kCGWindowLayer` / `kCGWindowAlpha` 一致 | [M1][M2][R2][L1][L4] |
| **A4** | Space membership 读 | `SLSCopySpacesForWindows(cid, 0x7, …)` 普通进程可用；一个窗口可以同时属于 3 个 Space（实测 Spool bar），空数组表示“无成员”而不是失败 | [M1][R1][Y1][L4] |
| **A5** | 显示器归属读 | `SLSCopyManagedDisplayForWindow` 返回显示 UUID，`SLSCopyBestManagedDisplayForRect` 是 fallback；两者都不经过 AX | [M1][R1][Y1][L1][L11] |
| **A6** | 存活/销毁确认 | 已销毁的窗口 ID 读回来是 `1000` + 哨兵矩形 `(inf,inf 0x0)`，`SLSWindowIteratorGetAlpha`=0；这是一条**可证伪**的“窗口真的没了”，比 CG 列表快照强 | [L9][M1] |
| **A7** | 视觉元数据读 | `SLSWindowIteratorGetCornerRadii` 在本机存在（macOS 26.x），返回 4 个数值；普通窗口 16，瞬态表面 0 | [M1][M3][L10] |
| **A8** | 窗口枚举 / 身份 | 已有的 `SLSWindowQueryWindows` + iterator + `_AXUIElementGetWindow` 路径本轮只需加读点，不加权限 | [M1][M3][R1][Y1] |
| **B1** | 写后校验改用私有读 | 真实写入仍必须走 AX，但**校验**可以不再问目标应用要 `AXPosition`/`AXSize`；直接命中 `src/manager/windows.rs:749` 的 AX 读回 | [L5][S7] |
| **B2** | 写入前约束预判 | 有了 A2 就能在写之前知道目标宽度是否落在应用声明区间内，`src/manager/windows.rs:127` 的“写 → 读回 → 发现只长了一部分 → 移到屏外重试”整条路径可被前置判断取代（而不是被删除，见 §6） | [L7][S7] |
| **B3** | 生命周期 / 几何事件 | WindowServer 通知可以替代一部分 AX observer 订阅，但**投递集合本机未验证**；`SLSRegisterConnectionNotifyProc` 对全部 44 个 ID 都返回 0，注册成功不是能力证据 | [L13][R3][S4] |
| **B4** | AX 元素寿命 | remote token 懒获取（而不是长期持有 `AXUIElementRef`）是减少 `kAXErrorInvalidUIElement` 的方向，但**本机无 AX 权限，未验证** | [L14][S1] |
| **C1** | 第三方窗口相对层级写 | `SLSOrderWindow` 对第三方窗口返回 `1000`，层级不变——与既有文档一致（本轮独立复核） | [L2][M1] |
| **C2** | 第三方窗口移动写 | `SLSMoveWindow` 对第三方窗口返回 `1000`；对**自有**窗口返回 0 且真的移动（AppKit 侧也看到新 frame） | [L2][M1][Y1] |
| **C3** | alpha / 阴影 / sticky / sub-level 写 | 只有 Dock scripting addition 路线；yabai 的 payload 里分别走 `SLSSetWindowAlpha`、tag bit 3、tag bit 11、`SLSSetWindowSubLevel` | [Y3][Y4] |
| **C4** | 真实 resize（第三方） | 没有任何 SIP-on 私有替代；yabai 固定提交里 `window_manager_resize_window` / `window_manager_set_window_frame` 仍然写 `kAXSize` / `kAXPosition` | [Y2][R4] |
| **C5** | 原生 Space 拓扑写 | 创建 / 删除 / 移动 / 按 ID 切换全部在 Dock payload 里（`add_space` / `remove_space` 靠特征扫描定位），普通进程拿不到 | [Y3][Y5][Y6][M1] |

最意外的一条是 **A6 + A1 的组合**：私有读不但覆盖 CG 列表覆盖不到的窗口（`orderOut` 的、非当前 Space 的），而且**对已销毁窗口给出明确的否定回答**，而 CG 那条路只能靠“这次快照里没有它”推断。也就是说，把观察源换成私有读之后，“窗口还在吗”从推断变成了可读事实。

第二意外的是 **`SLSPackagesGetWindowConstraints` 的语义与 Rift 的用法相反**：Rift 把它当作 iterator 全零时的回退（`window_server.rs:208-228`），但本机实测它对**第三方**窗口返回 `CGError 0` 且写出全零，把 `cid` 换成窗口真正的 owner connection 则返回 `268435459 (0x10000003)` 和 NaN——它只对调用者自己的连接有值（§4.4）。

## 2. 本机快照与实测纪律

| 项 | 值 |
| --- | --- |
| OS | macOS 26.6.2 (25G83)，`sw_vers` 复核 |
| 架构 | `uname -m` = `arm64`；SkyLight 的镜像是 **arm64e** slice（`dyld_info` 报告，x86_64 slice 不存在） |
| Spool 守护进程 | 核查开始时 `pgrep -fl spool` → `3695 ./target/debug/spool service run`；核查过程中该进程重启过，结束时为 `11775 ./target/debug/spool service run`。**所有实测都由独立探针进程完成，不经过 Spool**；Spool 只是同一会话里另一个窗口拥有者，用于交叉确认“第三方窗口”这一取证视角 |
| 仓库状态 | 核查期间 HEAD 从 `3482f7d` 前进到 `f5a8344`，且 `src/manager/app.rs` / `ax_census.rs` / `windows.rs` 有**另一条工作流**的未提交改动。本文没有修改 `src/` 下任何文件，也没有改 `Cargo` 依赖；引用的 `file:line` 对应本次阅读时的文件状态 |
| 探针权限 | `AXIsProcessTrusted()` = **0**（未新增、未申请任何权限）。因此凡是“创建 AX 元素后读属性”的用例都只能观察到 `-25211 kAXErrorAPIDisabled`，`_AXUIElementCreateWithRemoteToken` 本身的返回不受影响 |
| 未做 | 没有关闭 SIP、没有注入、没有请求新权限、没有改系统设置；没有对任何真实用户窗口做写操作 |

写探测只作用于探针自己创建的窗口：一个进程内的自有 `NSWindow`（`miniaturize` / `orderOut` / `setFrame`），以及第二个探针进程（`host`）创建的窗口——从调用方进程看它是**第三方窗口**（owner connection 不同），但它属于本次核查，不是用户窗口。`SLSOrderWindow` 只用 `order=1`（above），从不用 `order=0`（其语义是隐藏）。

探针源码：`examples/native_window_probe/sip_private_probe.m`（子命令 `symbols` / `read` / `own` / `width` / `spaces` / `sentinels` / `notify` / `host` / `foreign`）。构建：

```sh
clang -fobjc-arc -framework Cocoa -framework CoreGraphics \
  -o /tmp/sip_private_probe examples/native_window_probe/sip_private_probe.m
```

## 3. 证据来源现状：Mach-O 证据**可以**取得

| 工具 | 状态 | 说明 |
| --- | --- | --- |
| `/usr/bin/dyld_info` | **可用** | 对 `/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight` 直接 `-exports` 与 `-disassemble` 成功；该路径在磁盘上是指向 `Versions/Current/SkyLight` 的**断链**，dyld_info 会解析到 shared cache 中的镜像 |
| `nm` / `otool` | 不可用于 shared cache 镜像 | 与既有认知一致，未被使用 |
| `dyld_shared_cache_util` / `ipsw` / `jtool2` / `radare2` | **不存在** | 未能、也不需要用来抽取 |

- **[M1] 导出表**：`dyld_info -exports -arch arm64e` 共 2582 行（约 2580 个导出），含本文关心的全部符号（见 §4 各卡片的 offset）。
- **[M2] 反汇编**：`dyld_info -disassemble -arch arm64e` 输出 59 MB / 1.5 s，含 `_SLSWindowIteratorGetBounds` 等函数的完整反汇编；本文用它确定返回结构与参数形状。
- **[A1] SDK 桩**：`…/MacOSX.sdk/System/Library/PrivateFrameworks/SkyLight.framework/Versions/A/SkyLight.tbd`（1157 行，192 个唯一符号，`targets: [x86_64-macos, x86_64-maccatalyst, arm64e-macos, arm64e-maccatalyst]`）。它是 Apple 自己发布的私有框架桩，**不是**文档化 API，但比第三方头文件整理更接近一手契约；本文把“符号确实存在于 Apple 交付的 SDK 桩里”作为存在性的第一等证据。

两者能交叉验证：本文所有“存在”的符号，在 `.tbd` 与 `dyld_info -exports` 里同时出现；`_CGSGetWindowBounds` 是唯一例外（§4.5）。

## 4. 逐符号事实卡

卡片里的“跨进程”一律指：**普通、SIP-on、未注入、非窗口 owner 的进程**，对另一个进程的窗口调用。

### 4.1 `SLSWindowIteratorGetBounds`（A1 的核心）

```c
CGRect SLSWindowIteratorGetBounds(CFTypeRef iterator);
```

- **存在性**：Apple SDK 桩 [A1] + 导出表 offset `0x0047C23C` [M1] + 反汇编 [M2]。证据等级：公开契约 + Mach-O。
- **ABI（反汇编确认）**：函数先 `_iterator_seek`，再从迭代器物化记录里 `ldp d0,d1,[x19,#0x60]` / `ldp d2,d3,[x19,#0x70]` 返回——即 32 字节 `CGRect` 按 ARM64 HFA 规则放在 `d0..d3`，**不是** sret 指针。同一个记录里相邻字段分别是 `frameBounds`（`+0x80`/`+0x90`）、`lastNonEmptyFrameBounds`（`+0xa0`/`+0xb0`）、`screenRect`（`+0xc0`/`+0xd0`）、约束三元组（`+0xe0`/`+0xf0`/`+0x100`），`level`=`+0x58`、`alpha`=`+0x5c`。
- **跨进程**：普通进程对第三方窗口有效。实测 25 个窗口的 iterator `bounds` 与 `CGWindowListCopyWindowInfo` 的 `kCGWindowBounds` 逐点一致（含负坐标、含 1470 宽的全屏窗口）。
- **状态覆盖（本机实测）**：
  - 自有窗口 `orderOut` 之后：CG 的 `IncludingWindow` 查询已经查不到它（`pid=-1`、bounds 为 `(inf,inf 0x0)`），iterator 仍然 `present=1` 并返回 `(200.0,156.0 600.0x400.0)` [L3]。
  - 自有窗口 `miniaturized=1`：iterator 与 `SLSGetWindowBounds` 都返回最小化前的 frame `(200.0,156.0 600.0x400.0)`，`level=0`、`alpha=1.0` [L3]。
  - **非当前 Space 的窗口**：Space 570 与 576（都是 `type=4` 全屏 Space）不是当前 Space（当前是 Space 1），它们列出的 Chrome `wid=46791` / `wid=51710` / `wid=46794`、ChatGPT `wid=51304` 都不在 on-screen 列表里，iterator 仍返回有效几何（例：`46791 → (0.0,80.0 1470.0x876.0)`，`51304 → (0.0,33.0 1470.0x923.0)`）[L4]。
- **已知歧义**：
  - `frameBounds` ≠ `bounds`。普通窗口 `frameBounds` = `bounds` 四周各加阴影（实测 +68/+68、+46/+46）；但横跨全屏的窗口出现 `frameBounds` 比 `bounds` **小**、甚至 `0x0`（Chrome `wid=46794`：bounds `1470x47`，frameBounds `0x0`）。**它的确切定义未验证**，不要把它当作“含阴影的完整 frame”。
  - `lastNonEmptyFrameBounds` 在窗口从未非空过时是 `(inf,inf 0.0x0.0)`。
  - 迭代器本身不区分“窗口不存在”和“窗口存在但无数据”——两者都表现为 `SLSWindowIteratorAdvance` 返回 false（`present=0`）。

### 4.2 `SLSWindowIteratorGetConstraints`（A2 的核心）

```c
/* 返回类型不是 CGError —— 见下 */
void SLSWindowIteratorGetConstraints(CFTypeRef iterator, CGSize *min, CGSize *max, CGSize *cur);
```

- **存在性**：Apple SDK 桩 [A1] + offset `0x0047C75C` [M1] + 反汇编 [M2]。
- **ABI（反汇编确认，且推翻了一处常见假设）**：函数把三个输出指针经 `csel` 三元组选择后（`csel x20,x1,x2` / `csel x21,x2,x3` / `csel x22,x3,x4`，判据是迭代器 `+0x30` 的字段），调用 `_iterator_seek`，然后把记录里的三个 `CGSize`（`+0xe0`/`+0xf0`/`+0x100`）写到那三个指针；**它自身不设置返回值**——`x0` 是 `_iterator_seek` 残留的指针低 32 位。本机实测到的“返回值”是 `49621984`、`-2091150848`、`81069456` 这类截断指针值 [L1][L5]。
  → Rift 的 `src/sys/skylight.rs` 把它声明为返回 `CGError` 并丢弃 [R1]；任何检查该返回值的绑定都会拿到垃圾。**不要检查它**。
- **语义（本机实测，自有窗口 + 可控 AppKit 声明）**：

  | AppKit 声明（content 坐标） | 当前外框 | 约束包 min | 约束包 max | 约束包 cur |
  | --- | --- | --- | --- | --- |
  | 无（初始） | 600x400 | `0x0` | `0x0` | `0x0` |
  | `contentMinSize=400x300` | 600x400 | `400x332` | `100000x100000` | `600x400` |
  | `contentMinSize=700x300` | 600x400 | **`600x332`** | `100000x100000` | `600x400` |
  | `contentMinSize=400x300` | 800x400 | `400x332` | `100000x100000` | `800x400` |
  | `contentMaxSize=500x300` | 800x400 | `400x332` | `500x332` | `800x400` |
  | `contentMaxSize=1200x900` | 800x400 | `400x332` | `1200x932` | `800x400` |
  | `minSize=800x400`（外框坐标属性） | 800x400 | `800x400` | `1200x932` | `800x400` |

  三条结论：
  1. min/max 是**外框坐标**：content min/max 加 chrome（本机带标题栏窗口是 +32 高度），与 `native_width_constraint(min, max, padding)` 期望的输入口径一致 [S2]。
  2. `max = 100000x100000` 表示“应用没有声明上限”，是**哨兵而不是真实区间**——必须特殊处理，否则会把 `100000` 当成合法列宽上限参与投影。
  3. `min` 会被**夹到当前尺寸**：应用声明 content min 宽 700、当前外框只有 600 时，约束包报 `600x332`（少报 100）。喂进 `WidthConstraint` 之前必须知道这一点：约束包是“应用声明的下界”与“当前尺寸”的较小值，窗口已经小于声明下界时它会低报。

- **`cur` 不是实时几何**：它是应用**最后一次发布约束包时**的外框尺寸。本机实测：`setFrame` 到 900x700、100x100 之后 `cur` 仍停在 `600x732`；只有再次设置 min/max 触发重新发布时才跳到 `800x400` [L5]。跨进程读第三方数据也能看到同样的陈旧性（ChatGPT `wid=51304` 的 `cur=836x906`，而其 bounds 是 `1470x923`）[L4]。**不要用 `cur` 当几何源。**
- **跨进程**：有效。第三方窗口（Emacs、AutoFill、ChatGPT、Chrome）都能读到非零约束 [L1][L4]；对**已知声明**的第三方窗口（`host` 进程：`contentMinSize=420x310`、`contentMaxSize=880x690`）读回 `min=420x342`、`max=880x722`，与声明严格对应 [L7]。
- **已知歧义**：全零 = “没有约束包”（例如瞬态 `CursorUIViewService` 表面、`0x0` 尺寸的辅助窗口），**不是**“读失败”；函数本身不给返回值，所以“全零 vs 读不到”在 API 层无法区分，只能靠“窗口是否属于某类”的经验判断。`min=max=cur`（如 Control Center `42x33`、固定尺寸窗口 `514x520`）表示不可缩放。

### 4.3 `SLSPackagesGetWindowConstraints`

```c
CGError SLSPackagesGetWindowConstraints(int cid, uint32_t wid, CGSize *min, CGSize *max, CGSize *cur);
```

- **存在性**：Apple SDK 桩 [A1] + offset `0x003A0238` [M1]。签名取自 Rift [R1]。
- **跨进程（本机实测，与 Rift 的用法相反）**：

  | 调用方式 | 结果 |
  | --- | --- |
  | `cid = SLSMainConnectionID()`，**自有**窗口 | `err=0`，写出与 iterator 完全相同的 `min/max/cur` |
  | `cid = SLSMainConnectionID()`，**第三方**窗口 | `err=0`，写出**全零**（探针用 `-1x-1` 预填确认是“写了零”而不是“没写”） |
  | `cid =` 窗口真正的 owner connection，第三方窗口 | `err=268435459 (0x10000003)`，输出 NaN 垃圾 |

- **结论**：它只对**调用者自己连接**里的窗口有内容。Rift 把它作为 iterator 全零时的回退 [R1] 对第三方窗口**不产生数据**；Spool 若照抄该回退，会得到“成功但全零”的假值。证据等级：本机实测（单一 macOS 版本，未做跨版本）。
- **已知歧义**：`err=0` + 全零 与 `err=0` + 有值 都是“成功”；因此不能用它的返回码区分“未知”和“无约束”。

### 4.4 `SLSGetWindowBounds` / `CGSGetWindowBounds`

```c
CGError SLSGetWindowBounds(int cid, uint32_t wid, CGRect *frame);   /* 内部：_GetWindowBounds(cid, wid, frame, 0) */
```

- **存在性**：`_SLSGetWindowBounds` offset `0x0032F040` [M1]；`_CGSGetWindowBounds` **不在** `.tbd` [A1]，也**不在** `dyld_info -exports` 的导出表里 [M1]。
- **但运行时两个名字解析到同一个地址**：`dlsym(RTLD_DEFAULT, …)` 后 `SLSGetWindowBounds=0x18b5a8040`、`CGSGetWindowBounds=0x18b5a8040`（同一进程同一次运行）[L15]。所以本机存在 `CGSGetWindowBounds`，只是它的别名关系在导出表 dump 里看不见——**“符号不在导出表”与“符号不可用”是两件事**。
- **反汇编（参数形状确认）**：`_SLSGetWindowBounds` 是 `mov w3,#0; b _GetWindowBounds` 的尾调用，`_GetWindowBounds` 把 `x0..x3` 分别存为 cid / wid / frame* / flags，与 3 参数调用一致 [M2]。
- **跨进程**：自有与第三方窗口都返回 `0` 且与 iterator bounds 一致；`host` 窗口（第三方视角）同样 `err=0` [L1][L7][L2]。
- **最小化 / 屏外覆盖**：`miniaturize` 后仍返回最小化前的 frame；`orderOut` 后仍返回最后 frame；`setFrame` 到 `(-3000,-3000)` 时返回被系统夹住的实际位置 `(-560.0,924.0 600.0x400.0)`（AppKit 侧 frame `(-560.0,-368.0 …)`，同一窗口）[L3]。
- **失败语义（本机实测，重要）**：`wid=0` / `1` / `0xFFFFFFFF` / `999999` / 一个已销毁的窗口 ID，全部返回 `1000`（`kCGErrorFailure`）**并把输出矩形写成哨兵 `(inf,inf 0.0x0.0)`** [L9][L11]。也就是说失败时 out-param **会被覆盖**；调用者必须先看返回码，不能先看矩形。
- **已知歧义**：`(inf,inf 0x0)` 不是 `CGRectNull`（`{inf, inf, 0, 0}` 与 `CGRectNull` 的位模式不同），不要用 `CGRectIsNull` 判失败。

### 4.5 `SLSGetWindowLevel` / `SLSWindowIteratorGetLevel` / `SLSWindowIteratorGetAlpha`

```c
CGError SLSGetWindowLevel(int cid, uint32_t wid, int *level);
int     SLSWindowIteratorGetLevel(CFTypeRef iterator);   /* w0 = [it+0x58] */
float   SLSWindowIteratorGetAlpha(CFTypeRef iterator);   /* s0 = [it+0x5c] */
```

- **存在性**：`_SLSGetWindowLevel` offset `0x0032A398` [M1]（`.tbd` 亦列出 [A1]）；iterator 两个 getter 在 [M1][M2] 中确认（反汇编显示就地返回缓存字段，无 `iterator_seek` 之外的副作用）。
- **跨进程**：全部有效。实测 iterator `level` 与 CG `kCGWindowLayer` 一致（Spool bar `25`、ChatGPT 全屏窗口 `3`、Control Center `25`），`SLSGetWindowLevel` 同样返回 `0` + 相同数值；`alpha` 与 CG `kCGWindowAlpha` 一致（`wid=51439 → 0.000`）[L1][L4]。
- **已知歧义**：`SLSGetWindowLevel` 失败时**不覆盖** out-param（预填 `-1` 后仍是 `-1`），与 `SLSGetWindowBounds` 的“失败也写哨兵”相反 [L11]。另外 `SLSSetWindowLevel` 的返回值不可信（只发一条不等回复的 `mach_msg`，见 [sls-order-window-research.md] §2.4）——读函数没有这个问题。
- Spool 现状：`SLSGetWindowLevel` 未绑定（`src/manager/skylight.rs` 没有它），level 目前来自 CG `kCGWindowLayer`。

### 4.6 `SLSWindowIteratorGetCornerRadii`（Spool 已在按名探测）

```c
CFArrayRef SLSWindowIteratorGetCornerRadii(CFTypeRef iterator);   /* 猜测签名，见下 */
```

- **存在性**：Apple SDK 桩 [A1] + offset `0x0047C32C` [M1]；`dlsym` 在本机命中 [L10]。
- **本机实测**：普通带标题栏窗口返回 4 个元素、第一个 `16`；瞬态 `CursorUIViewService` 表面返回 4 个元素、第一个 `0` [L10][L1]。与 Spool `src/manager/windows.rs:1212` 的动态加载 + 取第 0 个的用法一致。
- **已知歧义**：函数的**返回类型与参数个数未做反汇编确认**（只确认了地址与调用可行）；“4 个元素分别对应 4 个角”是推测。`count>0 但值 0` 与 `count=0` 是两种不同情况（Spool 目前把 `is_empty` 当成 `None`）。
- 另有两个同族符号：`SLSWindowIteratorGetResolvedCornerRadii`、`SLSWindowIteratorGetCornerMaskFlags`（均在 [M1]，未实测）。

### 4.7 `SLSCopyManagedDisplayForWindow` / `SLSCopyBestManagedDisplayForRect`

```c
CFStringRef SLSCopyManagedDisplayForWindow(int cid, uint32_t wid);
CFStringRef SLSCopyBestManagedDisplayForRect(int cid, CGRect rect);
```

- **存在性**：`0x002F8804` / `0x002F8480` [M1]，均在 `.tbd` [A1]；yabai 的用法（先按窗口查、NULL 再用 bounds 查 rect）见 [Y1] `src/window.c:29-37`。
- **返回值**：显示标识 UUID 字符串，本机全部为 `37D8832A-2D66-02CA-B9F7-8F30A301B230`（与 `SLSCopyManagedDisplaySpaces` 的 `Display Identifier` 相同）[L4][L11]。
- **NULL 语义（本机实测，反直觉）**：

  | 输入 | `SLSCopyManagedDisplayForWindow` | `SLSCopyBestManagedDisplayForRect` |
  | --- | --- | --- |
  | 正常窗口（自有/第三方） | 非 NULL | 非 NULL |
  | `wid=0` | **NULL** | — |
  | `wid=1` / `0xFFFFFFFF` / `999999`（无名窗口） | **非 NULL**（返回主显示 UUID） | — |
  | 已销毁的真实窗口 ID | **NULL** | — |
  | `rect = (99999,99999,10,10)`（完全屏外） | — | **非 NULL**（被夹到最近显示器） |

  → 这两个函数的 NULL **不能**当“窗口不存在”的判据（`wid=1` 也给值），也不能当“矩形不可达”的判据（屏外矩形也给值）。`SLSCopyBestManagedDisplayForRect` 在本机没有观察到 NULL。
- **跨进程**：对第三方窗口有效 [L1][L4][L5]。

### 4.8 `SLSCopySpacesForWindows`

```c
CFArrayRef SLSCopySpacesForWindows(int cid, int selector, CFArrayRef window_list);
```

- **存在性**：`0x002F388C` [M1] + `.tbd` [A1]；yabai [Y1]、Rift [R1] 均为同一形状（Rift 用 `u32 selector`）。
- **跨进程**：普通进程对第三方窗口有效。yabai 在 daemon（无注入）里对任意窗口用 `0x7` [Y1]；Spool 也已在用 [S3]。
- **selector**：本机只验证了 `0x7`（yabai / Rift / Spool 共用的取值）。`0x0` 与 `0x1` 在所有被测窗口上都返回空数组 [L1][L4]。**`selector` 的完整取值表与语义未验证**；不要因为某取值返回空就推断“不属于任何 Space”。
- **NULL vs 空数组（本机实测）**：**没有观察到 NULL**。未知/已销毁的窗口 ID 返回的是**非 NULL 空数组** [L11][L9]。所以“空数组 = 无成员（合法事实）”“NULL = 调用本身失败”这条区分成立，但 NULL 在实践中极其罕见。
- **多 Space 语义**：返回的是**完整成员列表**，不是一个“主 Space”。实测 Spool bar 窗口 `spaces(0x7) = 3[570,576,1]`，即同时属于当前用户 Space 与两个全屏 Space [L4]；yabai 的 `window_is_sticky` 判据正是 `count > 1` [Y1]。**因此“把第一个元素当窗口的 Space”在跨 Space/全屏场景下会错**（yabai `window_space()` 正是这么做的，`src/window.c:67-87`）。
- **已知歧义**：数组元素的 CFNumber 类型不一（Spool 用 `SInt64Type`，yabai 用 `CFNumberGetType` 自适应）；本机实测 `SInt64Type` 可用 [S3]。

### 4.9 `SLSMoveWindow`

```c
CGError SLSMoveWindow(int cid, uint32_t wid, CGPoint *point);
```

- **存在性**：`0x00329784` [M1] + `.tbd` [A1]；yabai 有声明 [Y1]。
- **跨进程（本机实测）**：对第三方窗口（`host` 进程的窗口）返回 **`1000`**，位置不变（`before == after == (1127.0,46.0 500.0x722.0)`）[L2]。**普通进程不能移动别人的窗口。**
- **自有窗口**：返回 `0` 且**真的移动**——`SLSGetWindowBounds` 变成 `(260.0,660.0 …)`，同时 AppKit 侧 `w.frame` 也变成 `(260.0,-336.0 …)`（CG/AppKit 坐标翻转后一致）[L3]。所以对自有窗口它是可用的 compositor 级移动，但 Spool 已有公共 AppKit 可用，此路无额外收益。

### 4.10 `SLSOrderWindow`（复核既有结论）

- **本机复核结果与 [sls-order-window-research.md] 完全一致**：对第三方窗口 `order=1`（above）、`rel=0` 返回 **`1000`**，on-screen 列表顺序逐项不变（`48118,59724,51322,5066,59532` → 同）；对自有窗口返回 `0` [L2][L3]。
- 本轮把目标从“真实用户窗口（Finder/Chrome）”换成“另一探针进程的窗口”，结论不变，说明这不是某个应用的特殊性。

### 4.11 `SLSRegisterConnectionNotifyProc` / `SLSRemoveConnectionNotifyProc`

```c
CGError SLSRegisterConnectionNotifyProc(int cid, connection_callback *handler, uint32_t event, void *context);
CGError SLSRemoveConnectionNotifyProc(int cid, connection_callback *handler, uint32_t event, void *context);
```

- **存在性**：`0x0034B264` [M1] + `.tbd` [A1]；Spool 绑定见 `src/platform/notify.rs:17-29` [S4]，Rift 见 [R3]。
- **可用 event id 集合（本机实测，但只能证明“注册被接受”）**：对 44 个 ID（102/103/723/724/804/805/806/807/808/811/815/816/1204/1308/1322/1325…1342/1400…1416/1508/1700/`0xFFFFFFFF`）逐个注册，**全部返回 `0`** [L13]。
  → **注册成功不是能力证据**：连 `0xFFFFFFFF` 都“成功”。要判断投递必须真正观察回调（本文未做，属于“真机验收”清单）。
- 事件 ID 的来源是第三方头文件整理（`NUIKit/CGSInternal` 的 `CGSEvent.h`）转写的枚举，Spool 与 Rift 各自抄了一份（`src/platform/notify.rs:280-343` 与 [R3] 的 `KnownCGSEvent` 逐项一致，Spool 少了 `WindowDisplayChanged = 805`）。**这些 ID 的语义仅对少数几个有本机观察证据**（见 §5）。

### 4.12 `SLSRequestNotificationsForWindows`

```c
CGError SLSRequestNotificationsForWindows(int cid, uint32_t *window_list, int window_count);
```

- **存在性**：`0x003365F0` [M1] + `.tbd` [A1]；Spool 绑定 `src/manager/skylight.rs:54-60`（注释称 macOS 15+ 启用 `WindowClosed`），Rift 绑定 [R3] `update_window_notifications`。
- **版本门槛（实现证据，非本机可验）**：Spool 在 `crate::platform::macos_major_version() < 15` 时直接跳过（`src/manager.rs:999-1006`）[S5]；Rift 无门槛直接调用 [R3]。**本机是 macOS 26，无法验证 14/15 的差异**。
- **本机实测**：对**第三方**窗口调用返回 **`0`**（接受）[L2]。接受与否不能说明它实际启用了哪些通知。
- **已知歧义**：`WindowClosed = 804` 的实际投递只有“Spool 在 15+ 订阅它”这一条实现证据（`src/platform/notify.rs:142`、`:230-238`），本文没有为它构造真机验收。**“启用通知集合”目前没有可用的探测手段**。

### 4.13 `_AXUIElementCreateWithRemoteToken` / `_AXUIElementGetWindow`

```c
AXUIElementRef _AXUIElementCreateWithRemoteToken(CFDataRef data);
AXError        _AXUIElementGetWindow(AXUIElementRef ref, uint32_t *wid);
```

- **存在性**：两个都在 HIServices（`…/ApplicationServices.framework/Versions/A/Frameworks/HIServices.framework/HIServices`）导出表：`offset 0x5F5C` / `0x20FA4` [M3]；**不在** SkyLight `.tbd` 里 [A1]。yabai 有声明 [Y1]。
- **token 结构（实现证据，Spool 侧唯一权威）**：`src/manager/discovery.rs:569-588` 构造 20 字节 `CFMutableData`：`[0..4) = pid (native)`、`[4..8) = 0`、`[8..12) = 0x636f636f`（`"coco"`）、`[12..20) = element_id (u64)`。`element_id` 不是窗口 ID——Spool 是在 `0..0x7fff` 上**扫描**元素 ID，再用 `_AXUIElementGetWindow` 把命中的元素映射成窗口 ID（`src/manager/discovery.rs:449-468`、`ELEMENT_LIMIT = 0x7fff`）。
- **本机实测（受权限限制，只能答一半）**：用同一布局构造 token（`pid=host 进程`、`element_id=host 窗口 ID`），`_AXUIElementCreateWithRemoteToken` **返回非 NULL**（创建本身不需要权限），随后的 `_AXUIElementGetWindow` 与 `AXUIElementCopyAttributeValue(AXTitle)` 都返回 **`-25211 = kAXErrorAPIDisabled`**，`wid` 保持 0 [L14][A2]。
  → 因本机探针无 AX 权限（`AXIsProcessTrusted()=0`），**“能否为非活动 Space 的窗口创建可用 element”与“懒获取能否降低 `invalidated`”两条都未验证**，见 §9。
- **与 `invalidated` 的关系（推论，未验证）**：`ax-reliability-2026-09-17.md` 把 `kAXErrorInvalidUIElement (-25202)` 归为“元素寿命问题（27 号票 incarnation 议题）”。remote token 的价值在于**不持有元素**：每次要用时按 `(pid, element_id)` 重建，天然免疫“缓存的 element 已失效”。但这条只是机制上说得通，本轮**既没有观测 `invalidated` 的分布（普查未采集），也没有验证 token 重建的成功率**。

## 5. 事件与订阅：现有覆盖面

Spool 当前注册的事件（`src/platform/notify.rs:134-143`）[S4]：

| 事件 ID | 名称 | 注册条件 | Spool 的用法 |
| --- | --- | --- | --- |
| 1327 | `SpaceCreated` | 总是 | 经 `SLSSpaceGetType()==0` 过滤后发 `Event::SpaceCreated` |
| 1329 | `SpaceCurrentChanged` | 总是 | 只记日志（不发事件） |
| 1328 | `SpaceDestroyed` | 总是 | 发 `Event::SpaceDestroyed` |
| 1326 | `SpaceWindowDestroyed` | 总是 | 载荷 `(u64 space, u32 wid)` → `Event::WindowDestroyed{source: SpaceNotification}` |
| 804 | `WindowClosed` | **macOS ≥ 15** | 载荷 `u32 wid` → `Event::WindowDestroyed{source: WindowServer}` |
| 806/807/808/811/815/816/1333/1336/1341… | `WindowMoved`/`WindowResized`/`WindowReordered`/`WindowLevelChanged`/`WindowUnhidden`/`WindowHidden`/… | **不注册** | 代码里有 match 分支，但只 `debug!` 不产生事件 |

Rift 固定提交 [R3] 的 `src/sys/window_notify.rs` 与 Spool 是同一棵来源树（注释直接指向 yabai 的 `6f9006dd` 与 `NUIKit/CGSInternal`），它注册的是同一批 ID，并且能把 `WindowMoved`/`WindowResized`/`WindowClosed` 等的 `u32 wid` 解析出来（同一 `connection_callback` 分支）。

**结论**：事件侧目前对 AX 的依赖本来就很小（AX observer 用于窗口级通知，WindowServer 通知用于窗口/ Space 生命周期），本轮的**增量**不在“能不能收到”，而在“收到哪些、载荷是什么”。这需要一次真实的投递观测（§9 第 3 条），本文只贡献一条负面事实：**用注册返回值探测事件集合是无效的**（44/44 都返回 0）[L13]。

## 6. 能拿到什么 / 拿不到什么

### 6.1 resize（真实尺寸变更）

| 方向 | 结论 | 证据 |
| --- | --- | --- |
| **读**（当前 frame、最小/最大约束） | **能拿到**，而且不经 AX、不受目标应用响应影响 | A1、A2；[L3][L5][L7] |
| **写**（把第三方窗口 resize 到目标） | **拿不到私有替代**。yabai 固定提交的 `window_manager_resize_window`（`src/window_manager.c:425-433`）写 `kAXSizeAttribute`，`window_manager_set_window_frame`（`:729-761`）按 size → position → size 顺序写 `kAXSize`/`kAXPosition`；唯一“SA 分支”是 `do_window_move`（`SLSMoveWindowWithGroup`）与 `do_window_scale`（视觉 transform），都不是真实 resize | [Y2][R4] |
| **本机复核** | `SLSMoveWindow` 对第三方窗口 `1000`；yabai 的非 AX 移动路线依赖 Dock SA | [L2][Y3] |

对 Spool 的直接含义：`write_ax_frame`（`src/manager/windows.rs:749`）的**写入**部分无法去掉；但同一函数里的**读回**（`update_frame()` → `AXPosition`+`AXSize`，`src/manager/windows.rs:1057-1063`）可以换成私有 bounds 读，从而把“写成功与否”的判定从目标应用的响应里解耦。

### 6.2 层级（z-order）

- **相对排序写**：拿不到（C1）。`SLSOrderWindow` 对第三方 `1000`，对自有窗口是“返回 0 但 `above/below` 静默无效”（既有文档已证）。
- **数值 level 读**：能拿到（A3）。
- **数值 level 写**：拿不到（只有 SA `SLSSetWindowSubLevel`，`payload.m:758`，且 `SLSSetWindowLevel` 返回值本身不可信）[Y3][sls-order-window-research.md]。
- **焦点**：私有的 `_SLPSSetFrontProcessWithOptions` + `SLPSPostEventRecordTo` 路线 Spool 已在用（`src/manager/windows.rs` `make_key_window`），属于**已有的**私有写通道，不是本轮新增能力。

### 6.3 alpha / 阴影 / sticky / sub-level

四条写入**全部**只在 Dock payload 里，普通进程一个都没有：

| 能力 | yabai 实现位置 | 调用 | 本机复核 |
| --- | --- | --- | --- |
| alpha | `payload.m:670`、`:697` | `SLSSetWindowAlpha(SLSMainConnectionID(), wid, alpha)` | 未测第三方写（不做破坏性实验）；符号存在 [M1] |
| 阴影 | `payload.m:804-808` | tag bit 3（`1 << 3`）经 `SLSSetWindowTags`/`SLSClearWindowTags` | 同上 |
| sticky | `payload.m:770-774` | tag bit 11（`1 << 11`）经 `SLSSetWindowTags`/`SLSClearWindowTags` | 同上 |
| sub-level | `payload.m:758` | `SLSSetWindowSubLevel(SLSMainConnectionID(), wid, CGWindowLevelForKey(layer))` | 同上 |

证据：[Y3]（payload）、[Y4]（opcode 表 `SA_OPCODE_WINDOW_OPACITY/LAYER/STICKY/SHADOW`）。**读侧**（alpha / level / tags 位）可以拿到：`SLSWindowIteratorGetAlpha`、`SLSWindowIteratorGetTags`（Spool 已在读 tags，`src/manager.rs:1131`）。

### 6.4 原生 Space 拓扑

| 方向 | 结论 | 证据 |
| --- | --- | --- |
| 读拓扑/当前 Space | 能（已有 `SLSCopyManagedDisplaySpaces` / `SLSManagedDisplayGetCurrentSpace`，Spool 已用） | [S3][R1][Y1] |
| 读窗口 membership（含多 Space） | 能（A4） | [L4][Y1] |
| 读某 Space 的窗口清单 | 能（`SLSCopyWindowsWithOptionsAndTags` + `options=0x7` 含最小化，Spool 已用） | [S3][L4] |
| 创建 / 删除 / 移动 Space、按 ID 切换 | **拿不到**。yabai 的 `space --create/--destroy/--move` 走 `SA_OPCODE_SPACE_CREATE/DESTROY/MOVE`，payload 里的 `do_space_create`/`do_space_destroy`/`do_space_move` 需要 `add_space_fp`/`remove_space_fp` —— 这两个函数指针是用 `hex_find_seq` 在 **Dock.app 二进制里特征扫描**出来的 | [Y3][Y5][Y6] |
| 切换 Space（相对手势） | 不在本文范围；矩阵已记录为 EVENT / 私有手势路线，且不是按 ID 原子切换 | [macos-window-capability-matrix.md] |

## 7. 与 Spool 接缝的对应表

| 接缝 | 现状 | 可接入的私有读 | 改动规模 |
| --- | --- | --- | --- |
| `src/ecs/layout_intent.rs:100-134`（`WidthConstraint` / `native_width_constraint`，注释为 `dead_code` 的 “trusted constraint adapter seam”） | 结构已就位，没有数据源 | **约束包**（A2）。`native_width_constraint(min, max, padding)` 恰好按外框坐标 + padding 换算，与包的 `min/max` 口径一致 | 小：加一个“window id → (min,max)”的适配函数，把哨兵 `100000x100000` 映射为“无上限”（不是 `Interval{max:100000}`），把全零映射为 `Unsupported` |
| `src/manager/windows.rs:749-790` `write_ax_frame` 的 AX 读回 | `update_frame()` 读 `AXPosition`/`AXSize`（`:1057-1063`），用于判断“只长了一部分” | **`SLSGetWindowBounds`**（或 iterator bounds）替代读回（B1） | 中：`WindowApi` 增加一个“私有几何读”，mock 需要同构实现；保留 AX 读作为回退，因为私有读与 AX 读的口径差异（哨兵、最小化时的陈旧 frame）必须先验收 |
| `src/manager/windows.rs:127-141` `resize_staging_origin` + `:749` 的 3 次重试循环 | 靠“写 → 读回 → 发现只长了一部分 → 移到屏外再试”绕过尺寸约束 | **约束包前置判断**（B2）：目标宽度低于声明的 min 时直接判定不可能，不必试探；但**不能整段删除**——实测的夹取来源不止声明约束（见下） | 中：保留循环作为兜底，把“明知不可达”的输入提前拦住 |
| `src/manager.rs:1117-1170` `space_window_list_for_connection` | 已经在 `SLSWindowQueryWindows` + iterator 上迭代，只读 tags/attributes/parent/window id | **加读 bounds/level/alpha/constraints/screenRect** | 小：零新增系统调用、零新增权限——只是同一迭代器上多调几个 getter。成本实质是“每窗口多 5 次本地解码”，没有跨进程往返 |
| `src/manager.rs:999-1013` `request_window_notifications` | 只订阅（15+） | 无常量读替代；事件侧见 §5 | 无 |
| `src/platform/notify.rs:17-29`/`:134-143` 事件订阅 | 5 个 ID | 需要**投递观测**才能决定是否扩到 806/807/815/816/1338/1339/1342 | 小（加 ID）＋一次真机采集（判定价值） |
| `src/manager/windows.rs:243-262` `ax_window_id` / `try_ax_window_id` | 已用 `_AXUIElementGetWindow` | 无变化；A8 只是把它与新的私有读并列成“身份 + 几何”两源 | 无 |
| `src/manager/discovery.rs:457` / `:569-588` remote token | 已是 token 懒获取（`NativeProbe::token` 每次覆写 20 字节缓冲） | B4：把“不长期持有元素”作为规则固化；`invalidated` 的收益需普查数据 | 小（规则/注释级）＋需要普查 |
| `src/manager/windows.rs:1212` `SLSWindowIteratorGetCornerRadii` 动态探测 | 已按名探测、取第一个值 | A7 确认它可用，并区分“4 个元素但值为 0”与“空数组” | 小 |
| `src/manager/skylight.rs:76/89/105/121`（4 条被注释的绑定） | 注释掉了 `SLSGetWindowBounds`、`SLSMoveWindow`、`SLSCopyManagedDisplayForWindow`、`SLSCopyBestManagedDisplayForRect` | 建议**恢复前两条之外的 3 条读**：`SLSGetWindowBounds`（A1/A6）、`SLSCopyManagedDisplayForWindow`、`SLSCopyBestManagedDisplayForRect`（A5）。`SLSMoveWindow` **保持注释**（第三方 `1000`，自有窗口有公共 API） | 小：取消注释 + 修正文档注释（注意 `SLSGetWindowBounds` 失败会写哨兵、`SLSCopyManagedDisplayForWindow` 的 NULL 不代表窗口不存在） |
| `docs/research/ax-reliability-2026-09-17.md` 的三值读模型 | `Known/Absent/Unknown(why)` | 私有读给“边界窗口的 existence”补了第 4 类事实：**明确否定**（`1000` + 哨兵 + iterator `present=0`），比“清单里没有”强 | 小：在 `why` 里加 `NativeWindowAbsent` 之类，并把私有读并入普查漏斗（该文档已列“SkyLight 私有查询”为缺口） |

## 8. 建议实施顺序

1. **读优先（不改行为，先接数据）**
   1. 恢复 `src/manager/skylight.rs` 里 `SLSGetWindowBounds` 绑定，新增迭代器读点（bounds/level/alpha/constraints）到 `window_iterator_for_id` 的消费方；全部读失败保持 `Unknown`，不折叠成 `None`。
   2. 把私有 bounds 读接进 `ax_census`（作为 `source=skylight`），与 AX 读**并行**采集一段时间，对齐“AX frame vs 私有 bounds”的差异分布（最小化/非当前 Space/离屏夹取三类必须能区分）。
2. **能力探测（把假设变成可运行时判定）**
   1. 约束包探测：对每个新跟踪窗口读一次约束包，把 `(min,max)`、是否全零、是否 `100000` 哨兵记入能力快照；TTL 与失效触发沿用普查的结论。
   2. 明确**不做**的探测：不要用 `SLSRegisterConnectionNotifyProc` 的返回值探测事件支持（44/44 返回 0，本轮已证）；不要用 `SLSCopyManagedDisplayForWindow` 的 NULL 判断窗口存活。
3. **接缝落地**
   1. `native_width_constraint` 接数据源（哨兵与全零的映射规则先写测试）。
   2. `resize_staging_origin` 前置判断（只在“目标宽度 < 声明 min”时短路，其余仍走重试）。
   3. `write_ax_frame` 的读回切成私有读 + AX 回退。
4. **普查接入**：把 §7 最后一行列出的私有读并入 `ax-reliability-2026-09-17.md` 的漏斗，采集后再决定三值读需要几个 `why`、能力快照 TTL、以及“写前 settable 探测”是否仍值得。
5. **需要同步更新的既有文档**
   - `docs/research/macos-window-capability-matrix.md`：第 2 节“读取位置和尺寸（批量/事件路径）”与“读取尺寸约束”两行可以升级为“本机实测（macOS 26.6.2）”，并补上“约束包 = 外框坐标 + 哨兵 + min 会被夹到当前尺寸”的限定；第 6 节层级/alpha/阴影/sticky 各行的“SA 依赖”结论**不变**，但应补上本文 [L2] 的复核（`SLSMoveWindow` 第三方 `1000`）。
   - `docs/research/macos-window-control-backends.md`：`SLSMoveWindow` 那一行应写明“普通进程对第三方窗口实测 `1000`”，与既有“符号存在不等于可移动”呼应。
   - `docs/research/ax-reliability-2026-09-17.md`：把“SkyLight 私有查询”从“尚未接入”缺口清单里拆成具体读点，并采用本文的“私有读可给出明确否定”结论。
   - `docs/WINDOW_POLICY.md` / ADR 0006 / 0007：本文不改变它们的决定（写侧一条没拿到 → “desired 状态与 native 效果分离”的前提不变）；需要新增的只是一句“观察源可以不是 AX”，这属于 ADR 0006 的“observed facts”一侧，建议在 0007 的 consequences 里补一条指向本文。

## 9. 未验证项与需要真机验收的清单

**本文明确未取得证据的（不要当成结论）**

1. `_AXUIElementCreateWithRemoteToken` 对**非活动 Space 窗口**是否可创建可用 element：本机探针无 AX 权限（`AXIsProcessTrusted()=0`），后续调用一律 `-25211`。需要一台已授予辅助功能权限的机器，且需要至少两个用户 Space。
2. “懒获取 AX element”能否降低 `kAXErrorInvalidUIElement`：需要 `ax-reliability-2026-09-17.md` 的普查数据（`invalidated` 分布），本轮没有采集。
3. 窗口通知的**实际投递集合**与载荷：本轮只证明注册对 44 个 ID 都返回 `0`，未观察任何回调。需要在常驻进程里订阅并在真实操作（开关窗口、切 Space、最小化、全屏）下计时观测。
4. `SLSRequestNotificationsForWindows` 的 macOS 14/15 行为差异：本机是 26。
5. **非当前 *用户* Space** 的窗口：本机只有一个用户 Space（Space 1），另两个是 `type=4` 全屏 Space。本文对“非当前 Space”的结论来自全屏 Space（A4/§4.1），**不等价于**“非当前桌面 Space”，需要多 Space 环境复测。
6. 隐藏应用（`NSRunningApplication.hide`）的窗口读：未测（会改变用户可见状态）。
7. `frameBounds` 的确切定义（阴影 / 裁剪 / 可视区）：普通窗口表现为“bounds + 四周阴影”，但全屏宽窗口观测到更小甚至 `0x0`。需要专门的窗口形态矩阵。
8. `SLSCopySpacesForWindows` 的 `selector` 取值表；`SLSPackagesGetWindowConstraints` 第一个参数的真实含义（本机表现为“调用者连接”）。
9. 无响应应用（AX timeout）的窗口：私有读**理论上**不经过目标进程，但本轮没有构造出无响应应用来验证。
10. 跨版本：全部本机结论只对应 macOS 26.6.2 (25G83) / arm64。

**真机验收记录模板**（沿用能力矩阵第 11 节）：`OS build + 架构 + TCC/AX 状态 + 目标应用版本 + 操作前状态 + 返回值 + 操作后状态 + 收敛时间`，并且至少覆盖：普通窗口、固定尺寸窗口（`min=max`）、sheet/dialog、最小化、隐藏应用、原生全屏、非当前 Space、双显示器不同排列。

## 10. 来源

### Apple（公开契约）

- **[A1] SkyLight 私有框架 SDK 桩**：`/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk/System/Library/PrivateFrameworks/SkyLight.framework/Versions/A/SkyLight.tbd`（`tbd-version: 4`，`targets: [x86_64-macos, x86_64-maccatalyst, arm64e-macos, arm64e-maccatalyst]`，192 个唯一符号）。本文用到的符号全部在其中（`_SLSGetWindowBounds` 493 行附近、`_SLSPackagesGetWindowConstraints` 567 行、`_SLSRegisterConnectionNotifyProc` 586 行、`_SLSRequestNotificationsForWindows` 601 行、`_SLSWindowIteratorGetBounds`/`GetConstraints` 877-878 行）。注意：这是 Apple 交付的**私有框架桩**，不是文档化 API。
- **[A2] AX 错误码**：同 SDK `…/HIServices.framework/Headers/AXError.h:43`（`kAXErrorInvalidUIElement = -25202`）、`:70`（`kAXErrorAPIDisabled = -25211`）。
- **[A3] CGError**：同 SDK `…/CoreGraphics.framework/Headers/CGError.h:17-19`（`kCGErrorSuccess = 0`、`kCGErrorFailure = 1000`、`kCGErrorIllegalArgument = 1001`）。

### Mach-O 证据（本机）

- **[M1] 导出表**：`/usr/bin/dyld_info -exports -arch arm64e /System/Library/PrivateFrameworks/SkyLight.framework/SkyLight`（2026-09-17，2582 行）。本文引用的文件内偏移：`_SLSCopyBestManagedDisplayForRect 0x002F8480`、`_SLSCopyManagedDisplayForWindow 0x002F8804`、`_SLSCopySpacesForWindows 0x002F388C`、`_SLSGetWindowBounds 0x0032F040`、`_SLSGetWindowLevel 0x0032A398`、`_SLSMoveWindow 0x00329784`、`_SLSPackagesGetWindowConstraints 0x003A0238`、`_SLSRegisterConnectionNotifyProc 0x0034B264`、`_SLSRequestNotificationsForWindows 0x003365F0`、`_SLSSetWindowAlpha 0x0032DA5C`、`_SLSSetWindowShadowParameters 0x0032C780`、`_SLSSetWindowSubLevel 0x0032A488`、`_SLSSetWindowTags 0x0032DE68`、`_SLSSetWindowTransform 0x0033B38C`、`_SLSWindowIteratorGetBounds 0x0047C23C`、`_SLSWindowIteratorGetConstraints 0x0047C75C`、`_SLSWindowIteratorGetCornerRadii 0x0047C32C`、`_SLSWindowIteratorGetAlpha 0x0047C6C0`、`_SLSWindowIteratorGetLevel 0x0047C674`。**导出表中没有 `_CGS*` 符号**（0 个匹配）。
- **[M2] 反汇编**：`/usr/bin/dyld_info -disassemble -arch arm64e <同上路径>`，59 MB / 1.5 s。本文引用的片段：`_SLSWindowIteratorGetBounds`（`ldp d0,d1,[x19,#0x60]` / `ldp d2,d3,[x19,#0x70]` 后 `retab`）、`_SLSWindowIteratorGetConstraints`（`csel x20,x1,x2` 三元组 → `_iterator_seek` → `str q0,[x20]`/`[x21]`/`[x22]`，无返回值设置）、`_SLSWindowIteratorGetAlpha`（`ldr s0,[x19,#0x5c]`）、`_SLSWindowIteratorGetLevel`（`ldr w0,[x19,#0x58]`）、`_SLSGetWindowBounds`（`mov w3,#0; b _GetWindowBounds`）。
- **[M3] HIServices 导出表**：`/usr/bin/dyld_info -exports -arch arm64e /System/Library/Frameworks/ApplicationServices.framework/Versions/A/Frameworks/HIServices.framework/HIServices`（615 行）：`__AXUIElementCreateWithRemoteToken 0x5F5C`、`__AXUIElementGetWindow 0x20FA4`、`_AXUIElementCreateApplication 0x1FAEC`、`_AXUIElementSetMessagingTimeout 0x1FB20`。
- **[M4] 运行时别名**：`dlsym(RTLD_DEFAULT, …)` + `dladdr`：`SLSGetWindowBounds` 与 `CGSGetWindowBounds` 解析到**同一地址**（`0x18b5a8040`），两个名字的镜像均为 `…/SkyLight.framework/Versions/A/SkyLight`。注意 `CGSGetWindowBounds` 既不在 [M1] 导出表也不在 [A1] `.tbd` 中。

### Rift 固定提交 `beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1`

（本轮通过 `raw.githubusercontent.com` 按 SHA 取文件，非分支快照。）

- **[R1] SkyLight 绑定与签名**：[`src/sys/skylight.rs:436-476`](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/skylight.rs#L436)（`SLSWindowQueryWindows`、`SLSWindowIteratorGetBounds` → `CGRect`、`SLSWindowIteratorGetConstraints(iterator, min, max, cur) -> CGError`、`SLSPackagesGetWindowConstraints`、`SLSCopySpacesForWindows`）。注意 `SLSWindowIteratorGetConstraints` 的声明形状与 [M2] 反汇编不符（返回值）。
- **[R2] iterator 读点与约束回退**：[`src/sys/window_server.rs:185-228`](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/window_server.rs#L185)（`level`/`bounds`/`alpha`/`constraints`，全零时回退到 `SLSPackagesGetWindowConstraints`；该回退对第三方窗口在本机不产生数据，见 §4.3）、[`:597`](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/window_server.rs#L597)（约束的消费方）。
- **[R3] WindowServer 通知**：[`src/sys/window_notify.rs:54-104`](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/window_notify.rs#L54)（`init` / `SLSRegisterConnectionNotifyProc` / `update_window_notifications`）、[`:146-280`](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/window_notify.rs#L146)（每个事件的载荷解析）、[`src/sys/skylight.rs:164-232`](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/skylight.rs#L164)（`KnownCGSEvent` 枚举）。
- **[R4] AX 写入仍是 resize 路径**：[`src/sys/axuielement.rs:179`](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/axuielement.rs#L179)（`frame()`）、[`:262-263`](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/axuielement.rs#L262)（`set_size` 写 `AXSize` 属性）、[`:295`](https://github.com/acsandmann/rift/blob/beeac0e5c7dc5c6ae442bd55964af6c316dcd3b1/src/sys/axuielement.rs#L295)（`can_resize` 读 `AXSize` 的 settable）。

### yabai 固定提交 `dd845723416f5fe92af49fad5ebab00369e07edd`

- **[Y1] 声明与 daemon 侧读**：[`src/misc/extern.h:14-24,29-30,64-72`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/misc/extern.h#L14)（`SLSGetWindowBounds`、`SLSGetWindowLevel`、`SLSGetWindowAlpha`、`SLSCopyManagedDisplayForWindow`、`SLSCopyBestManagedDisplayForRect`、`SLSCopySpacesForWindows`、`SLSMoveWindow`、`SLSRequestNotificationsForWindows`、`SLSRegisterConnectionNotifyProc`）；[`src/window.c:29-87`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window.c#L29)（`window_display_uuid` 的 NULL 回退、`window_space` 用 `SLSCopySpacesForWindows(cid, 0x7, …)`）、[`:843-856`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window.c#L843)（`window_is_sticky` = 成员数 > 1）、[`:900-927`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window.c#L900)（`SLSWindowIteratorGetLevel` 优先、`SLSGetWindowLevel` 回退）。**该快照的 `extern.h` 没有声明 `SLSWindowIteratorGetBounds` / `SLSWindowIteratorGetConstraints`**——成熟管理器没有用它们。
- **[Y2] 真实 resize 仍写 AX**：[`src/window_manager.c:425-433`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window_manager.c#L425)（`window_manager_resize_window` 写 `kAXSizeAttribute`）、[`:729-761`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window_manager.c#L729)（`window_manager_set_window_frame` 按 size → position → size 写 AX）。
- **[Y3] Dock payload 的写入清单**：[`src/osax/payload.m:670,697`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/payload.m#L670)（`SLSSetWindowAlpha`）、[`:758`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/payload.m#L758)（`SLSSetWindowSubLevel`）、[`:770-774`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/payload.m#L770)（sticky = tag bit 11）、[`:795-808`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/payload.m#L795)（shadow = tag bit 3）、[`:460,516-560`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/payload.m#L516)（`do_space_move` / `do_space_destroy` / `do_space_create` / `do_space_focus`）、[`:326-348`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/payload.m#L326)（`add_space_fp` / `remove_space_fp` 由 `hex_find_seq` 特征扫描得到）、[`:938-974`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/payload.m#L938)（能力位与 opcode 分发）。
- **[Y4] SA opcode 表**：[`src/osax/common.h:9-45`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/osax/common.h#L9)（`OSAX_ATTRIB_ADD_SPACE`、`SA_OPCODE_SPACE_CREATE/DESTROY/MOVE`、`SA_OPCODE_WINDOW_OPACITY/LAYER/STICKY/SHADOW/ORDER`…）。
- **[Y5] Space 命令的 daemon 侧入口**：[`src/message.c:101-107`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/message.c#L101)（`space --create` / `--destroy` / `--move` 等）、[`:1858`](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/message.c#L1858)（`COMMAND_SPACE_CREATE` 分支）。
- **[Y6] SIP 要求**：[Disabling System Integrity Protection](https://github.com/asmvik/yabai/wiki/Disabling-System-Integrity-Protection)（Wiki 未固定 revision，核查日读取）——与 [sls-order-window-research.md] `[Y5]` 同一来源。

### Spool 源码（实现证据，本次阅读时的文件状态，HEAD `f5a8344`）

- **[S1] remote token**：`src/manager/discovery.rs:449-468`（扫描 `element_id` 并用 `_AXUIElementCreateWithRemoteToken` 创建 element）、`:569-588`（20 字节 token 布局：pid / 0 / `"coco"` / element_id）、`:20`（`ELEMENT_LIMIT = 0x7fff`）。
- **[S2] 约束接缝**：`src/ecs/layout_intent.rs:100-134`（`WidthConstraint`、`native_width_constraint`，`dead_code` + “trusted constraint adapter seam” 注释）、`:207`（已有测试用 `native_width_constraint(984.0, 1200.0, 8.0)`）。
- **[S3] 现有私有读**：`src/manager.rs:1117-1170`（`space_window_list_for_connection` 的 iterator 读点）、`:1181-1230`（`window_iterator_for_connection` / `window_iterator_for_id`）；`src/manager/skylight.rs`（`SLSCopySpacesForWindows`、`SLSCopyWindowsWithOptionsAndTags`、`SLSWindowQueryWindows` 等绑定）。
- **[S4] 事件订阅**：`src/platform/notify.rs:17-29`（注册/注销绑定）、`:134-143`（`start()` 注册的 5 个事件）、`:280-343`（`KnownCGSEvent` 枚举）、`:189-258`（载荷解析）。注意该枚举里**没有** `WindowDisplayChanged = 805`（Rift [R3] 有）。
- **[S5] 通知版本门槛**：`src/manager.rs:999-1013`（`macos_major_version() < 15` 直接返回 Ok）。
- **[S6] 被注释的绑定**：`src/manager/skylight.rs:76`（`SLSGetWindowBounds`）、`:89`（`SLSMoveWindow`）、`:105`（`SLSCopyManagedDisplayForWindow`）、`:121`（`SLSCopyBestManagedDisplayForRect`）。
- **[S7] 写路径**：`src/manager/windows.rs:127-141`（`resize_staging_origin`）、`:749-787`（`write_ax_frame`）、`:1057-1063`（`update_frame` 读 `AXPosition`/`AXSize`）、`:1212`（`SLSWindowIteratorGetCornerRadii` 动态探测）、`:243-262`（`ax_window_id`）。

### 本机实测记录（探针 `examples/native_window_probe/sip_private_probe.m`，macOS 26.6.2 / 25G83 / arm64）

每条都可用 `clang -fobjc-arc -framework Cocoa -framework CoreGraphics -o /tmp/sip_private_probe …` 后按子命令原样复现。

- **[L1] `read`（默认）**：25 个可枚举窗口的一览。观察：iterator `present=1` 25/25、`SLSGetWindowBounds err=0` 25/25、`SLSGetWindowLevel err=0` 25/25、`cornerRadii` 命中 25/25；9/25 有非零约束包；25/25 的 `SLSPackagesGetWindowConstraints(main_cid)` 返回 `err=0` + 全零。含 Emacs `min=73x29 max=100000x100000 cur=720x452`、AutoFill `min=10x14 … cur=312x237`。
- **[L2] `foreign <host_wid>`（第三方写探测，目标是另一探针进程的窗口）**：`SLSOrderWindow(order=1,rel=0)=1000`（`order_changed=0`）；`SLSRequestNotificationsForWindows(...)=0`；`SLSMoveWindow(+37,+29)=1000`（`before == after == (1127.0,46.0 500.0x722.0)`）；remote token 用例返回 `-25211`。
- **[L3] `own`（自有窗口状态矩阵）**：normal / at-max(900x700) / below-min(100x100) / minmax-changed / offscreen / minimized / deminiaturized / ordered-out。要点：`orderOut` 后 CG 查不到但 iterator + `SLSGetWindowBounds` 仍返回 `(200.0,156.0 600.0x400.0)`；`miniaturized=1` 时同样返回有效 frame；`setFrame(-3000,-3000)` 被夹到 `(-560.0,-924.0)`；`SLSOrderWindow` 自有 `=0`、`SLSMoveWindow` 自有 `=0` 且真的移动（AppKit frame 同步为 `(260.0,-336.0 …)`）。
- **[L4] `spaces`**：1 个显示器、3 个 Space（`id=1 type=0` 当前、`id=570 type=4`、`id=576 type=4`）；Space 1 列出 59 个窗口。Spool bar `wid=59658`：`spaces(0x7)=3[570,576,1]`、`level=25`（与 CG layer 一致）。非当前 Space 且不在 on-screen 列表的窗口仍可读：Chrome `wid=46791 → (0.0,80.0 1470.0x876.0)`、`wid=46794 → (0.0,65.0 1470.0x47.0)`、ChatGPT `wid=51304 → (0.0,33.0 1470.0x923.0)`。
- **[L5] `width`（约束包语义，可控 AppKit 声明）**：见 §4.2 表格。另：`setFrame` 到 900x700 时被屏幕可见区夹到 605x732（与 yabai `window_manager_set_window_frame` 注释里的 “macOS constraints (visible screen-area)” 一致 [Y2]）。
- **[L7] `read <host_pid>`（跨进程约束读，声明已知）**：`host` 声明 `contentMinSize=420x310` / `contentMaxSize=880x690`，调用方读到 `min=420x342 max=880x722 cur=500x722`；`pkgConstraints(main_cid) err=0 全零`；`pkgConstraints(owner_cid) err=268435459 min=nan`。
- **[L9] 窗口销毁后的读**：`host` 进程退出后对同一 `wid`（`59735`）再读：iterator `present=0`、全零几何；`SLSGetWindowBounds err=1000 (inf,inf 0.0x0.0)`；`SLSCopySpacesForWindows(0x7)=0[]`；`SLSCopyManagedDisplayForWindow=NULL`。
- **[L10] `SLSWindowIteratorGetCornerRadii`**：正常带标题栏窗口 `count=4 first=16`；瞬态 cursor 表面 `count=4 first=0`。
- **[L11] `sentinel`**：`wid=0/1/0xFFFFFFFF/999999` → `SLSGetWindowBounds`/`CGSGetWindowBounds` `1000` + `(inf,inf 0x0)`；`SLSGetWindowLevel` `1000` 且**不覆盖** out-param；`spaces(0x7)=0[]`；`SLSCopyManagedDisplayForWindow(0)=NULL` 但 `(1)/(0xFFFFFFFF)/(999999)` 非 NULL；`SLSCopyBestManagedDisplayForRect((99999,99999,10,10))` 非 NULL。
- **[L13] `notify`**：对 44 个事件 ID 调 `SLSRegisterConnectionNotifyProc`，全部返回 `0`（含 `0xFFFFFFFF`）。
- **[L14] AX 权限**：`AXIsProcessTrusted()=0`；`_AXUIElementCreateWithRemoteToken` 返回非 NULL，随后 `_AXUIElementGetWindow` / `AXUIElementCopyAttributeValue(AXTitle)` 均 `-25211`。
- **[L15] `symbols`**：40 个符号的 `dlsym` + `dladdr` 表，全部 present；`_AXUIElementGetWindow` / `_AXUIElementCreateWithRemoteToken` 落在 HIServices，其余落在 SkyLight。

## 11. 附录：探针关键片段（完整源码见 `examples/native_window_probe/sip_private_probe.m`）

读一个窗口的几何 + 约束包（`query_windows`，节选；一次查询是本地解码，没有跨进程往返）：

```c
CFTypeRef query = SLSWindowQueryWindows(cid, arr_of_window_ids, count);
CFTypeRef it = SLSWindowQueryResultCopyWindows(query);
while (SLSWindowIteratorAdvance(it)) {
    uint32_t wid = SLSWindowIteratorGetWindowID(it);
    CGRect   b   = SLSWindowIteratorGetBounds(it);       /* == CG kCGWindowBounds（实测） */
    int      lvl = SLSWindowIteratorGetLevel(it);        /* == CG kCGWindowLayer（实测） */
    float    a   = SLSWindowIteratorGetAlpha(it);        /* == CG kCGWindowAlpha（实测） */
    CGSize min = {-1,-1}, max = {-1,-1}, cur = {-1,-1};  /* 预填哨兵：区分“写了零”和“没写” */
    SLSWindowIteratorGetConstraints(it, &min, &max, &cur);   /* 不检查返回值，它是残留指针 */
}
```

“窗口真的没了”的判据（实测：`host` 进程退出后读同一 `wid`）：

```c
CGRect sls = {-1,-1,-1,-1};
CGError e = SLSGetWindowBounds(cid, wid, &sls);   /* 期望 1000，且 sls 被写成 (inf,inf 0x0) */
/* 同时：iterator 查询该 wid 时不 advance（present=0），spaces(0x7) 为非 NULL 空数组 */
```

第三方写权限测试（目标是第二个探针进程创建的窗口，从调用方看 owner connection 不同）：

```c
/* 结果：1000 / 1000 —— 层级与位置都不变 */
SLSOrderWindow(cid, host_wid, 1, 0);
SLSMoveWindow(cid, host_wid, &point);
/* 同一组调用对自有窗口：0 / 0，且真的移动 */
```

约束包语义的可控实验（`width` 模式，自有窗口）：

```objc
w.contentMinSize = NSMakeSize(700, 300);          /* 声明下界 */
[w setFrame:NSMakeRect(200,200,600,400) display:YES];   /* 当前外框比下界窄 */
/* 读回：min = 600x332 —— 应用声明的下界被夹到当前尺寸（低报） */
w.contentMaxSize = NSMakeSize(500, 300);
/* 读回：max = 500x332；未声明上限时 max = 100000x100000（哨兵，不是真实区间） */
```
