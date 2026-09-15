# 06 — Docs：Command Admission 词条与 ADR

Id: 06
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [ADR 0009 — Command Admission](../../../adr/0009-command-admission.md)
Blocked by: 05

## What to build

把新概念与决定写进文档。

- 在 `docs/CONTEXT.md` 增加 **Command Admission** 词条，并明确它与既有 **Window Admission** 的区别（前者是命令是否被接受，后者是窗口是否被跟踪）。
- 新增 `docs/adr/0009-command-admission.md`：记录"唯一命令读取者 + 计划边界 + 类型化拒绝"的决定、被否决的替代方案与后果。
- 更新 `docs/ARCHITECTURE.md` 中关于 `dispatch_actions`/命令执行的相关段落，使其与单一读取者 + 计划边界一致。

## Acceptance criteria

- [ ] `CONTEXT.md` 有 Command Admission 词条，且与 Window Admission 不含糊。
- [ ] ADR 0009 记录了决定、替代方案与后果。
- [ ] `ARCHITECTURE.md` 的命令执行描述与新结构一致。
- [ ] `cargo fmt --check`、`cargo clippy -D warnings`、`cargo test -p spool` 全绿。
