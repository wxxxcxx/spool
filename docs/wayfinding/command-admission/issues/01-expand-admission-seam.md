# 01 — Expand：Admission seam 与类型化拒绝

Id: 01
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [ADR 0009 — Command Admission](../../adr/0009-command-admission.md)
Blocked by: none — 可立即开始

## What to build

新增共享的 **Command Admission** 能力，但不改变任何现有路由。这是"让改动变简单"的一步。

- 一个 `Admission` SystemParam（或等价的小型耦合参数），捆绑准入所需的引用：窗口查询、`NativeTopology`、`WindowManager`、`FocusCoordinator`、`Config`。
- 它拥有配方方法：解析目标窗口、要求可见且可写、找唯一 strip、要求列可选、解析窗口的原生 Space 归属。方法返回**类型化拒绝**。
- 一个**有界**的类型化 `Rejection`，覆盖配方产出的原因；序列化到与今天逐字节相同的字符串（IPC 的 `admission_code()` 不变）。
- 直接测试：新增 `src/tests/command_admission.rs`，用 mock world 断言 `Rejection` 变体，覆盖缺失、不可见、受保护/在途、strip 歧义、列越界、占用列、topology 不完整等分支。

不接入 `dispatch_actions`，不改领域执行器；新能力只有测试使用。

## Acceptance criteria

- [ ] `Admission` 与 `Rejection` 就位，配方方法可被领域执行器调用。
- [ ] 类型化拒绝序列化出的字符串与现有对应码逐字节一致。
- [ ] `src/tests/command_admission.rs` 覆盖配方分支矩阵并全绿。
- [ ] 现有路由与领域执行器未被改动；`cargo fmt --check`、`cargo clippy -D warnings`、`cargo test -p spool` 全绿。
