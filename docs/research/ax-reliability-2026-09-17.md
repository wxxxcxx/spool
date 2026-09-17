# AX 与 CG 调用可靠性：普查计划（数据待采）

日期：2026-09-17。状态：**仪表已实现，数据尚未采集**。本文是采集方案与判读标准，不是结论。

## 为什么要普查

Accessibility 是**同步的跨进程协议**，由每个应用自己实现：读一个属性就是一条发往另一个进程的消息，而**失败**（`kAXErrorCannotComplete` -25204、`kAXErrorInvalidUIElement` -25202、`kAXErrorAPIDisabled` -25211）、**合法缺失**（`kAXErrorNoValue` -25212、`kAXErrorAttributeUnsupported` -25205）与**元素根本没有 AX 面**（真机观察：控制中心面板 `attribute_names: []`）是三种不同的事实。代码里它们大多在 `Option`/`ok()` 处被压成同一个 `None`，因此"API 不稳定"无法定位到具体是哪一类。

普查把每次调用按（来源、操作、属性、应用、结局）计数，并**同时计成功**，于是能给出**失败率**而不只是失败次数——这才是区分"这个应用从不实现该属性"与"这个应用此刻忙"的依据。

## 仪表（已实现）

`src/manager/ax_census.rs`：计数 + 每 5 分钟一条 `INFO` 汇总，**不改变任何行为**（结果原样透传）。

已接入的调用点：

| 来源 | 操作 | 覆盖面 |
| --- | --- | --- |
| AX | read | `AXRole`/`AXSubrole`/`AXTitle`/`AXParent`/`AXFullScreen`/`AXPosition`/`AXSize`/`AXCloseButton`/`AXMinimizeButton`/`AXWindows`（应用清单） |
| AX | settable | `AXPosition.settable`/`AXSize.settable` |
| AX | write | `AXPosition`/`AXSize`（真实的移动与缩放） |
| AX | observe | 应用级 AX 观察者注册 |
| CG | read | `CGWindowListCopyWindowInfo`（含缺字段 `kCGWindowLayer`/`kCGWindowAlpha`） |

**尚未接入**（已知缺口，采集时按"未覆盖"看待）：`_AXUIElementGetWindow`、窗口级观察者注册/移除、`AXFocusedWindow`/`AXFocusedUIElement`、`AXMain`/`AXMinimized`、`AXRaise`/perform action、应用创建（`AXUIElementCreateApplication`）与 `AXObserverCreate`、SkyLight 私有查询。这些会在访问层切片里并入同一个漏斗。

判读方式：每行是 `ax_census source=… operation=… subject=… app=… success=N failures=M <kind>=…`，按失败数降序。关心的量：

1. **每个应用的失败率**（`failures / (success + failures)`）：高失败率 + 低绝对量 = 该应用 AX 实现差；高 `unresponsive` = 该应用常忙。
2. **`no_value` 与 `unsupported` 的比例**：这两类是"合法缺失"，占比高说明属性本身可缺（例如面板没有关闭按钮），**不是**不稳定。
3. **`invalidated` 的分布**：元素寿命问题的规模（对应 27 号票的 incarnation 议题）。
4. **写操作的失败率**：`AXPosition`/`AXSize` 的 `unsupported`/`illegal_argument` 比例，决定"写成功≠做到"到底有多常见。

## 采集

```sh
# 停掉当前守护进程，改用前台运行并落盘（不安装 launch agent）
./target/debug/spool service stop
RUST_LOG=spool=info ./target/debug/spool service run > /tmp/spool-ax.log 2>&1 &

# 正常使用一段时间（建议 >= 1 天，覆盖开关窗口、切换 Space、最小化、跨显示器）
# 然后取最后一次累计汇总：
scripts/aggregate-ax-census.sh /tmp/spool-ax.log
```

若已安装 launch agent，则直接读它自己的日志：`spool logs -n all > /tmp/spool-ax.log`（前台启动的进程只写终端，这一点 `service logs` 会明确报错）。

## 采集时要注意

- 汇总每 5 分钟一次且**累计**；取最后一次即为整个运行期。
- `RUST_LOG=spool=info` 是本仪表的级别；调成 `debug` 会淹没在准入/焦点日志里，不必要。
- 隐私：日志只有窗口号、pid、bundle id、属性名与错误码，没有标题内容（`AXTitle` 只记失败，不记值）。

## 这份数据要回答的问题（访问层设计的前提）

1. 三值读（`Known/Absent/Unknown(why)`）里到底需要几个 `why`？——取决于上表哪几类真实出现、各占多少。
2. 能力快照的 TTL 与失效触发：`invalidated` 与 `unresponsive` 的分布决定"多久重读一次"与"要不要按应用退避"。
3. 每应用断路器阈值：`unresponsive` 的连续长度分布决定隔离阈值。
4. 写操作的 `unsupported` 比例：决定"写前先用 settable 探测"值不值。

数据采集完成后，结论写入本文（或另开一份带日期的研究），并据此写访问层的 ADR 与首个切片。
