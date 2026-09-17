# 窗口表面准入：把系统表面挡在管理之外

Label: wayfinder:map
Status: resolved

起因（2026-09-17，用户在本机发现）：macOS **控制中心的面板**（`com.apple.controlcenter`，CG 层 22、656×967、无 AX 属性可读）被 Spool 当作一个浮动窗口纳入管理，用户用 `spool window list` / `window inspect` 核对确认。

## Destination

让"这是一个窗口"这件事由证据决定，而不是由 subrole 字符串单独决定：已实施于 [01 号票](issues/01-window-chrome-evidence.md)，契约同步进 [窗口策略](../../WINDOW_POLICY.md)。

## Decisions so far

- [subrole 单独不构成窗口](issues/01-window-chrome-evidence.md) — 窗口样证据（chrome 按钮或可移动可缩放）对所有 subrole 路径生效；标题不作为证据。

## Not yet specified

- 层级门槛（layer ≥ Dock/状态栏层不接受）：用户选择了证据路线而非层级路线，层数只作为诊断信息保留。
- 系统 bundle 清单（`com.apple.controlcenter` 等）：作为可选的最后一道兜底，未实施。
- 已在旧判定下被纳入的窗口（例如 ChatGPT 的 84×76 层 3 悬浮条）：策略只在准入时生效，重启后不再纳入（retained state 是运行期的，没有落盘状态要清理）。
