# 03 — Migrate 配方（一）：targeted 与 transfer

Id: 03
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [ADR 0009 — Command Admission](../../../adr/0009-command-admission.md)
Blocked by: 02

## What to build

把 `targeted` 与 `transfer` 里重复的准入配方改为调用 `Admission`，行为不变。

- `targeted` 的"解析窗口→可见→可写→Space 归属"与"找 strip→唯一→列可选"改用准入方法。
- `transfer` 的源/目标 display 决议、可见 Space、strip 归属、几何新鲜度等检查改用准入方法，删除本地重复的 `layout(...)`/`eligible(...)` 闭包。
- 保持两类模块各自的失败策略（跳过、传播、不过滤）与原因码不变。

## Acceptance criteria

- [x] `targeted`/`transfer` 不再各自拼装准入检查；改为调用共享配方。
- [x] 现有 `command_dispatch.rs` 与新增准入测试全绿；无行为变化。
- [x] `cargo fmt --check`、`cargo clippy -D warnings`、`cargo test -p spool` 全绿。
