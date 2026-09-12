# Resource CLI 与原生采集设计审查

日期：2026-09-13。状态：设计与实施计划已修订，R1–R7 全部通过文档复审，等待用户最终实施确认。以下 R1–R7 保留初审原文，行号指修订前版本；当前条款通过章节链接定位。关闭仅表示设计缺口已补齐，不代表代码或现场验证通过。

审查针对当前工作区中的 [CLI 实施计划](../CLI_IMPLEMENTATION_PLAN.md)、[原生采集设计](../NATIVE_INSPECTION.md) 及关联 ADR。三个子 agent 分别检查命令契约、原生采集边界、迁移与动作接受回执；主 agent 合并结果并对照当前源码复核。本报告描述尚未实现的方案中的缺口及其触发后果，不表示这些新命令已经存在或已发生现场故障。

共确认 7 项问题：1 项 P1、6 项 P2。P1 涉及升级后服务无法启动；P2 涉及结果可信度、超时证据或请求反馈契约，应在对应实现前补齐。本轮没有发现需要推翻资源式命令组织、独立原生采集或声明式状态驱动方向的问题。

| 编号 | 优先级 | 问题 | 主要后果 |
| --- | --- | --- | --- |
| R1 | P1 | 启动迁移没有落实到全部生成入口及已有安装产物 | 升级后二进制无法由旧 launch agent / launcher 启动 |
| R2 | P2 | `--show` 组与叶路径的完整性判定不一致 | 相同请求字段和读取结果可能返回不同退出码 |
| R3 | P2 | 过滤字段的必需证据来源未固定 | 来源冲突可能被提前过滤成完整空结果 |
| R4 | P2 | helper 增量协议缺少操作状态和字段粒度约束 | 超时丢失已读属性，无法区分正在读取和未开始 |
| R5 | P2 | 协议版本检查晚于请求解码 | 新 CLI 面对旧 daemon 可能只得到断连错误 |
| R6 | P2 | 接受回执未覆盖 Bevy 启动前的权限等待 | 退出请求或未就绪请求可能无明确回应 |
| R7 | P2 | 跨来源关联缺少生命周期和采样范围规则 | ID 复用可能把不同窗口的证据关联在一起 |

## R1 · P1：补齐 Nix 入口和已安装产物的迁移路径

位置：[实施计划的迁移步骤](../CLI_IMPLEMENTATION_PLAN.md#implementation-sequence-and-files)，第 272–275 行。

计划要求更新生成的服务参数和 launcher 脚本，但没有明确列出两个独立的 Nix 生成入口，也没有定义已有安装产物如何迁移。[nix/darwin.nix](../../nix/darwin.nix) 第 38 行和 [nix/home.nix](../../nix/home.nix) 第 43 行都只设置 `Program = lib.getExe cfg.finalPackage`，不经过 Rust 服务配置生成器。仅修改 Rust 生成器不会迁移它们。

现有 Rust 生成的 plist 同样没有子命令，见 [service.rs](../../src/platform/service.rs) 第 270–283 行。该文件第 111–116 行在检测到已有安装时直接返回成功，第 172–179 行的 `start()` 也不会刷新已有 plist。已生成的图形启动器使用 `exec ... start`，见 [app_launcher.rs](../../src/platform/app_launcher.rs) 第 157–161 行。

触发场景是更新二进制、保留旧的启动配置。新 CLI 的裸命令显示帮助、旧 `start` 被删除后，这些启动路径将显示帮助或参数错误，无法启动服务。更新生成函数只能保证以后新生成的产物正确。

建议把两个 Nix 模块、已有 launch agent、已有 launcher 分别列入迁移矩阵，明确检测、提示及用户执行的更新步骤。可以要求显式重新安装或提供受控迁移机制；当前设计阶段不执行安装、替换或重启。

验收应覆盖旧 plist / launcher fixture，以及两种 Nix 模块输出的完整启动参数。现有 [nix/checks.nix](../../nix/checks.nix) 第 96–107 行检查多项 plist 属性，但没有检查启动参数；仅运行 Rust 测试不足以发现两个 Nix 入口的遗漏。

## R2 · P2：按展开后的字段集合统一完整性判定

位置：[实施计划的结果契约](../CLI_IMPLEMENTATION_PLAN.md#result-and-timeout-contracts)，第 187–191 行；关联第 133–137 行及 [详情选择契约](../NATIVE_INSPECTION.md#confirmed-detail-selection-model)。

计划允许 `--show ax` 枚举时某个 advertised 属性返回 unsupported，仍被视为完整记录；同时要求 specifically requested value 无法建立时标为不完整。另一方面，组选择展开为整个字段集合，组与叶路径组合需要去重。

例如窗口 advertised 属性中包含 `AXTitle`，读取它返回 unsupported。`--show ax` 与 `--show ax,ax.AXTitle` 展开、去重后采集相同字段，但按上述区别解释，退出码可能分别为 `0` 和 `3`。完整性因此取决于选择器的写法，无法仅由请求内容和读取结果解释。

建议统一依据展开后的请求字段集合及原生读取结果分类。明确区分确定不存在／不适用、读取失败、超时和无法表示；组与叶路径遵守同一套规则。这里不要求把所有 unsupported 一律判失败，要求的是相同字段请求不能因选择器拼法而改变结果。

验收应比较组选择与「同一组加其成员叶路径」的完整性和退出码，并分别覆盖 unsupported、超时及表示截断。

## R3 · P2：先定义过滤依赖，再评估候选

位置：[实施计划的过滤规则](../CLI_IMPLEMENTATION_PLAN.md#proposed-exact-filtering-and-unresolved-candidates)，第 153–167 行。

当前规则只规定多个来源被 consulted 时如何处理一致与冲突，没有固定各过滤字段必须读取的来源。与 “only enough evidence” 的优化规则组合后，提前判定和继续采集都可能被实现者认为符合文档。

例如 `window list --source native --title Report` 遇到 CG 标题 `Untitled`、AX 标题 `Report`。先读取 CG 并提前判 false 的路径会排除窗口；继续读取 AX 的路径则应保留 unresolved 候选。相同命令可能由于采集顺序或优化方式返回完整空列表或部分结果。

建议为每个过滤字段建立依赖表，规定必需来源、来源适用性、缺失结果及多个来源的合并规则，在判定候选前固定依赖集合。文档已对 `on-screen` 和 Space/display 关系作了部分规定，需要补齐 title/name 等字段。短路优化只能建立在这些预先确定的依赖和三值逻辑之上。

验收应改变来源返回顺序、模拟一方不可用、同时提供冲突标题，确认 matched / unresolved 和整体状态不变。文档尚未明确 list 支持 `--show`，因此本问题不依赖这一未确定的参数组合。

## R4 · P2：helper 需要字段级增量结果和可恢复的操作状态

位置：[实施计划的 helper 边界](../CLI_IMPLEMENTATION_PLAN.md#proposed-enforceable-native-collection-boundary)，第 213–226 行；关联 [已确认的超时要求](../NATIVE_INSPECTION.md#confirmed-scope)，第 24 行。

方案只规定流式发送 completed evidence records，但父进程被要求在终止 helper 后区分已完成、正在读取及预算耗尽前未开始的工作。记录粒度与开始状态没有定义，父进程未必具备生成这些结果的信息。

例如 `--show ax` 已枚举到 30 个属性，前 5 个成功、第 6 个阻塞。如果整份窗口详情完成后才发送，强制终止会丢失前 5 个值。即使逐属性发送结果，没有操作开始信息，父进程仍无法区分第 6 个正在读取和其余属性尚未开始。

建议协议及时发布对象清单和动态属性清单，并包含操作开始、证据结果、操作结束及整次采集结束标记，或具备等价信息的状态机。属性值读取与可写性读取应独立发布结果。父进程保留完整协议帧，并依据已知操作状态报告 interrupted / timed-out / skipped。清单枚举本身没有完成时，剩余工作量应保持未知，不能生成虚假的完整待办列表。

验收必须覆盖同一窗口部分属性完成后阻塞、开始消息后无结果、半帧输出及清单枚举未结束。已有 probe 的属性读取与属性名枚举本来就是独立调用，见 [main.m](../../examples/native_window_probe/main.m) 第 7–14、84–86 行；它直到第 158–163 行才整体序列化，不能直接充当保留部分结果的执行模型。

## R5 · P2：版本协商必须覆盖真正的旧 daemon

位置：[实施计划的 IPC 迁移](../CLI_IMPLEMENTATION_PLAN.md#accepted-action-semantics-and-ipc)，第 246–251 行。

计划要求扩展 typed requests、递增协议版本，并明确报告不匹配。当前 [local_ipc](../../crates/local_ipc/src/lib.rs) 第 400–403 行先解码包含 `Request` 的整个 `ClientFrame`，然后才检查版本。

新 CLI 若直接向现有 v5 daemon 发送旧端不认识的请求变体，旧端会在版本检查前解码失败并关闭连接。客户端可能只看到连接关闭，而不是文档承诺的版本不匹配。只调整新服务端的解码顺序无法改变仍在运行的旧服务端。

建议在计划中明确与请求 payload 分离的版本协商，以及新客户端识别 v5 daemon 的路径，例如使用旧端可解码、无副作用的兼容探测。方案必须在不自动升级或重启 daemon 的前提下给出可解释的结果。

验收使用固定 v5 peer / fixture，覆盖新 CLI → 旧 daemon、旧 CLI → 新 daemon，以及旧端未知的新请求。现有版本测试位于同一文件第 1466–1486 行，只改变版本号、仍发送已知的 `Query(State)`，不能证明新增请求的兼容反馈成立。

## R6 · P2：定义权限等待阶段的请求处理

位置：[实施计划的 admission 路径](../CLI_IMPLEMENTATION_PLAN.md#accepted-action-semantics-and-ipc)，第 231–243 行；实施步骤第 260–263 行。

计划把 checked admission 放入 daemon 的有序命令执行路径，但服务存在尚未创建 Bevy World 的可连接阶段。[main.rs](../../src/main.rs) 第 232–236 行先启动 IPC，再检查 Accessibility 权限并创建 Bevy；第 302–313 行的权限等待循环只特殊处理 `Exit` 和旧 `ActionRequested { Quit }`，其余事件被丢弃。

如果迁移只覆盖 ECS 命令路径，首次运行或启动时权限不足期间，新的 checked `service quit` 可能无法退出进程；依赖 ECS 的动作或 Spool 读取也可能只以超时结束，无法明确说明 daemon 尚未就绪。即便 quit 仍复用旧事件，也需要在此阶段交付接受回执。

建议把启动、等待授权、运行及退出阶段纳入请求契约。等待授权时，进程级 quit 应可接受并按退出协议返回；依赖 ECS 的请求应明确返回机器可读的 `not_ready` 等结果。无需为了接收请求而提前建立不完整的 World。

验收使用受控事件循环覆盖权限等待期间的 quit 回执、ECS 请求拒绝和就绪转换，不需要启动真实桌面 daemon。

## R7 · P2：跨来源关联保留身份依据与采样范围

位置：[原生证据关联规则](../NATIVE_INSPECTION.md#proposed-interface-and-supplementary-coverage)，第 61–65 行；[实施计划结果模型](../CLI_IMPLEMENTATION_PLAN.md#result-and-timeout-contracts)，第 183–195 行。

文档禁止单凭标题或几何证明身份，为无窗口 ID 的对象规定了 capture-local 引用，也提出来源／操作时间记录，但没有明确数字窗口 ID 在跨来源、跨采样时间关联时的充分性及身份变化后的处理。

待防护场景是采集先取得旧窗口的 CG 证据，随后取得相同 ID 的新 AX 对象。若只按数字 ID 归并，会生成包含不同生命周期证据的详情，并影响 PID、标题等过滤。这是依据现有生命周期防护提出的竞态场景，本轮没有观察到真实桌面正在发生此问题。非原子采集是已声明的限制，关联仍需保留能够支持它的身份依据。

仓库已经在 [app.rs](../../src/manager/app.rs) 第 319–329 行用 `(window_id, incarnation)` 检查所属身份，在 [windows.rs](../../src/manager/windows.rs) 第 799–804 行检查重新解析 AX root 的 PID；[window_state_sync.rs](../../src/tests/window_state_sync.rs) 第 2105、2170 行包含 ID 复用及旧 AX 身份残留的回归测试。计划中的 incarnation 检查目前只明确覆盖动作 admission。

建议所有原始对象都保留 capture-local 引用，关联结果单独记录依据、来源样本／操作的采样区间及可信状态。至少核对可获得的 owner PID、窗口 ID、AX 对象身份；出现身份变化、复用或歧义时保留分开的证据记录，不能作为已确认的同一对象参与过滤。同一 PID、同一 ID 也不能凭空提供生命周期保证；没有原生生命周期令牌时应明确证据边界。

验收覆盖相同 ID 但 owner PID 不同、同一 PID 下 AX 身份变化，以及采样间窗口消失。允许返回关联未决或部分结果，不应输出确定但错误的关联对象。

## 审查边界与下一步

建议先修订设计与实施计划，补齐上述契约和验收场景，再提交最终代码实施确认。现有 `service restart` 的 CLI 本地执行与 Lua IPC 执行可以合理共存，不列为主问题；复用平台读取函数时的副作用与错误丢弃风险已被现有独立采集原则覆盖，未重复计入发现。

本轮只新增本报告及文档索引链接，未修改运行代码、实现测试或原设计条款；未运行原生采集、构建、服务安装或 daemon 重启。源码与测试仅用于只读核对，不构成新实现或现场行为验证。

## 修订与复审结果

用户同意先修订文档再复审后，主 agent 更新了设计和实施计划；三个新的子 agent 分别复审 R2/R3、R4/R7、R1/R5/R6。复审提出的剩余问题是旧 plist 归属无法仅凭名称判定，补充了旧模板识别、新产物归属记录、未知配置恢复步骤和重复 stop 的完成态后，迁移子 agent 再次复核通过。

| 初审项 | 当前契约 | 文档复审结论 |
| --- | --- | --- |
| R1 | [启动产物与归属](../CLI_IMPLEMENTATION_PLAN.md#startup-artifact-migration-and-ownership)：分别迁移两种 Nix、Rust 服务、旧 launcher；旧模板识别、归属记录、未知配置恢复及停止状态有明确规则 | 关闭；补充修订后再次通过 |
| R2 | [选择](../CLI_IMPLEMENTATION_PLAN.md#proposed-default-views-and-groups)与[结果分类](../CLI_IMPLEMENTATION_PLAN.md#result-and-timeout-contracts)：先展开去重，同一字段不因选择器拼法改变完整性 | 关闭 |
| R3 | [过滤依赖表](../CLI_IMPLEMENTATION_PLAN.md#native-filter-dependency-table)：固定来源、库存适用性、三值逻辑和全局覆盖规则 | 关闭 |
| R4 | [增量 helper 协议](../CLI_IMPLEMENTATION_PLAN.md#incremental-helper-protocol)：字段级结果、操作生命周期、未知清单余量、半帧和结束标记丢失有明确处理 | 关闭 |
| R5 | [版本协商](../CLI_IMPLEMENTATION_PLAN.md#version-negotiation-and-v5-compatibility-rejection)：冻结旧端可解码的 sentinel，同连接协商且不分发查询 | 关闭 |
| R6 | [启动与退出请求](../CLI_IMPLEMENTATION_PLAN.md#startup-and-shutdown-request-handling)：权限等待仍可 quit，ECS 请求明确拒绝，接受回执与兼容 Ack 分离 | 关闭 |
| R7 | [身份与采样](../CLI_IMPLEMENTATION_PLAN.md#native-identity-and-sampling-correlation)：原始记录独立、PID/ID/AX 身份校验、读后复核和关联歧义均有契约 | 关闭 |

实施计划的[验收矩阵](../CLI_IMPLEMENTATION_PLAN.md#verification-and-acceptance)逐项对应 R1–R7；[最终确认清单](../CLI_IMPLEMENTATION_PLAN.md#final-approval-checkpoint)给出本次代码实施范围。原有资源组织、独立采集和声明式架构方向保持一致。

本次修订阶段只修改设计、实施计划和本审查记录。完成了文档链接与空白格式检查；未编写或运行实现测试，未修改运行代码，未执行原生采集、安装、激活 Nix 配置或重启 daemon。后续仍需用户明确批准才能开始实现，文档关闭不能替代实现后的验收。
