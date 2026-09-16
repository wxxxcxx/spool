# 恢复：每 Space 焦点记忆的候选与导入

Id: 29
Type: task
Label: wayfinder:task
Status: resolved
Assignee: none
Parent: [跨重启恢复的入口票（恢复地图）](25-cross-restart-recovery.md)
Blocked by: none


> **2026-09-16 反转**：本票整体被 [ADR 0010](../../../adr/0010-runtime-state-rebuilt-from-rules.md) 取代——retained state 只在运行期存在，没有落盘、候选、导入缝或 `session restore`。跨会话偏好改由**规则**表达（字段按需增补），未表达者按"观测 → 默认"重建、接受丢失。反转原因与落地见 [运行期状态与实现对账](../../runtime-state/map.md)。

## Question

恢复地图（25 号票）裁决的域顺序是布局 → 归属 → 焦点。前两个域已落地（[26 号票](26-recovery-first-slice.md) 的入口与启动所有者、[28 号票](28-recovery-membership.md) 的声明 Space）；本票是第三个域：把**每 Space 的焦点偏好与逻辑选择**保存成候选，并接进同一个可信映射入口。不重新表决任何已有政策。

## 决策沿用（不再询问）

- 候选优先、自动绑定另立研究票；导入必须由启动方担责、只改状态、不写原生；先校验全部绑定再应用（一条坏绑定拒绝整次导入）。
- 身份连续性用组合证据：窗口编号只在同一登录会话内有意义，且必须被 pid/bundle 佐证；[27 号票](27-window-identity-verification.md) 未核实前不放松佐证规则。
- 在途尝试不恢复；恢复的呈现交给既有动画管道，不加专用跳变路径。

## 接口提案

**保存**：`SpoolState` 新增 `focus: Vec<SavedFocus>`（`#[serde(default)]`，不升版本）：

```json
"focus": [
  {
    "space_id": 2,
    "preference": {"window_id": 42, "pid": 501, "bundle_id": "com.example.app"},
    "selection": {"window_id": 43, "pid": 501, "bundle_id": "com.example.app"}
  }
]
```

- 一个 Space 一条，按 `space_id` 排序；提示与成员/浮动/归属用同一份缓存身份（`window_id`/`pid`/`bundle_id`），保存不做原生查询。
- **与成员 slot 的差异**：成员 slot 的 `null` 保留结构（"这个 item 期望几个成员"），而焦点记忆里 `preference: null` 无法与"没有偏好"区分。因此**保存时解析不到身份的偏好/选择不写空值，而是省略该角色**；两个角色都解析不到时整条不落盘。不能用一个无法区分的空值冒充"没有偏好"这一断言。
- 三条保存路径与 `window inspect` 的抓取都包含它。

**导入**：`RestoreBindings` 新增 `focus` 组，沿用"一条绑定 = 一个被证明的映射"：

```json
"focus": [
  {"candidate_focus": 0, "target_space": 2, "role": "preference", "target_window_id": 42},
  {"candidate_focus": 0, "target_space": 2, "role": "selection",  "target_window_id": 43}
]
```

- `RestoreFocusBinding { candidate_focus, target_space, role, target_window_id }`，`role` 为 `preference` / `selection`（一个候选条目有两个独立提示，因此一条绑定只证明一个角色）。
- `TrustedFocusBinding::new` 要求候选条目**在该角色上确有提示**（候选没有的提示不是可证明的映射），且该提示的 `window_id`/`pid`/`bundle_id` 与现场窗口**三者全等**，否则拒绝。
- 目标 Space 必须存在且不是原生全屏 Space（`import_target_space_not_found` / `import_target_space_not_user`），与归属组同一检查。
- 同一 `(candidate, role)` 或同一 `(space, role)` 重复 → 拒绝；组内先校验全部绑定再写任何一条。
- `import_space_focus` 只写 `FocusCoordinator` 的每 Space 记忆：像 `set_space_preference` 一样是作者状态，不产生激活请求、不动确认历史与 ordering 记录、不写原生。分组顺序为 列 → 浮动 → 归属 → 焦点。

**不检查且记录在案的边界**：导入不验证现场窗口当前是否属于目标 Space。同一次导入可能正是建立该归属的请求（归属组对还没有 `DeclaredSpace` 的窗口用 `Commands` 补建，在本系统内尚未生效），而消费侧本来就按资格过滤（`restoration_entity` 带 eligible 过滤；导航在不属于活动条带时回退入口目标），因此留下的是惰性记忆而不是悬空效果。前台用户编辑仍由 `set_space_preference` 的归属检查把关。

## 验收矩阵

| 情境 | 必须证明 |
|---|---|
| 可信的偏好/选择绑定 | 该 Space 的记忆被恢复；`FocusSnapshot` 逐字段不变（零激活、零观测推进） |
| 候选缓存身份与现场窗口不符 | `import_binding_rejected`，记忆保持原值 |
| 候选在该角色上没有提示 | 拒绝（无提示不是可证明的映射） |
| 目标 Space 不存在 | `import_target_space_not_found`，记忆保持原值 |
| 组内一条坏绑定 | 同组已校验的好绑定也不被应用 |
| 保存往返 | 提示可往返；解析不到的提示被省略，不写 `null` 冒充"无偏好" |

## 边界

- 不恢复在途尝试（永不恢复）、不自动绑定、不做恢复 UI 或传输层。
- 仍只在同一登录会话内有意义（窗口编号与 Space 编号都是会话作用域的）；跨登出/重启的连续性等 [27 号票](27-window-identity-verification.md)。
- 导入只覆盖被指认的 Space/角色，其余记忆与确认历史保持原样。

## 已实施（2026-09-15）

- **保存**：`SpoolState` 新增 `focus: Vec<SavedFocus>`（`#[serde(default)]`，版本仍 v6）；每 Space 一条，`space_id` 唯一性进 `valid()`。`FocusCoordinator::focus_memory()` 只导出"至少有一个角色"的 Space；`from_layouts` 用保存成员 hint 的同一个闭包解析偏好/选择，解析不到的角色省略，两个角色都解析不到则整条不落盘。三条保存路径与 `window inspect` 的抓取都接上了（`Res<FocusCoordinator>`）。
- **导入**：`RestoreBindings` 新增 `focus` 组与 `FocusRole`（`preference` / `selection`，wire 名为 snake_case）。`TrustedFocusBinding::new` 要求候选在该角色上有提示且缓存身份三者全等；`import_space_focus` 先查重（`(candidate, role)` 与 `(space, role)`）再写 `FocusCoordinator::import_focus_memory`（作者转移：不记修复、不产生激活、不动确认顺序）。
- **入口**：`restore_intents` 的焦点组沿用归属组的两项 Space 检查，随后按 列 → 浮动 → 归属 → **焦点** 的顺序应用；组间仍是一条坏绑定拒绝整次导入。
- **CLI 不变**：`spool session restore --bindings <file|->` 的文档多一个 `focus` 组，无需新参数；CLI 解析测试的样本文档覆盖了该组。
- **测试**：`src/tests/session_restore.rs` 新增 5 条（可信偏好+选择恢复且快照逐字段不变、身份不符拒绝且记忆不变、角色无提示拒绝、目标 Space 不存在拒绝、组内坏绑定使整组不被应用）；`src/tests/state.rs` 新增 1 条保存往返（不可解析的提示被省略而不是写成 `null`）。

## 本片验证

本机（Apple M4，并行套件，`--locked`）：

- `cargo test --workspace --locked`：主程序 1193 通过 / 0 失败 / 2 原有忽略（上一片 1186，新增 7 条）；local-ipc 24、shared-types 96、其余 6 / 4，全绿。
- `cargo test -p spool --no-default-features`：1051 通过 / 0 失败 / 2 原有忽略。
- `cargo fmt --all --check`、`cargo clippy -p spool --all-targets`（默认与 `--no-default-features --locked -- -D warnings`）全部通过；`git diff --check` 与 `docs/**/*.md` 相对链接检查（0 失效）通过。
- 行为证据：新状态类型的测试在旧代码上无法编译（`cannot find type SavedFocus` 等），这是新状态的固有形态；三条关键规则用“去掉规则则用例失败”复核——去掉 `TrustedFocusBinding::new` 的身份比对后，身份不符用例不再被拒绝（`expect_err` 在 `an uncorroborated binding is refused: ()` 处 panic），组内坏绑定用例也随之失败；去掉“候选中该角色必须有提示”后，无提示用例走到写入（`an absent hint is not a mapping: ()`）；把“两个角色都解析不到则省略”改成无条件保留后，保存用例多出 `SavedFocus { space_id: 3, preference: None, selection: None }`。

## 本片实际边界

- 没有安装、daemon 重启或真实桌面操作；mock 通过只证明模型与调用协议。恢复仍是**同一个登录会话内**的候选导入，不是真实的跨进程自动恢复（自动绑定按 25 号票另立研究票）。
- 导入不覆盖晚到的本会话编辑这一既有语义只被既有列/高度测试覆盖；焦点组不引入新的覆盖规则（记忆没有版本号，导入只在启动窗口内、被指认时写入）。
