# 08 — Fold：会话终止的一处决定与按 intake 收尾

Id: 08
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [ADR 0009 — Command Admission](../../adr/0009-command-admission.md)
Blocked by: 07

## Why

`Action::Quit`/`Action::Restart` 是本主题里最后一处「同一 action 两处实现」：

- checked 请求把 lifecycle 置为 `Stopping`，等回执写完由 transport 发
  `Event::Exit`；
- keybinding / Bar / Lua 走 `command_quit_handler` / `command_restart_handler`
  **完全不标记 lifecycle**，于是关闭过程中其它 action 仍被接受；
- reader 自己再按 `matches!(action, Quit | Restart)` 推一遍含义（三处）。

并行分支的 `04ed06c` 已经解决这一点，其语义已按本分支结构并入。

## What to build

- `admission::aftermath(&Action) -> Aftermath`（`Nothing | Stop | Restart`）：
  纯函数、全函数，所以在 World 存在之前 transport 也能问它。`Stop` 与
  `Restart` 必须分开 —— restart 是外部停进程，对它发 `Event::Exit` 会停两次。
- 表（`execute`）对两者都置 `Stopping` 后返回，不执行效果；效果由拥有
  transport 的一方收尾：
  - checked：回执写完 → `conclude_from_wire`（`Event::Exit` / 交棒重启）；
  - fire-and-forget：`admit` = `execute` + `conclude_in_world`。
- 删除 `command_quit_handler`、`command_restart_handler`，以及 reader 里三处
  自行推导；`LifecycleEffect::{Deferred, Immediate}` 这个流水线参数随之消失
  （差异不再作为参数穿进流水线，而是「谁负责收尾」）。
- 行为变化：来自 keybinding / Bar / Lua 的 quit 与 restart 现在会标记
  `Stopping`，并只停一次。

## Acceptance criteria

- [x] `aftermath` 是唯一说明「哪些 action 结束会话」的地方。
- [x] 关闭中的会话不再接受第二批 action，也不会第二次停进程。
- [x] `a_bus_quit_stops_the_session_once` 覆盖两态（Stopping、只停一次）。
- [x] mock 记录 `take_quit_requests()`，平台不再被隐式假设。
- [x] `cargo fmt --check`、`cargo clippy --all-targets`、`cargo test -p spool` 全绿。

## Left open

- `Event::LayoutSpaceRequested` 仍是唯一的具名「非 action」，其门槛必须与它所
  延续的 action 一致。
- 并行分支的 `docs/adr/0009-uniform-action-admission.md` 与本分支的 ADR 是同一
  决定的两份草案；只能有一份落 main，另一份的内容已并入。
