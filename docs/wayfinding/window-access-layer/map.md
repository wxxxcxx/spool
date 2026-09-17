# 窗口访问层：把"读/写窗口"变成稳定的接口

Label: wayfinder:map
Status: open

起因（2026-09-17，用户）：排查控制中心面板被误纳管时，我们反复撞上"同一个事实有三种来源（AX / CG / 私有）、失败与合法缺失混在 `None` 里"。用户方向：**建一套能稳定读取与操作窗口的 API**。本图按用户选择走"先测量、再设计"（做法 A）。

## Destination

一个内部访问层：类型化的三值读写、按窗口的能力快照（带 generation 与失效）、按应用的健康度/退避、以及"期望状态 + 有界复验"的操作语义；诊断能说清**为什么**读失败。后端可换（AX 管语义与真实 resize，CG/SkyLight 管观测与归属，私有操作按能力门控）。

## Decisions so far

- [01 调用普查与采集](issues/01-call-census.md) — 仪表已实现（`src/manager/ax_census.rs`），数据待采；采集与判读标准见 [AX 与 CG 调用可靠性](../../research/ax-reliability-2026-09-17.md)。
- 设计（三值类型、能力快照、断路器、操作契约）**待数据**，未开票。

## Not yet specified

- 三值里 `why` 的最终枚举（由数据决定）。
- 能力快照的 TTL 与失效触发（由 `invalidated`/`unresponsive` 分布决定）。
- 是否需要一个只读的 CLI 出口暴露普查（目前只有日志汇总）。
- 私有 SkyLight 查询是否纳入同一漏斗。

## Out of scope

- 真实桌面验收（由用户按需进行）。
- 关闭 SIP / Dock 注入路线（研究结论见 `docs/research/macos-window-control-backends.md`）。
