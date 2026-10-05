# Roundtable v1 整改与循环复审

审查日期：2026-10-05。分支：`feat/roundtable-v1`。HEAD：`4946d8f6`。基线：`cb7596d38f9aa0df52740e939da7732ccfaaf5d8`。范围包含原分支的全部特性提交，以及当前未提交、未跟踪的整改源码与测试。

## 当前结论

**所有已确认问题已修复；最后独立复审在本次审查范围内没有未解决的可行动发现。** 原始 R01–R14 与循环追加 D01–D22 均已闭环。备份、订阅、四组圆桌集成回归，以及全部目标和桌面／服务器／MCP 生产配置 Clippy 检查通过，具体命令及证据见下表。

首次全库的两项失败已通过修正后的受影响模块回归验证；未再次完整运行全库，不将该历史结果写作最终全库全部通过。真实 Linux 隔离、付费 provider／adapter 兼容性和安装包运行仍未验证，不能据此启用功能或认定上线资格。

原始 14 项审查保留在 [原始报告](D:/MyCodeBuddy-roundtable/docs/reviews/2026-10-05-roundtable-v1-review.md)。本报告记录后续修复，避免把历史问题及旧验证结果误当成最终状态。所有改动仍未提交，执行开关保持关闭。

## 原始问题的整改

| 编号 | 修复及回归依据 |
| --- | --- |
| R01 | 修复测试接口、格式及定向前端 lint。统一 Rust 测试及 Clippy 的最终结果见下表。没有删除隔离断言或全局放宽警告。 |
| R02 | 18 个共享命令连接真实 SQLite、HTTP/Tauri、启动恢复、私有订阅和静态页面；生产执行器连接安装资格校验、OCI、私有 ACP、三工具 broker、固定来源模型网关。真实调度器使用受控执行器验证提交、接纳、发布、恢复和取消，不调用付费模型。 |
| R03 | 接纳读取并校验真正的 candidate，按照结果 scope 解析 claims、response、position 和 evidence；提交回执不再作为正文。 |
| R04 | close/publish 在事务内校验 room、current phase、boot/run、attempt/binding、policy/context fence 和冻结集合；控制及旧 epoch 不能覆盖新结果。 |
| R05 | 失败回滚只清理私有临时文件，不删除已安装或复用的内容哈希对象；覆盖重开及并发对象存储。 |
| R06 | 固定投影携带消息、证据和完整重放状态，并纳入哈希；冻结 closing 集合保留真实预期成员的缺席状态。 |
| R07 | 阶段终结按 turn 的当前 attempt/accepted 状态判定，排除旧重试；未启动成员在超时前不能使阶段提前关闭。 |
| R08 | launch/admission 上限按实际 turn 记账，同时保留房间总额与未来必要启动预算。 |
| R09 | 提交前预扣调用、参数及编码响应预算，预算不足不封存候选或写入业务结果。 |
| R10 | 已准入的参数错误也计入工具调用及响应额度；并发请求按 attempt 顺序结算。 |
| R11 | 普通会话发现前加载持久内部绑定，重启后继续过滤隐藏服务会话。 |
| R12 | 游标校验对象、房间、哈希和范围，拒绝非规范偏移；固定页面按实际字节数限额。 |
| R13 | 重同步恢复持久水位，预览与持久 seen 分开；重连验证不可变页与最终 H 哈希。 |
| R14 | 用量按 attempt/counter/epoch 保留增量累计与去重；迟到归档具有独立单调版本，不改业务投影。 |

## 循环复审追加问题

| 范围 | 修复 |
| --- | --- |
| D01–D09：初次产品集成复审 | 规范十进制水位；合法失败投影；订阅生命周期与容量；旧 epoch 失败隔离；克隆保留冻结源字节；最终综合拒绝 next-phase 输入；DTO 默认值规范化幂等；字节页上限；合法事件 cause。 |
| D10–D16：命令、恢复和协议复审 | 已完成请求在只读/关闭时重放原 ack；严格 usage DTO；WS 同 ID 替换与完成句柄清理；resume 持久化并验证新并发度；restart/retry 自动调度 successor；HTTP/WS 原始严格 JSON；丢失回复后的前端重试保持原 body/UUID。 |
| D17：异步控制错误 | 清理、advance 或 continuation 失败持久化 blocked 原因及新投影；事务 guard 防止旧或被 supersede 的操作污染新 epoch。实际异步失败回归已先观察到 `open` 而非 `blocked`。 |
| D18：清理持久化失败 | 保留诊断 capture 至事务提交；诊断插入和 attempt 引用一起回滚；历史启动记录用于重新定位，再执行实际 OS 清理证明，支持进程内和重开后重试。 |
| D19：启动 ack 丢失 | 从已持久化 intent 重建真实 OCI runtime/cgroup 身份；缺少 ack 不再生成无法回收的占位身份。 |
| D20：恢复保留已接纳成员 | 真实接纳保存 `valid`，恢复原先只识别 `accepted`；两个真实接纳后的存储回归分别复现 `InsufficientBudget` 与 `CannotReachQuorum`，真实调度器暂停／恢复也复现预算误拒。两个恢复消费者已兼容 `valid \| accepted`，保留弃权语义；三条回归最终全部通过，运行时断言首成员仅实际启动一次，存储断言保留 accepted ID／消息／attempt。最后交叉复审未发现其他同类持久状态读取遗漏。 |
| D21：旧声明归档丢失圆桌对象 | 后续全库验证及独立复审发现：归档不声明 managed sections，却带真实圆桌引用及正确对象时，stage 原先接受，apply 仅搬旧版三项并清理 staging，留下缺失对象。真实 SQLite／正确 blob 的回归已观察到有效失败；随后 Some(别名声明) 的共享校验和真实 Windows stage 两项回归也观察到失败。Windows 实际文件系统探测确认 `roundtable-objects::$INDEX_ALLOCATION/hash` 可写入并由规范路径读取。共享校验现要求规范 section 声明，覆盖大小写、尾点／空格及 NTFS 目录别名，保留合法旧版兼容；备份模块 67 项回归通过，包含正确声明后 stage→apply→校验恢复 DB 引用的正控。 |
| D22：慢请求阻塞网关关闭 | 最后只读复审发现：完整 HTTP headers 加半截请求体会使 Bytes extractor 一直等待；网关 graceful shutdown 等所有连接结束，而运行时先等待网关再终止沙箱，导致停止和回收等待环路。实际 TCP 回归先复现关闭超时，再通过：确认接受空闲、半截 headers、半截 body 三条连接并同步到 body 提取前，在客户端保持连接时取消，断言服务返回且全部客户端 EOF／reset。生产与测试共享启动函数；accepted IO 的持久取消 future 唤醒阻塞读写，保留 graceful join，不以 abort 主任务代替连接排空。独立 Unix 类型／lint 校验及交叉源码复审也通过。 |
| 模型网关生成和 effort 上限 | 单请求输出按预留 generation reserve 注入或校验；按实际出站 JSON 字节重新计量；ACP 必须确认实际 model/effort，显式 effort 每次请求保持一致。未显式选择 effort 时保留 adapter/provider 默认语义。 |
| 排队后重新准入 | 请求等待 transcript 锁期间，租约或策略可失效；取得锁后、发送前重新验证 revoked/expiry/ExecutionGate。真实 HTTP 回归阻塞首请求、确认第二请求确已排队，再令资格失效，断言只发送一次。 |
| 升级后的旧实例清理 | 安装信息严格解析和新业务资格验证分开；报告/核心哈希过期仍阻止新启动，但保留清理旧 owner 的固定 profile。清理验证 pinned runtime 哈希及真实 OS 所有权。配置缺失或无法解析仍保守阻塞，不合成证明。 |
| 关闭早期资源 | shutdown 同时枚举 quarantine、未回收启动日志和未确认 cleanup 的 attempt/binding；没有 intent 也不能跳过已登记资源。实际 SQLite 回归已先观察到错误释放锁。 |
| 冻结成员与历史缺席 | 以 PhaseSnapshot.members 为根集，补未创建 turn 的 Absent，拒绝快照外成员；历史读取使用同一预期成员集合。 |
| replacement 预算 | 新 revision 从剩余房间总额预留 replacement 和未来首次 waves 的最低时间；不增加房间余额，不因旧阶段已耗尽误拒绝。ready successor 的冻结 deadline 保留所分配 cap。 |
| 真实运行边界 | 持久预扣活动时间、预算耗尽后取消并回收；broker 关闭 abort/join handlers；流式生成消耗不退款且不确定远端工作保持；诊断跨 chunk 脱敏；资格绑定当前 provider/model/origin/OS/kernel/版本及核心源码哈希。 |

## 循环复审记录

| 阶段 | 结果与后续动作 |
| --- | --- |
| 原始分支审查 | 确认 R01–R14；沿原设计完成整改及产品接线。原始审查报告保留，不覆盖历史结论。 |
| 产品接线复审 | 追加 D01–D09，并用真实 SQLite、私有传输及不可变对象回归修复。 |
| 命令与恢复复审 | 追加 D10–D16，修复原 ack 重放、严格 DTO／JSON、并发参数、自动续跑和前端丢回复重试。 |
| 清理与运行边界复审 | 追加 D17–D19，以及早期资源关闭、冻结缺席成员、replacement 预算、网关额度／effort、排队后准入和过期安装清理问题；修复后再次交叉检查。 |
| 接纳状态交叉检查 | 发现 D20 的持久状态错配，先观察三个真实行为回归失败，再最小修复两个匹配点；三个回归及四个 integration targets 全部通过。两位其他范围的审查者再次核对此修复，无新增可行动残留。 |
| 全库验证后的追加检查 | 全库发现两处测试构造问题，纠正为真实 SQLite 和明确 JoinHandle 完成同步。复审再发现 D21 的未声明对象恢复丢失链，继续添加真实回归并修复。 |
| 最后备份与运行时交叉复审 | D21 的旧版清单与别名声明分别得到真实失败证据，追加实际 NTFS 别名探测；D22 的慢请求关闭得到真实 TCP 失败证据。修复后备份 67 项、订阅 8 项、集成 159 项全部通过（2 项 live 实验 ignored）。其他范围的审查者复核完整恢复引用链与 accepted IO 排空，无新增残留；全部目标及三种生产配置 Clippy exit 0，结束本轮整改。 |

网关额度新测试曾错误要求小数额度返回预算错误；严格 JSON 解析实际更早返回 `float_rejected`。最终测试逐项断言精确错误，保留零非法转发、默认输出额度注入和实际出站字节计量断言。诊断新测试也保留 EOF 部分秘密前缀脱敏断言；没有通过放宽安全要求解决测试失败。

## 验证

| 检查 | 结果 |
| --- | --- |
| 独立协议 crate `cargo test --locked` | 35 passed。 |
| 独立协议 crate `cargo clippy --locked --all-targets -- -D warnings` | 通过。 |
| 前端相关 7 个文件：`pnpm test src/components/roundtable src/lib/roundtable src/lib/transport/web-transport.test.ts` | 64 passed。 |
| 最终全量 `pnpm test` | 687 个文件通过，11,177 passed / 15 skipped。日志：[前端全量](D:/MyCodeBuddy-roundtable/src-tauri/target/frontend-final-test.log)（忽略的构建目录）。 |
| Roundtable 页面、组件、库、WebTransport、sidebar、spike 定向 ESLint | 通过。 |
| `pnpm build` | 通过；39 个静态页面，包含 `/roundtable`。 |
| 主 crate `cargo fmt --all --check` | D21／D22 最终冻结源码检查通过，exit 0。 |
| 主 crate 4 个 Roundtable integration targets | D21／D22 最终重跑 exit 0：IO 56 passed、runtime 62 passed / 2 ignored、service 17 passed、transport 24 passed；合计 159 passed / 2 ignored。包含 D22 真实连接关闭及 D20 保留已接纳成员的回归。日志：[集成回归](D:/MyCodeBuddy-roundtable/src-tauri/target/roundtable-final-integrations.log)。 |
| 主 crate `cargo clippy --locked --no-default-features --features test-utils --all-targets -- -D warnings` | D21／D22 最终冻结源码检查 exit 0；不放宽警告或删除断言。日志：[全部目标 Clippy](D:/MyCodeBuddy-roundtable/src-tauri/target/roundtable-final-clippy.log)。 |
| 主 crate 全库 `cargo test --locked --no-default-features --features test-utils --lib` | 首次完整运行：7,550 passed / 2 failed / 1 ignored，exit 101。两项失败为旧备份使用非 SQLite 假库、订阅测试用一次 yield 推定任务完成；已分别改为真实 SQLite 和明确 await 任务完成，保留安全断言。修正后受影响模块回归如下；没有把首次全库结果改写为全部通过。日志：[首次全库](D:/MyCodeBuddy-roundtable/src-tauri/target/roundtable-final-lib.log)。 |
| 最终备份库模块 `cargo test --locked --no-default-features --features test-utils --lib commands::backup` | 67 passed / 0 failed，exit 0；覆盖旧版兼容、D21 所有别名拒绝、正确声明完整恢复、missing／corrupt blob。日志：[备份回归](D:/MyCodeBuddy-roundtable/src-tauri/target/roundtable-final-backup-lib.log)。 |
| 最终订阅库模块 `cargo test --locked --no-default-features --features test-utils --lib web::ws` | 8 passed / 0 failed，exit 0；包含容量、同 ID 替换、完成句柄清理及原始严格 JSON。日志：[订阅回归](D:/MyCodeBuddy-roundtable/src-tauri/target/roundtable-final-ws-lib.log)。 |
| 默认桌面 `cargo clippy --locked --lib --bin codeg -- -D warnings` | exit 0；检查包含 Tauri 命令及默认桌面配置。构建脚本提示 sidecar 缺失并生成零字节检查占位，检查后已清理；不代表安装包可运行。日志：[桌面 Clippy](D:/MyCodeBuddy-roundtable/src-tauri/target/roundtable-production-desktop-clippy.log)。 |
| 服务器 `cargo clippy --locked --no-default-features --features server --bin codeg-server --lib -- -D warnings` | exit 0；未启用 test-utils。日志：[服务器 Clippy](D:/MyCodeBuddy-roundtable/src-tauri/target/roundtable-production-server-clippy.log)。 |
| MCP `cargo clippy --locked --no-default-features --features mcp-bin --bin codeg-mcp -- -D warnings` | exit 0；未启用 test-utils。日志：[MCP Clippy](D:/MyCodeBuddy-roundtable/src-tauri/target/roundtable-production-mcp-clippy.log)。 |
| 实际源码独立 OCI 边界 harness | Windows 单测/边界及 Linux cross-target check/Clippy 通过；live escape probe ignored。详情见协议修复记录。 |
| D22 实际连接模块 Unix 类型／lint harness | 用当前逐字节提取的模块及锁定 axum／tokio／tokio-util 版本，以 UnixListener 实例化共享 spawn；Linux cross-target check 和 Clippy `-D warnings` 均 exit 0。已核对源码哈希；这是模块类型检查，不是完整 Linux 应用构建或真实 socket／隔离资格实验。日志：[Unix check](C:/Users/drawpeng/AppData/Local/Temp/roundtable-gateway-unix-check/unix-check.log)、[Unix Clippy](C:/Users/drawpeng/AppData/Local/Temp/roundtable-gateway-unix-check/unix-clippy.log)。 |

全仓库 ESLint 的既有浏览器 JavaScript CRLF 等问题、全仓库 `tsc --noEmit` 的既有非 Roundtable 测试 fixture 错误，不能写作通过。本次改动文件的定向检查及 Next 构建另列，不用其结果替代全仓库检查。

Clippy 要求的整理包括原分支新增文件的 Copy 克隆、简单 Default、回调类型别名、参数分组和测试 helper 顺序。另将既有 `workflow_completion.rs` 的一个测试模块挪到生产定义之后，消除全目标检查的既有顺序错误；该文件没有业务逻辑变化。

## 证据边界

源码和受控执行器测试证明产品链已接通，以及状态、授权、预算和持久化回归行为；不证明真实 Linux 隔离、付费 provider/adapter 兼容性或生产部署资格。当前主机是 Windows，Linux live qualification 和 live escape 检查未执行，既有资格报告继续为 `blocked_platform`；未制造 passed 报告、未启用 rollout、未调用付费模型。

对象 GC 在没有持久引用/lease 排他证明时返回 `gc_reference_proof_required`。失败事务可能保留孤儿内容并保守占用配额；这是明确的保留策略，不能描述为完整 GC 已实现。

详细分工证据：[协议及前端](D:/MyCodeBuddy-roundtable/docs/reviews/2026-10-05-roundtable-fix-protocol.md)、[运行时](D:/MyCodeBuddy-roundtable/docs/reviews/2026-10-05-roundtable-fix-runtime.md)、[存储](D:/MyCodeBuddy-roundtable/docs/reviews/2026-10-05-roundtable-fix-storage.md)、[产品交叉复审](D:/MyCodeBuddy-roundtable/docs/reviews/2026-10-05-roundtable-product-integration-review.md)。
