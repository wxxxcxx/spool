# 浮动窗口 Actions 调研与 Spool 建议

调研日期：2026-09-04。本文只讨论公开 action 设计，不修改实现。来源限定为项目官方文档、手册或源码。

> 实施状态（2026-09-04）：后续设计评审已经收敛为统一的窗口语义 action，
> 本文下方的候选命名和兼容别名仅保留为调研过程，不再代表当前接口。
> 当前接口使用 `window move <direction>`、`window grow|shrink width|height`、
> `window maximize`、`window toggle floating|stack` 和 `window focus other-layer`；
> Lua 对应入口统一位于单数命名空间 `spool.action`，旧命令不保留别名。

## 结论

成熟窗口管理器通常把浮动窗口能力拆成四层：

1. **状态**：平铺/浮动切换，且提供幂等的 `enable` / `disable`；
2. **几何**：相对移动、相对缩放、居中、预设位置、可恢复最大化；
3. **选择与层级**：在平铺和浮动窗口之间切换焦点，在浮动窗口之间导航、raise/lower；
4. **拓扑**：移动到显示器或工作区，并明确 `follow` / `stay`。

对 Spool，建议优先补齐**浮动窗口几何操作和显式状态操作**，继续复用现有的 Space-local focus history、原生 Space 移动与 frame reconciliation。不要把以下能力包装成可靠承诺：

- 外部应用窗口的永久置顶；
- 跨所有原生 Space 的 sticky；
- 不依赖最小化或专用 Space 的任意窗口 scratchpad。

这些能力在“不关闭 SIP、不注入 Dock”的边界下无法像 X11/Wayland 合成器那样稳定控制。

## 第一手资料对比

### yabai

yabai 的窗口命令覆盖最完整，特点是把状态、几何、拓扑和层级拆成彼此正交的 action。

| 能力 | 命令/语义 |
| --- | --- |
| 切换浮动 | `yabai -m window --toggle float` |
| 移动 | `--move rel:dx:dy` 或 `--move abs:x:y` |
| 调整大小 | `--resize top:dx:dy`、`bottom:dx:dy`、`left:dx:dy`、`right:dx:dy`；浮动窗口还可直接 `--resize abs:w:h` |
| 居中 | `--grid 1:1:0:0:1:1` 是全屏网格；一般居中可由绝对坐标/脚本计算 |
| 吸附/预设 | `--grid rows:cols:x:y:w:h` |
| 最大化/全屏 | `--toggle zoom-parent`、`zoom-fullscreen`、`native-fullscreen`、`maximize` |
| Z 序 | `--raise`、`--lower`、`--sub-layer below\|normal\|above` |
| 移动到显示器/Space | `--display ...`、`--space ...` |
| 暂存 | `--scratchpad name` 与 `--scratchpad recover` |
| 最小化 | `--minimize`、`--deminimize` |

但手册明确标出 `raise/lower`、sticky、子层级和 scratchpad 等部分功能依赖关闭部分 SIP。这个限制说明 Spool 可以借鉴 action vocabulary，却不能在当前安全边界下照搬其能力保证。

来源：[yabai 手册的 Window 命令](https://github.com/asmvik/yabai/blob/master/doc/yabai.asciidoc#window)、[SIP 能力表](https://github.com/asmvik/yabai/blob/master/doc/yabai.asciidoc#system-integrity-protection)。

### AeroSpace

AeroSpace 强调“平铺树的 layout mode”，而不是维护一套完整的浮动几何命令：

| 能力 | 命令/语义 |
| --- | --- |
| 切换浮动 | `layout floating`、`layout tiling`；可用 `layout floating tiling` 在两者间切换 |
| 聚焦 | `focus left\|down\|up\|right`；`focus-back-and-forth` 回到上次焦点 |
| 移动/重排 | `move left\|down\|up\|right`，主要服务于树结构 |
| 调整大小 | `resize width\|height smart +/-N` 或 `resize ... set N` |
| 最大化 | `fullscreen` 是 AeroSpace 自己的 layout fullscreen；`macos-native-fullscreen` 使用原生全屏 |
| 显示器/工作区 | `move-node-to-monitor ...`、`move-node-to-workspace ...`，可配合 `--focus-follows-window` |
| 暂存式工作区 | `summon-workspace` 可把工作区移动到当前显示器，但不是单窗口 scratchpad |

值得 Spool 借鉴的是：

- 状态设置使用 `layout floating` / `layout tiling`，天然幂等；
- 跨拓扑移动把“窗口是否跟随焦点”设为显式 flag；
- 原生全屏与布局最大化是两个不同 action。

来源：[AeroSpace Commands](https://nikitabobko.github.io/AeroSpace/commands.html)、[AeroSpace Guide：floating windows](https://nikitabobko.github.io/AeroSpace/guide#floating-windows)。

### Amethyst

Amethyst 的浮动 action 很少，更多依靠规则和布局：

| 能力 | 命令/语义 |
| --- | --- |
| 切换浮动 | `toggle-float` |
| 全局浮动布局 | `select-floating-layout` |
| 聚焦 | `focus-ccw` / `focus-cw` 和方向聚焦；没有独立的“focus floating tier” |
| 显示器 | `throw-screen-ccw` / `throw-screen-cw` / `throw-screen-1...` |
| Space | `throw-space-left` / `throw-space-right` / `throw-space-1...` |

其官方快捷键配置没有暴露通用的浮动窗口相对移动、网格吸附、置顶或 scratchpad action。Amethyst 因而更适合证明“`toggle-float` 是最低必要能力”，不适合作为 Spool 完整浮动控制面的上限。

来源：[Amethyst 默认配置源码](https://github.com/ianyh/Amethyst/blob/development/Amethyst/default.amethyst)、[Amethyst 配置文档](https://ianyh.com/amethyst/)。

### i3 / Sway

i3 与 Sway 的语义高度一致。它们把浮动窗口置于平铺窗口之上的独立层，并提供明确的 mode、位置、大小和 scratchpad action：

| 能力 | 命令/语义 |
| --- | --- |
| 状态 | `floating enable\|disable\|toggle` |
| 聚焦层级 | `focus mode_toggle` 在 tiling/floating 间切换；`focus floating` / `focus tiling` |
| 移动 | `move left\|right\|up\|down [N px]`；浮动窗口还支持 `move position x y`、`move position center`、`move position mouse` |
| 调整大小 | `resize grow\|shrink width\|height N px`；Sway 另有 `resize set width height` |
| sticky | `sticky enable\|disable\|toggle`，仅对浮动窗口生效 |
| 工作区/输出 | `move container to workspace ...`、`move container to output ...` |
| 暂存 | `move scratchpad` 隐藏窗口；`scratchpad show` 轮换显示 |

i3 的关键启发不是照搬 sticky，而是：

- 为脚本同时提供 `enable`、`disable` 和 `toggle`；
- 将 `focus mode_toggle` 定义为两种窗口层级之间的往返，而非修改窗口状态；
- scratchpad 有稳定的“隐藏集合 + 循环唤回”模型。

来源：[i3 User's Guide：floating、focus、scratchpad](https://i3wm.org/docs/userguide.html)、[Sway 官方 sway.5 手册源码](https://github.com/swaywm/sway/blob/master/sway/sway.5.scd)。

### Hyprland

Hyprland 提供最细粒度的浮动交互 action：

| 能力 | dispatcher/语义 |
| --- | --- |
| 状态 | `togglefloating` |
| 移动/缩放 | `moveactive x y`、`resizeactive x y`、`moveactive exact x y`、`resizeactive exact w h` |
| 居中 | `centerwindow` |
| 最大化/全屏 | `fullscreen` 支持 full/maximize 等模式 |
| Z 序 | `alterzorder top\|bottom\|up\|down` |
| 置顶 | `pin` 让浮动窗口出现在所有工作区 |
| 工作区/显示器 | `movetoworkspace`、`movetoworkspacesilent`、`movewindow`、`movecurrentworkspacetomonitor` |
| 暂存 | special workspace 配合 `togglespecialworkspace` / `movetoworkspacesilent special:...` |

Hyprland 的 `pin` 和 special workspace 建立在 Wayland 合成器对所有表面的直接所有权上。Spool 只能借鉴其 action 拆分，不能据此推导 macOS 也能可靠实现相同效果。

来源：[Hyprland Dispatchers](https://wiki.hypr.land/0.46.0/Configuring/Dispatchers/)、[Special Workspaces](https://wiki.hypr.land/0.46.0/Configuring/Special-Workspace/)。

## Spool 当前已有能力

当前公开 vocabulary 已包含：

- `window togglefloating`；
- `window focus floating` / `window focus tiled`；
- 浮动窗口获得焦点后，方向 focus 按几何中心寻找同方向最近浮动窗口；
- `window raise floating`；
- `window togglefloatlayer`；
- `window center`、preset width grow/shrink、`window snap`；
- `window nextdisplay` / `nextdisplaysend`；
- `window move-to-space <window-id> <space-id> stay|follow`。

实现层已有 `Floating` marker、每个原生 Space 的平铺/浮动 focus history、`FloatingLayer`、声明式 frame pipeline，以及原生 Space membership reconciliation。因此新增 action 应当只表达**期望状态**，继续由现有 pipeline 收敛，不应直接在 command handler 中绕过 ECS 长期状态。

## 建议的 Spool action 集合

### P0：应优先实现

| 建议 action | 语义 | 备注 |
| --- | --- | --- |
| `window floating enable` | 当前窗口变为浮动；已浮动时 no-op | 新增幂等形式 |
| `window floating disable` | 当前窗口回到平铺 strip；已平铺时 no-op | 必须保留/恢复合理插入位置 |
| `window floating toggle` | 两种状态切换 | `window togglefloating` 可保留为兼容别名 |
| `window focus floating` | 聚焦本 Space 最近使用的可见浮动窗口 | 已有 |
| `window focus tiled` | 聚焦本 Space 最近使用的平铺窗口 | 已有 |
| `window focus mode-toggle` | 在上述两个 tier 间往返并 raise 目标 tier | 可收敛现有 `togglefloatlayer` 语义 |
| `window focus next\|previous` | 在当前窗口所属 tier 内按稳定顺序循环 | 浮动窗口用 Space-local floating order；平铺窗口用 layout order |
| `window focus west\|east\|north\|south` | 在当前窗口所属 tier 内导航 | 浮动窗口按屏幕几何；平铺窗口按 layout 邻接关系 |
| `window nudge west\|east\|north\|south [px]` | 只移动浮动窗口；默认步长来自配置 | 避免与现有 `window resize` 冲突 |
| `window stretch west\|east\|north\|south [px]` | 移动对应边，改变浮动窗口大小 | 方向表示哪条边向外移动；负值可收缩 |
| `window place <preset>` | 把浮动窗口放到命名预设 | 见下一节 |
| `window maximize` | 在 usable viewport 内最大化；再次执行恢复原 frame | 不创建全屏 Space |
| `window fullscreen` | 进入/退出 macOS 原生全屏 | 与 maximize 明确分开；全屏 Space 无 overlay |

推荐内置 preset：

```text
center
left-half right-half top-half bottom-half
top-left top-right bottom-left bottom-right
maximize
```

`place` 应允许用户配置自定义 ratio frame，而不是把所有位置都扩展成独立 action。当前规则中的 `grid = "cols:rows:x:y:w:h"` 可以复用为统一的 geometry 表示。

### P1：跨拓扑与层级

| 建议 action | 语义 |
| --- | --- |
| `window display next\|previous\|<id> follow\|stay` | 将窗口移到显示器，并明确是否跟随焦点 |
| `window space next\|previous\|<id> follow\|stay` | 将窗口移到普通原生 Space |
| `window raise` | best-effort raise 当前浮动窗口，并激活所属 app |
| `window raise floating` | raise 当前 Space 所有可见浮动窗口并恢复 last-floating focus；已有 |
| `window layer floating\|tiled\|toggle` | 选择本 Space 哪个 tier 在前；替代只支持 toggle 的接口 |

浮动窗口跨显示器时建议保持**相对中心位置和窗口尺寸**，然后 clamp 到目标 usable viewport；不要无条件变成平铺，也不要默认改成目标显示器的一半宽度。跨 Space 则必须更新 membership intent，等 WindowServer 确认后再 follow/focus。

### P2：暂存/隐藏

推荐先设计、后实现：

```text
window stash <name>
window summon <name>
window stash-cycle
```

候选实现只有两种，各有明确代价：

1. **最小化实现**：stash 时记住 Space/frame 并最小化，summon 时取消最小化、移到当前 Space、恢复 frame 和焦点。风险是 AX 最小化状态和应用行为不完全一致。
2. **专用原生 Space**：stash 时移入用户指定的普通 Space，summon 时移到当前 Space。更符合 Spool 的原生 Space 模型，但该 Space 对用户仍可见，不是 i3/Hyprland 的隐藏 workspace。

不建议默认为应用 `hide`，因为 macOS 的 hide 是应用级，会连带隐藏同一进程的其他窗口。

## 明确不建议的 action 承诺

| action | 原因 |
| --- | --- |
| `window topmost enable` | 无 SIP/Dock 注入时，对外部应用窗口无法提供稳定、持久、跨 app 的最高层级保证 |
| `window sticky enable` | 把任意外部窗口持续映射到所有原生 Space 涉及私有 WindowServer 能力，且全屏 Space 语义冲突 |
| `window lower` 的强保证 | AX raise 本身需要激活 app；对跨应用精确 lower/Z-order 不可靠 |
| “真正隐藏”的 scratchpad | macOS 没有 i3 scratchpad 或 Hyprland special workspace 对等物；只能用最小化或普通 Space 模拟 |

可以提供 `raise`、Space-local tier 切换和 stash 的 best-effort 语义，但文档必须准确描述边界。

## 状态与一致性要求

1. 浮动窗口仍是 **tracked window**，只是退出 `LayoutStrip`；不得退回旧的 unmanaged 概念。
2. 每个普通原生 Space 独立维护：last-floating、稳定的 floating order、tier front 状态和可选 stash 恢复信息。`focus floating` 使用最近焦点记录进入 tier；`focus next/previous` 使用稳定顺序循环，避免每次聚焦更新 MRU 后在两个窗口之间来回跳。
3. 几何 action 只作用于浮动窗口；对平铺窗口应明确 no-op 或提示“先浮动”，不可写入随后会被 layout 覆盖的 frame。
4. maximize/place/nudge/stretch 都写 Desired frame，经现有 frame pipeline 应用和重试。
5. `floating disable` 应恢复到此前平铺位置；若位置失效，再按当前焦点邻接位置插入，而不是永远 append。
6. Space/display 移动继续以 WindowServer membership 为权威；follow focus 必须发生在 membership 确认之后。
7. 全屏窗口不参与普通 Space 的浮动 tier、overlay 或浮动导航。

## 推荐落地顺序

1. 幂等 `floating enable/disable/toggle` 和兼容别名；
2. `focus next/previous`、当前 tier 内的方向 focus，以及显式 `layer floating/tiled/toggle`；
3. `nudge`、`stretch`、`place`、可恢复 `maximize`；
4. focused-window 版本的 display/Space `follow|stay`；
5. 最后单独做 stash 原型并在真实 macOS 应用上验证，不与基础 action 同批承诺。
