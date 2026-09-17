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

## 初步观察（非普查数据，2026-09-17 当天）

采集尚未跑满，但当天排查已经拿到几类**具体**事实；它们不是比率，只作为后续统计的对齐点。

### 1. 仪表本身暴露的一个代价（新证据）

第一份 5 分钟汇总：`calls=48249 distinct=491`。其中**单个窗口的 `AXCloseButton`/`AXMinimizeButton` 各被读 333 次、`success=0`、全部 `no_value`**——因为窗口 chrome 已改为对每个候选都读（见 [窗口策略](../../WINDOW_POLICY.md) 3.1），而应用审计每秒跑一次、每次都重建窗口对象。这是"窗口样证据"的真实成本，也正是能力快照必须存在的理由：这些读在同一秒内不会变。

### 2. Emacs 案例：0.25 秒对慢应用就是"不可读"

用户报告"Emacs 窗口没有被纳入管理"。事实链：

| 观察 | 证据 |
| --- | --- |
| Emacs（`org.gnu.Emacs`，pid 5243）**有** 6 个层 0、alpha 1.0 的 CG 窗口 | CG 探针（主框 720×452 在 (544,204)，另有 5 条 1470×33） |
| Spool 认为它有 0 个窗口 | `spool app list` → `window_count: 0`；`window reconcile` 两次（间隔越过 30s 退避上限）仍为 0 |
| 默认 0.25s 下：**观察者注册失败** | 日志 `AXObserverAddNotification(AXFocusedUIElementChanged): -25204`，随后 `application AX endpoint unavailable; backing off probes` |
| 改成 1.0s 后：**再无 Emacs 失败** | 同一日志里 `backing off` 行全部消失（`SPOOL_AX_TIMEOUT_SEC=1.0`） |
| 不是"窗口样证据"门槛挡的 | 通过守护进程自己的检查面读 Emacs 主框：`AXCloseButton` **存在**（值为 AXUIElement），`AXMinimizeButton`、`AXFullScreenButton` 均在 |
| 仍然没被纳入 | `window_count` 依旧 0（当时尚未定案，见下） |

机制（代码）：`reconcile.rs` 的 `can_probe = application_ax_ready(app) && refresh_application_observer(...)`，只有为真才调用 `refresh_application_inventory`。也就是说**订阅失败会把"读窗口清单"一起挡掉**，把应用放进指数退避（1s→30s 封顶）且每次重试都失败——外部表现与"这个应用坏了"完全一样。

余下的三种可能（普查按应用标签后即可分辨）：① Emacs 的 `AXWindows` 是空表（其 AX 实现稀疏）；② 分步发现没有 enqueue 到它的窗口；③ 候选被静默 `Ignore`/`Defer`。

**带应用标签的普查（同一晚，`SPOOL_AX_TIMEOUT_SEC=1.0`）把范围收窄到第三种**：

```
ax_census source=ax operation=read subject=AXWindows app=org.gnu.Emacs|5243 success=345 failures=0
ax_census source=ax operation=read subject=AXRole      app=org.gnu.Emacs|5243|59532 success=3
ax_census source=ax operation=read subject=AXSubrole   app=org.gnu.Emacs|5243|59532 success=3
ax_census source=ax operation=read subject=AXCloseButton app=org.gnu.Emacs|5243|59532 success=2
ax_census source=ax operation=settable subject=AXPosition.settable app=org.gnu.Emacs|5243|59532 success=2
...（`window_count` 仍为 0）
```

即：**窗口清单读得到、从不失败**（345 次/5 分钟 ≈ 审计频率），但 5 分钟里只有**一个窗口号 59532** 被完整读了 2–3 次——说明绝大多数候选在**进入准入之前**就被丢弃。最可能的丢弃点正是当时**尚未接仪表**的 `ax_window_id`（`_AXUIElementGetWindow`）：`window_inventory` 对清单里每个元素都要先取窗口号，取不到就静默 `continue`。这与另一条既有日志吻合：`triggers.rs:135 can not get current focus: Unable to get window id from element 0x...`。

因此已补两处仪表（下一次运行即可定案）：`subject=AXWindowId`（窗口号解析的成功/失败，按应用）与 `operation=admit`（每个候选的准入结局 `track`/`track_floating`/`ignore`/`defer`，按应用与窗口）。后者把"这个应用读不到"与"它的窗口读到了但被拒了"分开——这两者在外面看起来一样。

### 3. 当天日志里的其他失败类别

- **`-25204`（Cannot complete）分布很广**：Thaw(9)、DaisyDisk(3)、Spotlight(2)、Emacs(2)、DockDoor(1)、Calculator(1)、AlDente(1)——即"应用不按时回答"是这台机器上的主要失败形态。
- **私有接口返回空**：`topology.rs:84 unable to observe Space membership ... nullptr returned from SLSCopyWindowsWithOptionsAndTags`（多个 Space）。
- **属性不支持**：`unable to disable AXEnhancedUserInterface ... -25208`（NotImplemented）。
- **元素不是窗口**：`triggers.rs:135 can not get current focus: Unable to get window id from element 0x...`（`_AXUIElementGetWindow` 失败）——焦点元素确实不是窗口，这类不算故障但也需要区分。

### 4. 测量过程本身的教训（已修）

第一次采集返回"no census block"：**守护进程跑的是早于仪表的二进制**，而当时无法从日志区分"没失败"与"没装表"。现已加一行每进程一次的 `ax_census active ...`。另外两处仪表缺陷也已修：窗口级行原本没有应用标签（`app=?|?|59357`，因为读取发生在 pid 解析之前），汇总原本只打 40 行（正好会挡住被问到的那个应用）。

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
