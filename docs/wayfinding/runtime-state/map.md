# Spool 运行期状态与实现对账

Label: wayfinder:map
Status: open

执行 [ADR 0010](../../adr/0010-runtime-state-rebuilt-from-rules.md)（state 只在运行期存在）与 [ADR 0011](../../adr/0011-realization-outcomes-and-alignment.md)（结局三分类 + 对齐到显示值）。决策细节只存 ADR 与子票，本图只记录次序与边界。

## Destination

让"状态 == 显示"成为运行期的实际不变式：接受的编辑最终要么被实现、要么按证据对齐到屏幕上真实的值；同时取消命令级准入闸门，并把跨会话偏好的职责从"快照 + 人工指认导入"移交给"规则 + 观测重建"。

## Notes

- ADR 0010/0011 均为 **Accepted, Not implemented**；本图落地前，`ARCHITECTURE.md`/`CONFIGURATION.md` 仍按现状描述，落地时同步。
- 本轮决定由用户在 2026-09-16 逐项确认（十项，见各子票的"决策沿用"）。
- 反转记录：24 号票的保存半、26/28/29 号票整体、以及 25 号票 2026-09-15 的恢复裁决，被 ADR 0010 反转；票面与实施记录在 03 票落地时更新，不静默删。
- 27 号票（窗口编号复用/CFHash）**不再卡任何东西**，保持 `open`；探针与未验证清单已提交（`scripts/probe-window-identity.m`、`docs/research/window-identity-2026-09-16.md`）。

## Decisions so far

- [对齐机制](issues/01-alignment.md) — F2：结局三分类、证据门槛、宽限期、逐域换算、版本门与诊断；先接既有失败源，不动准入闸门。
- [撤掉准入闸门](issues/02-admission.md) — F1：Mission Control/初始化交给结局 3，退出全拒，`Writable` 分类退役。
- [退役落盘与启动重建](issues/03-retire-persistence.md) — F3：删 `state.json`/保存/候选/导入/CLI，启动按"规则 → 观测 → 默认"重建，更新受影响的票与文档。
- [规则字段缺口](issues/04-rule-gaps.md) — F4：宽度/高度/浮动位置/Space/焦点/栈成员，按需一张一票。

## Not yet specified

无（本图由 ADR 0010/0011 直接推出；每个字段是否值得成为规则，留到 F4 按实际需要决定）。

## Out of scope

- 真实桌面验收（overview 期间写几何是否有害、原生夹取与动画时序）：mock 证明不了，另立验收。
- 原生约束采集器（用于区分"被原生最小尺寸夹住"与"效果根本没落地"）：ADR 0011 记录为第 3 类保持未确认的前提。
- 安装、部署、daemon 重启与提交推送之外的操作。
