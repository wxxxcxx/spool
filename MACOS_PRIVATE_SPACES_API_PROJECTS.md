# macOS 私有 Spaces API 上游项目评估

> 调研日期：2026-08-28  
> 目标：寻找能替 Spool 持续维护 macOS 私有 Spaces 兼容性的第三方项目。  
> 约束：不向 Dock 注入代码、不关闭 SIP；需要枚举原生 Spaces、取得当前
> Space、查询窗口归属、把指定窗口移动到指定普通 Desktop Space。  
> 方法：只使用项目仓库、源码、提交、发行记录和项目 issue 等一手资料。

## 结论

截至 2026-08-28，**没有一个现成项目同时满足**以下全部条件：

- Rust/C ABI 可直接依赖；
- 有稳定版本和明确的 macOS 兼容策略；
- 覆盖按显示器枚举/当前 Space、窗口归属和按窗口 ID 移动；
- 在 SIP 开启、不注入 Dock 时工作；
- 上游明确承担私有 API 失效后的兼容维护。

最接近的是 **WindowKit**。它是目前候选中唯一真正发布为库、同时完整公开
Spool 所需四项 API 的项目，而且维护者正是首先在 DockDoor 中落地 Tahoe
bridged operation 的开发者。它的主要缺点是 Swift-only、只允许跟踪 `main`
分支、没有 tag/release，也没有为私有 Spaces 路径提供可见的系统集成测试。

如果接受外部进程，**mimi** 是第二选择：有版本发行、维护活跃，并且确实在跟进
macOS 27 的 Space 行为变化。但它的 CLI 只能移动“当前最前窗口”，不能按
Spool 给定的 window ID 执行操作，因此只能做可选后端，不能完整替代 Spool 的
平台层。

建议排序：

1. **WindowKit：条件采用候选**。先推动上游提供 tag 和窄 C ABI；成功后才能
   真正把兼容性责任交给上游。
2. **mimi：外部 CLI 备选**。适合用户快捷键触发的前台窗口移动，不适合 ECS
   对任意窗口进行确定性控制。
3. **yabai：兼容性 oracle，不作为依赖**。其版本适配最成熟，但它是另一个完整
   窗口管理器，不是库；与 Spool 同时运行会重叠窗口/事件所有权。
4. **tmc/apple：绑定库观察候选**。有版本标签和自动生成的 Go SkyLight 绑定，
   但高层 API 只有当前 Space 与窗口归属，移动仍需消费者自己组装低层 ObjC 调用。
5. **spacekit：实验和回归参考**。代码活跃且验证了 Tahoe 26.5，但私有实现位于
   Go `internal` 包，项目没有发行版本，也没有通用单窗口移动接口。
6. **DockDoor：只采用其抽取出的 WindowKit**。应用源码是 GPL-3.0，不应复制
   回 Spool；其可复用部分已经单独以 MIT 发布为 WindowKit。
7. **Hammerspoon `hs.spaces`：当前不合格**。枚举能力可用，但移动实现没有采用
   Tahoe bridged operation，Sequoia 失效 issue 仍开启。
8. **CGSInternal：历史头文件参考，不是维护项目**。最后提交停在 2016 年。

## 能力与可采用性总表

| 项目 | 形态 / 语言 | 许可证 | 维护证据 | 枚举 / 当前 | 窗口归属 | 窗口移动（SIP on） | Spool 可采用性 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| [WindowKit](https://github.com/ejbills/WindowKit) | Swift Package / Swift | MIT | 2026-08-25 仍提交；139 commits | 是 / 是 | 是 | 是，Tahoe bridged selector | **条件采用**；需 Swift-C-Rust 桥和版本 pin |
| [mimi](https://github.com/y3owk1n/mimi) | CLI/daemon / Go + ObjC | MIT | v0.10.1，2026-08-16；当天修复 macOS 27 | 内部有 / 有 | 内部有 | 是，但 CLI 仅最前窗口 | **可选外部后端**，不满足任意 window ID |
| [yabai](https://github.com/asmvik/yabai) | 完整 WM / C + ObjC | MIT | v7.1.25；2026-06 继续适配 26.6 | 是 / 是 | 是 | 26.4+ 是 | **参考/测试 oracle**，不是库且与 Spool 冲突 |
| [tmc/apple](https://github.com/tmc/apple) | 绑定库 / Go | MIT | v0.6.18，2026-08-21 | 高层否 / 是 | 是 | 低层 class binding 有；无高层封装 | **观察候选**；Go 且不承诺私有 API 兼容 |
| [spacekit](https://github.com/cehbz/spacekit) | 两个 CLI / Go + ObjC | MIT | 2026-07-09；无 tag | 内部有 / `status` | 内部有 | restore 内部使用，26.5 验证 | **参考**；`internal` 不可被外部 Go 模块导入 |
| [DockDoor](https://github.com/ejbills/DockDoor) | 应用 / Swift | GPL-3.0 | 1.39.5.1，2026-08-01；主干 2026-08-10 | 是 / 是 | 是 | 是 | **不要复制应用源码**；用 WindowKit |
| [Hammerspoon hs.spaces](https://github.com/Hammerspoon/hammerspoon/tree/master/extensions/spaces) | 应用扩展 / ObjC + Lua | MIT | Hammerspoon 1.1.1（2026-02）；Spaces 写路径最后修改 2024-08 | 是 / 是 | 是 | 当前实现失效 | **不采用** |
| [CGSInternal](https://github.com/NUIKit/CGSInternal) | C 头文件集合 | 文件头标注 MIT | 最后提交 2016-03 | 仅声明 | 仅声明 | 旧声明 | **只作历史词典** |

“维护活跃”只表示仓库仍有提交，不能自动证明每个 macOS 版本都执行过真实
WindowServer 集成测试。私有 API 的关键证据仍应是具体版本上的 issue、修复提交
和发行记录。

## 1. WindowKit：最接近可依赖库

仓库将自己定义为 macOS window discovery/tracking/manipulation Swift package，
要求 macOS 14+、Swift 5.9+，并直接警告私有 API 可能在任意系统更新中失效：
[README](https://github.com/ejbills/WindowKit/blob/main/README.md)、
[Package.swift](https://github.com/ejbills/WindowKit/blob/main/Package.swift)。许可证是
[MIT](https://github.com/ejbills/WindowKit/blob/main/LICENSE)，`NOTICE` 还记录了从
AltTab 衍生部分已获得原作者重新许可：
[NOTICE](https://github.com/ejbills/WindowKit/blob/main/NOTICE)。

它公开的 `WindowSpaces` API 正好覆盖：

- `managedDisplays()`：按显示器读取 Spaces 和 `currentSpaceID`；
- `currentManagedSpaceID()`：按鼠标所在显示器选择当前 Space；
- `spaces(forWindowID:)`：查询窗口属于哪些 Spaces；
- `move(windowID:toManagedSpace:)` / 批量版本：按 CGWindowID 移动。

源码见
[`SkyLightSpace.swift`](https://github.com/ejbills/WindowKit/blob/main/Sources/WindowKit/SystemBridge/SkyLightSpace.swift)。
读取侧用 `dlopen`/`dlsym` 获取 `SLSCopyManagedDisplaySpaces`，同时兼容字典中的
`ManagedSpaceID` 与 `id64`。移动侧不解析 SkyLight Mach-O，而是运行时检查
`SLSBridgedMoveWindowsToManagedSpaceOperation`、`initWithWindows:spaceID:` 和
`performWithWMBridgeDelegate`，然后异步提交。这条路径就是 2026-05 在 Tahoe
26.4/26.4.1 被验证为 SIP-on 可用的机制；验证记录见
[yabai #2788](https://github.com/asmvik/yabai/issues/2788)。

维护方面，Space 移动于
[2026-05-08 commit](https://github.com/ejbills/WindowKit/commit/5c7a107)
加入，此后增加了 active-display 选择、多屏 remap；仓库到
[2026-08-25 commit](https://github.com/ejbills/WindowKit/commit/2265e93b6b491220123a8798e620057788f2d13a)
仍在扩展 bridged Space 操作。

采用风险：

- 仓库没有 tag/release，README 明确示例依赖 `branch: "main"`；生产依赖只能 pin
  commit，无法获得语义版本兼容承诺。
- Spool 是 Rust；Swift Package 不能像 Cargo crate 一样直接消费。至少需要一个
  很窄的 Swift/C 导出层，再从 Rust FFI 调用。
- 公开 API 比 Spool 所需范围更大（窗口跟踪、预览、Dock AX 等）。直接链接整个
  包会扩大依赖和权限面；最好让上游拆出 `WindowSpacesKit` target 或 C ABI target。
- `Tests/` 中未发现针对 `WindowSpaces` / bridged move 的系统集成测试；API 会在
  类或 selector 缺失时明确报错，但“提交成功”仍不代表窗口已完成异步迁移。
- 它不提供切换普通 Space，也不提供 Mission Control 中普通 Space 的创建/删除。
  `WindowStash` 的 create/destroy 是隐藏的 unmanaged Space，不是用户 Desktop。

判断：**唯一值得进入技术验证阶段的库，但还不能直接称为稳定上游依赖。**

## 2. mimi：维护最积极的外部工具候选

[`mimi`](https://github.com/y3owk1n/mimi) 是 Go + Objective-C 的 CLI/daemon，
macOS 14+，MIT 许可证；当前
[v0.10.1](https://github.com/y3owk1n/mimi/releases/tag/v0.10.1) 发布于
2026-08-16。其提交历史显示项目会主动跟进平台变化，例如
[macOS 27 Space switching 修复](https://github.com/y3owk1n/mimi/commit/8443800)。

私有实现位于
[`internal/native/space.m`](https://github.com/y3owk1n/mimi/blob/main/internal/native/space.m)：
读取 `SLSCopyManagedDisplaySpaces` / `SLSManagedDisplayGetCurrentSpace`，移动时先从
SkyLight Mach-O 解析非导出 bridged perform 函数，再回退到
`SLSMoveWindowsToManagedSpace`。这表示它确实自己承担了一部分符号和系统版本
兼容工作，而不是只放一份静态声明。

但可消费接口不足：

- [`mimi action move_window_to_space`](https://github.com/y3owk1n/mimi/blob/main/docs/CLI.md)
  只接受目标编号/next/prev，并固定移动当前最前窗口；不接受 CGWindowID。
- Native 包位于 Go 的 `internal/`，不能被 Spool 或独立 helper 作为 Go module 导入。
- 作为子进程执行会重新解析焦点窗口，命令发出前后的焦点竞争可能移动错窗口；
  也无法批量移动 associated windows。
- 当前 Mach-O parser 依赖一个 C++ mangled 私有符号，维护复杂度比 WindowKit 的
  selector 路径更高。

判断：如果 Spool 的契约严格限定为“用户按键时移动此刻最前窗口”，可以把它作为
可选外部依赖；对于 ECS 持有的任意窗口实体，不够用。向 mimi 上游提出
`--window-id` 和机器可读 Spaces query，会比复制它的 Objective-C 源码更符合
“不自己维护”的目标。

## 3. yabai：最强维护记录，但不是库

[`yabai`](https://github.com/asmvik/yabai) 是完整窗口管理器而不是 SDK。它的
[MIT 许可证](https://github.com/asmvik/yabai/blob/master/LICENSE.txt)允许复用，
但复制源码意味着 Spool 重新承担维护，不能实现本次目标。

它是最佳兼容性 oracle：

- `SLSCopyManagedDisplaySpaces`、`SLSCopySpacesForWindows` 和按版本选择的移动路径
  集中在
  [`space_manager.c`](https://github.com/asmvik/yabai/blob/master/src/space_manager.c)；
- [v7.1.25 changelog](https://github.com/asmvik/yabai/blob/master/CHANGELOG.md)
  明确记录 2026-05-08 恢复 SIP-on 窗口跨 Space 移动；
- 主干到
  [2026-06-14 commit](https://github.com/asmvik/yabai/commit/dd845723416f5fe92af49fad5ebab00369e07edd)
  仍在为 macOS 26.6 更新 scripting-addition pattern。

在本次约束内，只能使用其无需 scripting addition 的查询和 Tahoe bridged move。
创建/删除/重排 Space 等能力仍属于需要部分关闭 SIP 的功能，仓库 README 也明确
说明 scripting addition 会注入 Dock：
[README](https://github.com/asmvik/yabai/blob/master/README.md)。

不能把 yabai 当共享库链接；其命令行是给正在运行的 yabai daemon 发消息。让
yabai 与 Spool 同时作为窗口管理器运行，会重复监听 AX/WindowServer 事件并同时
改变窗口状态。判断：**跟踪其 release/commit 作为兼容测试基准，不作为运行时依赖。**

## 4. tmc/apple：正规的 Go 绑定，但不是 Spaces 兼容层

[`tmc/apple`](https://github.com/tmc/apple) 是 MIT、带 tag 的 Go Apple framework
bindings，当前 [v0.6.18](https://github.com/tmc/apple/releases/tag/v0.6.18) 对应
2026-08-21 提交。它使用 `purego` 在运行时加载函数，不依赖 cgo；仓库包含自动
生成的 [`private/skylight`](https://github.com/tmc/apple/tree/main/private/skylight)
以及手写高层 [`x/skylight`](https://github.com/tmc/apple/tree/main/x/skylight)。

这是候选中最像“持续更新的通用私有 framework bindings”的项目。低层包已经生成
`SLSBridgedCopyManagedDisplaySpacesOperation`、
`SLSBridgedCopySpacesForWindowsOperation`、
`SLSBridgedMoveWindowsToManagedSpaceOperation` 和
`performWithWMBridgeDelegate` 的 ObjC wrapper；移动类见
[`sls_bridged_move_windows_to_managed_space_operation.gen.go`](https://github.com/tmc/apple/blob/main/private/skylight/sls_bridged_move_windows_to_managed_space_operation.gen.go)。

但它没有替 Spool 完成最后一层兼容封装：

- 高层 `x/skylight` 当前只公开 `ActiveSpace()` 和 `SpacesForWindow()`，没有按
  显示器解析 managed display dictionary，也没有 `MoveWindowToSpace()`；见
  [`x/skylight/skylight.go`](https://github.com/tmc/apple/blob/main/x/skylight/skylight.go)。
- 低层 generated wrapper 用 `SendIfResponds` 避免 selector 不存在时直接崩溃，
  但消费者仍需构造 Foundation 数组、提交 operation、验证异步结果和决定版本回退。
- 仓库 README 对 `private/` 明确写着“可能改变或消失，且不提供兼容保证”；它在
  维护绑定覆盖面，不承诺每个私有 API 的行为兼容。
- 它是 Go 库。Spool 若采用，需要额外 Go helper/C shared library，这会引入 Go
  runtime 和另一层 ABI；直接从 Rust 重写调用则又回到自行维护。

优点是它确实有真实 WindowServer 测试：`x/skylight/spaces_test.go` 用系统窗口
验证 `ActiveSpace` 与 `SpacesForWindow` 的解码一致性。判断：**值得持续关注或推动
上游加入高层 managed-display/move API，但当前不如 WindowKit 贴合 Spool。**

## 5. spacekit：有实测，但不可直接导入

[`cehbz/spacekit`](https://github.com/cehbz/spacekit) 是 MIT、Go + Objective-C
项目，包含 `spaceswitch` 和 `spacekeeper` 两个 CLI。它明确记录：读侧使用
`SLSCopyManagedDisplaySpaces` / `SLSCopySpacesForWindows`，restore 的写侧运行时
检查 `SLSBridgedMoveWindowsToManagedSpaceOperation`，并在 macOS 26.5 实测；见
[README](https://github.com/cehbz/spacekit/blob/main/README.md) 和
[`internal/skylight`](https://github.com/cehbz/spacekit/tree/main/internal/skylight)。

项目 2026-06/07 快速开发，最新提交是
[2026-07-09](https://github.com/cehbz/spacekit/commit/d53132a8ac11dad8f15c7161aac9c6484b7adb70)，
但没有 tag/release。其 SkyLight 代码在 Go `internal` 包，外部模块不能导入；
CLI 面向切换和整套布局 save/restore，没有“按给定 window ID 移动一次”的稳定
命令。判断：适合补充测试用例和行为证据，不是可依赖 API。

## 6. DockDoor：应用实现已经抽取为 WindowKit

[`DockDoor`](https://github.com/ejbills/DockDoor) 是活跃 Swift 应用，当前 tag
[1.39.5.1](https://github.com/ejbills/DockDoor/releases/tag/1.39.5.1)，主干到
[2026-08-10](https://github.com/ejbills/DockDoor/commit/f80d0576047b81005edd6284b8718a10fa9f39f5)
仍有提交。它在
[`PrivateApis.swift`](https://github.com/ejbills/DockDoor/blob/main/DockDoor/Utilities/PrivateApis.swift)
通过 operation class + selector 完成 SIP-on 移动。

但 DockDoor 是
[GPL-3.0](https://github.com/ejbills/DockDoor/blob/main/LICENSE) 应用，不提供库
product。直接抽取这份代码会带来许可证和同步维护成本。同一维护者已经把窗口和
Spaces 能力整理成 MIT Swift Package `WindowKit`，因此 DockDoor 的合理采用
路径就是依赖 WindowKit，而不是复制 `PrivateApis.swift`。

## 7. Hammerspoon `hs.spaces`：读取可用，写入已落后

Hammerspoon 项目本身仍活跃，MIT，最新 release 是
[1.1.1](https://github.com/Hammerspoon/hammerspoon/releases/tag/1.1.1)。`hs.spaces`
也提供 `allSpaces`、`activeSpaceOnScreen`、`windowSpaces`、`windowsForSpace` 和
`moveWindowToSpace`，但模块文档明确标记为 experimental，并说明混用私有 API
和 Dock Accessibility hacks：
[`spaces.lua`](https://github.com/Hammerspoon/hammerspoon/blob/master/extensions/spaces/spaces.lua)。

关键问题是写路径最后一次实质更新为
[2024-08-05 Sonoma workaround](https://github.com/Hammerspoon/hammerspoon/commit/63c8239c09ca6d32b0582001c9508b49cce859df)。
当前源码仍使用 compat-ID / `SLSMoveWindowsToManagedSpace`，没有 Tahoe bridged
operation：
[`libspaces.m`](https://github.com/Hammerspoon/hammerspoon/blob/master/extensions/spaces/libspaces.m)。
[issue #3698](https://github.com/Hammerspoon/hammerspoon/issues/3698) 记录 Sequoia
上函数返回 true 但窗口不移动，截至调研时仍开启。

判断：可以把 Hammerspoon 当外部读取/自动化工具，但不能把它作为当前
窗口-to-Space 写后端；运行整个 Hammerspoon 也不是 Rust 库集成方案。

## 8. CGSInternal：陈旧声明集合

[`NUIKit/CGSInternal`](https://github.com/NUIKit/CGSInternal) 只是一组 C headers，
`CGSSpace.h` 声明了旧 CGS 枚举、当前 Space、窗口归属和 add/remove window 等
接口：
[`CGSSpace.h`](https://github.com/NUIKit/CGSInternal/blob/master/CGSSpace.h)。文件头
标注 MIT，但仓库没有构建产物、运行时符号探测、版本选择、回退或测试，最后提交
停在
[2016-03-25](https://github.com/NUIKit/CGSInternal/commit/c4f6f559d624dc1cfc2bf24c8c19dbf653317fcf)。

其中 Space type 等定义也反映旧系统语义，不能直接作为 Tahoe 数据模型依据。
判断：仅用于查历史函数名和 query mask；它没有替任何消费者维护兼容性。

## 其他项目为何没有进入候选

- [AltTab PrivateApis.swift](https://github.com/lwouis/alt-tab-macos/blob/master/src/experimentations/PrivateApis.swift)
  是应用内部实验文件，源码注释直接称旧 move API unreliable；不是库，也没有把
  写入兼容性作为产品契约。
- [Powerspaces](https://github.com/sebastianpdw/powerspaces) 的 `SpaceKit` 是应用内
  target，重点是只读 Space membership 与按 Space 启动应用，没有任意窗口移动。
- [Control Room](https://github.com/dev-jonghoonpark/control-room) 使用
  `SLSCopyManagedDisplaySpaces` 和 per-Space window query 做预览，只覆盖读取。
- [SkyLight bridged headers](https://gist.github.com/stephancasas-openai/1b31a8d76c6a103e4676ac196e06e9d8)
  很适合核对 26.x operation class/selector，但它是头文件快照，不是兼容库。

## 推荐的下一步（仍不修改 Spool）

在采用任何实现前，先向 WindowKit 上游确认或提交需求：

1. 给 `WindowSpaces` 单独建立轻量 target，避免引入完整窗口预览/跟踪系统；
2. 发布 semver tag，不再要求依赖浮动 `main`；
3. 提供 C ABI：managed displays JSON/结构体、window spaces、move window；
4. 建立真实 macOS 14/15/26/27 测试矩阵，并明确每个版本移动能力是 supported、
   unsupported 还是 unverified；
5. 异步 move 提供 completion/可验证结果，至少明确 accepted 与 reconciled 的区别。

如果上游接受这些边界，Spool 只需维护一层很薄的 Rust FFI 和 ECS reconciliation，
私有 API 变化主要由 WindowKit 跟进。若上游不接受，当前生态里不存在“换一个依赖
就完全不用维护”的方案；最保守的产品策略仍是把私有写入保持为可选 capability，
而不是基础契约。
