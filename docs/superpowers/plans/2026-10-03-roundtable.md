# MyCodeBuddy 多智能体圆桌实施计划

版本 1.2 · 对应设计规范 v1.1 与待批准修订 · 2026 年 10 月 4 日

**执行说明：** 逐项完成下列复选框；每项先写可失败的测试、观察红灯，再做最小实现、运行绿灯并复核。本文件是实施规划，批准本文件不等于批准安装依赖、调用真实模型、扩大权限、推送或上线。先确认 G0 中明确列出的提案；先做 P00 可丢弃可行性探针，再完成共享资格链。G1 未通过前，不展开完整产品实现。执行人应同时阅读本计划与设计 v1.1。

**目标：** 在 MyCodeBuddy 共享 Rust 核心中实现可配置 N/R/C 的严格隔离圆桌，交付有依据的建议、回应、分歧与用户决策项。

**架构：** 新增独立 RoundtableService 与每房间 actor，复用现有 ACP 进程设施，增加受服务拥有的受限入口。把纯协议和 fake runtime 放入轻量 Rust crate，先验证准入、完成边界与控制协议；用不可变快照、原子接纳、完整投影及定向订阅贯穿桌面、Web 和远程桌面。每 attempt 新建 binding，主持独立，第一版只运行一个 active room。

**技术栈：** Rust 2021、Tokio、Serde、SeaORM/SQLite、现有 ACP 2.2.0、Tauri 2.11/Axum 0.8、Next.js 16/React 19/TypeScript/Vitest；不新增 Python 产品运行时。不升级现有技术栈来完成本功能。

**规范：** 同批交付的《MyCodeBuddy 多智能体圆桌协议设计 v1.1》。实施时将其原样放在 `docs/superpowers/specs/2026-10-03-roundtable-design-v1.1.md`，将本计划放在 `docs/superpowers/plans/2026-10-03-roundtable.md`；这些均是拟议仓库路径，本次没有写入源仓库。

## 1 本次结论和批准门

三份意见均已逐条核对。应保留现有架构，修补实施闭合与排序；不能将评审者未收到 v1.1 推断成 v1.1 不存在。此次交付同时包含原样设计 v1.1、计划 v1.1、变更提案和 SHA-256 清单。设计中的“v1.1”和本计划的“v1.1”是两个独立版本号。本计划不替换、修改或追认设计中的批准状态。

**G0 协议与实验范围确认。** 确认设计 §1 取舍、附录 A 的 A01–A09 明确提案、资源 profile 和首候选范围。A01–A09 目前均为 proposed；接受后记录决定 ID、日期及文档 hash。未接受的提案阻断相应接口冻结，不让执行者任选互斥实现。本次只修订文档，不执行项目测试、不改产品源码、不安装、不调用模型、不提交或上线。

**G0a 先消除最大可行性风险。** P00 在 P01 之前，以可丢弃隔离实验验证 rootless 进程、模型网关 endpoint、真实 companion 注入和原生工具边界。先做不调用模型的探针；若必须付费才可判断，单独提交模型/数据/额度批准。该试验只得出可行或阻断，不发资格证书。若不通，不先投入七个产品任务。

首候选为 Linux x86_64 / Debian 12，记录精确 kernel/build；运行时选择 rootless crun，记录实际官方包版本和二进制 SHA-256；镜像固定 digest，adapter 固定 @agentclientprotocol/codex-acp@2.1.1 app-server 模式，并记录实际内嵌 Codex、Node、codeg-mcp 及桥接程序 hash。所有实际启动路径均为镜像内绝对路径，禁止 PATH 优先和运行时 npx 下载。候选使用宿主已有且获准使用的 API 凭据，通过模型网关提供服务；不把 auth.json 挂进 sandbox。ChatGPT 账号登录能否适配是未验证兼容性，不断言所有账号用户不能使用；该路径不在首候选的发证范围内。缺少工具、权限或凭据时返回具体阻断，不自动安装、登录、获取新凭据或替换平台。

**G1 首适配器资格门。** P06a/P06b/P07a–P07e 与 P01/P02/P04/P05 的共同核心汇成 P08；资格工具可用内存存储，产品换为持久存储，但验证器、编码器、配额账本、token 认证和完成协议必须是同一实现。六能力之外，私有事件、侧栏隐藏、凭据不可达、实际二进制匹配也均为硬门。passed 必须有精确报告，不用 fake 证据替代。P00 成功不等于 G1；G1 前仅开展协议与资格最小链，不全面铺开数据库业务、控制台和界面。

**G2 持久 fake 纵向闭环。** P09a–P19 的适用协议测试通过；P13 初次装配保持 Recovering/Disabled，只有 P14 资源与时钟、P15 恢复均就绪后可开放 fake/product 各自入口。fake 不证明 OS 隔离。

**G3 双端闭环。** P20–P22 完成已认证唯一组合的桌面/Web/远程客户端验证，桌面编译与实际定向窗口验收独立执行。Windows/macOS 可运行纯协议/客户端测试；不得把不支持的本机 sandbox 探针失败当成协议红灯。真实执行端仍只有发证组合。

**G4 有限启用。** P01 先预注册质量任务和评分规则，P23 才在单独批准后执行固定任务实验及回滚。百分比门槛是提案，不是已生效的设计要求。用户另行批准启用精确资格键 allowlist，默认 false 持久开关从 P06a 起已存在。

没有排期或安全认证承诺。任何新执行环境、外部云端 Codex 或 ChatGPT Work 都须由用户本人启动；本计划不能替代该动作。

## 2 基线、复用点与测试边界

**集成目标固定为** `cb7596d38f9aa0df52740e939da7732ccfaaf5d8`，即 PR33 合入后的 main。PR33 受测 head 为 `d560b1d9`，对应 Test run `37180869458` 已成功；合并提交的独立 CI 状态应另行核验，不能直接套用 PR 的结果。此次重新取得源码并核验 HEAD 与上述 SHA 相同。旧计划的 2587471、abd3c67 和未合入修复线描述只保留于归档原包，不再作为施工基线。已有可靠性修复不重复移植。开始实现前若 main 再变化，先比较变更再决定是否更新基线，禁止悄悄漂移。

新增影响：Codex ACP 注册版本为 2.1.1；普通 companion 已具有浏览器、电脑和剪贴板工具，HostToolsPolicy::Agent 仅解决部分入口；LaneSender 没有现成的 owned reservation API；响应元数据 sessionFailure 已有读取路径，但圆桌必须将其接入私有完成判定。对应修订落实到 P07a、P07b、P07e 和 P12。

已静态核实的修改接缝：

- `src-tauri/src/acp/manager.rs`：`spawn_agent_with_attach_mode` 已允许 `parent_connection_id=None`、预分配 connection ID 和显式 emitter；普通 `send_prompt` 是异步业务入口，不能直接拿来当 gate 内的同步 try_enqueue
- `src-tauri/src/acp/agent_process.rs`：真实进程创建点 `AcpAgent::spawn_process`；必须在此之前选择已经封闭的 sandbox launcher，不能 spawn 后补权限
- `src-tauri/src/acp/connection.rs`：`ConnectionCommand::Prompt`、turn_generation、permissions、update/response；只加受限入口，不重写整份连接循环
- `src-tauri/src/auto_title/types.rs`、`internal_sessions.rs`、`src-tauri/src/db/entities/internal_agent_session.rs`：purpose、内部用途登记和 discovery lease 是不同控制点
- `src-tauri/src/acp/delegation/companion.rs` 与 `src-tauri/src/bin/codeg_mcp.rs`：现有工具组和 parent token 模型；圆桌增加明确 service 模式，不编造 parent
- `src-tauri/src/db/mod.rs`、`db/migration/mod.rs`：现有五连接池及迁移接缝；当前池对象上一次 PRAGMA 调用不构成逐连接保证
- `src-tauri/src/web/event_bridge.rs`、`web/ws.rs`、`web/router.rs`、`web/auth.rs`：现有全局广播与认证；新正文用私有 sink/订阅，HTTP 继续 POST /api/<command>
- `src-tauri/src/app_state.rs`、`lib.rs`、`server_bin/main.rs`：桌面与 server 共享服务生命周期
- `src/lib/transport/{types,tauri-transport,web-transport,remote-desktop-transport}.ts`：复用 invoke，扩充私有圆桌订阅，不另造 REST 风格

### 2.1 轻量测试策略

新增 `src-tauri/roundtable-protocol/` 为**独立 package**，包名 `roundtable-protocol`；它不依赖 codeg_lib、Tauri、SeaORM 或 agent runtime。主包随后以 path dependency 消费它。保留独立 Cargo.lock，并在 CI 比较两个锁中同名共享依赖的版本与来源一致；不把整个仓库改造成 workspace，不迁移既有模块。仅采用仓库已有版本线的 serde/serde_json/sha2/thiserror/uuid；Tokio 的 test-util 等放 dev-dependencies。第一次批准实现时生成锁文件，以后命令使用 --locked。

纯协议测试分别放 `roundtable-protocol/tests/*.rs`。真实 DB/ACP/transport 测试归入四个入口 `src-tauri/tests/{roundtable_protocol_io,roundtable_runtime,roundtable_service,roundtable_transport}.rs`，模块放 `tests/roundtable_cases/`，只编译普通 codeg_lib 依赖，不把所有新增测试塞进巨型 lib unit-test harness。已有低内存配置是 jobs=1、incremental=false、debug=0、单测试线程。近期完整 Rust lib-test 编译曾被 SIGKILL 9，而小 integration target 可完成；这是资源限制记录，不是产品失败，也不是忽略全套回归的理由。

下文命令均从**未来实施分支的仓库根目录**执行，`PTEST`/`ITEST` 为阅读缩写，实际复制时展开：

```bash
# PTEST <target>；首个锁文件尚未生成时仅首次去掉 --locked
cargo --config .cargo/low-memory.toml test --locked   --manifest-path src-tauri/roundtable-protocol/Cargo.toml --test <target>
# ITEST <target>
cargo --config .cargo/low-memory.toml test --locked   --manifest-path src-tauri/Cargo.toml --no-default-features   --features test-utils --test <target>
# 前端
pnpm exec vitest run <file-or-directory>
```

每任务命令写完整目标以便直接定位；第一次新增某target时同时创建其入口和mod声明，filter必须命中具名测试（0 tests不算绿灯）。新增 crate 先独立锁定依赖，P06a 是主包 path dependency 的唯一首次引入点，只刷新必要主锁项。P01 同时维护 `.gitignore`、独立 fmt/test/clippy CI 和共享依赖锁版本核验。不得在本次写计划阶段运行这些项目测试、安装依赖或实施代码。

### 2.2 分层文件责任

任务依赖中的P06/P07/P09/P16家族简称分别指该家族全部子任务，精确施工顺序按§7；历史T/F映射沿用这些家族简称。以下路径除第 2 节明确列出的既有文件外，均为**拟新增**：

- `src-tauri/roundtable-protocol/src/`：model、dto、canonical、budget、context、strategy、validation、admission、completion、projection、usage；纯类型/纯规则；协议budget只算纯规则
- `src-tauri/src/roundtable/`：service、actor、store、schema、runtime、qualification、sandbox、snapshot、objects、mcp、companion、control、recovery、clock、resources、diagnostics、events、authorization、api、acceptance、ownership、budget_ledger、command_processor、paging、usage、maintenance、feature_gate、qualification_harness、gateway、request_accounting、registry、ingress、tool_core、relay；I/O 和所有权
- `src-tauri/src/roundtable/sandbox/linux_oci.rs`：候选平台唯一隔离实现；其他平台返回 policy_unenforceable
- `src-tauri/tests/roundtable_*.rs` 与 `tests/roundtable_support/mod.rs`：按接口分拆集成测试，fake 时钟/队列/进程/故障注入
- `src/lib/roundtable/`：类型、API、reducer、stream、安全展示；`src/components/roundtable/`：创建、详情、结果；`src/app/roundtable/page.tsx`：静态路由
- `docs/roundtable/`：规范、资格报告、fixtures、质量评分与操作说明；不新增第二套会话运行时

## 3 全局约束

下列数值以 v1.1 为准；新建议另行标注，不沿用 v1.0 的旧预算。

1. N≥2，R≥0，1≤C≤N；3/2/3 仅预设。B=N×(1+R)+1，默认最大尝试 B+floor(0.4B)：3/2 为 10/14，7/5 为 43/60。checked arithmetic，溢出为 invalid_argument
2. Q=max(2,floor(N/2)+1)，全部 N 为 expected；主持独立 speaker、participant_id=null、Q=1。abstain 不算 valid。达到 Q 不提前发布
3. 每 turn/revision 最多 2 次准入、2 次 launch_start，每 attempt 最多一次启动。admitting 后崩溃消耗尝试且 uncertain，不重发；可证明未入队的 reserved 不耗 prompt 次数
4. launch=30s、prompt=180s、validation=5s、cleanup target=10s（cancel 宽限 5s），T=225s。D=2×ceil(N/C)×T，综合 2T；room_ms=(1+R)×D+2×T；3/2/3 总活跃预算 1800s。timeout 相等即超时
5. 活跃时长按 room 单调墙钟，不将并发时长相加；含启动、验证、取消和清理，不含排队/paused。≤1s 预扣 checkpoint，崩溃不退未知余片；清理超额单记 overrun
6. 每 attempt 新 binding，空闲进程上限 0；slot permit 持到本地清理证明；一个全局 active-room permit，resume 原子取得 min(C,remaining) 个 slot
7. member JSON 默认 8 KiB、硬上限 64 KiB；claims=1…20（abstain=0），responses≤30，open_questions/position_changes≤20；每 attempt 最多 3 次可修复的不合格提交；再有不合格则 invalid，首个有效结果 sealed
8. evidence 每次返回≤8 KiB、每 attempt 累计≤32 KiB；插话总额默认 16 KiB；这些配额进入 context_hash。强制回应目标最多 4（基础目标 1 + incoming challenges≤3）
9. 所有 callback 核对 boot_epoch/run_epoch/phase_id/phase_revision/attempt_id/binding_id/incarnation/context_hash/policy_hash；主持所有路由均用 speaker_id
10. roundtable.engine.lock 的文件句柄持到所有受控运行停止；无法取得时只读历史。锁基于规范 DB 身份；不支持网络文件系统、多主机共享 SQLite 或绕锁写者
11. 仅 strict_snapshot_v1；不可信/过期证书不发送 prompt。只开放 read_evidence/search_evidence/submit_result；permission 立即拒绝，未知/写工具无入口
12. 消息/关系/预算/投影/event 同事务；published 正文与 membership 永不覆写；每个影响客户端业务状态的事务一个 ProjectionV1 + projection_committed；内部预扣 checkpoint 不递增 room.seq/revision，投影预算为明确采样值，不能每 token 一次
13. HTTP 只用 POST /api/<command>；snake_case DTO；分页默认100、最多500/1MiB先到为准；未知 projecting schema/cause 停止消费
14. 诊断 assistant 前后片段合计≤64KiB；不存思考、token、凭据。blob 按 fsync→rename→父目录 fsync→hash 校验→DB 引用；GC 宽限至少24h
15. 预览可30–50ms合并，计划选40ms；不改变顺序，不充当 accepted 结果。费用不足/不可知显示 unknown，不显示0
16. 首版无交互审批、运行中增资料、外部抓取、原 session 冷恢复、多 active room、自动执行建议、删除/独立导出/运行后改名、自由讨论。只留 strategy 接缝

**实现建议而非规范既定值：** P01 profile 初始请求上限256KiB、JSON深度32、topic16KiB、单成员角色8KiB；source单文件16MiB/合计64MiB/最多1000文件；scratch64MiB、日志8MiB/每binding；room不可变存储256MiB、data_dir圆桌配额2GiB；最大10000个业务事件/room，满额前为收敛终态保留128事件。运维 N/R 上限建议16/8，必须可配置、preflight回显且仍跑通7/5/2的参数检查。这些值在G0确认并记录版本；容量证明不足时仍可拒绝实际开始。不得把这些值当已测得性能或暗中改小强制验收。

## 4 重点复核的五类隐含输入

1. 数字溢出、重复 JSON key、深层对象和未知字段不能绕过字节/预算限制：P01/P02/P18
2. spawn 已登记但未返回时 stop，加上 OS PID 重用，不得漏清理或杀错进程：P05/P07/P13
3. JSON 转义放大与非 ASCII 文本不能把“字节上界”误当 token 上界：P02/P10/P11
4. DB commit 已成但 ACK 丢失，加上第二次 restart，不得重复 prompt、创建第二 successor 或发布旧 closing 集合：P12/P15
5. 浏览器断线重连、过时按钮 revision 与未知事件版本同时出现，不得误展示已暂停/完成或读取当前页替代历史页：P17/P18/P19/P21

## 5 共享接口约定

### 5.1 类型归属与命名

本计划中 `Arc` 统一为 `std::sync::Arc`。每次新增 Rust 模块时，同时更新其所属 `src-tauri/roundtable-protocol/src/lib.rs` 或 `src-tauri/src/roundtable/mod.rs` 的模块声明与必要导出；不修改无关模块。

P01 的 `model.rs` 定义 UUID 字符串新类型 RoomId、PrincipalId、ParticipantId、SpeakerId、PhaseId、TurnId、AttemptId、BindingId、IncarnationId、OperationId、RequestId、SubmissionId、MessageId、ClaimId、ResponseId、EvidenceId、SnapshotId、ProjectionId、ManifestId、SubscriptionId；Hash256 为32字节值，序列化为64位小写十六进制；Seq/Revision/Epoch 及累计时长内部为 u64，对外为无前导零十进制字符串（A01）；MonoMs 只内部使用，不跨进程存绝对值。SubmissionId 是 attempt 内1…64字节短字符串，其余 ID 由宿主生成。所有这些类型有 Eq/Hash/Serialize/Deserialize；身份 DTO 不允许模型填写。

`RtResult<T> = Result<T, RtError>`。RtError 含 code/message/retryable/current_revision/details；ErrorCode 实现 v1.1 §13.2 闭集并保留内部诊断分类。对外不泄漏凭据、存在性或进程路径。`ActorContext { principal_id, scope: OperatorScope, client: ClientIdentity }` 仅可信入口构造；body 没有 principal 字段。

`Fence` 含设计 §4.2 的九个字段；`ServiceOwner { room_id, attempt_id, boot_epoch }`；`ProcessTreeProof { instance_id, incarnation, process_tree_empty }` 仅由隔离器给出；运行层聚合 `CleanupProof { process: ProcessTreeProof, mailbox_empty, tools_drained, ingress_drained }`；`PermitReleaseProof { lease_id, proofs: BTreeMap<IncarnationId,CleanupProof> }` 必须完整匹配该 lease 的全部 incarnation 才可释放。`QualifiedProfile` 是证书与容量/launch政策的只读组合，不因品牌名自动构造。

`RoundtableConfigV1`、`PhaseSnapshotV1`、`DeliveryManifestV1`、`ContextStateV1`、`MemberResultV1`、`ModeratorResultV1`、`ProjectionV1`、`MessageManifestV1`、`DurableEnvelopeV1`、`PreviewFrameV1` 的字段完整遵循 v1.1 §6–13。详细 wire schema 和黄金 fixtures 在 P01 形成并冻结；本计划不另发不完整的第二套 JSON schema。

`SchedulingIntent { phase_id, speaker_id, ordinal, is_retry }` 只表达调度意图。`RuntimeTurnCompleted { fence, finish_reason, ingress_watermark, tool_barrier, candidate_id }` 不是 accepted。`CandidateReceipt { submission_id, payload_hash, candidate_id, state: staged }` 不是 accepted。`MutationAck` 与 `ControlOperationV1` 分别表示受理和操作进度。

### 5.2 测试夹具约定

协议 crate 每个 test target 有自己的小 fixture，禁止复制产品算法。P01 提供 `tests/support/mod.rs`：`config(n:u32,r:u32,c:u32)->RoundtableConfigV1`、`id<T:FromStr>(label:&str)->T`（稳定映射测试标签）、`member(kind:MemberKind,claim_count:usize)->MemberResultV1`、`fence(epoch:u64)->Fence`。后续 task 在自己的 support 中增加本任务实际需要的 fake，不预造通用测试框架。

集成测试 support 在 P09/P13 逐步建立 `Harness`，拥有临时 data_dir、真实 SQLite、可控单调时钟、FakeRuntime、失败点和私有 sink。它的公开观测是 `snapshot()->ProjectionV1`、`admitted()->Vec<AttemptId>`、`live_incarnations()->Vec<IncarnationId>`、`global_events()->Vec<serde_json::Value>`；状态变更必须通过被测接口，禁止测试直接把 accepted/closing 写成期望值后宣称端到端通过。确需恢复种子时用带完整来源的持久 fixture。

## 6 可逐项执行的任务

### P00 前置可丢弃可行性探针

**依赖与环境：** G0实验范围确认；LINUX。先于P01产品实现。无模型探针可先执行；真实请求、安装与凭据使用另需明确授权。

**文件：** 临时隔离目录中的 `spike/{probe.mjs,fixtures,report.json}`，不导入产品；归档脱敏结论到 `docs/roundtable/qualification/spike-verdict.md`。报告记录 `os_build,kernel,crun_version,crun_sha256,image_digest,adapter_sha256,codex_sha256,node_sha256,mcp_sha256`。

**接口与验证：** `probeEnvironment(profile, cases): Promise<SpikeReport>`；SpikeReport含每项实际命令hash、退出状态、期望拒绝/实际拒绝、未测项和阻断原因，不含认证secret。不把P00脚本作为后续生产实现。

- [ ] **红灯。** 写 `missing_profile_is_blocked`、`no_approval_never_contacts_model`，断言没有精确二进制hash/授权时 `can_probe=false`、模型请求计数0。运行 `node --test spike/probe.test.mjs`，只把断言失败视为红灯。
- [ ] **实现和真实探针。** 用合成诱饵验证rootless crun创建/枚举/回收、隔离namespace/cgroup、Codex自定义base_url、CLI实际拉起codeg-mcp并经挂载socket就绪、shell/原生fs不能越界。固定本地loopback relay→宿主Unix网关socket，宿主模型凭据不挂载。shell进程能启动不自动失败；其越界读写、外联或绕过工具能力必须失败。未获真实请求批准时endpoint兼容标not_tested，不伪造成功。
- [ ] **绿灯和裁决。** 重跑同测试，逐项保留证据；任一关键链不可行即阻断G1投入，明确改adapter/换候选需要新的决定。四条全可行仍不发证。报告独立复核后本地提交 `docs(roundtable): record bounded feasibility spike`。



### P01 冻结类型、配置和黄金契约

**依赖与出口：** G0；输出协议 package 和无 I/O 契约，不触碰真实 ACP。覆盖 T01/T06/T14/T16。

**文件：** 新增 `src-tauri/roundtable-protocol/{Cargo.toml,Cargo.lock,src/lib.rs,src/model.rs,src/dto.rs,src/canonical.rs,tests/contracts.rs,tests/support/mod.rs}`；新增 `docs/roundtable/fixtures/{config,member,moderator,projection,errors,commands}.json`。

**接口：**
```rust
pub fn decode_config(bytes: &[u8], limits: &ParseLimits)
    -> RtResult<RoundtableConfigV1>;
pub fn validate_config(config: &RoundtableConfigV1,
    limits: &ResourceLimits) -> RtResult<()>;
pub fn canonical_hash<T: serde::Serialize>(value: &T) -> RtResult<Hash256>;
pub fn mutation_methods() -> &'static [&'static str];
pub fn read_methods() -> &'static [&'static str];
```

**冻结补充：** 此任务先把附录A已批准的字段落实为Rust类型、JSON Schema与黄金字节fixture；未批准则停在G0，不先做一套临时wire。18命令由单一`CommandNameV1::ALL`生成handler与fixture覆盖表。完整 `Fence` 字段为boot_epoch/run_epoch/phase_id/phase_revision/attempt_id/binding_id/incarnation/context_hash/policy_hash。公共API错误码严格按设计§13.2；attempt_closed/stale_fence/queue_rejected/snapshot_unstable/context_contract_violation是内部原因，映射公开invalid_state/invalid_argument/capability_unqualified等并在details.reason携带，不能擅自扩充公共闭集。

**配套文件：** 同任务新增 `src-tauri/.gitignore` 的 `/roundtable-protocol/target/`，修改 `.github/workflows/test.yml` 显式跑独立crate fmt/test/clippy；新增 `scripts/check-roundtable-locks.mjs`，比较两份Cargo.lock共享依赖(name,version,source,checksum)并失败报告差异，主path依赖仍只在P06a引入。`contracts.rs`先断言18名全等和无重复，不仅断言某删命令不存在。P01同时预注册 `docs/roundtable/quality-protocol.md` 与固定holdout fixture hash（附录D），不能用P08/P22调试样例替代质量保留集。

- [ ] **1. 写失败测试。** `contracts.rs` 定义 `config_ranges_and_closed_commands`、`strict_json_and_canonical_hash`。用真实 JSON 原文测试重复 key（不能先解析成 Value）、深度33、NaN、未知字段、N=1、C=0、C>N、unknown strategy 均拒绝；N=7/R=5/C=2 合法。相同参数显式默认/省略默认归一后 hash 相同；键顺序变化 hash 相同，数组顺序不同 hash 不同。断言：
```rust
assert!(validate_config(&config(7,5,2), &limits).is_ok());
assert_eq!(decode_config(br#"{"n":2,"n":3}"#, &parse).unwrap_err().code,
    ErrorCode::InvalidArgument);
assert!(!mutation_methods().contains(&"roundtable_delete"));
assert!(!mutation_methods().contains(&"roundtable_export"));
```
config fixture 采用真实完整字段；上面的重复键反例即使还有缺字段也必须命中 duplicate_key 细节而非缺字段。冻结所有状态枚举、18个命令（16个独立名称加attach与detach）、错误映射、主持 speaker 非空 fixture。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/roundtable-protocol/Cargo.toml --test contracts`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** 先创建最小 package 并写 tests，观察缺接口失败后实现解析/类型。规范 JSON 以排序对象 key、UTF-8、有限整数和确定转义为规则，schema先限字节再解析；不借 serde_json::Value 的 last-key-wins 处理重复 key。业务 hash 包含已补全默认值。配置最大值在ResourceLimits，不能硬编码3/2/3。Phase/attempt/room枚举按规范逐项列出，schema version=1。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：错误码、nullable主持字段、默认值与显式值归一；新增 package 不依赖 app，profile 建议值与 v1.1 值分开标识。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "feat(roundtable): freeze protocol contracts"`；不 push、不 merge。

### P02 实现尝试、时间与上下文容量纯规则

**依赖与出口：** P01；T01/T12/T13/T18。

**文件：** 新增 `roundtable-protocol/src/{budget,context}.rs`、`tests/budget_context.rs`（均在 `src-tauri/roundtable-protocol/`）。

**接口：**
```rust
pub fn budget_plan(n: u32, r: u32, c: u32,
    timeouts: Timeouts) -> RtResult<BudgetPlan>;
pub fn reserve_attempts(state: &BudgetState, request: ReservationRequest)
    -> RtResult<BudgetReservation>;
pub fn preflight_context(config: &RoundtableConfigV1,
    input: &EncodedInputBounds, capacity: &dyn TokenBound,
    profile: &QualifiedContextProfile)
    -> RtResult<ContextBound>;
pub trait TokenBound {
    fn upper_bound(&self, utf8: &[u8]) -> RtResult<u64>;
    fn capacity_tokens(&self) -> Option<u64>;
}
```

**共享编码器与新类型：** `DeliveryEncoder::encode(&PhaseSnapshotV1,&RoleSnapshot,&BindingId,&dyn TokenBound,&QualifiedContextProfile)->RtResult<DeliveryManifestV1>` 定义在本crate `context.rs`，供P07c和P10共用。`ContextBound { exact_bytes,future_bytes,verified_token_upper_bound,generation_reserve_tokens,profile_hash }`；`QualifiedContextProfile`按A05，`RequestTranscriptBound`含adapter注入、已有输出/调用参数和所有工具回包。P02提供纯 `bound_request(&RequestTranscriptBound,&QualifiedContextProfile)->RtResult<u64>`；P06b执行逐请求硬检查。3-invalid→valid、重复receipt、空/错误search、合法最大参数联合fixture必须超限拒绝或证明每请求<=capacity。

**参数期望：** 附录C给N/C组合的D及R=0/1/2/5完整room_ms表；测试读取静态表，不用被测budget_plan生成期望。A04计时/业务事件/字节联算测试 `checkpoint_budget_fits_event_and_storage_limits`，7/5/2用满11250000ms不产生11250个业务投影；数据目录预留仍覆盖终态字节。checked加减乘除全覆盖。

- [ ] **1. 写失败测试。** `parameterized_budget_and_repair_slots` 遍历N=2/3/7、R=0/1/2/5、C=1/2/N（去掉非法组合）。BudgetPlan 字段base_attempts/max_attempts/slot_ms/discussion_ms/room_ms，断言：
```rust
let b = budget_plan(3,2,3,Timeouts::default()).unwrap();
assert_eq!((b.base_attempts,b.max_attempts), (10,14));
assert_eq!((b.slot_ms,b.discussion_ms,b.room_ms),(225_000,450_000,1_800_000));
let b = budget_plan(7,5,2,Timeouts::default()).unwrap();
assert_eq!((b.base_attempts,b.max_attempts,b.room_ms),(43,60,11_250_000));
```
`capacity_includes_every_byte` 分别扩大问题、角色、插话、结果、证据、schema、工具、JSON转义和生成预留，逐一断言上界增加；3×3×65536=589824仅是成员结果正文。emoji/中文/反斜线 fixture 的 exact_bytes 等于最终编码字节长度。capacity=None 返回capacity_unknown；下溢、乘法溢出拒绝；optional retry不能抢未首发slot、未来阶段N和主持1次的储备。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/roundtable-protocol/Cargo.toml --test budget_context`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** BudgetPlan完全用checked整数计算。ContextBound含exact已知字节、future上界字节、token_upper_bound、generation_reserve_tokens。未来结果按配置S；编码器对未来字节的JSON转义取可证明最坏界，不能只加S。TokenBound由与资格profile tokenizer_id/hash匹配的可信实现提供算法/模型容量；fake只用于测试，不能当真实证明。加入deadline未到用严格 `<`，最大2次launch/2次准入、取消/uncertain消耗、reserved未入队例外。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：N²/R²增长与全计划preflight；容量未知明确拒绝；C降低不自动补时，room墙钟不相加。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "feat(roundtable): bound attempts time and context"`；不 push、不 merge。

### P03 实现有界阶段策略、回应目标与 coverage

**依赖与出口：** P01/P02；T01/T08/T09/T22。

**文件：** 新增 `src-tauri/roundtable-protocol/src/strategy.rs`、`tests/strategy.rs`。

**接口：**
```rust
pub fn next_intents(view: &StrategyView) -> RtResult<Vec<SchedulingIntent>>;
pub fn assigned_targets(previous: &PublishedPhase,
    expected: &[SpeakerOrdinal], history: &PublishedHistory)
    -> RtResult<Vec<SpeakerTargets>>;
pub fn phase_quorum(n: u32, kind: PhaseKind) -> u32;
pub fn coverage(history: &PublishedHistory,
    targets: &[SpeakerTargets]) -> CoverageV1;
```

- [ ] **1. 写失败测试。** `ring_targets_and_incoming_challenges` 固定3成员ordinal=0/1/2、各20条claim；断言仅每份结果第一条claim分给下一位不同speaker，缺席成员仍在expected，只有两位有效时互评；第k轮只从上一已发布阶段的responses中选指向本人历史claim的challenge，critical优先、发布seq/source ordinal/response ordinal排序的最多3条challenge。`empty_claims_cannot_seed_next_phase`：两个非空summary但claims=[]不能变valid；abstain不计Q。`quorum_waits_for_terminal_slots`：N=3已valid2但slot3运行不关；N=2一失败不成功。断言：
```rust
assert_eq!(phase_quorum(2,PhaseKind::Proposal),2);
assert_eq!(phase_quorum(7,PhaseKind::Critique),4);
assert_eq!(phase_quorum(7,PhaseKind::Synthesis),1);
assert!(targets.iter().all(|t| t.required.len() <= 4));
assert_eq!(coverage(&partial,&targets).unanswered_required,1);
```
R=0依然有独立主持，标记parallel_consultation。另用R=5固定例证明早期已处理与未处理challenge都不自动重入强制候选；仅上一阶段新challenge可入选，历史未回应仍显示coverage。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/roundtable-protocol/Cargo.toml --test strategy`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** StrategyView只含不可变published前缀、当前slot终态和预算；不读当前preview/staged，不执行I/O。首次队列优先、ordinal稳定。阶段关闭仅全slot终结或时限到。目标分配一次冻结，失败不重分配。CoverageV1由宿主算assigned/answered/unanswered、有效/缺席/abstained与support边，不从主持payload接收。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：assigned与answered分开；不把缺席当支持，不把claim引用有效说成语义支持。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "feat(roundtable): schedule bounded rounds and coverage"`；不 push、不 merge。

### P04 实现结果 schema、别名校验和封存协议

**依赖与出口：** P01；T06/T08/T09。消费P01的目标类型与手写发布fixture，不依赖P03算法。

**文件：** 新增 `src-tauri/roundtable-protocol/src/validation.rs`、`tests/result_contract.rs`、`docs/roundtable/fixtures/result-counterexamples.json`。

**接口：**
```rust
pub fn validate_result(raw: &[u8], scope: &ResultScope)
    -> Result<ValidatedResult, Vec<FieldError>>;
pub fn submit_candidate(state: &SubmissionState, submission_id: &SubmissionId,
    raw: &[u8], scope: &ResultScope) -> SubmissionDecision;
pub fn validate_consensus(item: &ConsensusItemV1,
    published: &PublishedHistory) -> Result<(), Vec<FieldError>>;
```

**边界与并发责任：** “前三次可修复、第4次不合格终结”作为G0明确解释；第三次后下一次合格可sealed。纯transition测试只证明规则；P11用同规则在唯一约束/CAS事务中证明两个有效并发只一候选、第三/四次invalid并发不丢增量。纯测试不宣称数据库竞态已证明。

- [ ] **1. 写失败测试。** `submission_seals_once`：同ID/同规范hash原receipt；同ID不同payload= submission_conflict；首次valid后不同ID/payload=result_already_sealed；前三次不合格返回field_errors；第4次不合格将attempt判invalid并关闭进一步提交；第三次后若下一次合格仍可sealed。`references_and_identity` 覆盖错误别名、peer staged evidence、伪造speaker、错phase kind、自我support、两个target同时填、自己历史claim归属错误、new_local_claim_key不存在、重复local_key。`consensus_needs_distinct_support_edges` 仅不同speaker、published、同一目标的support可声称explicit_agreement。断言：
```rust
assert_eq!(third_invalid.next_state.invalid_count,3);
assert!(!third_invalid.closes_attempt);
assert!(fourth_invalid.closes_attempt);
assert_eq!(receipt.state,CandidateState::Staged);
assert!(validate_result(&empty_proposal,&scope).is_err());
assert!(validate_consensus(&unsupported,&published).is_err());
```
用实际8KiB/64KiB边界、20/21claims、30/31responses测试，不用字符数代字节。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/roundtable-protocol/Cargo.toml --test result_contract`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** ResultScope含phase kind、speaker、可见别名、强制目标、已发布历史和配额；工具payload禁止身份选择。字段错误包含路径/闭集code/可读说明，不含token。ModeratorResult每个结论至少一有效message/claim/evidence引用，无证据推断标inference；coverage字段拒绝。提交只产生staged候选，不判断normal finish或写accepted。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：最终文本不是后备解析通道；abstain必须reason且不计Q；确切错误路径和同payload幂等。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "feat(roundtable): validate and seal structured results"`；不 push、不 merge。

### P05 构建 fake runtime 准入与完成边界最小核

**依赖与出口：** P01/P02/P04；M0a出口，T02/T03/T06/T07/T21。

**文件：** 新增 `roundtable-protocol/src/{admission,completion}.rs`、`tests/{admission_races,completion_barrier}.rs`；路径均在 `src-tauri/roundtable-protocol/`。

**接口：**
```rust
pub trait LocalPromptQueue {
    fn try_enqueue(&self, prompt: AdmittedPrompt) -> Result<(), QueueReject>;
}
pub fn check_admission(current: &AdmissionState, fence: &Fence,
    now: MonoMs) -> RtResult<()>;
pub fn close_mcp_admission(state: &mut CompletionState,
    ingress_watermark: u64) -> CompletionBarrier;
pub fn complete_turn(state: &CompletionState,
    barrier: &CompletionBarrier) -> RtResult<RuntimeTurnCompleted>;
```

**受控调度和活性：** 用单线程协作式failpoint在reserved、registered、gate_acquired、before_enqueue等显式检查点让出，不使用线程sleep制造顺序。`actor_remains_responsive_while_completion_barrier_waits`令已准入handler请求actor回复；barrier由受控任务等待，actor可回复与处理stop。`deadline_expired_during_admitting_commit_rejects_enqueue`在DB模拟await中推进clock，提交返回后重新采样并拒绝发送。`disabled_gate_rejects_ready_attempt_before_enqueue`验证ready并非永久授权。此任务出口仅M0a模型核，不代替P13真实DB/gate证据。

- [ ] **1. 写失败测试。** `stop_before_gate_rejects_old_enqueue`：A取得gate前暂停，B先提交stop，恢复A必须0次入队。`enqueue_inside_gate_precedes_stop_commit`：A持gate完成检查后暂停；发起B但只断言B尚未提交，不等待其完成；放行A入队释放gate，再断言B提交。因此测试没有自己造死锁。队列满为queue_rejected且admitted数不变；admitting提交后模拟崩溃不得重发。`response_waits_for_prior_ingress_and_admitted_tools` 延迟最后ACP update和已准入MCP handler，prompt response到达仍不complete；未准入迟到submit=attempt_closed。断言：
```rust
assert_eq!(fake.enqueued_count(),0); // stop先提交场景
assert!(barrier.pending_tools.contains(&handler_id));
assert!(complete_turn(&pending,&barrier).is_err());
assert_eq!(late.code,ErrorCode::AttemptClosed);
```
支持bootstrap未返回已登记incarnation；取消尾部update违反归属时退役binding，不能借给下一attempt。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/roundtable-protocol/Cargo.toml --test admission_races`；随后另行运行 `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/roundtable-protocol/Cargo.toml --test completion_barrier`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** 测试核以共享Mutex模拟RoomAdmissionGate，fake持久提交点有明确failpoint；产品gate在P13接真实DB。顺序固定reserved→登记incarnation→ready→持gate提交admitting→同步非阻塞try_enqueue→admitted。完成时持gate关闭新MCP准入并截取已准入handler集合，释放gate再等待集合和ACP水位；只在normal finish+一个sealed candidate+barrier收敛生成RuntimeTurnCompleted。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：不在gate/事务内等待网络、进程或MCP回包；明确本地零新增准入和residual_remote_work并存。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "test(roundtable): prove admission and completion races"`；不 push、不 merge。

### P06a 建立隔离接口和前置执行门禁

**依赖与环境：** P00可行，P01/P02；DEV纯规则，LINUX真OS探针。首次引入主包path依赖；生产默认关闭。

**文件：** 新增 `src-tauri/src/roundtable/{mod,qualification,sandbox,feature_gate}.rs`、`sandbox/linux_oci.rs`、`tests/roundtable_cases/sandbox.rs`；修改 `src-tauri/{Cargo.toml,Cargo.lock,src/lib.rs}`。持久rollout配置固定存于data_dir/roundtable/execution-policy.json，按temp写/fsync/rename/父目录fsync原子更新，默认false；读取失败只读且禁止准入，不依赖P09数据库迁移，不修改普通会话实体。

**接口：** `evaluate_certificate(&QualificationKey, Option<&QualificationReport>)->QualificationStatus`；`build_sandbox_plan(&SandboxInput)->RtResult<SandboxPlan>`；`IsolationProvider::prepare(&SandboxPlan)->PreparedSandbox`、`spawn(&PreparedSandbox,&LaunchIntent)->SandboxInstance`、`discover_owned(&DbIdentity)->Vec<SandboxInstance>`、`reap(&SandboxInstance)->ProcessTreeProof` 均async；`ExecutionGate::check(&ExecutionScope,&AdmissionFacts,MonoMs)->RtResult<GatePermit>` 为同步纯检查。`SandboxInstance { runtime_id, incarnation, boot_epoch, image_digest, cgroup_handle, owner_label }` 不以PID作为唯一身份。

- [ ] **红灯。** 写 `certificate_invalidates_every_component`、`default_persistent_gate_denies_product`、`scope_cannot_convert_qualification_to_product`、`sandbox_blocks_host_escape`。精确改任一实际二进制/OS/镜像/policy/tool/corehash，旧passed失效；重启设置仍false。诱饵绝对路径/HOME/项目/其他scratch、/proc、继承FD、symlink/device/socket、全局MCP、任意网络越界均实际尝试且拒绝。
- [ ] **运行。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_runtime sandbox`；OS部分显式LINUX且单独ignored过滤，不在Windows普通测试中错误失败。
- [ ] **实现。** rootless crun仅通过版本/hash固定的绝对路径启动，OCI计划采用user/mount/pid/network namespaces、只读镜像、drop caps/no-new-privileges/seccomp/cgroup限额，无host network/Docker socket/HOME/项目挂载。环境先env_clear再白名单，二进制允许列表进入证书。新增 `LaunchIntentStore` trait：`record(&LaunchIntent)->RtResult<()>`、`mark_spawned(IncarnationId,&SandboxInstance)->RtResult<()>`、`list_unreaped()->RtResult<Vec<LaunchIntent>>`、`mark_reaped(IncarnationId)->RtResult<()>`。P06a资格用独占目录内fsync追加JSON journal作为持久实现，重启重放且拒绝截断/损坏记录；P09b迁移新增rt_launch_intents并以事务adapter实现同trait。持久LaunchIntent必须在spawn前可恢复登记，标签包含canonical DB identity/boot/incarnation；spawn ACK丢失时discover仍能找到。最终四部分cleanup由运行层聚合，隔离层不虚称mailbox/tools排空。
- [ ] **绿灯与提交。** 同命令加P01/P02回归；恢复枚举不可证明就blocked，不释放资源。提交 `feat(roundtable): gate and prepare isolated runtime`。

### P06b 实现宿主模型网关和容量账本

**依赖与环境：** P01/P02/P04/P06a；DEV假upstream，LINUX真实候选。此组件属于资格安全边界，必须在P08之前存在。

**文件：** 新增 `src-tauri/src/roundtable/{gateway,request_accounting}.rs`、`tests/roundtable_cases/gateway.rs`；新增镜像内只做loopback到Unix socket转发的 `src-tauri/src/roundtable/relay.rs` 模式，从现有可控helper入口调用，不新增src/bin安装包。HostModelGateway可独立实例化，由P07c QualificationHarness在同进程拥有并传入ExecutionGate；P13之后再由RoundtableService拥有同一组件，不以P13为资格前置。

**接口：** `GatewayLease { attempt_id,fence,recipient,policy_generation,expires_at_mono,context_profile }`；`HostModelGateway::register(GatewayLease)->GatewayHandle`、`revoke(AttemptId)`、`handle(GatewayHandle,ModelRequest)->RtResult<ModelResponse>`；`RequestAccounting::authorize(&EncodedModelRequest,&QualifiedContextProfile)->RequestPermit`。GatewayHandle是实例专用Unix socket与随机capability，不是任意URL代理。

- [ ] **红灯。** `gateway_rejects_wrong_model_method_and_redirect`、`every_request_includes_prior_output_bounds`、`all_tool_envelopes_count_toward_limit`、`revoked_or_expired_lease_never_forwards`。断言POST /v1/responses之外全部拒绝（含CONNECT）；body模型必须与批准recipient一致；任意URL、redirect、远端history引用、未认证工具类型拒绝；第65个模型请求/第129个工具调用不转发；修复3次后合格、重复receipt、空search与field_errors组合仍逐请求证明容量。
- [ ] **运行。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_runtime gateway`。
- [ ] **实现。** sandbox内endpoint固定 `http://127.0.0.1:39173/v1`；relay只能连该实例挂载的Unix socket，无任意目标参数。宿主网关使用批准的HTTPS origin及准确模型，验证证书、禁重定向和代理目标注入；请求并发每attempt=1，body/response/次数/累计输出配额按A05。只允许通过资格审查的模型操作；不能给CLI添加未认证的remote tools/browsing。凭据只在宿主注入，Authorization不回传或入日志。现有HTTP库优先复用；新增依赖须单独批准/锁定。路径不受adapter支持即G1 failed，不偷偷改为通用代理。
- [ ] **绿灯与提交。** 同命令加P02容量测试；Fake/Qualification/Product均显式scope，独立本地lease到期可在DB阻塞时撤销。提交 `feat(roundtable): enforce bounded model gateway`。

### P07a 新增服务拥有策略和同步准入接缝

**依赖与环境：** P01/P05/P06a；DEV，生产关闭。

**文件：** 新增 `src-tauri/src/roundtable/runtime.rs`；修改 `acp/{manager,connection,agent_process,host_tools_policy}.rs`、`auto_title/types.rs`；测试 `tests/roundtable_cases/runtime_admission.rs`。所有路径前缀为src-tauri/src，测试除外。

**接口：** `prepare_roundtable_connection(&ConnectionManager,RoundtableLaunch,Arc<dyn PrivateRuntimeSink>)->RtResult<PreparedRoundtableConnection>`；`PreparedPrompt { owned_permit: LaneOwnedPermit<ConnectionCommand>, state: Arc<RwLock<ConnectionState>>, prompt: AdmittedPrompt }`；`try_enqueue(PreparedPrompt)->Result<TurnGeneration,QueueReject>` 同步且不await。`ConnectionOwner = Window{label,operation_id}|Service{room_id,attempt_id,boot_epoch}`；服务兼容底层必填label时固定 `__roundtable_service__`，清理判断以owner枚举为准，不把哨兵当真实观察窗口。

- [ ] **红灯。** `roundtable_policy_is_not_hidden_generation`、`try_write_failure_does_not_mutate_turn`、`generation_is_updated_before_send`、`observer_close_does_not_reap_service`。Roundtable hidden_generation=false；显式permission deny、hostfs/terminal=false、仅roundtable工具；队列满/try_write失败=queue_rejected，代际和turn_in_flight不改。成功一次则parent_turn_generation+1、active_turn_generation一致、turn_in_flight=true、active_provider_turn_id=None；所有溢出路径拒绝且无半状态。
- [ ] **运行。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_runtime runtime_admission`。
- [ ] **实现。** 先扩展 LaneSender，新增封装 Tokio OwnedPermit 的 LaneOwnedPermit 与 reserve_owned 接口。封装必须持有 LaneSender 的 liveness 所有权直到 send/drop，不能让底层 permit 活着而 sender_owners 先归零。当前 reserve 返回借用 Permit，不能假设已有 owned API。增加 last_sender_drop_with_pending_permit 与 cancel_owned_reservation_releases_capacity 两个回归测试。在gate外准备 LaneOwnedPermit，在gate内一次try_write取得状态、重查无in-flight并完整执行基线manager.rs同步尾部，再permit.send；不得跳过generation映射。没有普通conversation capture/mandatory delegation路由。service路径绕开PATH/npx与宿主继承env，只用P06a PreparedSandbox。`#[cfg(any(test,feature="test-utils"))]`仅暴露精确spawn/admission failpoint，不依赖只在lib单元测试编译的旧hook。
- [ ] **绿灯与提交。** 同命令；User/Delegation/InternalProbe/InternalTitle/InternalTranslate所有旧fixture回归，默认feature编译另验。提交 `feat(roundtable): admit service-owned ACP turns`。

### P07b 隔离原始事件和完成边界

**依赖与环境：** P05/P07a；DEV，DESKTOP定向投递另验。

**文件：** 新增 `src-tauri/src/roundtable/ingress.rs`；修改 `acp/connection.rs`、`web/event_bridge.rs`；测试 `tests/roundtable_cases/private_ingress.rs`。

**接口：** `PrivateRuntimeSink::push(RuntimeIngress)->RtResult<()>`；`RuntimeIngress { fence,turn_generation,ingress_seq,kind }`；`CompletionCoordinator::begin(CompletionMarker)->CompletionBarrier` 和 `on_barrier_drained(BarrierFact)->RuntimeTurnCompleted`。BarrierFact含fence/已截取handler集合/ACP水位，不能自行接纳。

- [ ] **红灯。** `private_sink_receives_raw_events_only`、`actor_remains_responsive_while_completion_barrier_waits`、`late_update_never_belongs_to_next_attempt`：私有sink收到update/permission/lifecycle；legacy bus及Web全局广播均零正文；已准入handler等待actor回复时completion不能阻塞actor；stop仍可提交；未准入迟到submit=attempt_closed。
- [ ] **运行。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_runtime private_ingress`。
- [ ] **实现。** 每incarnation单有序ACP ingress，MCP独立通道通过已准入集合收敛；关闭准入在gate内完成，barrier/reap由受控任务等待，actor继续处理消息。取消尾部有明确归属，异常退役binding。私有emitter分支必须在原始emit_with_state路径之前分流，不能只过滤派生roundtable事件。
- [ ] **绿灯与提交。** 同命令加P05；桌面分支用同一runtime测试目标加默认feature；P17再验窗口订阅。提交 `feat(roundtable): keep runtime ingress private and ordered`。

### P07c 建立真实companion与共享三工具核心

**依赖与环境：** P02/P04/P06b/P07a/P07b；DEV假传输，LINUX实际CLI spawn。

**文件：** 新增 `src-tauri/src/roundtable/{companion,tool_core,qualification_harness}.rs`；修改 `src-tauri/src/bin/codeg_mcp.rs`为薄参数入口、公共解析器放 `roundtable/companion.rs`；测试 `tests/roundtable_cases/companion.rs`。

**接口：** `CompanionMode = LegacyParent | ServiceRoundtable{socket_path,incarnation}`；`ToolStore` async接口 `get_evidence(ObjectRef)`、`submit(SubmissionId,ValidatedResult)->CandidateReceipt`；`dispatch_tool(AdmittedToolScope,RoundtableToolCall,&dyn ToolStore)->RtResult<RoundtableToolResponse>`。`InMemoryToolStore`仅用于资格；P11 DurableToolStore复用同dispatch/validation/编码/配额，实现不复制一套。

- [ ] **红灯。** `companion_service_mode_has_exactly_three_tools`、`token_is_attempt_scoped_even_if_agent_reads_it`、`service_watchdog_uses_socket_eof`、`qualification_uses_production_encoder_and_validator`。广告与可调用集合精确3工具；跨attempt/room/fence token全部拒绝；env token不进入argv/log/prompt/URL；服务模式不读取宿主parent PID；broker EOF终止；成功receipt先到CLI，才可正常completion。
- [ ] **运行。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_runtime companion`，另运行附录B的codeg-mcp bin测试。
- [ ] **实现。** MCP的env将token传给实际CLI子进程，socket挂载只属于该instance；token绑定完整身份/fence/可见别名/工具版本，立即可撤销。token不对同sandbox agent保密，但只能访问已经授权的同attempt能力；不把鉴别模型身份当目标。计费凭据绝不进入sandbox。使用P02 DeliveryEncoder和P04验证器，A05计量覆盖所有回复与重试；schema完全相同。
- [ ] **绿灯与提交。** 同命令和bin目标；P08必须实测真实孙进程链，fake env/FD成功不能发证。提交 `feat(roundtable): add scoped service companion and shared tools`。

### P07d 隐藏内部会话并保留迁移兼容

**依赖与环境：** P07a/P07c；DEV及LINUX实际discovery/import。

**文件：** 修改 `src-tauri/src/auto_title/internal_sessions.rs`、`commands/conversations.rs`及实际公共parser导入入口；新增 `roundtable/registry.rs`、`tests/roundtable_cases/registry.rs`。资格阶段使用独立临时RegistryStore；不修改正式internal_agent_session实体列，也不向旧purpose字符串列写roundtable值。

**接口：** `RoundtableSessionRegistry::reserve_root(PathBuf)->RootLease`、`begin_discovery(AgentType)->DiscoveryLease`、`register(InternalBindingRecord)->RtResult<()>`、`is_roundtable(AgentType,ExternalId,Path)->bool`；记录room/binding/incarnation与reserved root。P09b再把同trait接入新rt_internal_bindings表，不把新值写入旧枚举列。

- [ ] **红灯。** `discovery_before_registration_stays_hidden`、`restart_parser_and_direct_lookup_respect_registry`、`baseline_database_ordinary_sessions_still_open`。register ACK之前靠root/discovery lease过滤；注册后按external_id过滤；按ID详情也隐藏；关闭观察窗不清运行；旧数据库正常会话查询不引用新增列。
- [ ] **运行。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_runtime registry`。
- [ ] **实现。** 所有普通发现/恢复/import统一调用registry判断。资格专用临时目录生命周期由harness保持；生产启动前P09b完成持久映射。降级旧版本无法理解圆桌表：历史保留但圆桌不可用；必须先disable/drain再降级，旧版不得自动导入reserved roundtable根；如旧版无法保证过滤，则不支持直接对该data_dir降级，使用升级前备份，绝不静默“兼容”。
- [ ] **绿灯与提交。** 同命令、基线DB入口回归；P08验证真实sidebar零导入。提交 `feat(roundtable): suppress internal session discovery`。

### P07e 封闭完整启动能力并验证真实失败语义

**依赖与出口：** P06a/P06b/P07a/P07b/P07c/P07d；DEV 静态与 fake，LINUX 真实资格。此项是 P08 和 G1 的新增硬前置，不后移到 UI 阶段。

**文件：** 新增 `src-tauri/src/roundtable/capabilities.rs`、`tests/roundtable_cases/capability_boundary.rs`；修改 roundtable runtime/companion/qualification，以及 `acp/connection.rs` 的专属 service 分支。保持普通用户会话原行为。

**接口：** `LaunchCapabilityManifestV1` 完整列出 launcher、adapter、helper、配置、环境键集合、MCP server 和每个工具名、原生 fs/shell/subagent、网络目的地、挂载与继承 FD。敏感值不写入 manifest；版本和规范 hash 纳入 qualification/policy。`verify_service_manifest` 对未知能力和普通 companion token 拒绝，不依靠 UI 开关隐藏。

**先红灯：** service 模式只广告且只可调用 read_evidence/search_evidence/submit_result；将普通设置中 browser、browser_eval、computer、computer_launch、computer_clipboard、delegation、sessions、ask、feedback、tasks、automations、taskboard 全开，服务仍精确三工具。伪造普通 token、旧 feature flag、tool 名、附加 MCP、全局配置、原生 shell 外联均不能扩大能力。HostToolsPolicy::Agent 单独开启的对照必须无法取得资格。

**失败语义：** P07b 的 completion 只有在有序 ingress 与已准入 MCP handlers 全部 drain 后才裁决。关联同 fence/turn 的 severity=error sessionFailure，无论来自 update 还是 prompt response 元数据，都阻止 accepted；end_turn 不是成功证明。HTTP400+end_turn、先提交候选后 response error、迟到但在 barrier 内的 error、旧 turn error、warning、重复 error 各有 fixture。旧 turn 不污染新 attempt；未知失败记录形状 fail closed 并标明 adapter 不兼容，不凭空新增公共错误码。

**实现与绿灯：** 服务入口从空能力集合构建独立 manifest，不调用普通 flags 后再删几个字段；dispatch、宿主 broker 和启动清单同时强制白名单。重用现有 typed sessionFailure 解析，不重复实现脆弱字符串识别。执行 `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_runtime capability_boundary`；确认具名测试非零，再跑 private_ingress、companion、runtime_admission。真实路径列入 P08；fake 成功不发证。提交 `feat(roundtable): seal launch capabilities and completion failures`。

### P08 执行并裁决首适配器可行性门

**依赖与出口：** P01/P02/P04/P05/P06a/P06b/P07a–P07e；M0b/G1，仅在具体实验获批准后执行。T04/T05/T06/T07/T18。

**文件：** 新增 `src-tauri/tests/roundtable_cases/roundtable_qualification.rs`（真实实验标记ignored）与 `docs/roundtable/qualification/linux-codex-2.1.1/{report.json,evidence-index.md}`；可修改P06精确profile，变更后从头验资格。

**接口：**
```rust
pub async fn qualify_adapter(profile: &QualificationProfile,
    approval: &QualificationApproval,
    cases: &[QualificationCase]) -> RtResult<QualificationReport>;
// 报告键包括全部实际版本/hash；passed只在六项能力及私有事件、侧栏/发现过滤、凭据不可达、实际二进制匹配、共享核心hash全部有证据时生成。
// QualificationApproval：实验ID、允许模型/资料、attempt上限、费用限额、有效期。
```

**必需报告字段与失败条件：** QualificationReport记录实际执行树hash、shared_core_hashes={canonical,delivery_encoder,validator,tool_core,request_accounting,ingress}、精确profile、probe input/output hash、时间、缺失项和异常。资格链复用P07c的实际核心；P10/P11不能重新编码。核心改变即旧报告失效并重跑受影响资格，全产品G3再验证持久适配器。未获明确批准的真实实验保持ignored。报告入库前剔除token/env/Authorization/真实私密资料，只留合成fixture和可校验trace；任何必需检查缺证据不得passed。

**额外反例：** P00实际endpoint未通过或native read绕过上下文边界必须failed。真实CLI拉起companion必须使用env token/挂载socket/EOF生命周期，不能拿假FD测试充数。报告单列actual_resolved_binary_matches_certificate、ordinary_sidebar_imports=0、global_body_events=0，以及全无model_credential_material_in_sandbox。六能力全部true而上述任一缺失也失败。取消/重启后按instance labels枚举且零孤儿；API凭据与ChatGPT账号认证范围分别记录。

- [ ] **1. 写失败测试。** 先用fake broker在 `qualification_report_requires_all_evidence` 中故意缺new_session、roundtable_mcp、ordered_turn_completion、cancel_and_reap、strict_isolation、bounded_context_delivery中的任一项，断言不能passed。批准真实实验后用合成非敏感fixture，不直接上传项目；从隔离前spawn、额外MCP污染、读取诱饵、网络出口到submit receipt→ACP normal response有序链逐项留下hash/时间/trace。fixture主持participant=null，2成员+独立主持；延迟尾部update、已准入MCP、取消忽略TERM子进程都必须实测。断言：
```rust
assert!(report.required_capabilities.iter().all(|c| c.status==CheckStatus::Passed));
assert!(report.model_credentials_visible_to_agent==false);
assert!(report.idle_processes_after_reap==0);
assert!(report.normal_completion_after_submit_receipt);
```
付费前验证token上界算法和模型容量出处；如果不能获得可靠边界，结果为capacity_unknown/failed而不是估计通过。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_runtime roundtable_qualification::qualification_report_requires_all_evidence`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** 先只实现报告校验和ignored实验入口。实际执行命令另为 `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_runtime roundtable_qualification -- --ignored --nocapture`，只能在用户确认具体profile、合成资料、模型服务、费用限额后运行，并消耗资格实验预算，不自动安装/升级/登录。不启动外部云端编码任务或ChatGPT Work。成功报告只为精确组合发证；端点重定向/代理支持不足、native工具泄漏、tool receipt顺序不可证均阻断。报告记录failed/not_tested/expired，不用“看起来可用”。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：G1未过不启动P09以后全面实现；评审明确允许、拒绝或修改候选，不让实验自动变成产品上线。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "test(roundtable): record first adapter qualification verdict"`；不 push、不 merge。

### P09a 独立修复逐连接SQLite配置

**依赖与环境：** G1；DEV可用SQLite，足够内存验证机。全应用基础设施单独提交，不藏进圆桌表迁移。

**文件与接口：** 修改 `src-tauri/src/db/mod.rs`，新增 `tests/roundtable_cases/db_profile.rs`；`open_configured_sqlite(&DbOpenOptions)->RtResult<DatabaseConnection>`、`verify_connection_profile(&DatabaseConnection)->ConnectionProfileReport`。通过已有SeaORM/sqlx连接配置接缝设置每个连接；若需显式sqlx依赖，只可锁中同版本且先批准，不擅加版本线。

- [ ] 红灯：同时占用五个不同物理连接并检查 foreign_keys=ON、busy_timeout=5000、journal_mode=WAL、synchronous=NORMAL、cache_size=-8000；销毁/重开也一致。测试重点是synchronous/cache_size，不能预称另外四条默认foreign_keys一定OFF。
- [ ] 运行 `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_protocol_io db_profile`。
- [ ] 实现逐连接hook/配置，并回归基线数据库正常应用入口、普通会话/迁移；默认只承诺进程崩溃恢复。若另批FULL，单独性能、兼容与断电故障门，不在本任务暗中改全库耐久等级。
- [ ] 同命令绿灯和数据库既有目标回归后提交 `fix(db): initialize every SQLite pool connection`。P09b仅消费此接口，不重复修基础设施。

### P09b 新增持久模型和登记适配器

**依赖与出口：** G1通过；P01/P07d；P09a。T03/T10/T19。

**文件：** 新增 `src-tauri/src/roundtable/{store,schema}.rs`、`src-tauri/src/db/migration/m20261003_000001_roundtable.rs`、`src-tauri/tests/roundtable_cases/roundtable_store_schema.rs`、`tests/roundtable_support/mod.rs`；修改既有 `db/{mod.rs,migration/mod.rs}`。P07d RegistryStore在本迁移接入新rt_internal_bindings表；既有internal_agent_session实体/字符串枚举不变。

**接口：**
```rust
pub async fn open_roundtable_store(conn: DatabaseConnection)
    -> RtResult<RoundtableStore>;
pub async fn verify_connection_profile(pool: &DatabaseConnection)
    -> RtResult<ConnectionProfileReport>;
pub async fn migrate_roundtable(conn: &DatabaseConnection) -> RtResult<()>;
```

- [ ] **1. 写失败测试。** 复用P09a已通过的ConnectionProfileReport，不重复修改全库配置。`roundtable_constraints_reject_cross_room_graph` 覆盖ordinal/phase revision/turn/attempt/event/command/submission唯一约束，主持speaker非空，跨room claim/evidence外键不通过，同turn active或uncertain只一个。迁移重复运行无重复表/行，失败后圆桌不可启动而旧会话可读。断言：
```rust
assert_eq!(report.distinct_connections,5);
assert!(report.connections.iter().all(|c| c.foreign_keys && c.busy_timeout_ms==5000));
assert_eq!(cross_room_insert.unwrap_err().code,ErrorCode::InvalidArgument);
assert_eq!(count_active_attempts(turn).await,1);
```
'独立进程崩溃恢复'与'断电持久'两个profile分开测试。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_protocol_io roundtable_store_schema`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** 所有物理表使用rt_前缀，新增rt_launch_intents支持P06a LaunchIntentStore、rt_internal_bindings支持P07d RegistryStore，包含v1.1 §10.1完整逻辑模型，业务列/关系不可全部藏进不受约束JSON。不可变body/projection可JSON，唯一键与room一致性使用组合FK/条件更新。连接配置仅消费P09a接口，本任务不引新sqlx依赖或改变NORMAL/FULL政策。取消owner lease/outbox/approval表。Store用短事务，无模型/进程等待。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：5个不同物理连接证据；migration注册而不是只建测试库；正确性不依赖未经证实BEGIN IMMEDIATE。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "feat(roundtable): persist constrained roundtable state"`；不 push、不 merge。

### P10 构建不可变资料、对象写入与证据索引

**依赖与出口：** P02/P09；T17/T18/T19。

**文件：** 新增 `src-tauri/src/roundtable/{snapshot,objects}.rs`、`tests/roundtable_cases/roundtable_snapshot.rs`；增加fixture文件位于 `src-tauri/tests/fixtures/roundtable/snapshot/`。

**接口：**
```rust
pub async fn capture_snapshot(selection: SourceSelection,
    limits: SnapshotLimits, objects: &ObjectStore) -> RtResult<SourceManifestV1>;
impl ObjectStore {
    pub async fn put(&self, bytes: &[u8]) -> RtResult<ObjectRef>;
    pub async fn get_verified(&self, object: &ObjectRef) -> RtResult<Vec<u8>>;
}
pub fn freeze_phase(input: PhaseInput) -> RtResult<PhaseSnapshotV1>;
pub fn build_delivery(snapshot: &PhaseSnapshotV1, role: &RoleSnapshot,
    binding: &BindingId, capacity: &dyn TokenBound,
    profile: &QualifiedContextProfile) -> RtResult<DeliveryManifestV1>;
```

**确认与空间：** P10只负责真实资料来源/持久对象适配，P02编码器原样复用。`freeze_preflight(&ActorContext,&RoomDraft,&SourceManifestV1,&ResolvedRecipients)->RtResult<PreflightRecordV1>`按A02创建服务端记录，start引用该ID；`confirmed_manifest_cannot_change_before_start`验证原文件变化不重抓、provider映射变更必重新确认。`ReservationLedger::reserve_capture(principal,estimated_bytes)->StorageLease`在读取/临时写入之前保留全目录空间；并发preflight不能各自通过后超额。失败部分副本只有确认无引用且清理成功后退预留。

- [ ] **1. 写失败测试。** `dirty_and_untracked_change_manifest` 同base_commit修改已选dirty/untracked字节，manifest_hash不同；冻结后修改原文件，读取仍是原snapshot。`capture_rejects_escape_and_mutation` 测绝对路径、..、symlink、跨根、device/socket及读取中变化；有界重读建议最多2次（新实现决定、写入profile），仍变化报snapshot_unstable。测试非法UTF-8标binary、不可进文本工具，CRLF/无末行换行line offsets准确。`blob_failure_never_commits_reference` 在write/fsync/rename/目录fsync/hash/DB commit逐处失败。断言：
```rust
assert_ne!(before.manifest_hash,after.manifest_hash);
assert_eq!(frozen_bytes,b"selected dirty contents\n");
assert_eq!(unstable.unwrap_err().code,ErrorCode::SnapshotUnstable);
assert_eq!(delivery.exact_bytes,delivery.prompt_utf8.len() as u64);
```
Windows式路径用输入字符串fixture；大小写双文件在测试临时目录动态生成，先探测文件系统大小写能力，不支持时带原因跳过该物理fixture并保留纯路径比较断言。不能提交Windows无法checkout的同目录碰撞文件。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_protocol_io roundtable_snapshot`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** 从受控句柄复制实际字节，read-before/after metadata验证，拒绝symlink且不用硬链接；来源分类tracked/dirty/untracked、encoding/size/mode/capture时间、内容hash/行表全部记录。manifest按规范化路径字节序排序；base_commit仅元数据。输入、角色和模板分别hash；semantic context不含deadline/资源等待/C运行变更。delivery含绑定及实际bytes；新binding context_state=fresh/前置游标空。来源排除清单与模型接收方回显给用户确认，秘密名默认排除不能代替确认。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：文件间不是原子时刻；读取只能来自manifest对象；预检容量与实际构建再检相互一致。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "feat(roundtable): freeze content-addressed evidence snapshots"`；不 push、不 merge。

### P11 接通 attempt token、证据工具与候选存储

**依赖与出口：** P04/P07/P09/P10；T04/T06/T07/T17/T18。

**文件：** 新增 `src-tauri/src/roundtable/mcp.rs`、`tests/roundtable_cases/roundtable_mcp.rs`；扩充既有的新 `roundtable/{companion,store}.rs`。

**接口：**
```rust
pub async fn dispatch_tool(token: &AttemptToken, call: RoundtableToolCall,
    authority: &dyn ToolAuthority) -> RtResult<RoundtableToolResponse>;
#[async_trait::async_trait]
pub trait ToolAuthority: Send + Sync {
    async fn admit_tool(&self, token: &AttemptToken) -> RtResult<AdmittedToolScope>;
}
pub async fn read_evidence(scope: &AdmittedToolScope,
    args: ReadEvidenceArgs) -> RtResult<EvidenceSlice>;
pub async fn search_evidence(scope: &AdmittedToolScope,
    args: SearchEvidenceArgs) -> RtResult<EvidenceSearchPage>;
pub async fn persist_candidate(scope: &AdmittedToolScope,
    submission_id: SubmissionId, raw: Vec<u8>) -> RtResult<CandidateReceipt>;
```

**共享实现与事务并发：** `DurableToolStore`实现P07c的ToolStore；不复制解析、别名、schema、回复编码或配额逻辑。同attempt token准入、handler登记和所有回复配额扣减均线性化。`concurrent_valid_submissions_seal_once`断言两个同时有效只有1 candidate且两receipt行为确定；`concurrent_invalid_third_fourth_close_once`断言invalid_count=4、closed=true、后续无候选；同ID不同payload冲突，不因重试免配额。旧`scope`不能在stop后通过延迟事务写accepted，候选即使留审计也不可提升。

- [ ] **1. 写失败测试。** `token_scope_and_tool_budget`：token绑定完整fence/身份/工具版本/可见别名，不接受模型指定room/attempt；过期、跨attempt、stop后新工具拒绝。每次编码响应8192字节、累计32768边界；插话配额16384。read只file_alias和有界行，search仅有界字面量，regex元字符当文本。`candidate_receipt_survives_lost_reply` 先commit后丢reply，同ID重试得原receipt；取消/abnormal finish永不accepted。断言：
```rust
assert!(slice.encoded_bytes <= 8192);
assert!(usage.returned_bytes <= 32768);
assert_eq!(retry_receipt,first_receipt);
assert_eq!(accepted_count_after_cancel,0);
```
peer staged evidence拒绝；证据owner/staged_phase/revision/published_seq均正确；stop前准入只读可完成但不能晚写accepted。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_protocol_io roundtable_mcp`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** 本任务先由测试authority接P05 gate；P13的actor实现ToolAuthority。工具请求gate准入，取得不可变句柄与handler注册后离gate做I/O；正常完成屏障只等待当时已准入集合。persist_candidate调用P04纯校验后短事务写submission，超过3次不合格关闭attempt。证据返回精确excerpt/line或byte范围、total_lines/has_more和E别名；URL/CLI原生read标unverified。通道只认专用服务owner，不用CompanionLeaseRegistry当房间租约。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：field_errors不会误发accepted；token撤销、配额扣减和并发handler注册必须线性化。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "feat(roundtable): persist scoped evidence and submissions"`；不 push、不 merge。

### P12 实现原子接纳、closing 集合与完整投影

**依赖与出口：** P03/P09/P11，且先完成本任务的时钟前置子步骤；T09/T10/T14/T19。

**P12 时钟前置：** 在编写 acceptance 前新增 roundtable/clock.rs 的 MonoClock 与 FakeClock、截止判定接口及不可回拨测试；由 P12 拥有其首次引入，P14 只接入监督器和账本，不再后置定义 P12 已需要的接口。accept 在 DB 等待后、commit 前重采样，同 A07；精确相等为超时。测试阻塞事务、UTC 跳变与 commit 晚返回。这样不引入 P12→P14→P13→P12 环。

**文件：** 新增 `src-tauri/src/roundtable/acceptance.rs`、`src-tauri/roundtable-protocol/src/projection.rs`、`src-tauri/tests/roundtable_cases/roundtable_acceptance.rs`；扩充 `roundtable/store.rs`。

**接口：**
```rust
impl RoundtableStore {
    pub async fn accept(&self, input: AcceptInput) -> RtResult<ProjectionRef>;
    pub async fn close_phase(&self, input: CloseInput) -> RtResult<ClosingSetRef>;
    pub async fn publish(&self, input: PublishInput) -> RtResult<ProjectionRef>;
    pub async fn projection(&self, room: &RoomId,
        id: Option<&ProjectionId>) -> RtResult<ProjectionV1>;
}
pub fn project(state: &RoomAggregate) -> RtResult<ProjectionV1>;
```

**时钟精确定义：** `AcceptInput { completion:RuntimeTurnCompleted, candidate_id, fence, deadline_mono }`不接受调用者预先给的now；Store从注入MonotonicClock取A07的最终事务决策样本。延迟获取写锁、每步SQL、最终commit分别设故障点。前两者导致决策时到期则全回滚；最终commit晚于deadline但决策早于deadline按A07记录decision/commit审计，失败commit无accepted。若A07未批准，此任务不得擅自选择另一时点。

- [ ] **1. 写失败测试。** `acceptance_rolls_back_every_semantic_row` 在message/claims/responses/position_changes/evidence/budget/revision/projection/event每步注入失败，每次断言全体表无accepted半状态。正常接受1次，重复回调只1message；completed必须主持published且cleanup全真。`close_freezes_and_publishes_ordinal` 按2/0/1完成但发布0/1/2；deadline相等拒绝；Q达标但slot未终结仍running。`commit_without_ui_ack_does_not_rerun` commit后丢通知，replay读同一结果，admitted次数不变。断言：
```rust
assert_eq!(after_failure.accepted_messages,0);
assert_eq!(after_success.accepted_messages,1);
assert_eq!(published_ordinals,vec![0,1,2]);
assert_eq!(events.len(),projection_versions.len());
```
每个projection校验room/seq/resulting_revision/hash及所有消息/证据manifest引用存在。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_protocol_io roundtable_acceptance`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** AcceptInput只能来自当前actor持gate、normal finish、完整barrier与sealed candidate；事务重验room/phase/fence/turn/deadline/证据可见性。业务关系、预算、revision、一个完整ProjectionV1及一个event原子提交。close用CAS冻结accepted/outcome/context/fence，写未完cancel意图；cleanup后publish重验覆盖控制再写immutable membership/evidence published_seq/coverage，创建下一ready。无控制但不足Q则phase_failed；主持失败进入paused+synthesis_failed。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：不能先给UI或MCP说accepted再commit；DB busy只重试纯事务，不重发prompt。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "feat(roundtable): atomically accept close and publish"`；不 push、不 merge。

### P13 装配唯一协调器、actor 与服务所有权

**依赖与出口：** P05/P07/P09/P12；T02/T03/T21。

**文件：** 新增 `src-tauri/src/roundtable/{service,actor,ownership}.rs`、`tests/roundtable_service.rs`；修改既有 `src-tauri/src/{app_state,lib}.rs`、`server_bin/main.rs`。

**接口：**
```rust
impl RoundtableService {
    pub async fn open(config: ServiceConfig, store: RoundtableStore,
        runtime: Arc<dyn ParticipantRuntime>) -> RtResult<Arc<Self>>;
    pub async fn send(&self, room: RoomId, msg: RoomMessage) -> RtResult<RoomReply>;
    pub async fn shutdown(&self) -> RtResult<ShutdownReport>;
}
#[async_trait::async_trait]
pub trait ParticipantRuntime: Send + Sync {
    async fn prepare(&self, launch: RoundtableLaunch) -> RtResult<PreparedRoundtableConnection>;
    async fn cancel_and_reap(&self, identity: RuntimeIdentity) -> RtResult<CleanupProof>;
}
```

**全局冷启动隔离：** `ServiceReadiness=ReadOnly|Recovering|Ready|Blocked{reason}`。持锁后递增boot、先撤旧gateway/ingress、扫描rt_launch_intents与IsolationProvider::discover_owned，重建QuarantineLease；所有旧实例、ACK丢失实例与未知占用逐个核对。不能只阻止room A恢复却允许room B启动。P14执行lease/资源和P15恢复完成之前open只返回非可运行服务；所有生产写入口要求协调器令牌，拿不到锁的RoundtableReadService使用SQLite只读连接且没有mutation方法，pause/interject/create/preflight捕获也不得绕过。

**新增验证：** `crash_recovery_blocks_other_room_until_cleanup`保留A子进程杀掉宿主，再获锁的B start必须capacity_limited/runtime_unavailable；清理全证后才可Ready。`spawn_succeeded_but_registration_ack_lost_is_discovered`、`partial_cleanup_cannot_release_bundle`、`pid_reuse_does_not_match_instance`。双进程用 `std::env::current_exe()` 重启同测试二进制并加过滤/角色env，不新增src/bin辅助程序。文件锁只证明所有权，不证明静止。

- [ ] **1. 写失败测试。** `two_processes_one_coordinator` 启动两个测试辅助进程指向同一规范DB目录，只有一个能写/运行；另一历史只读且start/resume拒绝，automation lock不构成通过依据。持锁重启boot_epoch增加，旧boot回调全拒绝。`actor_gate_enforces_fake_races_on_real_store` 把P05 A/B调度在真实actor+DB重跑；bootstrap未返回也可清理。断言：
```rust
assert_eq!(writable_services,1);
assert!(new_boot>old_boot);
assert_eq!(old_callback.code,ErrorCode::StaleFence);
assert_eq!(post_stop_new_admissions,0);
```
关闭观察窗口不取消room；应用退出先撤准入、清理、持久状态，最后释放lock。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_service`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** OS文件锁复用现有automation的跨平台锁技术但单独文件/所有权，lock路径由规范化DB身份确定，持句柄到cleanup完成。锁后短事务增加全局boot_epoch；每room一个actor且所有控制/接纳/最终enqueue/工具注册共用gate。runtime任务只发ReadyToEnqueue消息；actor持gate提交admitting后调用预取queue handle的try_enqueue，再记录admitted；记录失败进入uncertain。Web和Tauri从同一AppState取Arc服务，不各建实例。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：进程PID不是incarnation；gate不持有网络等待；锁失败仍支持授权历史读取。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "feat(roundtable): own coordinator lifecycle and room actors"`；不 push、不 merge。

### P14 实现运行预算、预扣计时与原子资源许可

**依赖与出口：** P02/P13；T12/T13/T21。

**文件：** 新增 `src-tauri/src/roundtable/{clock,resources,budget_ledger}.rs`、`tests/roundtable_cases/roundtable_budget_runtime.rs`。

**接口：**
```rust
pub trait MonotonicClock: Send + Sync { fn now_ms(&self) -> MonoMs; }
impl ResourceAllocator {
    pub fn try_acquire(&self, room: RoomId, slots: u32) -> RtResult<PermitBundle>;
    pub fn release(&self, bundle: PermitBundle, proof: &PermitReleaseProof) -> RtResult<()>;
}
pub async fn checkpoint_active(store: &RoundtableStore, room: &RoomId,
    now: MonoMs, reserve_next_ms: u64) -> RtResult<TimeLedger>;
pub async fn reserve_budget(store: &RoundtableStore,
    request: BudgetReservationRequest) -> RtResult<BudgetReservation>;
```

**不依赖DB响应的撤销：** `ExecutionLease { generation, prepaid_until:MonoMs, revoked:AtomicBool }`在当前service内由独立监督任务驱动，queue/gateway/tool入口均检查；actor等5s SQLite busy时，≤1s的预付片到期也能关门并启动清理。`checkpoint_db_stall_expires_local_execution_lease`阻塞存储超过1000ms，断言新入队/网关转发/工具准入均0，已有远端工作只标residual，UI不得无持久commit显示stopped。释放阻塞DB后，即使旧checkpoint晚到成功也不得重开lease；续期CAS要求generation仍当前且unrevoked、旧prepaid区间尚未过期，过期后只能经新的获准恢复/准入建立lease。新增 `late_checkpoint_ack_cannot_resurrect_expired_lease` 断言晚ACK后新入队/转发/工具仍0。持gate DB等待也必须受同监督deadline限制，超时撤销本地执行；不得靠继续借未来预算维持运行。

**proof与字节预留：** `ResourceAllocator::release(PermitBundle,PermitReleaseProof)`完整比较lease_id与全部incarnation集合。单个ProcessTreeProof不能释放多slot；未能枚举或partial proof保留quarantine。A04内部checkpoint不发业务event；业务事务取同账本样本。存储/事件收敛预留与控制预算一起CAS，测试并发capture、disk full和满额终态仍有保留空间。

- [ ] **1. 写失败测试。** `active_room_permit_is_global_and_atomic` 两room同时start仅一获准，其它429 capacity_limited；resume remaining=1/C=3只取1，不足时0占用；permit到reap后才释放。`prepaid_monotonic_slices` fake clock验证≤1000ms预扣、存储失败撤准入且仍清理；UTC前后跳不改deadline；N并发只扣room墙钟；队列/paused不扣；崩溃余片不退款。断言：
```rust
assert_eq!(ledger.active_ms_after_three_parallel_1s,1000);
assert_eq!(bundle.slot_count(),1);
assert_eq!(held_slots_after_failed_acquire,0);
assert!(ledger.cleanup_overrun_ms>0);
```
第二次launch失败turn终结；两次已准入均暂停后不能再补第三次；减C重新算剩余时间，拒绝时不暗中加额度。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_service roundtable_budget_runtime`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** 一个service级分配器涵盖所有start/resume/restart/retry_synthesis，all-or-none，不逐slot等待。budget reservation与控制/重试决策同事务，保留所有未来首次尝试；首次ordinal优先再重试。1s以内预扣是执行续行前提，正常停顿仅退款已知未用片；timeout严格边界后禁accept和新prompt但继续cleanup。scratch/log/helper上限由profile测量和执行，超额终止attempt，0闲置CLI。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：default30分钟是最长允许预算而非预计耗时；不能把ACP attempts当计费请求数。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "feat(roundtable): enforce active budgets and cleanup permits"`；不 push、不 merge。

### P15 实现幂等控制、恢复矩阵和主持单独重试

**依赖与出口：** P12–P14；T10/T11/T12/T13/T21。

**文件：** 新增 `src-tauri/src/roundtable/{control,recovery,command_processor}.rs`、`tests/roundtable_cases/roundtable_control_recovery.rs`。

**接口：**
```rust
pub async fn apply_command(actor: &RoomActor, principal: PrincipalId,
    command: MutationCommandV1) -> RtResult<MutationAck>;
pub fn recovery_action(state: &RecoveryState) -> RtResult<RecoveryAction>;
pub async fn advance_control(actor: &RoomActor,
    operation: OperationId) -> RtResult<ControlOperationV1>;
pub async fn recover_service(service: &RoundtableService) -> RtResult<RecoveryReport>;
```

**恢复顺序：** recover_service只能在P13全局Recovering阶段运行，先核对旧实例/清理、重建许可与prepaid风险，再按八行矩阵恢复；最后全部可确认静止且P14监督器在线才置Ready。任何新room同样受此全局屏障。所有start/resume/restart/retry_synthesis/自动下一阶段都在最终enqueue重查ExecutionGate，不因初次start获准而永久绕过disable。

- [ ] **1. 写失败测试。** `closing_control_matrix_at_every_crash_point` 对v1.1 §11.5八行矩阵逐行，且在requested/revoking/cleaning/applying/done边界crash重开：无控制按冻结集合发布；pause保留集合不补slot；restart旧集合不发且唯一successor；stop只stopped；running崩溃paused+recovery_required；published不重复；stopping继续清理；丢ACK按原operation恢复。`idempotency_processing_and_stop_supersession`：同principal/api_major/request同hash重放原ack，不同hash409；processing崩溃查事务/operation证据后再决策。两个不同restart第二个control_in_progress，stop可覆盖、其他不能覆盖stop。断言：
```rust
assert_eq!(successor_count,1);
assert_eq!(published_old_revision_after_stop,0);
assert_eq!(retry_synthesis_admitted_speakers,vec![moderator]);
assert_eq!(budget_rejected_snapshot,before_restart);
```
next_phase到synthesis返回no_next_phase；边界竞争响应实际target phase；连续pause耗尽turn给cannot_reach_quorum出口；recovery_consent=false不发新prompt。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_service roundtable_control_recovery`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** 命令scope=(principal,api_major,request_id)，canonical hash含全部有效参数/默认值不含凭据；唯一冲突查询既有行。长操作提交operation后command=completed回受理ack，步骤CAS推进。restart破坏进度前同事务预留当前replacement+后续最低次数/时间，再增epoch。恢复closing事务把决策授权绑定新boot与recovery_operation、保留旧closing来源；旧runtime不得写。uncertain先证明本地静止再释放permit，远端unknown另记并向用户要费用风险确认。retry_synthesis新revision同公共hash只跑主持；插话改变hash须restart。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：进度不可逆前先预留；预算不足保持原工作；blocked有preflight/克隆/停止或人工清理出口，不能永久无解释卡中间态。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "feat(roundtable): recover idempotent fenced controls"`；不 push、不 merge。

### P16a 落实计量和失败诊断

**依赖与出口：** P09/P10/P13–P15；T19/T20/T21。

**文件：** 新增 `src-tauri/src/roundtable/{diagnostics,usage}.rs`、`src-tauri/roundtable-protocol/src/usage.rs`、`tests/roundtable_cases/roundtable_durability_usage.rs`。maintenance与backup/restore的文件和验收移至P16b，不在本提交混入。

**接口：**
```rust
pub fn fold_measurement(state: &UsageState, m: MeasurementV1) -> UsageState;
pub async fn archive_late_measurement(service: &RoundtableService,
    attempt: AttemptId, m: MeasurementV1) -> RtResult<()>;
pub async fn seal_diagnostic(input: DiagnosticInput) -> RtResult<DiagnosticRef>;
```

- [ ] **1. 写失败测试。** `usage_scope_dedup_and_reset` 同counter cumulative=10,15,15只累计15；同epoch乱序seq=2/value15、seq=1/value10、seq=3/value20，confirmed=20；只有可信reset/lifecycle变化才新epoch，无序且下降无法判别时记uncertain（A06）；occupancy=1000不加收费；缺数据总价unknown；unattributed不强塞本turn。终态旧epoch迟到measurement只改计量记录，不变room status/revision/accepted/published/调度。`diagnostic_is_bounded_and_redacted` 超64KiB保留前后片段、total/hash/truncated，secret被脱敏，thinking丢弃。备份/GC故障测试由P16b负责。断言：
```rust
assert_eq!(usage.confirmed_output_tokens,Some(15));
assert_eq!(room_after_late_usage,room_before);
assert!(diagnostic.assistant_excerpt_bytes<=65536);
```
真实断电/文件系统故障VM试验单独记录，不用单元测试替代。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_protocol_io roundtable_durability_usage`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** MeasurementV1完整保存unit/source/scope/semantics/counter_id/epoch/dedupe/value可空/observed_at；当前service计量入口单独授权归档旧attempt，不放宽业务fence。诊断到房间未来明确删除前保留，配额不足preflight拒绝或可见截断诊断，不能删证据。正常cleanup仅清scratch；FULL与断电承诺必须有实测profile证据。P16b消费DiagnosticRef和已冻结对象引用。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：日志不含token/思考；孤儿GC与尚未commit新blob竞争；正常退出不清消息/证据。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "feat(roundtable): retain bounded usage and diagnostics"`；不 push、不 merge。

### P16b 实现一致备份和安全对象清理

**依赖与环境：** P09b/P10/P16a；DEV；断电承诺另需专用VM/文件系统实验。

**文件与接口：** 新增 `roundtable/maintenance.rs`、`tests/roundtable_cases/backup_gc.rs`；修改 `commands/backup/{manifest,sections,source,restore}.rs`，均src-tauri/src前缀。`backup_roundtable(&RoundtableStore)->RtResult<BackupManifest>`；`gc_unreferenced(&RoundtableStore,AuditUtc)->RtResult<GcReport>`。

- [ ] 红灯：`backup_gc_and_disk_failures`固定DB水位+全部引用清单，缺任一对象或hash错则恢复只读且禁止运行；历史manifest、诊断、source、projection、备份和pending写入lease引用对象均不可删；age<24h不可删；并发capture临时对象不能被GC抢删。断言恢复不借当前文件替旧对象、referenced_object不在deleted中。
- [ ] 运行 `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_protocol_io backup_gc`。
- [ ] 实现先pin manifest/字节预留再复制并验hash，结束/失败后释放已知占用；GC先用引用快照标记，再删除前重验引用/lease且宽限≥24h。备份临时复制占用进入A04全局账本。无用户删除API不等于存储无上限。
- [ ] 同命令、backup既有回归绿灯后提交 `feat(roundtable): preserve consistent backup and object references`；任何断电结果单列，普通测试不作断电证据。

### P17 实现不可变分页、授权订阅和预览通道

**依赖与出口：** P12/P13/P15/P16；T14/T15/T16。

**文件：** 新增 `src-tauri/src/roundtable/{events,authorization,paging}.rs`、`tests/roundtable_cases/roundtable_events.rs`；修改既有 `src-tauri/src/web/{ws,ws_attach,event_bridge}.rs`。

**接口：**
```rust
pub fn authorize_room(actor: &ActorContext, room: &RoomRecord) -> RtResult<()>;
impl SubscriptionHub {
    pub async fn attach(&self, actor: ActorContext,
        request: AttachRequestV1, sink: PrivateClientSink) -> RtResult<AttachReplyV1>;
    pub async fn detach(&self, actor: ActorContext,
        subscription: SubscriptionId) -> RtResult<()>;
}
pub async fn read_manifest_page(actor: &ActorContext,
    request: MessagesRequestV1, store: &RoundtableStore) -> RtResult<MessagePageV1>;
pub fn apply_projection(previous: &ProjectionV1,
    event: &DurableEnvelopeV1, fetched: ProjectionV1) -> RtResult<ProjectionV1>;
```

**授权对象与时钟视图：** 本任务消费A03固定ObjectRef/usage读取contract，不临时造无取数路径引用；大projection/manifest按固定hash对象读取。预算投影只含A04采样账本，不把独立tick混入fold。指标由P13–P16真实计数器提供get(metrics)，包括unknown usage与收敛保留余额。桌面事件实测用附录B带默认feature的roundtable_transport desktop；无默认feature命令只证明core/Web。

- [ ] **1. 写失败测试。** `fold_any_L_H_matches_snapshot` 在create/config/start/attempt/accept/close/publish/input/control/recovery/terminal全cause生成历史，遍历L≤H，fold hash==H projection，含C/binding/revision。未知schema/cause或hash缺失停止且seq不动。`manifest_stays_at_H_after_restart` H取页后restart，旧页body_hash/membership不变。`hot_replay_buffers_gt_H` replay跨多页，>H不可提前应用；重复、乱序、断连、drop wake、缓冲溢出均resync。断言：
```rust
assert_eq!(folded.hash(),at_h.hash());
assert_eq!(old_page_before,old_page_after_restart);
assert_eq!(seq_before_replay_done,l);
assert!(unauthorized_sink.frames().is_empty());
```
completion scope/另room token不可读/订阅/取证据；Tauri仅目标窗口；preview合并40ms保first/last_chunk_seq，speaker含主持，旧incarnation丢。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_transport roundtable_events`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** 先授权并登记有界buffer，再一致性读H；冷返回snapshot引用，热仅在L hash匹配且日志完整时返回固定L/H/generation replay cursor。事件从DB按seq拉取，内存通知只唤醒；周期补拉用于丢唤醒，不设delivered/outbox。签名cursor绑定principal/room/projection/manifest/offset/version，H membership永久保存到房间历史删除。慢客户端先丢preview再detach/resync；accepted后用结果替预览。原始ACP sink与定向投递都禁止legacy emit_event/global broadcaster。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：客户端只在fetched投影验证后替换，不能只收到引用就推进seq；热页与实时帧严格屏障。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "feat(roundtable): replay immutable authorized room projections"`；不 push、不 merge。

### P18 统一桌面、Web 与远程命令闭集

**依赖与出口：** P01/P13–P17；T06/T11/T12/T16。

**文件：** 新增 `src-tauri/src/roundtable/api.rs`、`src-tauri/src/commands/roundtable.rs`、`src-tauri/src/web/handlers/roundtable.rs`、`src-tauri/tests/roundtable_cases/roundtable_api.rs`；修改既有 `commands/mod.rs`、`web/{handlers/mod.rs,router.rs,auth.rs}`、`lib.rs`。

**接口：**
```rust
pub async fn execute(actor: ActorContext, request: RoundtableRequestV1,
    service: &RoundtableService) -> RtResult<RoundtableResponseV1>;
pub fn trusted_desktop_actor(window: &TrustedWindow,
    principal: PrincipalId) -> RtResult<ActorContext>;
pub fn trusted_web_actor(auth: &OperatorAuthorization,
    principal: PrincipalId) -> RtResult<ActorContext>;
pub fn http_status(error: &RtError) -> u16;
```

**不可漂移开始：** `roundtable_start`必须携带confirmed_preflight_id；按A02读取同冻结对象。原文件已变仍A；provider_ref解析版本/实际模型接收方变化返回reconfirmation_required，绝不静默换Y。所有18命令和GetReadV1的各模式均列入fixture，不额外加命令。get(usage)刷新计量版本不动业务seq；get(metrics)只读现有观测。默认feature的Tauri wrapper编译与真实窗口测试独立于core parity。

- [ ] **1. 写失败测试。** `tauri_http_contract_parity` 同18个实际命令（attach/detach分开）共用黄金请求/响应，HTTP POST /api/roundtable_*与invoke一致；GET/PATCH无旁路。body伪造principal拒绝；completion token不能构造operator。错误400/401/403/404/409/422/429/503按规范，未知/不可达room统一404。`command_ack_is_not_terminal_state` pause受理仅pausing，等持久投影才paused。读页默认100/最大500/1MiB，不截断JSON。断言：
```rust
assert_eq!(tauri_json,http_json);
assert_eq!(http_status(&err(ErrorCode::CapacityLimited)),429);
assert_eq!(ack.status,RoomStatus::Pausing);
assert_eq!(unknown_room.status(),hidden_room.status());
```
clone只新draft、不启动；update_draft仅draft且完整config；start重新查证书/资料/容量，模型/effort回显不替换。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_transport roundtable_api`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** AppState持久principal初始化一次，可信桌面和operator token映射同principal，completion scope无操作权。18命令及get的各读取模式逐项调用同execute/authorize_room；list仅该workspace/principal，evidence与历史projection也授权。token走既有Bearer/WS subprotocol，不进URL。远程部署TLS/Origin保护检查在preflight阻断，不扩大为全站CORS重构。只提供闭集按钮。单大对象以受签名对象引用由同命令的对象读取模式获取：在G0 DTO冻结时明确 `roundtable_get {room_id,read:{kind:object,object_ref,cursor?}}`（完整判别联合按A03） 与分块上限，不能发布不可取回的裸引用；此为补齐v1.1对象取数路径的显式提案，不添加绕授权下载路由。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：对象读取模式是新增DTO决定须G0确认；request_id重试不换，force_latest默认false。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "feat(roundtable): expose shared authorized command contract"`；不 push、不 merge。

### P19 实现前端类型、不可变 reducer 和断线续接

**依赖与出口：** P17/P18；G2，T14/T15/T16。

**文件：** 新增 `src/lib/roundtable/{types,api,reducer,stream}.ts`、`{api,reducer,stream}.test.ts`；修改既有 `src/lib/transport/{types,tauri-transport,web-transport,remote-desktop-transport}.ts` 及其定向测试。

**接口：**
```typescript
// TypeScript 接口
export function invokeRoundtable<K extends RoundtableCommand>(
  command: K, args: ArgsByCommand[K]): Promise<ResultByCommand[K]>;
export function reduceProjection(state: RoomView, event: DurableEnvelopeV1,
  projection: ProjectionV1): ReduceResult;
export function createRoundtableStream(transport: RoundtableTransport,
  roomId: string, onState: (state: RoomView) => void): RoomStream;
// RoomStream: attach():Promise<void>; detach():Promise<void>;
// ReduceResult: {kind:"applied",state}|{kind:"resync",reason}|{kind:"upgrade_required"}.
```

**跨语言完整校验：** P01 canonical UTF-8 hex/hash黄金fixture同时由TS读取；u64 wire字符串禁止经Number转换，用BigInt或长度/字典序比较。A01 serializer输出确定字节、crypto.subtle SHA-256验证正文，不仅比较两个服务器hash字段。API层在非安全Web上下文返回可解释的校验不可用，不能降级接受；测试mock subtle缺失与篡改正文但原hash保留的拒绝。热replay完成前visibleSeq=L，最后验证成功才替换。

- [ ] **1. 写失败测试。** `reducer.test.ts` 读后端黄金projection/events，各L/H hash一致；未知cause/schema/缺object不推进，duplicate不回退；C/revision/control/binding都更新。`stream.test.ts` 多页热replay期间>H暂存，完成后按序；overflow/drop/reconnect重新握手；preview按attempt/incarnation/epoch/revision丢旧，主持participant=null可展示。断言：
```typescript
expect(reduceProjection(view, unknown, projection).kind).toBe("upgrade_required");
expect(stream.visibleSeq()).toBe(L); // replay尚未全部完成
expect(stream.view().projectionHash).toBe(atH.projectionHash);
expect(transport.globalListenersForRoomBodies()).toHaveLength(0);
```
api对网络不确定重试同request_id，409只获取最新状态不自动重发破坏性命令。
- [ ] **2. 运行红灯。** `pnpm exec vitest run src/lib/roundtable src/lib/transport`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** 前后端wire字段snake_case，UI层映射不能丢resulting_revision。用现有transport的invoke和新的private subscription接口，远程桌面仍经授权Web服务。对象按ID/hash取指定版本，原子替换RoomView；不把事件当失效通知去拿“当前最新”。RoomStream detach只取消观察，不stop服务。请求并发用generation取消旧响应覆盖新视图，buffer有上限并反馈resync。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：不靠preview恢复业务；旧按钮状态不把ack当完成；frontend哈希使用相同规范字节fixture与Web Crypto SHA-256；缺少安全上下文/crypto.subtle时明确阻断校验，不能改为只比较服务器hash字符串。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "feat(roundtable): add replay-safe client transport"`；不 push、不 merge。

### P20 实现可配置创建页与无付费 preflight

**依赖与出口：** G2与G1均通过；P02/P10/P18/P19。T01/T09/T13/T18。

**文件：** 新增 `src/app/roundtable/page.tsx`、`src/components/roundtable/{roundtable-create,roundtable-preflight}.tsx`及 `.test.tsx`；修改既有 `src/components/layout/sidebar.tsx` 仅增加feature-flag入口、`src/i18n/messages/{ar,de,en,es,fr,ja,ko,pt,zh-CN,zh-TW}.json`。

**接口：**
```typescript
// TypeScript 接口
export function RoundtableCreate(props: {workspaceId: string;
  api: RoundtableApi; onCreated: (roomId: string) => void}): React.ReactElement;
export function RoundtablePreflight(props: {report: PreflightReportV1;
  onConfirmSources: () => void}): React.ReactElement;
// RoundtableApi复用P19接口；创建draft不自动start。
```

**确认UX：** 先create draft再确认型preflight；估算型preflight不允许start。按钮提交用户当前看到的confirmed_preflight_id；配置/接收方/源选择变化立即使本地确认失效。服务端409 reconfirmation_required回到新清单确认，不自动重试同按钮动作。不得从持久flag=true推断当前证书或执行端已Ready。

- [ ] **1. 写失败测试。** `configuration_is_not_three_by_two` 改N=7/R=5/C=2发完整config，显示43基础/60最大ACP尝试；R=0显示“并行咨询”；N=2显示一位失败即无法达Q。默认3/2/3显示最大30分钟；减预算时修复无时隙有说明。`preflight_fail_closed_before_payment` not_tested/expired/policy_unenforceable/capacity_unknown均禁start；preflight不调用runtime prompt。断言：
```typescript
expect(screen.getByText("43")).toBeVisible();
expect(api.start).not.toHaveBeenCalled();
expect(screen.getByRole("button", {name: /开始/})).toBeDisabled();
```
资料manifest/排除项/模型接收方/主持/实际effort/unknown费用全部可见且确认后才start，测试 provider 显式固定 zh-CN，另设 ar/RTL 用例；测试键盘与RTL布局。
- [ ] **2. 运行红灯。** `pnpm exec vitest run src/components/roundtable/roundtable-create.test.tsx src/components/roundtable/roundtable-preflight.test.tsx`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** 创建页使用受控数字输入，合法范围由服务器limits回显；N/R/角色/模型/来源开始后冻结，C仅paused变更。preflight展示精确资料清单、六能力证书、输入容量/生成预留、存储/进程配额、尝试和时间。资料选择冻结由宿主执行，新的sources须新room。static路由用query room，不依赖动态服务端Next路由。i18n补齐现有十语言key，产品中文固定文案与v1.1一致。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：模型服务必然接收所选资料，不能写成绝不外传；入口默认关闭、不恢复DAG或污染普通会话侧栏。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "feat(roundtable): create configurable qualified discussions"`；不 push、不 merge。

### P21 实现详情、控制、引用与安全结果展示

**依赖与出口：** P15/P16/P19/P20；T08/T09/T11/T12/T16/T20。

**文件：** 新增 `src/components/roundtable/{roundtable-detail,roundtable-controls,roundtable-results,safe-roundtable-content}.tsx`及各 `.test.tsx`；新增 `src/lib/roundtable/safe-content.ts` 与 `.test.ts`。

**接口：**
```typescript
// TypeScript 接口
export function RoundtableDetail(props: {roomId:string; api:RoundtableApi}): React.ReactElement;
export function RoundtableControls(props: {view:RoomView;
  onCommand:(command:MutationCommandV1)=>Promise<MutationAck>}): React.ReactElement;
export function SafeRoundtableContent(props: {text:string;
  onEvidence:(id:string)=>void}): React.ReactElement;
export function allowedLink(url:string): string | null;
```

- [ ] **1. 写失败测试。** `controls_reflect_durable_state` 点击“立即暂停，保留已完成结果”先说明丢弃进行中回答/已耗次数；ack后仍显示清理，不提前paused。resume显示补几个slot及已耗尽slot；synthesis_failed只显示“只重试主持”，不足预算给克隆/停止。`results_do_not_invent_consensus` 缺席/未回应/少数风险独立显示；预览标“生成中，不是最终结果”；2/Q仍等其它成员。`unsafe_content_never_executes_or_fetches` raw HTML/script、javascript/data、自动remote image阻止；外链允许http/https需用户点击，evidence走授权API。断言：
```typescript
expect(screen.getByText("等待其余成员")).toBeVisible();
expect(allowedLink("javascript:alert(1)")).toBeNull();
expect(container.querySelector('[src^="https://tracker.invalid"]')).toBeNull();
expect(container.querySelector("script,iframe,object")).toBeNull();
expect(screen.getByText("unknown")).toBeVisible();
```
所有中文文案测试固定 zh-CN；键盘、文字状态、适度aria-live、RTL、主持独立card和实际证据版本均测试。真实浏览器另验证未发生外部资源请求，不能仅依赖jsdom默认不加载图片。
- [ ] **2. 运行红灯。** `pnpm exec vitest run src/components/roundtable src/lib/roundtable/safe-content.test.ts`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** 稳定ordinal布局显示C/N、模型/effort、等待/生成/验证/清理/缺席，roundtable进度独立于Simple workflow。按服务coverage显示，不根据summary猜共识；引用跳到确切message/claim/response/evidence版本。只显示confirmed输出token/unknown数/ACP尝试与room时长，不合成虚构费用。所有控制携带当前revision并处理409刷新；恢复费用风险明确确认，不能自动consent。初版正文优先纯文本加受控引用，Markdown只在同安全测试通过后启用。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：没有删除/导出/运行后改名按钮；外部内容不自动请求网络；quoted引用不等于已支持结论。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "feat(roundtable): show safe results and honest controls"`；不 push、不 merge。

### P22 验证唯一已认证组合的双端完整闭环

**依赖与出口：** P01–P21；G3/M2，所有T01–T21。

**文件：** 新增 `src-tauri/tests/roundtable_cases/roundtable_end_to_end.rs`、`docs/roundtable/acceptance-matrix.md`、`docs/roundtable/qualification/linux-codex-2.1.1/product-regression.md`；新增 `src/lib/roundtable/end-to-end.test.ts`。

**接口：**
```rust
pub async fn exercise_room(case: EndToEndCase,
    driver: &dyn AcceptanceDriver) -> RtResult<AcceptanceReport>;
// EndToEndCase固定config/资料/hash、客户端类型、故障点与期望轨迹。
// AcceptanceDriver的fake与已认证真实实现分开标记，报告不得混用证据。
```

**三种构建与真实覆盖：** 绿灯必须逐行执行 `pnpm rust:check:desktop:low-memory`、`pnpm rust:check:server:low-memory`、`pnpm rust:check:mcp:low-memory`，再四个integration目标、带默认feature桌面target及bin测试；详见附录B与最终门。真实桌面窗口、Linux sandbox、远程TLS/Origin为外部门，不与无Tauri core测试混淆。P00/G1报告不能代替本任务持久工具/真实DB路径重验。

- [ ] **1. 写失败测试。** 先fake全轨迹：reserve→admit→submit→accept→close→publish，分别N=2/3/7，R=0/1/2/5，C=1/2/N；7/5/2允许因真实容量明确拒绝，但fake足额profile必须跑完整。桌面窗口、Web socket、remote desktop共同读同room，关闭观察者不停止。双进程/迟到update/stop/DB busy/崩溃/磁盘满的已定义反例回归。断言：
```rust
assert_eq!(report.member_turns,n*(1+r));
assert_eq!(report.moderator_turns,1);
assert_eq!(report.idle_processes,0);
assert_eq!(report.global_body_leaks,0);
assert_eq!(report.unexplained_intermediate_states,0);
```
真实收费用例单独批准后在P08相同精确profile跑2/1/1和3/2/3；取消、主持失败重试、Web重连、native sidebar扫描必测。证书任何漂移先过期，不继续。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_service roundtable_end_to_end`；随后另行运行 `pnpm exec vitest run src/lib/roundtable/end-to-end.test.ts`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** 按T01–T21逐项附命令、profile、fixture hash、pass/fail和原始证据索引，不能以几条happy-path替代。核验桌面default、server无default及codeg-mcp构建；远程TLS/Origin部署单独验证。真实进程回收包含helper/子进程，slow客户端不拖actor。完整历史backup/restore后继续读旧H清单和投影；没有恢复付费工作直到明确consent。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：fake不是OS隔离证据；真实实验不是普遍平台支持；任何未通过T项显式阻断G3。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "test(roundtable): verify qualified desktop and web lifecycle"`；不 push、不 merge。

### P23 预注册质量门槛、有限启用与回滚

**依赖与出口：** P22；G4/M3，T22及I1–I10最终核验。

**文件：** 新增 `docs/roundtable/{quality-protocol,quality-results,operations,release-gates}.md`、`src-tauri/tests/fixtures/roundtable/quality/`；复用P06a已实现并持久默认关闭的 `src-tauri/src/roundtable/feature_gate.rs`、`tests/roundtable_cases/roundtable_rollout.rs`；修改P20入口读取feature flag，不默认开。

**接口：**
```rust
pub fn may_start(flag: &RolloutFlag, profile: &QualifiedProfile,
    preflight: &PreflightReportV1) -> RtResult<()>;
pub async fn disable_and_drain(service: &RoundtableService)
    -> RtResult<ShutdownReport>;
// RolloutFlag: 默认false；允许的精确QualificationKey集合，不是品牌列表。
```

**已有门禁与评分：** 本任务不首次创建执行gate，只配置P06a持久allowlist、做disable-before-enqueue和重启后仍disabled回滚演练。使用P01封存的质量holdout与附录D原始评分行，全部运行失败保留；80%/90%等仅在预先批准后有裁决效力。无预算匹配基线时结论限“在已报告额外资源下的收益”，不能宣称等预算或因果优越。get(metrics)必须真能读取uncertain、overrun、拒绝原因、事件/存储占用和unknown比例。

- [ ] **1. 写失败测试。** `disabled_blocks_new_admission_but_keeps_history`：flag=false不准新prompt，已有运行走fence/取消/cleanup，history/get不受影响；证书漂移立刻禁启动。T22先冻结合成任务、评分和模式盲化，再单独批准模型实验：同snapshot比较单模型、R=0、R=2；保留事实错误、反例、少数风险与全部失败样本。断言：
```rust
assert_eq!(new_admissions_after_disable,0);
assert!(history_still_readable);
assert_eq!(result.blind_task_set_hash,preregistered_hash);
assert!(result.reports_attempts_and_elapsed_and_unknown_usage);
```
不能用模型自报confidence或JSON合格率宣称质量提升。
- [ ] **2. 运行红灯。** `cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_service roundtable_rollout`。首次应因本任务缺失的接口或断言失败；若失败是依赖下载、编译资源或环境问题，先记录为环境阻断，不能冒充红灯。
- [ ] **3. 最小实现。** 质量建议门槛在P01预注册且G4实验前由用户明确确认：至少12个固定任务（事实核查/方案权衡/反例追踪各4），三模式各跑2次；两位不知道模式的评审独立评分、分歧留档。每组预埋关键错误/实质质疑/少数风险；报告发现率、无依据新增结论、实质回应、少数风险保留及额外attempt/时长。建议启用条件：所有安全门零失败；R2关键错误发现率不低于两基线、关键质疑实质回应≥80%、少数风险保留≥90%、无依据结论率不高于两基线。小样本只称试验结果，不宣称统计优越；任一关键任务倒退需审阅并决定修复或暂不开启。这些是待确认门槛，不是v1.1既有数值。rollback只禁新准入并安全停止，保留历史；更多平台/品牌/live reuse/strict resume/自由讨论另案。
- [ ] **4. 运行绿灯。** 重跑步骤 2，同一测试文件全部通过；再跑本任务直接依赖任务的回归命令。不得删除反例或降低断言来过关。
- [ ] **5. 独立复核并本地提交。** 重点检查：质量门槛必须先定后跑；结果不佳也保留，不按偏好挑样本；用户最后批准才启用。只暂存本任务列出的文件与必要锁文件，检查 `git diff --cached`，再执行 `git commit -m "docs(roundtable): gate rollout on safety and discussion quality"`；不 push、不 merge。

## 7 真实依赖和可执行增量

P00先于产品投资。G0通过后，P01建立契约；以下列出真实边，斜杠只在明确“全部前置”列表使用，不表达并行实现：

- P01→P02；P01+P02→P03；P01→P04（P04测试用冻结targets fixture，不必等待P03算法）；P01+P02+P04→P05
- P00+P01+P02→P06a；P01+P02+P04+P06a→P06b
- P01+P05+P06a→P07a→P07b；P02+P04+P06b+P07a+P07b→P07c→P07d
- P06a+P06b+P07a+P07b+P07c+P07d→P07e
- P01+P02+P04+P05+P06a+P06b+P07a+P07b+P07c+P07d+P07e→P08→G1
- G1→P09a→P09b→P10→P11→P12；P03必须在P12之前通过
- P05+P07a+P07b+P09b+P12→P13→P14→P15→P16a→P16b→P17→P18→P19→G2
- G1+G2→P20→P21→P22→G3→P23→G4

可并行的是P03纯策略与不依赖其算法的资格链；P04不依赖P03产品实现，只消费P01冻结类型和手写fixture。其他串行依赖不再用斜杠暗示可同时落地。P07a–e与P09a/b、P16a/b每个子任务各自独立审查与本地提交。P13服务存在但不运行；P14预算和P15恢复齐备才可Ready，消除不安全中间提交。所有产品入口从P06a起默认关闭。

停线条件：协议补充未批准、候选不通、安装/凭据/实验未获授权、容量未知、隔离/凭据/实际二进制不匹配、旧实例无法枚举或清理、存储/对象不完整、真实安全门失败。不得靠增加prompt次数、弱化profile、借普通会话或放开原生工具绕过。

## 8 验收追踪：T01–T22

| 设计验收 | 任务与具体门 |
|---|---|
| T01 参数化 | P01/P02/P03/P20/P22；2/3/7、0/1/2/5、C合法组合及7/5/2 |
| T02 最终准入 | P05纯反例与P13真实actor/DB；gate A/B、queue full、admitting crash |
| T03 所有权 | P07/P13；双进程锁、boot、bootstrap、PID重用 |
| T04 严格隔离资格 | P06/P08真实OS探针，P11工具范围，P22真实回归 |
| T05 专用连接 | P07/P08/P22；三工具、permission deny、discovery/import不漏 |
| T06 结果通道 | P01/P04/P07/P11/P18；schema、身份、receipt、cancel不accept |
| T07 完成边界 | P05/P07/P08/P11；ACP水位+已准入MCP集合、迟到闭门 |
| T08 可执行阶段 | P03/P04/P21；非空claim、abstain、目标冻结与覆盖 |
| T09 隔离/barrier | P03/P12/P20/P21；同阶段不可见、Q不提前、ordinal、主持支持边 |
| T10 原子接纳 | P09/P12/P15；全关系故障注入、ACK丢失、命令幂等 |
| T11 控制恢复 | P15/P18/P21；八行矩阵、每步crash、stop覆盖、唯一successor |
| T12 重开预算 | P02/P14/P15/P21；破坏前预留、连续暂停、主持单独重试 |
| T13 时间 | P02/P14/P15/P20；225/450/1800、UTC跳变、C降低、checkpoint/overrun |
| T14 投影 | P01/P12/P17/P19；任意L/H hash、未知cause不推进 |
| T15 分页订阅 | P17/P19；旧H固定清单、热页屏障、慢客户端、预览批次 |
| T16 权限/泄漏 | P07/P17/P18/P19/P21/P22；双端授权、原始ACP私有、安全文本 |
| T17 快照 | P10/P11；实际dirty/untracked、unstable、证据字节与发布归属 |
| T18 上下文 | P02/P06/P08/P10/P11/P20；完整上界、fresh binding、容量未知拒绝 |
| T19 耐久 | P09/P10/P12/P16/P22；逐连接/故障窗口/备份GC；断电独立外部门 |
| T20 计量 | P16/P21；scope/semantics/dedupe/reset、unknown和受限迟到归档 |
| T21 资源/活性 | P05/P13/P14/P15/P16/P22；原子permit、首次优先、0闲置、本地清理后推进 |
| T22 讨论质量 | P03/P21机械coverage；P23固定盲评，不与JSON合法性混淆 |

## 9 评审处理追踪：F01–F57

本节F01–F57是设计评审的历史索引，不是本次三份实施计划评审。本次评审逐项处理见附录E。此表中的旧P06指P06a/P06b，P07指P07a–e，P09指P09a/b，P16指P16a/b，具体职责以任务正文及附录F为准。每一行继承v1.1的接受/部分接受结论；本计划不重新把其限定意见扩写为已证实漏洞。D表示按规范明确延期或不采用，不是漏做。

| 处理项 | 实施落点或明确延期 |
|---|---|
| F01 共享核心与身份 | P01/P03/P12/P13/P16 |
| F02 资格前置 | G1，P06–P08先于P09–P23 |
| F03 副本不等于隔离 | P06/P08/P10，D：不提供弱隔离档 |
| F04 Codex/Grok/Windows限定 | P06/P08精确证书；D：其他平台品牌未认证即关闭 |
| F05 用途与隐藏侧栏 | P07完整policy/registry/discovery/import |
| F06 delegation风险限定 | P07显式工具组；不宣称普通Agent必然暴露delegation |
| F07 复用运行时 | P07/P13薄wrapper；D：不建service_session进程栈 |
| F08 JSON抽取与UUID | P04/P11别名+submit；D：最终assistant抽取 |
| F09 只读证据通道 | P06/P11，host fs关闭且独立evidence工具 |
| F10 主持speaker | P01/P07/P17/P19/P21 |
| F11 原子语义关系 | P09/P12故障注入全链 |
| F12 幂等principal | P01/P15/P18 |
| F13 修复时隙 | P02/P14/P20，225/450/1800预算 |
| F14 同session修复 | P04同prompt修正；D：结束后同session复用，改新binding |
| F15 HTTP形状 | P18/P19 POST /api/command |
| F16 全局事件泄漏 | P07/P17/P22覆盖原始ACP及派生正文 |
| F17 resume省略ID限定 | P07/P15默认重建；D：strict_resume，不改通用ACP契约 |
| F18 输入上界与快照 | P02/P10/P11/P20 |
| F19 简化租约/outbox | P09/P13/P15/P17；保留dispatch uncertain |
| F20 首版削减 | D：审批/副作用账本/FIFO保留槽；P06/P14保持fail-closed |
| F21 立即暂停语义 | P15/P21，明示未完输出损失与次数 |
| F22 主持单独重试 | P15/P21 |
| F23 N=2的Q=2 | P03/P20/P21 |
| F24 计量与时间简化 | P14/P16；D：四级质量计费与跨重启绝对deadline |
| F25 服务级CORS范围 | P18/P22部署门；D：圆桌内全站CORS重构 |
| F26 PRAGMA | P09逐连接及P16外部耐久门 |
| F27 check/enqueue竞态 | P05/P13 gate A/B |
| F28 可fold载荷 | P12/P17/P19完整ProjectionV1 |
| F29 at_seq历史页 | P17固定membership、hash、签名cursor |
| F30 未知投影事件 | P17/P19 fail-closed，不跳seq |
| F31 closing控制意图 | P15矩阵、CAS、stop覆盖、唯一successor |
| F32 预算不足无出口 | P14/P15/P21预留及blocked出口 |
| F33 活性约束 | P13/P15/P22，前提成立则推进，否则解释阻断 |
| F34 证书与隔离顺序 | P06/P08版本/hash变更过期、先隔离后spawn |
| F35 update排空 | P05/P07/P08/P11双传输完成屏障 |
| F36 空claims | P03/P04拒绝空proposal/critique |
| F37 目标与incoming挑战 | P03/P21，最多4目标、失败不重分配 |
| F38 公共/实际context分开 | P02/P10/P11，fresh每attempt；D：live session裁剪宣称 |
| F39 N²/R²增长 | P02全计划容量、P20可见阻断 |
| F40 ACP不是计费请求 | P02/P14/P16/P20 |
| F41 usage语义去重 | P16/P21 |
| F42 活跃时长/暂停 | P14/P15/P21 |
| F43 原子permit/首次优先 | P03/P14；D：跨房间公平队列 |
| F44 闲置进程/scratch | P06/P07/P14/P16/P22 |
| F45 dirty与evidence归属 | P10/P11/P12 |
| F46 读取与扩大来源 | P10/P11/P20；D：运行中新增源，须clone |
| F47 coverage与支持边 | P03/P04/P21；语义正确性另由P23 |
| F48 质量比较 | P23三模式、盲评、消耗与失败留档 |
| F49 revision传播 | P12/P17/P19/P21 |
| F50 失败原文 | P16有界诊断、脱敏，不依赖preview |
| F51 API缺口取舍 | P01/P18/P21：draft可改名；D：删除/独立导出/运行后改名 |
| F52 DB/blob整体耐久 | P09/P10/P16/P22 |
| F53 迟到usage | P16专用当前service入口，旧epoch不得写业务 |
| F54 不可信前端 | P21/P22安全文本与受控引用 |
| F55 八个明确反例 | P05/P15/P17/P03/P10/P02映射T02/14/15/11/08/07/12/17 |
| F56 缩范围保不变量 | G1及P01–P23顺序；D：自由讨论只有接缝 |
| F57 代码依据强度 | 第2节固定基线，P07/P09/P13/P17/P22；不虚构缺失canvas内容 |

## 10 明确不做与替代路径

- 交互审批、写工具、浏览器、委托/任务创建、外部网页抓取：工具不可广告也不可调用，未知一律拒绝
- 运行中扩大资料范围：clone新draft并重新确认范围、接收方、预算和资格
- 原生session冷恢复、live reuse、同session跨prompt修复：每attempt新binding；原session恢复另认证
- 多active room、公平FIFO、保留槽：capacity_limited明确返回，不悄悄排队
- 自由讨论/通用DAG/Python调度：仅strategy type/version与SchedulingIntent接缝，不实现后续策略
- 删除/独立导出/批量管理/运行后改名：无API和按钮；已有全库备份仍需包含引用对象
- 自动执行建议：输出只供用户判断，不自动改项目、发消息、调用外部agent或创建云端编码任务
- Windows/macOS/其他品牌真实支持：保持not_tested，按精确组合新增证书，不能复用Linux结果

## 11 最终验证命令与外部门

必须另外逐行执行下列格式与lint门（独立crate不被主包fmt自动覆盖）：

```bash
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo fmt --manifest-path src-tauri/roundtable-protocol/Cargo.toml -- --check
cargo clippy --locked --manifest-path src-tauri/roundtable-protocol/Cargo.toml --all-targets -- -D warnings
cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets --features test-utils -- -D warnings
cargo clippy --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server --lib -- -D warnings
cargo clippy --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features mcp-bin --bin codeg-mcp -- -D warnings
node scripts/check-roundtable-locks.mjs
pnpm lint .
```

每项绿色不等于全分支可以发布。P22之后在有足够内存和受支持工具链的实施环境运行：

```bash
cargo --config .cargo/low-memory.toml test --locked   --manifest-path src-tauri/roundtable-protocol/Cargo.toml
cargo --config .cargo/low-memory.toml test --locked   --manifest-path src-tauri/Cargo.toml --no-default-features   --features test-utils --test roundtable_service roundtable_end_to_end
pnpm rust:check:low-memory
pnpm rust:check:desktop:low-memory
pnpm rust:check:server:low-memory
pnpm rust:check:mcp:low-memory
pnpm exec tsc --noEmit
pnpm test
pnpm test:release
pnpm build
```

四个 `roundtable_*.rs` integration入口必须各自跑一次，并纳入CI清单，不只运行上面end_to_end目标。小target便于定位和低内存开发，**完整Rust lib与既有integration全套仍是发布门**：在足够内存的验证机运行下列命令；若本机再次SIGKILL，记录日志/峰值RSS/环境后将门标blocked并转到获准环境执行，不说全套通过，不自行启动云端工作。

```bash
cargo --config .cargo/low-memory.toml test --locked   --manifest-path src-tauri/Cargo.toml --no-default-features   --features test-utils --lib --tests
```

外部门另外包括：真实T04资格与T07边界、Linux隔离/进程树/credential gateway、桌面真实窗口定向投递、远程部署TLS/Origin、P23质量实验、以及需要断电持久承诺时的VM/文件系统断电测试。ordinary unit test不能替代这些。若用户要求通过外部云端Codex或ChatGPT Work执行，必须请用户本人启动；本计划不授权自行调用。

## 12 自审与交接

本版逐条区分规范已规定、计划遗漏、真实接口冲突、待验证假设与新增提案。设计§1–17、T01–T22及I1–I10有任务归属；附录E覆盖本次三份意见，附录F保留旧任务到新任务映射。所有测试名称、命令、passed条件均是未来验收，不是本次运行报告。没有因文档完成而给任何OS/adapter发证。

交接包必须同时提供unchanged设计、此计划及其A–F附录、README版本关系与SHA256SUMS。执行人先验证sha256，读G0/A项，取得对应实施授权并选择实际环境。批准文档不自动批准安装、凭据、模型实验、仓库修改或上线；本次没有这些行为。

## 13 可复查依据

1. 协议：《MyCodeBuddy 多智能体圆桌协议设计 v1.1》，2026-10-03，§1–20；同批文档为规范来源
2. 固定基线：https://github.com/icannotwait/MyCodeBuddy/commit/cb7596d38f9aa0df52740e939da7732ccfaaf5d8
3. 独立修复线：https://github.com/icannotwait/MyCodeBuddy/commit/6a66c0a2757052cf4647164b2f6c3b44e1f986ec
4. 既有ACP服务启动：https://github.com/icannotwait/MyCodeBuddy/blob/cb7596d38f9aa0df52740e939da7732ccfaaf5d8/src-tauri/src/acp/manager.rs
5. 首候选版本依据：https://github.com/icannotwait/MyCodeBuddy/blob/cb7596d38f9aa0df52740e939da7732ccfaaf5d8/src-tauri/src/acp/registry.rs
6. server与依赖边界：https://github.com/icannotwait/MyCodeBuddy/blob/cb7596d38f9aa0df52740e939da7732ccfaaf5d8/src-tauri/Cargo.toml
7. DB逐连接核验接缝：https://github.com/icannotwait/MyCodeBuddy/blob/cb7596d38f9aa0df52740e939da7732ccfaaf5d8/src-tauri/src/db/mod.rs
8. 低内存测试配置：https://github.com/icannotwait/MyCodeBuddy/blob/cb7596d38f9aa0df52740e939da7732ccfaaf5d8/.cargo/low-memory.toml

本文件中的测试、命令与资格实验均是待实施步骤，不是已经运行或通过的报告。

## A 明确待批准的协议补充

下列提案是本计划 v1.2 的一部分，状态均为 proposed。设计 v1.1 原文件保持原字节。G0 必须逐项接受或退回；未接受时相关实现不得冻结为生产契约。实现代码与 schema 将记录 `contract_profile=roundtable_plan_1_2` 及批准后的 amendment_manifest_hash。它不是冒称已经发布的设计 v1.2。

### A01 无损 wire 和规范字节

内部 Seq/Revision/Epoch/u64 计数保留 u64；wire 采用正则 `0|[1-9][0-9]*` 的十进制字符串，范围 0…18446744073709551615。N/R/C、ordinal、字节页长等有显式≤2^53−1的安全整数边界；所有浮点、NaN、Infinity、负零和未知字段拒绝。ID 为宿主生成的小写规范 UUID；SubmissionId 例外，为1…64个ASCII字母、数字、下划线或连字符。

CanonicalV1：先按 schema 补齐有效默认值；规定 nullable 字段显式 null，规定 optional 字段仅在 absent 时省略；不把 absent 与 null 自动互换。对象 key 按 UTF-8 字节序排序，数组顺序保留，不做 Unicode NFC/NFD 转换。UTF-8 编码；引号与反斜线转义为反斜线形式，U+0000…001F 一律小写 `\u00xx`；其余有效 Unicode 原样输出；拒绝孤立 surrogate。无 BOM、空白或末尾换行。hash 字段不包含在其所校验 body 中，采用 `ProjectionRef {id,hash}` 外包 `ProjectionBodyV1`，其余内容对象同样处理。签名 cursor 使用单独 MAC，不把 hash 当授权凭据。

黄金 fixture 同时保存输入原始字节、归一结果、规范 UTF-8 hex 和 SHA-256，不只保存解析后的对象。Rust/TypeScript 对中文、emoji、控制字符、键顺序、null/absent、默认值、2^53±1、u64::MAX 都逐字节对比。浏览器使用 Web Crypto SHA-256；无 crypto.subtle 的非安全上下文拒绝投影校验并显示 secure_context_required，不能只比较服务器给出的字符串。远程产品访问须已满足设计 TLS/Origin 条件；不新增未经批准的哈希依赖。

### A02 不可变确认对象

`PreflightRecordV1 { preflight_id, principal_id, room_id, revision, config_hash, source_manifest_id, source_manifest_hash, recipients: Vec<ResolvedRecipientV1>, policy_hash, qualification_keys, limits_hash, created_at, expires_at }` 存于服务端。`ResolvedRecipientV1 { provider_ref, provider_config_version, endpoint_origin, model, effort }` 不包含凭据或凭据 hash。有效期建议30分钟，写入 profile，只限制首次start的确认凭据；成功start后生成绑定同不可变对象的RunAuthorization，后续attempt只核验该授权绑定与当前资格/控制状态，不因初始30分钟经过而否认已授权的长运行。过期未start只可重新确认原对象或明确捕获新对象，不能暗中改来源。

`roundtable_preflight` 接受 draft 的 room_id/revision 与完整 config/source_refs，创建并返回冻结资料和上述记录；尚无 room 的配置估算仍可调用，但其返回 `confirmable=false`，不能用于 start。`roundtable_start` 新增 `confirmed_preflight_id`，由明确的用户确认动作提交；幂等请求 hash 包含该 ID。服务复核主体、room/revision、冻结hash、实际接收方配置版本及当前证书。原工作区改变仍使用已确认的不可变 A；provider/endpoint/model/effort 或政策漂移返回409 invalid_state，details.reason=reconfirmation_required，不发送 B/Y。单纯证书失效返回422 capability_unqualified。一次确认覆盖随后按同快照/同接收方运行的所有 attempts，不逐次弹窗。

### A03 原命令内的对象与计量读取

完整闭集恰为18个名称：roundtable_preflight、roundtable_create、roundtable_update_draft、roundtable_start、roundtable_get、roundtable_list、roundtable_pause、roundtable_resume、roundtable_stop、roundtable_interject、roundtable_retry_synthesis、roundtable_events、roundtable_messages、roundtable_evidence、roundtable_operation、roundtable_clone、roundtable_attach、roundtable_detach。attach/detach 在设计表合为一行，因此表为17行；不新增第19个下载命令。

`roundtable_get { room_id, read?: GetReadV1 }` 默认读取当前投影；`GetReadV1` 是 projection{projection_id?}、object{object_ref,cursor?}、usage{after_usage_version?}、metrics{} 判别联合，互斥字段拒绝。object_ref=`{object_id,kind,content_hash,total_bytes}`；kind 限 projection/message/manifest/diagnostic/source_excerpt，source原始对象不能由猜测ID任意取回。每次读取都先 authorize_room 并检查该room的可读引用关系，诊断与证据同授权。分块为连续字节、返回base64、offset/end/total/hash，chunk≤256KiB，base64包装仍≤1MiB；cursor MAC绑定principal/room/object/hash/offset/version，有效30分钟。过期返回409 invalid_state/details.reason=resync_required；允许重新申请同不可变对象的cursor，绝不切到当前最新版。

`UsageViewV1 { usage_version, measurements, totals, unknown_count }` 独立刷新，不修改room.seq/revision/状态。`MetricsViewV1 { uncertain_count, cleanup_overrun_ms, admission_rejections_by_reason, event_count, reserved_event_slots, storage_used_bytes, storage_reserved_bytes, unknown_measurements }` 是授权只读运维快照，无秘密、路径或消息正文。

### A04 计时和配额的组合解释

设计 §9.2 的内部 checkpoint 与 §12.1 “影响客户端业务状态”事务区分。`TimeLedger { ledger_seq, remaining_room_ms, remaining_phase_ms, prepaid_until, last_sample_mono }` 的≤1000ms预扣只更新内部账本，不产生业务event或控制revision；跨重启不保存可复用的绝对monotonic值。业务投影预算带 `ledger_seq`、`sampled_active_ms` 和 `sampled_at_utc`，语义是该业务提交点的持久样本，不能被客户端用作启动授权。所有业务预算预留/结算仍与投影/event同事务。

新增候选profile：业务投影最多64KiB，room event硬上限10000，其中128个终态收敛保留；控制受理最多32次、插话最多64次，重复幂等/无变化读不耗事件。事件发出点同时冻结：每attempt最多8次业务投影，phase最多6次，control最多8次；tool候选/证据staging和ingress/chunk为内部账本，不单独发业务投影，accept/publish时原子带入；假执行计数超此上界即context_contract_violation并阻断资格，不能静默调高。preflight按 `12+8×max_attempts+6×(R+2)+8×32+64` 预留普通事件上界，再加128，不得仅预留计数不预留字节。收敛字节保留至少128×64KiB，再加全部可能活动binding的64KiB诊断及对象元数据上界。事件、引用对象、临时副本、draft、并发preflight、诊断、备份锁定引用都进入同一data_dir ReservationLedger；先预留后写，已知清理成功才归还。超profile上界在付费前拒绝，运行中提前进入可持久收敛路径；不删除历史以假装有空间。

### A05 候选上下文和凭据通道

此项是严格候选profile，不扩大设计允许工具。首候选不向 agent 挂载资料文件正文，资料只通过三工具交付；设计 §6.2 允许原生读副本的其他候选必须另计读回文本并发证，不能以unverified标签绕过容量限制。镜像运行必需文件仍受只读与不可外泄认证约束。

`QualifiedContextProfile { tokenizer_id, tokenizer_hash, model_capacity_tokens, max_model_requests:64, max_tool_calls:128, max_tool_reply_bytes:131072, max_attempt_generated_utf8_bytes:262144, max_request_body_bytes:1048576, generation_reserve_tokens:8192, adapter_hidden_bound_tokens, proof_ref }` 为首候选提案；模型容量和tokenizer hash从真实已核实数据取得，不能填估值。证据响应继续每次≤8192/attempt≤32768字节；所有工具编码信封、field_errors、空结果、receipt及重复提交回包同时计入128KiB工具累计，错误路径不免费。即使请求失败或重复receipt，也消耗请求/工具次数预算；缓存幂等不创建第二候选，但不免传输限额。

初始完整前缀使用共享DeliveryEncoder预检；随后网关对每次实际模型请求 j 验证 `verified_input_bound(j)+generation_reserve_tokens <= capacity`，其中含adapter注入、此前assistant输出、工具调用参数和所有回包。生成预留是每次请求的上界；累计生成另受256KiB限制。禁止隐形压缩、无法解析的remote previous_response_id、不可见的服务端历史和未认证自动重试；无法得到可靠请求上界即capacity_unknown，不发该请求。模型请求计数与ACP尝试数明确分离。

服务companion通过MCP stdio配置的env传入attempt-scoped token，不依赖宿主给孙进程继承FD，也不放argv。agent可能读到同一sandbox的env/token；安全保证是无法扩大到别的attempt/room/工具/来源，不声称token对agent不可见。服务模式用宿主broker socket EOF与受限握手deadline判活，不传宿主 `--parent-pid`。真正模型凭据仅在宿主网关，agent token不是模型凭据。日志/诊断移除完整env和token，不保留token前缀。

### A06 累计计量乱序修正

设计 §9.4 “回退或重置另起epoch”需显式改为：epoch只由可信session/request生命周期、提供商epoch或已验证reset事件建立。相同epoch的已确认高水位只取正向差额；seq较旧事件不重置；无seq且下降不能区分乱序/重置时记录unknown/uncertain，不增加confirmed。此项修正需要批准，不能只在计划测试里悄悄改变设计。迟到usage仍只能经当前service独立归档入口写measurement；读法见A03。

### A07 准入与接纳时钟点

最终enqueue仍是设计 §4.3 的本地准入线性化点。admitting事务完成后，紧邻非阻塞enqueue重新读取单调时钟，检查当前fence、所有权/Ready、rollout generation、资格、确认对象、资源与预付执行lease。任一失败且确定未入队，记录dispatch_state=not_dispatched（附内部原因，不新增公开attempt状态），归还未耗prompt预留；launch计数不退。崩溃留下admitting仍uncertain，不准靠重发消歧。

接纳采用明确的“事务决策点”语义：先取得写事务并完成所有可等待的读取/语义写入，在持actor/gate、提交前最后一刻读取单调时钟，若 now>=deadline 则回滚；否则记录accepted_at_decision并提交。只有commit成功才对外accepted。commit完成可以晚于该决策点；不声称SQLite fsync一定在deadline之前完成。若不接受此语义，必须修改此提案后再冻结，不能用事务开始前的旧时间样本代替。这保留在事务内判定截止的设计要求，并将不可避免的提交延迟明确暴露。

### A08 资格和执行范围标识

`ExecutionScope = Fake | Qualification{approval_id,expires_at,attempt_limit,spend_limit,recipient,fixture_hash} | Product{rollout_generation}`。三者不可互转。生产持久设置默认 `{enabled:false, generation:0, allowed_qualification_keys:[]}`；修改后递增generation，最终本地准入及网关再次核验。资格执行独立预算且仅合成非敏感fixture，不代表用户产品数据授权。disable_and_drain先撤回本地lease/网关准入，再持久fence/清理；存储失效也立即禁止新准入，但不伪报持久stopped。

### A09 完整能力清单与完成结果判定

状态 proposed。这是本版重新核对源码后提出的合同，尚未批准。`LaunchCapabilityManifestV1` 与其规范 hash 成为资格对象的一部分；服务工具集合精确等于三工具，普通 companion 和原生能力不得继承进入该集合。所有真实启动入口必须核对同一清单；任何新增工具、配置或桥接通道均令旧资格失效。原生执行若保留，只能在已认证 OS 隔离和配额下运行，绝不等于允许宿主能力。

`RuntimeTurnCompleted` 增加规范化 outcome 与有来源的 failure 摘要，仍不是 accepted。完成 barrier 收齐本 turn 的 response/update/工具关闭事实后，error failure 优先于 end_turn 和 staged candidate；warning 不自动失败；无法识别的版本化记录不能证明成功。诊断脱敏且遵循原上限。A09 不增加命令，不变更公开错误闭集，不授权新工具或真实模型调用。P01 黄金契约、P07b/P07e 和 P08 使用同一实现；G0 逐项记录批准后方可冻结。

## B 任务级执行环境与固定命令

DEV为已获准的Windows/macOS/Linux开发环境，只运行对应平台支持的纯协议、fake I/O或前端；LINUX为记录精确profile的Debian12隔离验证机；DESKTOP为有tauri-runtime依赖且可显示真实窗口的验证机。任务环境完整分配：P01–P05为DEV；P06a/P06b/P07a–e的fake为DEV、实际进程/网关/discovery为LINUX；P08为LINUX；P09a–P16b为DEV支持的SQLite/fake目标，其中孤儿进程与实例枚举真测试为LINUX；P17/P18为DEV核心加DESKTOP定向路径；P19–P21为DEV前端；P22为LINUX执行端加DESKTOP/Web/远程客户端；P23为LINUX已认证实验与DEV评分。所有多命令示例逐行执行，PowerShell5.1不要求支持 `&&`。这份文件中的命令均未执行。

P01只启用独立协议crate CI，并记录下列未来产品矩阵；P06a/P09a/P13/P17分别在首次新增runtime/protocol_io/service/transport入口与case模块的同提交启用对应CI目标，不提前引用尚不存在的target。以下是可复制命令，之后各任务用同一入口加测试模块过滤，避免15个独立巨型链接目标。协议crate可保留小型独立targets。集成入口固定四个，各自用 `#[path="roundtable_cases/<case>.rs"] mod <case>;` 纳入多个模块。

```bash
cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_protocol_io
cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_runtime
cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_service
cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_transport
cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --features test-utils --test roundtable_transport desktop
cargo --config .cargo/low-memory.toml test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features mcp-bin --bin codeg-mcp
```

上面最后两条分别覆盖默认桌面feature与bin单元测试。core parity不等于Tauri宏/窗口投递通过；`--test`可能编译bin，但不执行bin内 `#[cfg(test)]` 单元测试。所有现有User、Delegation、InternalProbe、InternalTitle、InternalTranslate路径均做回归。`test-utils`只开放必要的failpoint与测试构造器；release未启用时必须不存在。

执行授权后，在用户选定环境创建隔离worktree：先核验base对象与当前修改，再 `git worktree add -b feat/roundtable-v1 ../MyCodeBuddy-roundtable cb7596d38f9aa0df52740e939da7732ccfaaf5d8`。若已存在同名分支/路径先核查，不覆盖。本次不运行该命令。首提交把同包设计原文与本计划写入上述docs路径，随后才按任务实现；不修改原工作区，不自动移植修复线。

每个任务完整周期为：写本节具名失败测试与精确断言 → 用该任务命令观察因缺功能产生的红灯 → 实现列出的接口 → 同命令绿灯及直接依赖回归 → 检查本任务diff、本地提交。环境/权限/内存失败是blocked，不能冒充红灯。新模块同步更新lib.rs/mod.rs导出；不添加无范围 `allow(dead_code)` 来掩盖未接线接口。

## C 可直接断言的预算表

T=225000ms，D=2×ceil(N/C)×T，room=(1+R)×D+450000。下表是静态期望，测试不得调用被测函数来生成同一张期望表。N=2时C=N与C=2相同，只测一次；N=3、7时C为1/2/N。

| N与C | D毫秒 | R0房间毫秒 | R1房间毫秒 | R2房间毫秒 | R5房间毫秒 |
|---|---|---|---|---|---|
| 2 / 1 | 900000 | 1350000 | 2250000 | 3150000 | 5850000 |
| 2 / 2 | 450000 | 900000 | 1350000 | 1800000 | 3150000 |
| 3 / 1 | 1350000 | 1800000 | 3150000 | 4500000 | 8550000 |
| 3 / 2 | 900000 | 1350000 | 2250000 | 3150000 | 5850000 |
| 3 / 3 | 450000 | 900000 | 1350000 | 1800000 | 3150000 |
| 7 / 1 | 3150000 | 3600000 | 6750000 | 9900000 | 19350000 |
| 7 / 2 | 1800000 | 2250000 | 4050000 | 5850000 | 11250000 |
| 7 / 7 | 450000 | 900000 | 1350000 | 1800000 | 3150000 |

| N | R0基础与最大 | R1基础与最大 | R2基础与最大 | R5基础与最大 |
|---|---|---|---|---|
| 2 | 3 / 4 | 5 / 7 | 7 / 9 | 13 / 18 |
| 3 | 4 / 5 | 7 / 9 | 10 / 14 | 19 / 26 |
| 7 | 8 / 11 | 15 / 21 | 22 / 30 | 43 / 60 |

以上数值不代表真实模型能容纳对应上下文。7/5/2预算计算必须正确，容量不够时可以在付费前明确拒绝；fake足额profile仍须跑通参数化全轨迹。

## D 预注册质量和观测口径

P01固定12任务：事实核查、方案权衡、反例追踪各4；三模式单模型、R0、R2各2次，共72次运行。任务与评分要点封存hash，P08/P22调试只用独立开发集。实验前指定两位盲评者与一位分歧裁决者；同一运行去除模式名称/模型标记后独立评分，再按事先规则裁决，不凭现场偏好删题。

原始行 `QualityScoreRow { task_id, mode, repetition, check_id, check_type, rater_id, score:0|1, failure_type?, adjudicated_score?, evidence_ref }`。check_type为seeded_error、substantive_response、minority_risk；每个task预先定义同资料可回答的机会集，三模式相同，不按成功产出动态缩小分母。两次重复分别为独立run行；指标先给每task/每mode两次的分子分母，再给宏平均和原始总分子/总分母，裁决门默认用宏平均，不能混用口径挑更好值。

关键错误发现率=正确指出的预埋关键错误数/预埋错误机会数。实质回应率=对预埋关键质疑给出有依据的回应数/预埋回应机会数。少数风险保留率=主持结果准确保留的重要少数风险数/预埋风险机会数。失败、超时、无成员输出或主持失败均保留对应机会，未交付项计0，不从分母移除；机械assigned/answered coverage另列，不充当语义回应评分。

无依据新增结论记录 `unsupported_count` 与 `all_asserted_conclusions`；输出为空时该比例为N/A，同时该run失败且三个机会指标计0，不能凭零输出获得“幻觉更少”的奖励。另报告失败率和每run无依据结论计数。建议门为安全零失败；R2宏平均关键错误发现率不低于两个基线；实质回应≥80%；少数风险保留≥90%；无依据结论在有输出运行的比例与每run计数都不劣于基线。所有数值仍须G0/实验前批准；任何关键任务退步须显式裁决修复或暂不开启，不能平均掩盖。

报告每run ACP attempts、底层已观察请求数、room活跃时长、失败类别、confirmed token和unknown measurement，保留全部失败样本。72次小样本只能描述这一组任务在已报告额外消耗下的结果，不宣称等预算优势、显著性或讨论机制单独带来的因果收益。更强结论需要另批预算匹配实验。

运维指标通过A03 `get(metrics)`可读取；P13负责启动/quarantine和拒绝原因，P14负责active/overrun/预留，P16a负责unknown/diagnostic，P17负责event/订阅水位。指标本身也不得泄漏token/正文。P23实测接口返回值与故障轨迹一致，而不是仅把指标名称写进操作文档。

## E 三份实施评审的逐项处理

来源R1/R2/R3分别为本次 `imp plan review1.txt`、`imp plan review2.txt`、`imp plan review3.txt`。它们不同于设计文件附录的旧V1/V2/V3和F编号。以下合并重复意见但逐列来源、裁决、证据与落点；“部分接受”明确保留或不采用的部分。

E01 R1阻断1、R2阻断1、R3范围声明/局部6：部分接受。设计v1.1实际存在，且本次可读；“不存在”不能跨环境成立。评审者文件包缺失导致核验受限是真实交付问题。修复为同包附原样MD/DOCX及hash/versionmap，G0不再依赖拟议路径。旧v1.0不得补写本版协议。

E02 R1核对无误、R2基线核对、R3保留项1–3：接受保留架构和证据边界。共享Rust服务、fresh binding、单active room、三工具、取消/fence、固定快照/分页和独立质量门继续；静态接缝存在不等于已完成安全验证。全任务和G1/G3保留fake与真实证据分层。

E03 R1阻断2、粒度末项、R2 P06/G0-4、R3问题1：接受提前验证最大风险。新增P00先测rootless/endpoint/实际companion/原生工具边界，失败不全面开工。选择crun仅为候选；精确版本/hash必须由实际环境记录，未发证。普通CLI只读模式不能替代strict隔离，依据设计§3.1/5；P06a/P08。

E04 R1阻断2账号登录推断：部分接受。必须实测凭据代理和auth兼容，不据计划推断所有ChatGPT账号用户一定不能用。本候选范围明确宿主API凭据，账号登录另记not_tested；不把auth.json放sandbox。G0/P00/P08/A05。

E05 R1阻断3、R2 P06及bash小项：接受环境分层。DEV/LINUX/DESKTOP逐任务区分，PowerShell逐行不依赖&&；Windows纯规则不因OCI不可用红灯。case-collision运行时临时生成并探测文件系统。P10/附录B。

E06 R1高4、R2 MCP项、R3问题1：部分接受接口冲突，拒绝把泄露token笼统称“无害”。现有stdio由CLI生成孙进程，宿主FD假设不成立；v1.1本身没有禁止argv的额外规定，旧计划自行加了FD。改MCP env传attempt-scoped token，承认agent可见但不能扩大scope；模型凭据仍不可见。EOF替宿主PID，socket挂载。P07c/A05真实链硬门。

E07 R1高5、R2接缝：接受。manager同步尾部不止channel send，必须OwnedPermit+try_write+generation/active_turn/turn_in_flight/provider-id重置原子完成。P07a具体断言；本版补上LaneOwnedPermit实现与liveness回归，锁失败不留下代际污染。

E08 R1高6、R2测试覆盖：接受。旧stub_direct_spawn/hold_after_admission cfg(test)不足以给外部integration使用，P07a窄test-utils hook并验证release不导出；不新建通用生产测试框架。

E09 R1高7、R2绿灯项、R3局部执行边界：接受Tauri覆盖不足；部分纠正“bin根本不编译”。无默认feature不编译Tauri路径，但integration可编译bin而不执行其单元测试。P07c解析迁lib、显式bin target、默认feature桌面target及真窗口验收；附录B/最终门。

E10 R1高8：预防性接受。原计划没明确新增src/bin helper，不能说已经存在打包错误；基线Cargo警告属实。双进程用current_exe，relay复用现有受控helper入口，P13/P06b不增加会被NSIS自动纳入的辅助bin。

E11 R1高9、验证lint、R2 path依赖、R3局部2：接受。P01补target忽略、独立crate CI fmt/test/clippy、共享依赖锁版本来源核验；P06a唯一引主path dependency。保留独立package，不擅转全仓workspace；最终补pnpm lint及三mode clippy。

E12 R1高10、R2基线PATH/G0-5：接受。证书锁实际运行的镜像内绝对二进制、Node/CLI/adapter/MCP/核心hash；绕开PATH优先、npx动态拉取；env_clear白名单。P06a/P07a/P08。没有实际hash的资格报告不能passed。

E13 R1高11、R2 PRAGMA核对：接受独立基础设施任务，限定证据强度。一次pool execute不能保证每连接；不能断言其他连接foreign_keys必OFF。P09a同时占五连接重点synchronous/cache_size及重建，默认NORMAL，不未经批准宣称FULL断电耐久。

E14 R1高12 InternalProbe：接受。P07a回归五种原用途，包括InternalProbe，Roundtable不进hidden_generation。

E15 R1高12降级、R3问题11：接受实体迁移同交付要求。资格阶段独立registry，不提前改生产ORM；P09b新增rt_internal_bindings，与适配器同提交。旧版不能安全过滤时明确不能直接原目录降级，须drain并用升级前备份，不能把新purpose写给旧枚举而假称兼容。

E16 R1高13哈希：接受Web Crypto环境风险，不采用只比较服务器hash字符串的弱化。设计§12.1要求正文校验；A01固定字节，安全context SHA-256，缺crypto.subtle明确阻断；不用未经批准新依赖。P19。

E17 R1内部命令数、R2阻断2/G0-3、R3局部1：接受计划17误写。实际设计§13.1为16名加attach/detach=18（17表行）。单枚举驱动契约，object/usage在get判别读取模式，新增字段明确提案A03，不加第19命令。P01/P18。

E18 R2阻断3/G0-2、R1数值保留：部分接受计划缺公式，不接受真实v1.1缺公式。设计§9.2已有(1+R)D+2T；计划内联并附全部静态期望。7/5/2=11250000ms，非12600000。P02/附录C。

E19 R1内部P05门名、R2 P05竞态：接受。P05是M0a出口；协作式failpoint而非sleep，A/B顺序不自造死锁，纯模型不宣称真实准入证明。P13真实DB重跑。

E20 R1内部并行、R2依赖链、R3局部3：接受。删除斜杠伪并行；给真实边，只有P03策略可与不依赖其算法的资格链并行。P04消费P01目标fixture，P13直到资源/恢复齐备不Ready。§7。

E21 R1内部文件遗漏/重名、粒度P07/P15/P17、R3问题11：接受维护可审查边界。P06拆隔离/网关，P07拆策略/私有ingress/companion/registry，P09拆基础配置/表，P16拆计量诊断/备份GC。完整模块表；command_processor/budget_ledger避免双commands/budget混淆。P15控制矩阵和P17订阅保留各自协议闭合，内部可多小commit，但不得拆出可运行的不安全状态。

E22 R1内部HEAD和修复线、R2基线：保留历史评审的跨环境核验结论。原v1.1读取abd3c674环境中的2587471及两条修复对象；这些是历史记录。当前v1.2施工基线已按§2改为PR33合并提交cb7596d，相关重叠接缝重新核对，不重复移植旧修复。

E23 R1集成target过多：接受。四个产品integration入口，模块case定位，协议小crate独立targets；发布仍完整lib/既有integration，不能以4GiB限制删除门。附录B/§11。

E24 R1无实施分支和文档路径：接受。附录B先核base/改动，再在用户选定环境建立隔离worktree；设计放docs/superpowers/specs路径。此次文档工作不运行worktree命令。

E25 R1字节串换行、TS围栏、locale、jsdom图片；R2相同小项：接受并直接修正。P10字节串显式\n；P19–21 TypeScript标注；中文测试固定zh-CN；P21检查渲染不存在remote src/script/iframe/object，真浏览器另验网络，不能仅靠jsdom不发请求。

E26 R2 v1.0 I1–I10/F55不对齐：部分接受显式追踪，否定错误版本推论。真实v1.1§14.3有I1–I10；八反例映射六任务是合法多对多。附录F逐场景列T02/14/15/11/08/07/12/17，补精确任务，不减少反例。

E27 R2同prompt第四次invalid、R3并发提交：接受明确冻结边界和新增竞态。按前三次可修复，第4次invalid关闭；第三次后valid仍可sealed。P04纯规则/P11事务CAS并发两valid与invalid3/4，错误计数不丢、唯一sealed。

E28 R2 P08 docs提交类型/脱敏：接受。含测试实现采用test提交类型；profile/report入库先脱敏env/token/Authorization，合成资料hash与可复查trace保留。P08。

E29 R2非持锁只读、R3问题3：接受加强服务边界。RoundtableReadService真只读SQLite连接，无mutations；非持锁进程不能通过pause/interject/preflight捕获旁路写。不同全库业务不受圆桌锁授权，不冒称应用全库单写者。P13。

E30 R2 flag存储、R3问题7：接受。P06a最先落持久默认false门、三执行scope；P23仅有限allowlist/rollback。所有自动下一阶段、resume/restart/retry与最终enqueue再查generation；ready后关闭不发送。A08/P13–15。

E31 R3问题1 gateway/工具/编码器/附加证据：接受施工闭合。P02统一DeliveryEncoder，P06b网关包含身份/body/model/预算/撤销/生命周期，P07c共享tool_core+内存adapter，P11只替持久store。六能力之外sidebar/private sink和二进制/凭据检查是必需，不漏掉。核心变化令资格失效。P08/G1。

E32 R3问题2多模型请求和完整上下文：部分接受新增实施证明，不宣称v1.1从未考虑。设计§8.3已要求同turn循环限制，§9.1已区分ACP和模型请求；A05/P02/P06b具体限制每request输入+生成、累计输出/工具/错误/重复receipt，未知即关门。fresh binding仅消除跨attempt历史。

E33 R3问题2原生读：部分接受边界风险，拒绝将“任何原生read都被规范禁止”写成事实。设计允许副本原生读但引用unverified；本候选A05更严格不挂资料正文，只走MCP，其他候选须把原生读纳容量/隔离证据重新批准认证。

E34 R3问题3冷启动、CleanupProof和permit：接受。文件锁不证明旧进程已消失；持久launch intent在spawn前登记，实例labels/discovery覆盖ACK丢失。P13启动quarantine阻止所有room，P06a仅process proof，P14运行聚合精确整个lease实例集合，partial不能释放；真实存活孤儿/PID复用反例。

E35 R3问题4确认版本：接受显式DTO补充，限定现有规范已有冻结快照。A02 preflight_id绑定实际manifest/config/recipient/policy/qualification；原文件变仍A，provider漂移不送Y；P10/P18/P20。不是每attempt重新确认。

E36 R3问题5 checkpoint与10000event冲突：部分接受组合歧义，不断言所有运行必超额。v1.1§12.1只要求业务状态事务投影，旧计划把它扩大成每事务。A04明确内部ledger不发event/seq，业务预算仍原子投影；11250秒长例验证，投影取持久采样。若不接受A04则不得静默采用另一方案。

E37 R3问题5终态字节/并发draft：接受。预留128事件同时按64KiB投影上限及诊断预留字节；统一ReservationLedger纳临时/preflight/draft/对象/备份，写前保留。P02/P10/P14/P16b。

E38 R3问题6.1 DB等待后deadline：接受。P05/P13 admitting提交返回后紧邻enqueue重采样；明确未发终结不耗prompt但耗launch，admitting崩溃仍uncertain。接纳A07固定提交前决策点，DB排队后新样本，到期回滚，commit晚回不谎称精确毫秒停机。P12故障注入。

E39 R3问题6.2 actor等待barrier：计划闭合接受，规范已有设计§8.1不持actor等待。P07b具体受控任务等待、actor继续收handler/stop消息；P05可控反例。不是只释放mutex就完成活性。

E40 R3问题6.3 1秒片与5秒DBbusy：接受。P14独立本地lease监督queue/gateway/tools，数据库等待不续借未扣预算，过期撤准入/清理；不能持久化时不显示stopped。远端残余另记。

E41 R3问题8多claim与旧challenge饥饿：部分接受测试不足，不改算法。设计§7.2已经第一条claim、仅上一阶段challenge；补每人20claim与R5夹具，基础目标1、incoming最多3、全部valid消息至少分配；不引入全历史未结任务重开框架。P03。

E42 R3问题9 usage倒退epoch与UI：接受乱序修正并显式A06，因为设计本身写回退新epoch；可信reset才变epoch，seq2=15/seq1=10/seq3=20结果20，未知标unknown。UI独立刷新已在设计§9.4，A03补具体读取/usage_version，P16a不改业务revision。

E43 R3问题10大整数/字节：接受。A01 u64十进制字符串、默认/null/absence与Unicode转义定界、self-hash排除，黄金expected bytes+hash；P01/P19跨Rust/TS逐字节测试，不放宽校验。

E44 R3局部5、R2 object_ref：接受P01提前完整冻结；A03对象种类、固定hash、授权、分块上限、MAC cursor和到期处理，旧页引用不能读“最新”。不新增旁路下载endpoint。P17/P18消费者按此实现。

E45 R1范围最小切线、R2 P06不是最小实现、R3排序：接受风险分层，不削减隔离承诺。P00可丢弃探针、P06/07共享资格纵向链是最小有意义切线；G1通过才完整产品。gate/迁移/资源/恢复先到位后接入口，不以UI隐藏代替禁运行。

E46 R2质量建议未确认、小样本、R3质量分母/失败/额外资源/提前冻结：接受。P01封存holdout和附录D行结构，失败机会不消失、双评+裁决、宏平均及原始分子分母、N/A空输出显式失败，P23真实试验另批。80/90是提案；不宣称统计/等预算/因果优势。

E47 R3质量运维指标：接受。A03真实metrics读取由P13–17计数实现，P23验证uncertain/overrun/拒绝/事件存储/unknown；无需新监控平台，不能只列名。

E48 R1/R2建议G0全部、R3最终顺序与边界：接受具体freeze gate，未批准A项不可冻结临时实现；先核包完整，再按真实DAG和任务级环境。规范未认证组合依旧not_tested，P22真实产品路径复验，G4用户决定有限启用。保留完整Rust lib与既有integration发布门，不以此次文档自审替代运行证据。

## F 旧任务映射和不变量追踪

旧计划P01–P23的覆盖均保留；“拆分”改变施工/提交边界，不删原验收。旧P08之前新增P00先排可行性风险。

| 旧任务 | 新任务与变化 |
|---|---|
| P01 契约 | P01加A01–A09、18命令、CI/锁文件与质量预注册 |
| P02 预算上下文 | P02共享DeliveryEncoder及全模型请求账本；P06b执行 |
| P03 策略 | P03按规范第一claim与上一阶段challenge，补20claim/R5 |
| P04 结果 | P04纯规则，P07c共享核心，P11并发持久封存 |
| P05 fake核 | P05协作调度/actor活性/最终时钟；出口M0a |
| P06 隔离 | P00风险探针，P06a隔离/门禁，P06b宿主网关 |
| P07 ACP链 | P07a策略同步准入，P07b私有ingress，P07c companion/三工具，P07d registry，P07e完整能力与完成失败 |
| P08 资格 | P08共享核心与真实链全必需证据门 |
| P09 DB | P09a全库逐连接独立修复，P09b表与持久registry同交付 |
| P10 快照 | P10内容对象/确认绑定/全局存储预留；编码前移P02 |
| P11 MCP | P11 DurableToolStore复用P07c；并发CAS测试 |
| P12 接纳 | P12先引入MonoClock/FakeClock，再做语义事务/投影/发布，A07精确时钟语义 |
| P13 服务 | P13所有权与全局恢复quarantine；不早于P14/P15就Ready |
| P14 运行预算 | P14独立执行lease、内部checkpoint、完整permit proof |
| P15 控制恢复 | P15八行矩阵、全局启动屏障与各入口gate |
| P16 运维 | P16a usage/诊断；P16b备份/GC；迟到读走A03 |
| P17 事件 | P17固定分页/授权/指标，桌面真实target另验 |
| P18 API | P18唯一18命令，确认对象与get各模式明确 |
| P19 客户端 | P19规范字节/hash、u64字符串、安全context、重放 |
| P20 创建 | P20确认preflight ID与漂移失效 |
| P21 详情 | P21安全DOM/真浏览器、locale、忠实控制和coverage |
| P22 产品闭环 | P22三构建面/四integration/真实资格持久路径复验 |
| P23 启用 | 生产gate前移P06a，质量协议前移P01；P23运行评估/回滚/有限批准 |

I1唯一accepted/当前phase：P09b/P11/P12；I2 actor/fence及迟到计量边界：P07a/P13/P15/P16a；I3不可变published正文与membership：P10/P12/P17；I4冻结输入：P02/P03/P10/P11；I5原子关系/预算/投影/event：P12/P14；I6 stop后零新增本地准入：P05/P07a/P13/P14/P15；I7清理前不复用permit/binding：P06a/P13/P14/P15；I8连续seq/重放：P12/P17/P19；I9不合格不运行：P00/P06a/P06b/P08/P18/P22；I10条件具备则收敛否则明确阻断：P05/P07b/P13/P14/P15/P21。

F55八个反例逐一对应：准入竞态T02→P05/P13；fold闭合T14→P12/P17/P19；H分页T15→P17/P19；closing+控制T11→P15；空claims T08→P03/P04；延迟update T07→P05/P07b/P08；预算不足T12→P02/P14/P15；dirty快照T17→P10。八场景可以共用任务，不要求八个不同任务编号。
