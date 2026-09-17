# subrole 单独不构成窗口：把窗口样证据作用于所有准入路径

Id: 01
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [窗口表面准入](../map.md)
Blocked by: none

## Question

`admission()` 只要 `role == AXWindow` 且 subrole ∈ {`AXStandardWindow`, `AXFloatingWindow`, `AXDialog`, `AXSystemDialog`} 就返回 `Track`，**完全不看窗口样证据**；只有 subrole 不在其中（走到 fallback）时才会检查几何、在屏表面与关闭/最小化按钮。macOS 给自家的面板也用这四个 subrole 之一，于是控制中心的面板长驱直入。

## 决策（用户 2026-09-17 选择做法 B）

**窗口样证据对所有准入路径生效**：关闭或最小化按钮至少存在一个，**或** AXPosition 与 AXSize 都明确可写（无边框但可移动可缩放的窗口也算窗口）。

- 明确缺失（两个按钮都为 false 且移动/缩放至少一个明确为 false）→ **排除**。
- 读取失败 → **暂缓**（失败不等于没有）。
- 显式 `track=true` / `track=false` 规则单独决定，不受此门槛影响。
- **标题不作为证据**：控制中心的面板本身有标题（"Control Center"）。
- 层级（CG layer）**不作为**本次判据；层数保留在诊断里（`window inspect --source native --show cg`）。

## 已实施（2026-09-17）

- 政策：`window_policy::admission_with_fallback` 在 subrole 路径上加 `chrome_evidence`（按钮 → 能力），`FallbackEvidence` 新增 `movable`/`resizable`。fallback 路径的证据集不变（几何 + 在屏表面 + chrome）。
- 采集：`read_fallback_step` 新增 5（movable）、6（resizable）；**每个候选都读** 2/3（按钮）与 5/6（能力），0/1/4（几何/表面）仍只在 fallback 路径读（`src/manager/windows.rs` 的直接构造路径与 `src/manager/discovery.rs` 的分步发现共享同一顺序，后者抽成 `evidence_step`）。
- 测试：`a_known_subrole_without_window_evidence_is_not_a_window`（四个 subrole × 无 chrome → 排除；有按钮 → Track；无按钮但可移动可缩放 → Track；证据未知 → Defer；`track` 规则两个方向单独决定）；既有 fallback 测试与 discovery 的"标题恢复重试"用例按新证据集更新。
- 验证：`cargo test --workspace --locked` = 1170 / 24 / 96 / 6 / 4，`--no-default-features` = 1028；`fmt` 与两套 clippy `-D warnings` 通过。

## 边界与残余风险

- **真实桌面验收未做**：需要重启守护进程（旧进程仍跑旧判定）后重开控制中心，确认它不再出现在 `spool window list`。已纳入的旧窗口不会立刻消失（策略只作用于准入），但重启后不会再有。
- 残余风险：应用把真·窗口的 chrome 与能力都读成失败（而非明确不存在）时，窗口会停在"暂缓"而**不被管理**（可用 `track=true` 放行）。这是"未知不等于存在"的代价，与既有 fallback 的取舍一致。
