# 窗口唯一标识的核实（待核实）

Id: 27
Type: task
Label: wayfinder:task
Status: open
Assignee: none
Parent: [跨重启恢复的入口票（恢复地图）](25-cross-restart-recovery.md)
Blocked by: none

## Question

恢复地图把身份连续性定成"组合证据"，其中一条关键前提是**窗口编号可能被复用**，因此编号不能单独充当身份。这条前提今天只存在于两处**没有一手出处**的断言里：

1. `src/platform.rs` 的 `WindowIncarnation` 注释：*"Stable identity of one AX window object during its lifetime. Unlike [`WinID`], this changes when `WindowServer` reuses an integer ID."*
2. 本仓研究只说到"未规定"：`docs/research/native-tab-platform-observation-2026-09-11.md` 引 Apple 文档确认 `kCGWindowNumber` **在当前用户会话内唯一**、`NSWindow.windowNumber` 是另一回事（应用内 window-device 编号），紧接着写 *"These sources do not specify a tab member's server-device lifetime or ID behavior across selection, merging, and detaching. Do not turn common runtime mappings into an unconditional identity guarantee."*

"未规定"不等于"会被复用"。本票把这一点标为**待核实**，并列出核实后可能的结论分支。核实之前，25/26 号票里"编号可能被复用"的说法一律按**待核实的推断**对待，不得当作既成事实引用。

## 要核实的问题

1. **`kCGWindowNumber` 在窗口销毁后是否会被复用？** 同一登录会话内：创建/销毁若干窗口后，先前用过的编号是否会被新窗口拿到？若会被复用，间隔与条件是什么（同进程内复用？跨进程？立即还是延迟）？
2. **是否存在跨窗口稳定且唯一的标识？** 待查候选（都不假定可用）：
   - `kAXIdentifierAttribute`（`AXIdentifier`）：应用可自愿提供的稳定标识（UI 测试常用）——若某应用实现了它，可能是最强的单条证据；
   - `NSWindow.identifier` 与应用的状态恢复（state restoration）标识；
   - 私有的 CGS/surface 编号（`CGSWindow`、`SLS*` 通道上的窗口标识）与它们的作用域；
   - `_AXUIElementGetWindow` 返回值的语义与作用域（本仓正在用它取 `WinID`）。
3. **`CFHash(AXUIElement)` 到底哈希什么？** 现在 `WindowIncarnation` 就是它（`manager/windows.rs`）。需要确认：同一窗口通过不同 AX 路径取得的两个元素是否哈希相同（若不同，则 incarnation 会**假阴性**，把同一窗口判成换了实例）；以及对象释放后哈希值是否可能被复用（若会，则 incarnation 在长时间运行下也会**假阳性**）。

## 核实方式（建议）

1. **运行时探针**（沿用本仓的探针文化：`docs/examples/*_probe.md`、`scripts/probe-native-spaces.c`）：一个最小的 AppKit 程序，同一会话内循环创建/销毁窗口并记录每个窗口的 `kCGWindowNumber` 与 `NSWindow.windowNumber`，观察编号是否重现；再对同一个窗口用两条 AX 路径取元素，比较 `CFHash`。
2. **一手文档/头文件核对**：`CGWindow.h`、`NSWindow.h`（`windowNumber`、`identifier`、`isRestorable`）、`AXAttributeConstants.h`（`kAXIdentifierAttribute`）与公开文档中关于编号分配/寿命的表述，逐条记下"哪一条支持哪个结论"。
3. 结论写入 `docs/research/`（本仓研究文档的既有位置），并在本票记录支持与反对的具体出处；探针代码若值得留存则按既有惯例放进 `examples/`。

## 结论分支与后果（先写清，避免事后找理由）

- **若编号在同一会话内不会被复用**：恢复地图的证据阶梯可以简化——同会话内编号可作为强身份，`WindowIncarnation` 的注释需要改写，`MoveWindowIdentity` 等三元组仍保留但理由变化（防的是"元素被替换"而非"编号被复用"）。
- **若会被复用**：现行谨慎策略（编号必须被 pid/bundle/AX 属性/几何佐证）维持不变，并应把"复用现象"的现场证据补进 `platform.rs` 注释与该研究文档。
- **若 `AXIdentifier` 之类应用提供的稳定标识可用**：它成为恢复的主证据，编号降为佐证；需要同时研究"应用未提供时"的回落阶梯与误绑定代价。
- **若 `CFHash` 会假阴性/假阳性**：这是**独立于恢复**的既有缺陷（运行期的替换判定、焦点激活、几何在途屏障都用它），需要单独一张票评估影响面。

## 边界

- 本次不实现恢复；只核实事实并把结论写进研究文档与本票。
- 探针只在本机、单会话内观察；不声称跨登出/重启的结论，除非有相应证据。
- 不改动运行期行为（`incarnation` 语义若需变，另开票）。

## Resolution

待核实。核实前，25/26 号票中"编号可能被复用"的表述按推断对待。
