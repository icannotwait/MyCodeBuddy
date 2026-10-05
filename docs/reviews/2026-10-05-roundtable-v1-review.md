# Roundtable v1 分支审查

审查日期：2026-10-05。审查对象：`feat/roundtable-v1` 的特性提交，以及审查开始时的暂存、未暂存、未跟踪内容。

## 结论

**不建议按完整圆桌功能合并或启用。** 本次发现 14 项需要处理的问题：6 项 P1、8 项 P2。已实际复现测试目标编译失败、前端重放重复、阶段终结计数错误；其余问题由源码及跨模块调用链确认，并在各项说明证据边界。

当前分支包含协议核、数据库模型及相当数量的局部测试，但服务、真实运行时、命令、备份和界面之间尚未形成产品闭环。默认关闭执行是正确的发布策略；**关闭开关与缺少实现是两回事**。本报告没有把未启用的路径描述成已经发生的生产事故，也没有把 fake 测试通过当作隔离资格通过。

值得保留的实现包括：独立协议 crate、每物理 SQLite 连接的 PRAGMA 配置、关系约束、部分事务回滚测试、私有 ACP 事件入口，以及明确记录 `blocked_platform` 的资格报告。修复应沿这些已有模块完成接线，不必再增加一套抽象框架。

## 审查范围与基线

| 项目 | 值 |
| --- | --- |
| 分支 | `feat/roundtable-v1` |
| 分叉基线 | `cb7596d38f9aa0df52740e939da7732ccfaaf5d8` |
| 审查 HEAD | `4946d8f6` |
| 基线依据 | `git merge-base HEAD main` 与 `git merge-base HEAD origin/main` 均返回同一提交；计划也指定此基线 |
| 特性提交 | 32 个，完整清单见附录 |
| 已提交差异 | 168 个文件，43,221 行新增、248 行删除 |
| 暂存／未暂存修改 | 均为空 |
| 原有未跟踪内容 | 根目录 `nul`，0 字节；已检查，不包含可审查代码，未删除 |
| 本次新增内容 | 仅本报告；未修复源码、未提交、未推送、未启用功能 |

核对依据为 [设计 v1.1](D:/MyCodeBuddy-roundtable/docs/superpowers/specs/2026-10-03-roundtable-design-v1.1.md)、[实施计划](D:/MyCodeBuddy-roundtable/docs/superpowers/plans/2026-10-03-roundtable.md)、黄金契约及实际实现。审查按协议、ACP／运行时、持久化、服务／传输／前端分工，主审汇总并核对关键证据。覆盖全部变更模块与提交范围；不等同于执行所有平台、所有测试或对每条状态组合做穷举验证。

## 问题总览

P1：阻止本分支作为完整功能交付，或涉及结果正确性、数据丢失和状态隔离。P2：确定的局部功能／边界错误，需在相关路径接通前修复。当前多数圆桌业务路径尚未启用，严重程度表示其在既定调用条件下的影响。

| 编号 | 级别 | 问题 |
| --- | --- | --- |
| R01 | P1 | 新增运行时测试无法编译，质量门未通过 |
| R02 | P1 | 产品调用链仍由占位实现构成，开关无法使功能可用 |
| R03 | P1 | 接纳把提交回执当成模型正文，丢失结果及语义关系 |
| R04 | P1 | 发布事务忽略 fence 和控制状态，可覆盖停止结果 |
| R05 | P1 | 重开对象存储后，失败回滚可删除历史引用对象 |
| R06 | P1 | 投影丢弃消息与证据清单，无法按固定版本完整恢复 |
| R07 | P2 | 按历史 attempt 数终结阶段，重试会导致提前关闭 |
| R08 | P2 | 每 turn 启动上限错误地在整个 revision 间共享 |
| R09 | P2 | 工具提交先封存数据，后检查预算 |
| R10 | P2 | 参数错误绕过工具调用与响应字节额度 |
| R11 | P2 | 启动未加载持久内部会话过滤记录 |
| R12 | P2 | 分页游标越界导致 panic，且未绑定固定对象 |
| R13 | P2 | 前端重同步没有恢复重放水位，产生重复或漏消息 |
| R14 | P2 | 用量新 epoch 覆盖历史累计值 |

## 详细发现

### R01 — P1：新增运行时测试无法编译，质量门未通过

位置：[capability_boundary.rs:16](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/capability_boundary.rs:16)。

测试导入 `codeg_lib::roundtable::AdvertisedToolsNotUsed`，但该符号不存在。实际运行 `roundtable_runtime` 目标得到 `E0432`，编译退出码为 1；新增 CI job 也执行这个目标，因此不是仅本机缺少 Linux 隔离设施造成的阻塞。

此外，定向 ESLint 有 5 项错误，主 crate `cargo fmt --check` 失败，定向库 Clippy 在 `-D warnings` 下有 35 项错误。这些结果不另拆成风格 findings，但它们都是现有 CI 的交付阻断。完整命令见验证表。

建议：先修复不存在的测试接口及实际质量门；保持测试断言，不以删除安全测试或全局放宽 lint 代替修复。

### R02 — P1：产品链路未完成，不能通过切换发布开关获得圆桌功能

主要位置：[service.rs:251](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/service.rs:251)、[api.rs:57](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/api.rs:57)、[HTTP handler](D:/MyCodeBuddy-roundtable/src-tauri/src/web/handlers/roundtable.rs:7)。

确定的接线缺口如下：

| 层 | 当前实现 | 缺失行为 |
| --- | --- | --- |
| 服务启动 | AppState／Tauri 创建空 `RoundtableSlot`；生产源码没有 `RoundtableService::open` 调用或实际 `ParticipantRuntime` 实现 | 没有安装可工作的协调器 |
| 服务命令 | `start` 丢弃 room 后返回错误；create／preflight／pause／interject 分支最终均返回 `InvalidState`；`record_admitted` 只检查 store 是否存在 | 没有实际创建、启动和持久准入 |
| HTTP／Tauri | 18 个 HTTP 路由均固定返回 503；桌面文件只 re-export 普通函数，无命令注册；`execute` 拼接固定响应 JSON | 没有接到服务、授权及持久操作 |
| 执行进程／MCP | 生产 spawn 固定关闭；[codeg_mcp.rs:105](D:/MyCodeBuddy-roundtable/src-tauri/src/bin/codeg_mcp.rs:105) 丢弃 stdin；companion 使用 `FakeBrokerTransport` | 没有实际服务工具请求／响应链 |
| 控制与恢复 | [control.rs:323](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/control.rs:323) 使用进程全局内存 book；recovery 仅检查内存标志 | 不具备计划中的跨重启幂等控制及恢复 |
| 备份 | [maintenance.rs:63](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/maintenance.rs:63) 处理内存 catalog；原备份文件只新增常量和固定返回值函数 | 没有实际对象打包／校验恢复；managed sections 未接入 roundtable |
| 前端 | 创建／详情组件仅由测试引用，无页面入口；开始、暂停、重试按钮没有动作；API helper 不发请求 | 没有可操作的圆桌客户端 |

这不是要求现在开启未经资格验证的运行时。应继续保持默认关闭，同时把代码状态明确标为协议／原型阶段，或完成计划要求的真实服务链和持久 fake 闭环，再声称实现了该功能。

现有“端到端”证明不足：[e2e.rs:61](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/e2e.rs:61) 仅增加几个计数器；transport 测试把同一 `execute` 函数调用两遍当作双端 parity；WebSocket 的几个安全断言实际检查的是返回 `true` 的函数。这些不能替代真实路由、订阅和恢复测试。

### R03 — P1：接纳时把回执写成消息正文

位置：[acceptance.rs:340](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/acceptance.rs:340)，相关写入与解析在 372、390–398 行；上游：[mcp.rs:795](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/mcp.rs:795)。

MCP 将规范化模型结果写入 `rt_submissions.candidate_ref`，将 `CandidateReceipt` 写入 `receipt_json`。接纳事务却读取后者，写进 `rt_messages.body_json`，并从中提取 claims、responses、position changes 和 evidence。

触发：通过实际持久提交接口提交一份合法结果，再调用接纳。结果正文变成包含 submission/candidate 标识的回执，语义关系丢失，`body_hash` 仍是原结果 hash，和落库正文不一致。当前验收测试手工往 `receipt_json` 填 `{}` 或自制结果，绕过了真实提交契约。

建议：读取并验证真正 candidate，按 `ValidatedResult` 及别名映射落库。不能只替换字段名：当前 claims 插入还期待 `statement`，协议 `ClaimV1` 使用 `text`。增加一条真实 `submit → accept → read message/claims/evidence` 的跨模块回归。

证据：静态跨模块契约确认，未运行完整产品流程。

### R04 — P1：发布未检查 fence，旧任务可以覆盖停止结果

位置：[acceptance.rs:641](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/acceptance.rs:641)，综合结果完成写入见 [732 行](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/acceptance.rs:732)。

`publish_in` 只验证 cleanup 标志、phase 仍为 closing、accepted 数等，完全不读取 `input.fence`，也不核对 room 状态、run／boot epoch 与 active control。

触发：phase 进入 closing 后，stop／pause／restart 已更新房间控制状态，但旧发布任务继续执行。只要 phase 仍为 closing，它仍能发布旧集合、创建后继；synthesis 分支还无条件把房间改成 `completed`。调用方预检查不能代替这个事务内检查。

建议：发布事务内校验全部相关 fence、房间控制状态及当前授权的冻结集合，明确处理暂停／停止／重启的覆盖规则；测试 stale fence 和 closing 期间 stop 的交错。当前证据为事务源码确认，生产控制接线尚未完成。

### R05 — P1：对象回滚可能删除历史清单引用的数据

位置：[objects.rs:323](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/objects.rs:323)，回滚调用：[snapshot.rs:400](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/snapshot.rs:400)。

`ObjectStore::open` 每次初始化空 `committed` 集合。`put` 发现已有相同 hash 文件会复用，但回滚保护只看当前实例的内存集合，不查询持久引用，也不区分文件是否由本次创建。

触发步骤：

1. 提交清单 A，使其引用对象 X。
2. 重开 ObjectStore，再捕获相同文件，复用 X。
3. 新清单因版本冲突、外键或存储错误提交失败。
4. `discard_ids` 认为 X 未提交而删除它；A 的数据库记录仍然存在。

影响：历史证据变为永久缺失。现有同实例保护测试覆盖不到重开情形。

建议：失败回滚只清理本次独占创建且确认无引用的对象。复用对象不要在回滚路径删除；持久引用回收留给已有 GC 机制完善后处理。证据为源码生命周期确认。

### R06 — P1：投影没有携带恢复消息与证据所需的固定清单

位置：[projection.rs:43](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/src/projection.rs:43)、[model.rs:1575](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/src/model.rs:1575)。

`RoomAggregate` 有 messages 和 evidence manifests，但 `project()` 只验证它们存在，最后只序列化并返回 `state.body`。`ProjectionBodyV1` 没有这些清单或固定 manifest 引用，也缺少计划要求的 participants、turn／attempt、operation、input、budget 和 coverage 数据。

影响：只拿到一个不可变 projection 的客户端无法确定该版本包含哪些结果与证据。相同 body 配上不同消息集合仍产生相同 hash。之后读取“当前消息”无法补救历史水位的一致性，重连恢复并不闭合。

建议：将恢复所需状态或可按 hash 读取的不可变清单引用纳入 projection body 和 hash；验证不同已发布集合必然得到不同投影，并可仅凭固定投影恢复。证据为协议类型与编码路径确认。

### R07 — P2：历史重试次数被当成已经终结的 turn 数

位置：[acceptance.rs:528](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/acceptance.rs:528)，主持失败判断也受影响，见 556–566 行。

`terminal` 对所有终态 attempts 求 `COUNT(*)`，却与 turns 数比较。三个 turn 中 A 先 failed 后 accepted，B accepted，C 仍 validating，就得到 terminal=3、turns=3；若达到 quorum，会提前冻结阶段并拒绝 C 随后完成的结果。主持失败后成功重试，也仍会被历史 failed attempt 判为综合失败。

已使用内存 SQLite 执行该查询复现：`turns=3, terminal_used_by_close=3, unfinished_attempts=1, premature_close=true`。

建议：按每个 turn 的当前最终状态判断是否已终结，排除旧 attempt 和正在执行的重试；不能仅把总计数改成一个粗略的 distinct count。

### R08 — P2：启动额度按整个 revision 共享，第三个成员首次启动即失败

位置：[budget.rs:275](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/src/budget.rs:275)。

全房间 `BudgetState.revisions` 只按 `request.revision` 查找 `TurnAccounting`，把每 turn 最多两次 launch／admission 的限制应用到了该 revision 的所有成员。N=3 时，三位成员各发起第一次启动，第三位就被 `launch_limit` 拒绝；前两位也会互相消耗修复额度。

现有 [budget_context.rs:391](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/tests/budget_context.rs:391) 把不同 ordinal 的第三次启动失败当成正确结果，所以测试绿灯不能证明规则正确。产品预算包装直接调用此函数，但尚未接入完整启动流程。

建议：以实际 turn 身份（phase revision／speaker，至少包含 `AttemptSlot`）记账，并保留独立房间总预算；同时检查重开 revision 的 spent 归属。证据为规则实现及测试语义确认。

### R09 — P2：预算拒绝之前已经封存了提交

位置：[tool_core.rs:516](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/tool_core.rs:516)，费用检查在 [531 行](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/tool_core.rs:531)。

`submit_result` 先 await `submit_canonical`／`note_field_errors` 持久变更，之后才调用可失败的 `charge_ledger`。

触发：该 attempt 已达到 `max_tool_calls`，再提交一份有效结果。数据库先封存 candidate，调用随后返回 `tool_budget`，内存 submission 状态却没有更新。调用者看到失败，持久层却已接受且无法正常修正。无需并发即可发生。

建议：在持久变更之前保留预算，并保证持久提交、账本及返回结果一致；补一条额度耗尽时数据库不能出现新 sealed candidate 的回归。证据为共享 tool core 的操作顺序确认。

### R10 — P2：无效工具请求不消耗调用与响应额度

位置：[tool_core.rs:437](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/tool_core.rs:437)，同类路径见 408–415、482–488 行。

参数解析、非法 alias、行范围及 query bounds 的错误直接提前返回，未经过统一 `charge_exchange`。反复调用 `search_evidence({file_alias:"x",query:"a",limit:0})` 会持续返回 `query_bounds`，但 `tool_calls` 和 `tool_reply_bytes` 不增长。部分非法读取还会先完整加载对象再失败。

影响：失败响应也应计入 attempt 预算的约束被绕过；执行次数与错误返回开销不能按上限收敛。当前是真实工具核心中的缺陷，尚无已启用的公共产品调用链。

建议：在共同 dispatch 边界统一为所有已准入请求计数，并对成功／错误响应统一做有界记账，避免每个提前返回分支单独补丁。

### R11 — P2：重启后没有从数据库恢复内部会话隐藏记录

位置：[registry.rs:285](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/registry.rs:285)。

桌面与服务器启动都调用 `install_process_discovery`，但它总是构造 `Self::in_memory()`。虽然 `RoundtableStore` 实现了 `RegistryStore`，没有启动路径把它传进去并加载 `rt_internal_bindings`。

触发前提：数据目录已有圆桌内部 binding 及对应转录。重启后，parser、direct lookup、import 的新增过滤查询空的进程内 registry，不能利用持久记录隐藏这些内部会话。

建议：在普通扫描开始前加载真实数据库 registry，再安装过滤与 discovery gate。当前生产执行关闭，不能把此项表述为已经发生的数据泄漏；它是历史数据恢复与过滤的确定接线缺陷。

### R12 — P2：分页游标越界会 panic，且没有绑定历史清单

位置：[paging.rs:19](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/paging.rs:19)。

`start` 直接来自游标数字，`end` 被截到 entries 长度，随后执行 `entries[start..end]`。例如一个元素的列表，cursor=`"2"`、limit=100，得到切片 `2..1`，必然 panic；极大游标的加法还可能溢出。非法非数字游标反而默默回到第一页。

此外，游标只保存偏移，未绑定 manifest／body hash。函数接受调用者传入的新 entries 与 hash，不检查它们是否与上一页相同，无法保证约定的固定水位分页。

建议：严格解析并检查 cursor／limit，使用 checked arithmetic，返回结构化错误；游标绑定固定清单身份并校验。证据为静态边界推导；实际 HTTP 路由当前仍固定 503，未声称可远程触发。

### R13 — P2：前端重同步丢失重放水位，出现重复和序号冲突

位置：[reducer.ts:16](D:/MyCodeBuddy-roundtable/src/lib/roundtable/reducer.ts:16)、[reducer.ts:30](D:/MyCodeBuddy-roundtable/src/lib/roundtable/reducer.ts:30)。

`resyncRoundtable` 只替换 accepted 文本，既不携带 last_seq，也不重建外部 `seen`。实际执行现有 TypeScript 函数：先处理 seq=1，再 resync 到包含 seq=1、2 的服务端状态，最后重放 seq=2，得到 `accepted=["one","two","two"]`。

同一个 `seen` 还用于 preview 和 durable accepted。以各自序号 1 先预览后接纳，accepted 会被直接丢弃，保留 draft。这不符合预览序列与持久事件序列分离的约定。现有事件类型也没有 room／epoch／incarnation 的隔离信息。

建议：把持久水位与固定 projection 一起恢复，隔离预览和持久序号空间；不要用外部可变 Set 代替可恢复状态。增加 resync 后重放重叠区间的最小回归。

证据：通过 TypeScript transpile 后直接执行当前源码复现，未修改仓库或测试。

### R14 — P2：新计量 epoch 覆盖累计用量

位置：[usage.rs:124](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/src/usage.rs:124)，新 epoch 与 reset 分支见 77–95 行。

先收到 output_tokens/epoch=1/value=20，再收到可信新 epoch=2/value=5，`confirmed_output_tokens` 被赋值成 5，而非保留先前用量并累计为 25。`trusted_reset` 同样覆盖；计数器身份没有包含 source／scope，同名独立计数器还可能混在一起。

现有 `usage_scope_dedup_and_reset` 没有真正改变 epoch 或传 `trusted_reset=true`。当前 fold 主要由测试调用，服务计量入口另行把 measurement 放进内存 Vec，未形成持久产品查询链。

建议：按完整计数器身份保存各 epoch 高水位，仅将确认增量累加到总量，保留 unknown／uncertain 语义。证据为计量状态转换确认。

## 启用前仍需验证的事项

以下属于接线缺口或验证边界，不重复算作已证实的生产漏洞：

- `DeliveryEncoder::prompt_utf8` 当前只编码 phase hash、角色、schema、tools 等元数据，没有讨论问题、插话、已发布正文与目标 aliases。接入真实执行前必须验证模型确实收到完整的有界上下文，并且预算测量的是最终发送字节。
- Roundtable purpose 不属于 hidden generation，而实际 ACP permission handler 的拒绝分支仍只判断 hidden generation；新增 roundtable fs／terminal policy helper 没有接到实际 host handlers。保持执行关闭，先在真实受限运行路径验证这些边界。
- 工具 `FieldErrors` 响应 body 只标记种类，详细字段错误留在 Rust `decision` 中。接 MCP wire 时应验证模型收到机器可读修复信息。
- 投影应用 helper 只比较提供的 hash 标识并递增当前 seq；不能替代对象内容校验、实际持久序号和未知事件重同步流程。
- [资格报告](D:/MyCodeBuddy-roundtable/docs/roundtable/qualification/linux-codex-2.1.1/report.json) 明确为 `blocked_platform`、`g1_passed=false`、`qualification_issued=false`。本次未调用真实模型、未执行 Linux OCI 资格或质量实验，不能授予 G1／G3／G4 通过结论。

## 验证结果

所有结果来自本次审查；命令按所在目录执行。未通过的门没有通过修改源码予以修复。

| 检查 | 结果 | 说明 |
| --- | --- | --- |
| `pnpm test -- src/lib/roundtable src/components/roundtable` | 通过，5 文件／5 测试 | 仅局部组件与 helper |
| `cargo test --locked --manifest-path src-tauri/roundtable-protocol/Cargo.toml` | 通过，19 测试 | 6 个 integration targets；不涉及主 crate 产品接线 |
| `node --test spike/probe.test.mjs` | 通过，2 测试 | missing profile 与无授权不联络模型 |
| `node scripts/check-roundtable-locks.mjs` | 通过 | 33 个共享 lock 包匹配 |
| `cargo test --locked --no-default-features --features test-utils --test roundtable_runtime` | **失败，编译 E0432** | 不存在的 `AdvertisedToolsNotUsed` 导入；组合四目标运行时先在此失败 |
| `cargo test --locked --no-default-features --features test-utils --test roundtable_transport --test roundtable_service --test roundtable_protocol_io` | 通过，48 测试 | transport 2、service 17、protocol_io 29；单独重跑，未包含失败 runtime 目标 |
| `pnpm lint src/lib/roundtable src/components/roundtable src/components/layout/sidebar.tsx` | **失败，5 项错误** | stream.ts 与 roundtable-detail.test.tsx 的 Prettier 错误 |
| 主 crate `cargo fmt --check` | **失败** | 包含新增／修改 Rust 文件 |
| `cargo clippy --locked --no-default-features --features test-utils --lib -- -D warnings` | **失败，35 项错误** | 包括新模块 dead code、clone_on_copy 等；仅这一窄配置，不代表所有 feature 诊断数量 |
| `git diff --check main...HEAD` | 未通过 | 两个 EOF 空行及专门 CRLF fixture 的空白提示；后者不直接作为缺陷 |
| 原查询的 SQLite 终态计数反例 | 已复现 | 一 turn 重试会抵消另一未完成 turn 的计数 |
| 当前 TypeScript reducer 的重同步／重放反例 | 已复现 | 重复 accepted；预览／持久序号冲突导致丢失 accepted |

未运行：全量前端 suite／build、完整 Rust 回归、桌面窗口与远程客户端验收、server／mcp 全 feature 矩阵、真实 Linux sandbox、模型调用及付费质量实验。已有明确编译和质量门失败，本次采用最窄目标定位，并未用全量回归替代问题分析。

## 建议处理顺序

1. 修复 R01，恢复可执行的测试及质量门。
2. 用真实 SQLite 和 fake runtime 接一条 `create → submit → accept → close → publish → restart/read` 纵向流程；优先修复 R03–R08，避免继续堆只验证局部模拟状态的测试。
3. 修复工具预算、持久过滤、固定投影／分页与重连恢复；再连接真实 HTTP／Tauri／前端，并执行备份恢复验证。
4. 保持产品 gate 关闭，按精确平台与版本完成真实资格验证后，再决定是否启用。

## 附录：完整提交与文件清单

下列清单由审查基线与 HEAD 的 Git 差异生成；文件行数为 diff 统计，不表示测试覆盖率。

### 32 个提交（按时间顺序）

```text
dc91b878 docs(roundtable): record design v1.1 and plan v1.2
75c46c69 docs(roundtable): record bounded feasibility spike
739fb5c0 feat(roundtable): freeze protocol contracts
aa099709 fix(roundtable): align frozen wire with the golden contract
6faea795 feat(roundtable): bound attempts time and context
cfe91277 feat(roundtable): schedule bounded rounds and coverage
223b1abb feat(roundtable): validate and seal structured results
91b03335 test(roundtable): prove admission and completion races
e52f001f fix(roundtable): sample the completion gate around close
e89b5184 fix(roundtable): read the completion gate inside close
ab80659e feat(roundtable): gate and prepare isolated runtime
93853386 feat(roundtable): enforce bounded model gateway
a1f4a4e2 fix(roundtable): enforce reserve and per-envelope tool caps
4f32bc71 feat(roundtable): admit service-owned ACP turns
4f44a316 feat(roundtable): keep runtime ingress private and ordered
6566e254 feat(roundtable): add scoped service companion and shared tools
6c43f389 feat(roundtable): suppress internal session discovery
699d38ec feat(roundtable): seal launch capabilities and completion failures
48d76560 test(roundtable): record first adapter qualification verdict
0aa30e50 fix(db): initialize every SQLite pool connection
e567655a feat(roundtable): persist constrained roundtable state
0f4816c0 fix(roundtable): keep durability comments out of the source scan
8dec0731 feat(roundtable): freeze content-addressed evidence snapshots
ba9f5397 feat(roundtable): persist scoped evidence and submissions
c5729a35 fix(roundtable): compile the protocol suite on this toolchain
ca9df033 feat(roundtable): atomically accept close and publish
8dc41f1b feat(roundtable): own coordinator lifecycle and room actors
b328fb19 feat(roundtable): enforce active budgets and cleanup permits
77d1d309 feat(roundtable): keep closing controls idempotent
b4ae8b6f feat(roundtable): retain bounded usage and diagnostics
5667110c feat(roundtable): preserve backup, replay, commands, and the rollout gate
4946d8f6 feat(roundtable): add replay-safe roundtable client surfaces
```

### 168 个变更文件

| 文件 | 新增 | 删除 |
| --- | ---: | ---: |
| [.github/workflows/test.yml](D:/MyCodeBuddy-roundtable/.github/workflows/test.yml) | 32 | 0 |
| [docs/roundtable/fixtures/commands.json](D:/MyCodeBuddy-roundtable/docs/roundtable/fixtures/commands.json) | 49 | 0 |
| [docs/roundtable/fixtures/config.json](D:/MyCodeBuddy-roundtable/docs/roundtable/fixtures/config.json) | 188 | 0 |
| [docs/roundtable/fixtures/errors.json](D:/MyCodeBuddy-roundtable/docs/roundtable/fixtures/errors.json) | 98 | 0 |
| [docs/roundtable/fixtures/member.json](D:/MyCodeBuddy-roundtable/docs/roundtable/fixtures/member.json) | 156 | 0 |
| [docs/roundtable/fixtures/moderator.json](D:/MyCodeBuddy-roundtable/docs/roundtable/fixtures/moderator.json) | 346 | 0 |
| [docs/roundtable/fixtures/projection.json](D:/MyCodeBuddy-roundtable/docs/roundtable/fixtures/projection.json) | 150 | 0 |
| [docs/roundtable/fixtures/quality-holdout.json](D:/MyCodeBuddy-roundtable/docs/roundtable/fixtures/quality-holdout.json) | 270 | 0 |
| [docs/roundtable/fixtures/result-counterexamples.json](D:/MyCodeBuddy-roundtable/docs/roundtable/fixtures/result-counterexamples.json) | 290 | 0 |
| [docs/roundtable/qualification/linux-codex-2.1.1/evidence-index.md](D:/MyCodeBuddy-roundtable/docs/roundtable/qualification/linux-codex-2.1.1/evidence-index.md) | 38 | 0 |
| [docs/roundtable/qualification/linux-codex-2.1.1/report.json](D:/MyCodeBuddy-roundtable/docs/roundtable/qualification/linux-codex-2.1.1/report.json) | 160 | 0 |
| [docs/roundtable/qualification/spike-verdict.md](D:/MyCodeBuddy-roundtable/docs/roundtable/qualification/spike-verdict.md) | 68 | 0 |
| [docs/roundtable/quality-protocol.md](D:/MyCodeBuddy-roundtable/docs/roundtable/quality-protocol.md) | 47 | 0 |
| [docs/superpowers/plans/2026-10-03-roundtable.md](D:/MyCodeBuddy-roundtable/docs/superpowers/plans/2026-10-03-roundtable.md) | 1388 | 0 |
| [docs/superpowers/rollout/2026-10-03-roundtable.md](D:/MyCodeBuddy-roundtable/docs/superpowers/rollout/2026-10-03-roundtable.md) | 9 | 0 |
| [docs/superpowers/specs/2026-10-03-roundtable-design-v1.1.md](D:/MyCodeBuddy-roundtable/docs/superpowers/specs/2026-10-03-roundtable-design-v1.1.md) | 720 | 0 |
| [scripts/check-roundtable-locks.mjs](D:/MyCodeBuddy-roundtable/scripts/check-roundtable-locks.mjs) | 84 | 0 |
| [spike/fixtures/cases.json](D:/MyCodeBuddy-roundtable/spike/fixtures/cases.json) | 53 | 0 |
| [spike/probe.mjs](D:/MyCodeBuddy-roundtable/spike/probe.mjs) | 353 | 0 |
| [spike/probe.test.mjs](D:/MyCodeBuddy-roundtable/spike/probe.test.mjs) | 89 | 0 |
| [spike/report.json](D:/MyCodeBuddy-roundtable/spike/report.json) | 142 | 0 |
| [src-tauri/.gitignore](D:/MyCodeBuddy-roundtable/src-tauri/.gitignore) | 3 | 0 |
| [src-tauri/Cargo.lock](D:/MyCodeBuddy-roundtable/src-tauri/Cargo.lock) | 12 | 0 |
| [src-tauri/Cargo.toml](D:/MyCodeBuddy-roundtable/src-tauri/Cargo.toml) | 3 | 0 |
| [src-tauri/roundtable-protocol/Cargo.lock](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/Cargo.lock) | 297 | 0 |
| [src-tauri/roundtable-protocol/Cargo.toml](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/Cargo.toml) | 17 | 0 |
| [src-tauri/roundtable-protocol/src/admission.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/src/admission.rs) | 715 | 0 |
| [src-tauri/roundtable-protocol/src/budget.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/src/budget.rs) | 640 | 0 |
| [src-tauri/roundtable-protocol/src/canonical.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/src/canonical.rs) | 437 | 0 |
| [src-tauri/roundtable-protocol/src/completion.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/src/completion.rs) | 517 | 0 |
| [src-tauri/roundtable-protocol/src/context.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/src/context.rs) | 502 | 0 |
| [src-tauri/roundtable-protocol/src/dto.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/src/dto.rs) | 274 | 0 |
| [src-tauri/roundtable-protocol/src/lib.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/src/lib.rs) | 36 | 0 |
| [src-tauri/roundtable-protocol/src/model.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/src/model.rs) | 1856 | 0 |
| [src-tauri/roundtable-protocol/src/projection.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/src/projection.rs) | 49 | 0 |
| [src-tauri/roundtable-protocol/src/strategy.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/src/strategy.rs) | 728 | 0 |
| [src-tauri/roundtable-protocol/src/usage.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/src/usage.rs) | 127 | 0 |
| [src-tauri/roundtable-protocol/src/validation.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/src/validation.rs) | 1130 | 0 |
| [src-tauri/roundtable-protocol/tests/admission_races.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/tests/admission_races.rs) | 260 | 0 |
| [src-tauri/roundtable-protocol/tests/budget_context.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/tests/budget_context.rs) | 1054 | 0 |
| [src-tauri/roundtable-protocol/tests/completion_barrier.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/tests/completion_barrier.rs) | 219 | 0 |
| [src-tauri/roundtable-protocol/tests/contracts.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/tests/contracts.rs) | 1106 | 0 |
| [src-tauri/roundtable-protocol/tests/result_contract.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/tests/result_contract.rs) | 1023 | 0 |
| [src-tauri/roundtable-protocol/tests/strategy.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/tests/strategy.rs) | 967 | 0 |
| [src-tauri/roundtable-protocol/tests/support/mod.rs](D:/MyCodeBuddy-roundtable/src-tauri/roundtable-protocol/tests/support/mod.rs) | 128 | 0 |
| [src-tauri/src/acp/agent_process.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/acp/agent_process.rs) | 154 | 1 |
| [src-tauri/src/acp/connection.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/acp/connection.rs) | 216 | 12 |
| [src-tauri/src/acp/host_tools_policy.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/acp/host_tools_policy.rs) | 32 | 0 |
| [src-tauri/src/acp/manager.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/acp/manager.rs) | 218 | 19 |
| [src-tauri/src/app_state.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/app_state.rs) | 4 | 0 |
| [src-tauri/src/auto_title/internal_sessions.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/auto_title/internal_sessions.rs) | 111 | 4 |
| [src-tauri/src/auto_title/types.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/auto_title/types.rs) | 9 | 0 |
| [src-tauri/src/bin/codeg_mcp.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/bin/codeg_mcp.rs) | 63 | 168 |
| [src-tauri/src/commands/backup/manifest.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/commands/backup/manifest.rs) | 4 | 0 |
| [src-tauri/src/commands/backup/restore.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/commands/backup/restore.rs) | 4 | 0 |
| [src-tauri/src/commands/backup/sections.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/commands/backup/sections.rs) | 4 | 0 |
| [src-tauri/src/commands/backup/source.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/commands/backup/source.rs) | 4 | 0 |
| [src-tauri/src/commands/conversations.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/commands/conversations.rs) | 33 | 4 |
| [src-tauri/src/commands/mod.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/commands/mod.rs) | 1 | 0 |
| [src-tauri/src/commands/roundtable.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/commands/roundtable.rs) | 3 | 0 |
| [src-tauri/src/db/migration/m20261003_000001_roundtable.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/db/migration/m20261003_000001_roundtable.rs) | 34 | 0 |
| [src-tauri/src/db/migration/mod.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/db/migration/mod.rs) | 2 | 0 |
| [src-tauri/src/db/mod.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/db/mod.rs) | 193 | 33 |
| [src-tauri/src/db/service/import_service.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/db/service/import_service.rs) | 31 | 0 |
| [src-tauri/src/lib.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/lib.rs) | 5 | 0 |
| [src-tauri/src/parsers/mod.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/parsers/mod.rs) | 37 | 7 |
| [src-tauri/src/roundtable/acceptance.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/acceptance.rs) | 1296 | 0 |
| [src-tauri/src/roundtable/actor.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/actor.rs) | 87 | 0 |
| [src-tauri/src/roundtable/api.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/api.rs) | 117 | 0 |
| [src-tauri/src/roundtable/authorization.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/authorization.rs) | 36 | 0 |
| [src-tauri/src/roundtable/budget_ledger.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/budget_ledger.rs) | 162 | 0 |
| [src-tauri/src/roundtable/capabilities.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/capabilities.rs) | 494 | 0 |
| [src-tauri/src/roundtable/clock.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/clock.rs) | 167 | 0 |
| [src-tauri/src/roundtable/command_processor.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/command_processor.rs) | 21 | 0 |
| [src-tauri/src/roundtable/companion.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/companion.rs) | 575 | 0 |
| [src-tauri/src/roundtable/control.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/control.rs) | 350 | 0 |
| [src-tauri/src/roundtable/diagnostics.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/diagnostics.rs) | 45 | 0 |
| [src-tauri/src/roundtable/e2e.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/e2e.rs) | 72 | 0 |
| [src-tauri/src/roundtable/events.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/events.rs) | 94 | 0 |
| [src-tauri/src/roundtable/feature_gate.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/feature_gate.rs) | 302 | 0 |
| [src-tauri/src/roundtable/gateway.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/gateway.rs) | 723 | 0 |
| [src-tauri/src/roundtable/ingress.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/ingress.rs) | 486 | 0 |
| [src-tauri/src/roundtable/maintenance.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/maintenance.rs) | 125 | 0 |
| [src-tauri/src/roundtable/mcp.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/mcp.rs) | 1306 | 0 |
| [src-tauri/src/roundtable/mod.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/mod.rs) | 191 | 0 |
| [src-tauri/src/roundtable/objects.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/objects.rs) | 426 | 0 |
| [src-tauri/src/roundtable/ownership.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/ownership.rs) | 155 | 0 |
| [src-tauri/src/roundtable/paging.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/paging.rs) | 27 | 0 |
| [src-tauri/src/roundtable/qualification.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/qualification.rs) | 138 | 0 |
| [src-tauri/src/roundtable/qualification_harness.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/qualification_harness.rs) | 280 | 0 |
| [src-tauri/src/roundtable/recovery.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/recovery.rs) | 57 | 0 |
| [src-tauri/src/roundtable/registry.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/registry.rs) | 602 | 0 |
| [src-tauri/src/roundtable/relay.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/relay.rs) | 160 | 0 |
| [src-tauri/src/roundtable/request_accounting.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/request_accounting.rs) | 237 | 0 |
| [src-tauri/src/roundtable/resources.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/resources.rs) | 221 | 0 |
| [src-tauri/src/roundtable/rollout.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/rollout.rs) | 28 | 0 |
| [src-tauri/src/roundtable/runtime.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/runtime.rs) | 250 | 0 |
| [src-tauri/src/roundtable/sandbox.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/sandbox.rs) | 610 | 0 |
| [src-tauri/src/roundtable/sandbox/linux_oci.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/sandbox/linux_oci.rs) | 226 | 0 |
| [src-tauri/src/roundtable/schema.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/schema.rs) | 511 | 0 |
| [src-tauri/src/roundtable/service.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/service.rs) | 438 | 0 |
| [src-tauri/src/roundtable/snapshot.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/snapshot.rs) | 801 | 0 |
| [src-tauri/src/roundtable/store.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/store.rs) | 1264 | 0 |
| [src-tauri/src/roundtable/tool_core.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/tool_core.rs) | 690 | 0 |
| [src-tauri/src/roundtable/usage.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/roundtable/usage.rs) | 36 | 0 |
| [src-tauri/src/server_bin/main.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/server_bin/main.rs) | 2 | 0 |
| [src-tauri/src/web/event_bridge.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/web/event_bridge.rs) | 17 | 0 |
| [src-tauri/src/web/handlers/mod.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/web/handlers/mod.rs) | 1 | 0 |
| [src-tauri/src/web/handlers/roundtable.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/web/handlers/roundtable.rs) | 12 | 0 |
| [src-tauri/src/web/mod.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/web/mod.rs) | 4 | 0 |
| [src-tauri/src/web/router.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/web/router.rs) | 18 | 0 |
| [src-tauri/src/web/ws.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/web/ws.rs) | 4 | 0 |
| [src-tauri/src/web/ws_attach.rs](D:/MyCodeBuddy-roundtable/src-tauri/src/web/ws_attach.rs) | 4 | 0 |
| [src-tauri/tests/fixtures/roundtable/snapshot/.gitattributes](D:/MyCodeBuddy-roundtable/src-tauri/tests/fixtures/roundtable/snapshot/.gitattributes) | 1 | 0 |
| [src-tauri/tests/fixtures/roundtable/snapshot/binary.bin](D:/MyCodeBuddy-roundtable/src-tauri/tests/fixtures/roundtable/snapshot/binary.bin) | - | - |
| [src-tauri/tests/fixtures/roundtable/snapshot/crlf.txt](D:/MyCodeBuddy-roundtable/src-tauri/tests/fixtures/roundtable/snapshot/crlf.txt) | 2 | 0 |
| [src-tauri/tests/fixtures/roundtable/snapshot/dirty.txt](D:/MyCodeBuddy-roundtable/src-tauri/tests/fixtures/roundtable/snapshot/dirty.txt) | 1 | 0 |
| [src-tauri/tests/fixtures/roundtable/snapshot/no-final-newline.txt](D:/MyCodeBuddy-roundtable/src-tauri/tests/fixtures/roundtable/snapshot/no-final-newline.txt) | 1 | 0 |
| [src-tauri/tests/fixtures/roundtable/snapshot/tracked.txt](D:/MyCodeBuddy-roundtable/src-tauri/tests/fixtures/roundtable/snapshot/tracked.txt) | 1 | 0 |
| [src-tauri/tests/fixtures/roundtable/snapshot/untracked.txt](D:/MyCodeBuddy-roundtable/src-tauri/tests/fixtures/roundtable/snapshot/untracked.txt) | 1 | 0 |
| [src-tauri/tests/roundtable_cases/capability_boundary.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/capability_boundary.rs) | 658 | 0 |
| [src-tauri/tests/roundtable_cases/companion.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/companion.rs) | 690 | 0 |
| [src-tauri/tests/roundtable_cases/db_profile.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/db_profile.rs) | 119 | 0 |
| [src-tauri/tests/roundtable_cases/gateway.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/gateway.rs) | 957 | 0 |
| [src-tauri/tests/roundtable_cases/private_ingress.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/private_ingress.rs) | 502 | 0 |
| [src-tauri/tests/roundtable_cases/registry.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/registry.rs) | 450 | 0 |
| [src-tauri/tests/roundtable_cases/roundtable_acceptance.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/roundtable_acceptance.rs) | 660 | 0 |
| [src-tauri/tests/roundtable_cases/roundtable_backup_gc.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/roundtable_backup_gc.rs) | 88 | 0 |
| [src-tauri/tests/roundtable_cases/roundtable_budget_runtime.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/roundtable_budget_runtime.rs) | 178 | 0 |
| [src-tauri/tests/roundtable_cases/roundtable_control_recovery.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/roundtable_control_recovery.rs) | 338 | 0 |
| [src-tauri/tests/roundtable_cases/roundtable_durability_usage.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/roundtable_durability_usage.rs) | 171 | 0 |
| [src-tauri/tests/roundtable_cases/roundtable_mcp.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/roundtable_mcp.rs) | 1069 | 0 |
| [src-tauri/tests/roundtable_cases/roundtable_qualification.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/roundtable_qualification.rs) | 1483 | 0 |
| [src-tauri/tests/roundtable_cases/roundtable_rollout.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/roundtable_rollout.rs) | 31 | 0 |
| [src-tauri/tests/roundtable_cases/roundtable_snapshot.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/roundtable_snapshot.rs) | 812 | 0 |
| [src-tauri/tests/roundtable_cases/roundtable_store_schema.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/roundtable_store_schema.rs) | 750 | 0 |
| [src-tauri/tests/roundtable_cases/runtime_admission.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/runtime_admission.rs) | 507 | 0 |
| [src-tauri/tests/roundtable_cases/sandbox.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_cases/sandbox.rs) | 604 | 0 |
| [src-tauri/tests/roundtable_protocol_io.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_protocol_io.rs) | 23 | 0 |
| [src-tauri/tests/roundtable_runtime.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_runtime.rs) | 23 | 0 |
| [src-tauri/tests/roundtable_service.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_service.rs) | 475 | 0 |
| [src-tauri/tests/roundtable_support/mod.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_support/mod.rs) | 99 | 0 |
| [src-tauri/tests/roundtable_transport.rs](D:/MyCodeBuddy-roundtable/src-tauri/tests/roundtable_transport.rs) | 213 | 0 |
| [src/components/layout/sidebar.tsx](D:/MyCodeBuddy-roundtable/src/components/layout/sidebar.tsx) | 2 | 0 |
| [src/components/roundtable/create-roundtable.test.tsx](D:/MyCodeBuddy-roundtable/src/components/roundtable/create-roundtable.test.tsx) | 32 | 0 |
| [src/components/roundtable/create-roundtable.tsx](D:/MyCodeBuddy-roundtable/src/components/roundtable/create-roundtable.tsx) | 27 | 0 |
| [src/components/roundtable/preflight-confirmation.tsx](D:/MyCodeBuddy-roundtable/src/components/roundtable/preflight-confirmation.tsx) | 30 | 0 |
| [src/components/roundtable/roundtable-controls.tsx](D:/MyCodeBuddy-roundtable/src/components/roundtable/roundtable-controls.tsx) | 14 | 0 |
| [src/components/roundtable/roundtable-detail.test.tsx](D:/MyCodeBuddy-roundtable/src/components/roundtable/roundtable-detail.test.tsx) | 42 | 0 |
| [src/components/roundtable/roundtable-detail.tsx](D:/MyCodeBuddy-roundtable/src/components/roundtable/roundtable-detail.tsx) | 26 | 0 |
| [src/components/roundtable/roundtable-safe-content.tsx](D:/MyCodeBuddy-roundtable/src/components/roundtable/roundtable-safe-content.tsx) | 21 | 0 |
| [src/i18n/messages/ar.json](D:/MyCodeBuddy-roundtable/src/i18n/messages/ar.json) | 9 | 0 |
| [src/i18n/messages/de.json](D:/MyCodeBuddy-roundtable/src/i18n/messages/de.json) | 9 | 0 |
| [src/i18n/messages/en.json](D:/MyCodeBuddy-roundtable/src/i18n/messages/en.json) | 9 | 0 |
| [src/i18n/messages/es.json](D:/MyCodeBuddy-roundtable/src/i18n/messages/es.json) | 9 | 0 |
| [src/i18n/messages/fr.json](D:/MyCodeBuddy-roundtable/src/i18n/messages/fr.json) | 9 | 0 |
| [src/i18n/messages/ja.json](D:/MyCodeBuddy-roundtable/src/i18n/messages/ja.json) | 9 | 0 |
| [src/i18n/messages/ko.json](D:/MyCodeBuddy-roundtable/src/i18n/messages/ko.json) | 9 | 0 |
| [src/i18n/messages/pt.json](D:/MyCodeBuddy-roundtable/src/i18n/messages/pt.json) | 9 | 0 |
| [src/i18n/messages/zh-CN.json](D:/MyCodeBuddy-roundtable/src/i18n/messages/zh-CN.json) | 9 | 0 |
| [src/i18n/messages/zh-TW.json](D:/MyCodeBuddy-roundtable/src/i18n/messages/zh-TW.json) | 9 | 0 |
| [src/lib/roundtable/api.test.ts](D:/MyCodeBuddy-roundtable/src/lib/roundtable/api.test.ts) | 15 | 0 |
| [src/lib/roundtable/api.ts](D:/MyCodeBuddy-roundtable/src/lib/roundtable/api.ts) | 24 | 0 |
| [src/lib/roundtable/reducer.test.ts](D:/MyCodeBuddy-roundtable/src/lib/roundtable/reducer.test.ts) | 36 | 0 |
| [src/lib/roundtable/reducer.ts](D:/MyCodeBuddy-roundtable/src/lib/roundtable/reducer.ts) | 37 | 0 |
| [src/lib/roundtable/stream.test.ts](D:/MyCodeBuddy-roundtable/src/lib/roundtable/stream.test.ts) | 18 | 0 |
| [src/lib/roundtable/stream.ts](D:/MyCodeBuddy-roundtable/src/lib/roundtable/stream.ts) | 14 | 0 |
| [src/lib/roundtable/types.ts](D:/MyCodeBuddy-roundtable/src/lib/roundtable/types.ts) | 26 | 0 |
