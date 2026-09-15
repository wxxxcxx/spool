# 05 — Contract：删除重复检查与裸字符串原因

Id: 05
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [ADR 0009 — Command Admission](../../adr/0009-command-admission.md)
Blocked by: 04

## What to build

配方迁移完成后，删除所有已无调用者的重复检查、闭包与裸字符串原因码。

- 删除各领域模块中已由 `Admission` 取代的本地检查与辅助闭包。
- 删除配方覆盖范围内已无生产者的裸 `rejected("...")` 字符串，改用 `Rejection`。
- 确认无死代码、无重复定义。

## Acceptance criteria

- [ ] 无残留的重复准入检查/闭包/裸字符串原因（配方覆盖范围内）。
- [ ] IPC 拒绝码字符串保持不变。
- [ ] `cargo fmt --check`、`cargo clippy -D warnings`、`cargo test -p spool` 全绿。
