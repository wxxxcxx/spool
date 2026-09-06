# macOS 窗口控制路线：AX、私有接口与 Dock 注入

核查日期：2026-09-05。主体核查 yabai 官方项目 Wiki 与本次获取的 `master` 源码；不是固定发布版或固定提交快照。Wiki 显示最后编辑日期为 2026-04-18。补充主任务提供的 Spool 源码结论与 Apple 分发规则边界。未安装、注入或进行任何运行时验证；不涉及 Apple AX 调研或后端选型建议。[1][5]

## 两种机制不能混同

- **不注入的私有 API 调用**：`src/space.c` 在 yabai 进程内通过 `g_connection` 调用 `SLSCopyManagedDisplayForSpace`、`SLSCopyWindowsWithOptionsAndTags` 等，查询 Space、显示器与窗口关系；这条调用路径没有经过 Dock payload。收益是获得额外拓扑/窗口信息。不能仅凭“使用私有 API”就断言必须关闭 SIP，也不能据此推导可任意修改其他应用窗口。[2]
- **Dock 注入**：官方 Wiki 明确将 scripting addition 注入 `Dock.app` 与部分关闭 SIP 联系起来，以获得原本限于 Dock 的控制能力。收益包括移动/交换/创建/销毁 Space、移除阴影、透明度、动画、scratchpad、窗口层级、sticky 和画中画。该清单是文档声明，不是本机兼容性测试结果。[1]
- **不是通用的无 AX resize 后端**：payload 的 `do_window_move` 调用 `SLSMoveWindowWithGroup`，`do_window_scale` 使用窗口变换；但 `window_manager_resize_window` 和 `window_manager_set_window_frame` 仍写入 `kAXSizeAttribute`，后者也写入 `kAXPositionAttribute`。因此，存在非 AX 移动/视觉缩放能力，不等于所有真实尺寸调整都绕过 AX，更不证明可以突破应用的尺寸约束。[3][4]

## 风险与证据边界

1. **安全保护减弱**：Wiki 要求按处理器/系统版本放宽文件系统、调试等保护；Apple Silicon 还涉及 NVRAM 保护和非 Apple 签名 arm64e 二进制要求。这不是普通辅助功能授权的等价替代；本文不提供关闭保护的命令。[1]
2. **版本适配成本**：payload 包含架构分支、系统版本判断和二进制特征扫描 `hex_find_seq`。据此推断，系统/Dock 更新可能使内部定位或调用失效；版本判断存在不等于该版本已完整验证。[3]
3. **故障影响范围**：代码进入 Dock 进程，推断其缺陷可能影响 Dock/Space 相关行为；非注入路径不具有这一相同的注入故障面，但私有接口仍存在兼容性风险。这里没有测得崩溃率、性能收益或跨版本可靠性。[1][2][3]

## 补充边界（2026-09-05）

- **私有 API 不等于关闭 SIP**：据主任务本日源码核查，Spool 的 `src/manager.rs:535-568` 通过 `bridged_window_move_class()` 动态检测能力，并使用 `performWithWMBridgeDelegate` 移动其他进程的窗口到 Space；不能将其一概描述为需要关闭 SIP。`src/manager/skylight.rs:39-44` 的 `SLSMoveWindowsToManagedSpace` 仅用于自身 overlay。此处转录主任务结论，未独立复核或运行测试，也不保证所有系统版本都可用。
- **Mac App Store 与 Developer ID 分开讨论**：Apple 审核指南 2.5.1 要求仅使用公开 API，2.4.5(i) 要求 Mac App Store 应用适当沙盒化。使用私有 API 因而不符合前述公开 API 要求；不能由这些商店规则推导“无法进行 Developer ID 签名”，本文也未核查商店外签名或公证结果。[5]

## AX 限制与 Spool 选型

Apple SDK `AXUIElement.h` 的 `AXUIElementIsAttributeSettable`、`AXUIElementSetAttributeValue` 文档列出属性不支持、元素失效、消息失败/超时及目标进程未完整实现辅助功能 API 等错误。可写性检测不等于任意目标尺寸都能执行成功。[6]

Spool 的真实位置/尺寸操作在 `src/manager/windows.rs` 的 `set_ax_position`、`set_ax_size`；应用级消息超时在 `src/manager/app.rs:36` 设为 0.25 秒，并在第 221 行应用。这是消息等待限制，不是整帧性能保证。本次已直接复核上述源码，以及 `src/manager.rs:113` 的动态类/方法检测和第 535-568 行的 Space 操作；符号存在不能当作操作成功的证明。

以下为工程建议，不是性能测试结论：

- 保留 AX 作为应用语义和真实 resize 路径，以 CoreGraphics/私有查询补充观测；私有操作按能力启用，操作后核实实际结果。
- Dock 注入可作为明确接受安全与版本成本的可选增强，不应仅为了普通平铺而成为默认强制依赖。
- 区分 desired 与 observed 状态，合并过期几何请求，采用有界重试；无法正常 resize 的窗口保留浮动及规则覆盖。
- 合成变换改变显示结果，不等于应用内容按新尺寸重新布局。不能将动画更流畅推导为 AX 已被全面替代。[3][4]

## 来源

- [1] [Disabling System Integrity Protection](https://github.com/asmvik/yabai/wiki/Disabling-System-Integrity-Protection)，Wiki 最后编辑 2026-04-18，本次核查 2026-09-05。
- [2] [src/space.c](https://github.com/asmvik/yabai/blob/master/src/space.c)，重点：`space_display_id`、`space_window_list_for_connection`；本次核查 2026-09-05。
- [3] [src/osax/payload.m](https://github.com/asmvik/yabai/blob/master/src/osax/payload.m)，重点：`verify_os_version`、`hex_find_seq`、`do_window_move`、`do_window_scale`；本次核查 2026-09-05。
- [4] [src/window_manager.c](https://github.com/asmvik/yabai/blob/master/src/window_manager.c)，重点：`window_manager_resize_window`、`window_manager_set_window_frame`；本次核查 2026-09-05。
- [5] [Apple App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/)，重点：2.5.1、2.4.5(i)；本次核查 2026-09-05。
- [6] 本机 Apple SDK：`/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk/System/Library/Frameworks/ApplicationServices.framework/Frameworks/HIServices.framework/Headers/AXUIElement.h`，第 187-248 行，本次直接核对。
