# MyCodeBuddy 多智能体圆桌协议设计

版本 1.1 · 完整替代稿 · 2026 年 10 月 3 日

## 1 结论和批准边界

保留 Rust 共享核心中的独立 RoundtableService，以及“独立提案 → R 轮交叉评议 → 独立主持综合”的有界策略。成员数 N、轮数 R、并发 C 均为配置；3、2、3 只是预设。桌面、Web 与远程桌面共用控制面，不引入 Python 运行时，不伪造父 MCP 调用，不套用 brainstorm workflow 的业务状态。

v1.1 将原稿的原则补成可以检验的协议，并削减第一版能力：同一数据目录最多运行一个圆桌；不提供交互审批、运行中扩大资料范围、外部网页抓取、原会话冷恢复、跨房间公平队列或自动执行建议。保留取消、fence、清理屏障、不可变证据、幂等命令和持久重放。复杂度通过减少能力降低，不能通过降低隔离承诺降低。

本稿建议采用严格隔离档。一次性资料副本是实现材料，不单独构成安全边界。没有通过资格验证的平台与 CLI 组合不能启用真实运行；当前没有任何组合因本文而获得认证。协议冻结和 fake runtime 原型可以先获批准，真实启用必须等首个适配器验证通过。本文只完成设计修订，没有启动原型、调用模型、执行项目测试或改动项目代码。

### 1.1 产品结果

圆桌应交付建议、依据、被回应的质疑、仍存在的分歧、风险和需要用户决定的问题。成功不等于一致；成员失败不算赞同；引用存在不等于引用支持结论。服务验证身份、引用和覆盖，固定质量评审验证讨论是否比单模型或并行咨询更有价值。

### 1.2 建议决策

以下是本稿的推荐取舍，尚不代表用户已批准实施：

- 安全门槛高于未经隔离的普通对话；只提供 strict_snapshot_v1，不提供“与普通对话风险相同”的隐式降级
- 结果必须走专用 roundtable MCP 工具组；最终 assistant 文本只作预览
- 成员与主持记录只在圆桌详情可见，不进入普通会话侧栏
- 暂停立即中断，保留已接纳结果，丢弃未完成回答；按钮明确说明
- 综合失败允许只重试主持；不强制重跑已发布讨论
- 同时一个运行中圆桌是首版资源策略，不是 N/R/C 架构上限
- 每个 attempt 默认新建 binding；原会话复用和冷恢复留待独立认证

## 2 基线和证据范围

统一静态基线为 2587471a0ead1c5be4afda1ae3ff27bdea80606c。原始工作区 HEAD 仍是 abd3c6746c43c1925837acbb317c21afd53ce968；本次通过本地 Git 对象读取 2587471，未切换或修改该工作区。两者之间是三个 UI 与 workflow 投影提交；本文关注的 ACP、传输、事件桥和数据库文件在这一区间没有变化。[S1]

另有修复提交 6a66c0a2757052cf4647164b2f6c3b44e1f986ec，其 tree 为 0c26e3b88fc91f0d7dac064a79098929a3c3eab5，与本地四提交修复工作区 HEAD 4e4d327 的 tree 相同。这是独立修复线，不能假定已进入主线。它改变 watchdog、workflow 文档绑定和 UI 防护，没有实现圆桌隔离、目的类型、MCP 结果通道或持久事件协议。本设计以 2587471 为集成目标，必要时再对已合并的修复重新核对。[S2]

评审来源为三份完整文本，分别标记 V1、V2、V3。V1/V2 提及本地 Windows canvas，但 canvas 本体未提供；处理登记覆盖可取得文本中的全部明确发现，不虚构 canvas 内“25 条发现”的内容。V3 是设计反例评审，不应表述成产品中已复现的漏洞。源码、原设计与外部协议的核对均与实际运行验证分开。

## 3 现有基础与新增责任

### 3.1 静态核对结论

- ConnectionManager 已有无父连接的 spawn_agent、显式 emitter、预分配 connection incarnation 及取消清理能力。新增的是服务所有权和准入约束，不是第二套进程运行时。[S3]
- 现有 ConnectionPurpose 包含 User、Delegation、InternalProbe、InternalTitle、InternalTranslate。hidden generation 的特殊行为不能直接套给圆桌：它既抑制 companion 注入，也影响工具和 maxTurns；圆桌需要独立的策略字段。[S4]
- HostToolsPolicy::Agent 不广告宿主 fs/terminal，并已抑制 delegation 组；所以“普通用途必然拿到 delegate_to_agent”过于绝对。其他 companion 组、CLI 自带工具和全局配置仍需隔离。[S5]
- 现有 hosted fs 能力同时打开读与写；Grok 的 terminal 已有特殊处理，并非所有 hosted agent 都打开 terminal。关闭宿主能力不等于关闭 CLI 自身工具。[S5]
- 源码明确记载 codex-acp ≥1.7 的 read-only 预设使用 workspace-write。这证明现有模式名不足以作安全认证，不证明新增 OS 隔离不可能。[S6]
- ResumeExistingOnly 不回退 session/new，但允许响应省略 session ID，采用请求 ID；这符合常见 ACP 响应形状，不能称现有实现违反“必须返回 ID”。圆桌若要求显式匹配，将是新增、更严格且可能减少兼容性的契约。[S7]
- Web invoke 固定 POST /api/<command>；新协议沿用它。emit_event 会全窗口发出并镜像全局 Web broadcaster，圆桌正文不能使用该函数。[S8]
- 连接重放环有事件数量、单事件字节与总字节限制，不是圆桌持久日志。[S9]
- codeg.db.lock 保证的是唯一 automation engine，不是唯一应用进程或全库唯一写者。圆桌必须另行取得明确的协调器所有权。[S10]
- 数据库池创建后调用 apply_sqlite_pragmas，缺少逐连接配置保证；不能据此声称其余四条连接一定禁用外键，也不能未经测试承诺 FULL 耐久性。[S11]

### 3.2 组件

RoundtableService 是唯一圆桌业务写入者。每个已加载 room 有一个 actor 和一个 RoomAdmissionGate。Scheduler 是纯函数，只产生 turn intent；ParticipantRuntime 复用 ACP 管理器执行受限动作；SnapshotBuilder、ResultValidator、RoundtableStore、SubscriptionHub 分别拥有资料、契约、事务、授权投递责任。

建议模块为 roundtable/{service,model,store,strategy,context,validation,budget,runtime,recovery,events,policy}.rs。commands/roundtable.rs 与 web/handlers/roundtable.rs 仅构造可信 actor、解析同一 DTO 并调用共享服务。前端使用 lib/roundtable/{types,api,reducer,stream} 与专用详情组件；静态路由可为 /roundtable?room=<id>。

不新增独立 service_session 进程栈；可以给既有 spawn 增加薄的 service-owned wrapper。它必须携带 ServiceOwner、隔离证书、预分配 incarnation 和专用事件 sink，不从虚构 parent_connection_id 继承权限。CompanionLeaseRegistry 仍只是 companion 就绪机制，不承担房间租约。

## 4 身份 所有权和准入线性化

### 4.1 稳定身份

principal_id 是数据目录初始化时生成并持久化的单操作者身份。可信桌面窗口和该实例有效 operator token 映射为同一主体；body 中自报身份无效。completion 专用 scope 不获得房间操作权。

room_id 标识讨论；participant_id 标识稳定成员角色；speaker_id 统一标识成员与主持；session_binding_id 标识一次会话世代；attempt_id 标识一次 ACP prompt 尝试；connection_incarnation 标识一个进程连接实例。主持的 participant_id 为 null，speaker_id 必填。所有预览、结果、turn、binding 和用量都以 speaker_id 路由。模型不提交这些身份字段。

每个 (phase_id, speaker_id) 有一个 logical turn，最多一个 accepted result。N 不含主持，主持独立会话并复用用户指定成员的提供商配置。phase_index 为 0、1…R、R+1；phase_revision 表示同一阶段重开；run_epoch 表示控制失效代际；boot_epoch 表示协调器启动代际，三者互不替代。

### 4.2 单协调器

RoundtableService 在数据目录上独占 roundtable.engine.lock，持锁文件句柄持续到停止全部受控运行。拿不到锁时只读历史，拒绝 start/resume；不得依据 automation engine 已持锁推断自己获权。锁路径绑定同一个规范化数据库身份；不支持网络文件系统、多主机共享 SQLite 或绕过锁的写者。

拿锁后在数据库事务中递增全局 roundtable_boot_epoch。每个运行回调必须匹配 (boot_epoch, run_epoch, phase_id, phase_revision, attempt_id, binding_id, incarnation, context_hash, policy_hash)。去掉每房间续租和租约定时器。旧进程仍需实际清理，boot_epoch 不能终止它们。

### 4.3 最终本地准入

所有控制命令、接纳事务和最终准入经过同一 room actor；运行任务只能发消息请求 actor 准入，不能自己检查后直接发送。具体顺序如下：

1. actor 在短事务中预留 attempt、预算和 incarnation，写 dispatch_state=reserved；外部运行尚未准入
2. 隔离器先准备受限环境；进程创建入口也经过 RoomAdmissionGate 注册 incarnation，再启动。取消令牌在注册前后均检查；stop 必须能看见已注册但尚未完成 spawn 的实例
3. initialize/new 和 roundtable MCP ready 完成后，运行任务提交 ReadyToEnqueue 给 actor
4. actor 持有 RoomAdmissionGate，核对当前 fence、deadline、cleanup 和预算；事务将 dispatch_state 改为 admitting 并提交
5. 保持同一 gate，执行一次有界、本地、非阻塞 try_enqueue。成功的这一条指令是“本地最终准入”的线性化点；失败意味着没有准入，记录 queue_rejected。不得持 gate 等待网络、模型或远端 ACK
6. actor 记录 admitted；若 enqueue 已成功而持久状态未写成，恢复必须按 uncertain 处理。此窗口不能通过重发来“补齐”

pause/stop/restart 的 fence 提交也持同一 gate。因而旧 attempt 要么先完成 try_enqueue 再提交控制 fence，要么控制先提交而旧准入被拒绝。不存在“检查通过，stop 提交，旧任务随后成功 enqueue”的合法路径。DB busy 只有限重试纯事务，不重发 prompt；gate 阻塞时间需有上限并可诊断。

read_evidence、search_evidence、submit_result 的服务准入同样经过 gate。只读操作在 gate 内取得不可变对象句柄后可在外部读取；stop 前准入的只读操作可能随后完成，但 stop 后不得新准入，也不得再把结果接纳为业务状态。未知或写工具没有准入路径。

停止承诺是零新增本地准入，不是停止后零网络字节或零远端推理。停止前入队的请求可能稍后被提供商执行，标记 residual_remote_work。队列发送端还应检查已撤销标志并丢弃可撤销工作，但不把这一优化当成远端 exactly-once 保证。

## 5 专用连接和严格隔离档

### 5.1 Roundtable 连接用途

新增 ConnectionPurpose::Roundtable，以及独立 launch policy：service_owned=true、interactive_permissions=deny、host_fs=false、host_terminal=false、companion_groups=[roundtable]、ordinary_conversation_import=false、automatic_title=false。不要把 Roundtable 简单加入 is_hidden_generation，否则容易连同结果 MCP 或多轮能力一起关闭。

服务启动时 parent_connection_id=None，owner 是 room/attempt，窗口仅观察。关闭详情或浏览器断线不停止服务端圆桌。应用退出走停止清理和持久恢复流程。ACP permission 请求立即返回取消或拒绝，并写 attempt 诊断；不产生权限卡，不等待人类回答，不自动改全局 full-access 配置。

抑制 ordinary import 还需扩展 internal session registry 的用途分类，预留圆桌 cwd 根，复用握手期间的 discovery 排他保护，直到 (agent_type, external_id, room_id, binding_id) 已持久登记。恢复扫描、原生会话导入和所有 parser 路径都检查这份登记和预留根。仅增加 purpose enum 不足以防侧栏重新导入。[S4]

### 5.2 可认证的执行边界

strict_snapshot_v1 的启动条件是：在 agent 及其子进程执行第一条指令之前，强制隔离已经生效。允许读的内容为运行所需只读二进制/依赖、认证所需最小配置和该成员资料副本；允许写的内容仅为该 binding 的私有 scratch、日志与缓存。真实项目、其他成员 scratch、用户任意目录不可达；副本不得硬链接回原文件。默认不挂载完整 HOME，不导入工作区启动脚本、shell profile 或全局 MCP 列表。

网络仅可访问已配置模型服务和专用本地 MCP 端点。模型服务必然接收所选资料，创建页明示数据接收方；不能承诺防止数据被发送给用户已选择的模型服务。禁止任意第三方出口、浏览器、委托、任务创建和外部业务写入。最小认证文件的内容不能通过 fs/MCP 返回给模型。需要 token 代理、端点限制或独立凭据目录的具体实现写入适配器证书；无法证明边界时拒绝启动。

隔离器接口准备一个不可变 SandboxPlan：只读挂载清单、可写 scratch、网络目的地、继承环境白名单、凭据提供方式、子进程约束、进程树回收方式及其 hash。实际平台实现可以采用可证明满足此计划的容器、系统沙箱或其他 OS 机制，但本文不预设 Windows/macOS/Linux 任一现有实现已经满足要求。单靠 cwd、只读文件属性、mode 名、permission deny、提示词或复制目录均不合格。

### 5.3 资格证书和 preflight

证书键为 OS 与版本、CLI 二进制 hash/版本、adapter 版本、隔离实现版本、启动计划 hash、policy hash、工具契约版本。任一变化使资格失效；自动升级不能沿用旧证书。至少验证全局 bypassPermissions、额外 MCP、绝对路径、目录穿越、符号链接、子进程和网络绕过。资格状态为 not_tested、passed、failed、expired，并附可复查报告；品牌名不能替代证书。

首版要求 new_session、roundtable_mcp、ordered_turn_completion、cancel_and_reap、strict_isolation、bounded_context_delivery 均通过。preflight 不运行付费 prompt；真实资格实验属于 M0b，需要单独批准。登录、安装、升级、权限变化不自动发生。MCP 工具列表与握手在每个 attempt 的 prompt 前重新检查，不合格则记 launch_failed，不发送 prompt。

当前资格矩阵所有平台/CLI 组合均为 not_tested；因此真实功能保持关闭。这个结论是启用条件尚未证明，不是架构不可实现。

## 6 资料快照和证据

### 6.1 快照生成算法

创建 draft 后、start 前，用户明确选择文件集或确认的工作区范围。宿主执行快照：

1. 规范化根与相对路径，拒绝绝对路径、..、设备、socket、目录逃逸；首版拒绝 symlink 和跨根链接，提供被排除清单
2. 对每个选中文件通过受控句柄读取，记录读取前后必要元数据；若内容变化则有界重读，仍变化则 snapshot_unstable。不宣称所有文件是同一操作系统原子时刻
3. 将实际字节复制入不可变内容对象并计算 SHA-256；文本编码、字节数、行起点表一并记录，非法编码以二进制附件标记且不进入文本证据工具
4. manifest 按规范化路径字节序排列，包含 path、content_hash、size、encoding、mode、来源类型 tracked/dirty/untracked 和 capture 时间。base_commit 可空，仅作来源背景
5. workspace_snapshot_id 由服务生成；manifest_hash 覆盖实际内容和元数据。选中但未提交或未跟踪内容照实纳入。同 commit 不同工作区字节必然产生不同 manifest_hash
6. 展示资料清单、排除项和接收的模型服务。冻结后不追读原工作区；成员副本来自同一 manifest，且禁止共享可写文件

秘密文件默认排除，不能保证靠文件名识别所有秘密；用户确认的是实际选中资料范围。运行中不扩大来源；新增文件须克隆新房间。普通文本插话可在现有数据接收方和大小限制内追加。

### 6.2 证据记录

证据包含 evidence_id、workspace_snapshot_id、manifest_hash、path、content_hash、line/byte range、excerpt_hash、excerpt、owner_attempt_id、staged_phase_id、phase_revision、published_seq。输入证据 owner_attempt_id 可空且发布于输入快照；attempt 新读取的证据在当前 attempt 可见，阶段发布前对 peer 不可见。宿主登记证据后返回短别名 E1、E2，同时保存完整 ID 映射。

模型提供的 URL、CLI 原生 read 通知或自述“看过文件”仅为 unverified_reference，不能成为 verified evidence。原生读可以帮助思考，但规范结果的证据必须通过 roundtable 工具取得。快照版本而非 base_commit 决定引用内容。

路径检索在宿主的已冻结 manifest 内进行，不能拼接模型路径访问原文件系统。读取工具只接受 file_alias、start_line、end_line 等有界参数；返回精确截段、总行数、是否还有内容。search 使用有界字面文本检索，首版不执行任意 regex 或外部 grep 命令。限制每次返回 8 KiB、每 attempt 累计返回 32 KiB，实际阈值为显式配置并进入 context_hash。

## 7 阶段策略和输出契约

### 7.1 阶段和 barrier

N≥2，R≥0，1≤C≤N。运维可设资源上限，preflight 返回实际限制；调度器不得写死 3/2。R=0 标为“并行咨询”，仍有独立主持综合。首版开始后 N、R、角色、模型和来源冻结；C 仅 paused 可改，须重新检验剩余时间容量。

proposal 的公共输入不包含任何同阶段输出。critique_k 使用此前已发布结果的固定公共视图，角色与指定回应目标单独列出。synthesis 使用所有已发布阶段。人类可以看到并行预览，成员不能读取 peer 的当前预览、staged 结果或完成先后。

讨论阶段 expected 集合固定为全部 N 位成员，Q=max(2,floor(N/2)+1)。失败不缩小 expected 或 Q；N=2 时一位失败就无法形成 quorum，创建页明示。到全部 slot 终结或阶段剩余时限耗尽才关闭，达到 Q 不提前发布。关闭冻结 accepted 集合；清理后按 participant ordinal 原子发布。未达 Q 则失败；主持只有一个 slot，Q=1。

### 7.2 确定性回应目标

每份 valid proposal 至少一个非空、可回应的 claim；abstain 是单独结果，不计入 Q。critique 也至少一个 claim，并必须回答分配目标；不能以空 claims+非空 summary 让下一阶段无合法输入。

critique_1：按 ordinal 排列有效 proposal 的成员环，每份 proposal 的第一条 claim 分配给环中的下一位不同成员；其他预期成员分配环中第一位非自身的 claim。这样每个有效 proposal 至少被分配一次，也保证每位发言者有合法目标。两位有效成员必然互评。目标在阶段开始时冻结。

critique_k：保留上述对上一已发布阶段的基本覆盖，再为每位成员追加上一阶段针对其历史 claim 的最多 3 条 challenge，按 priority=critical 优先，再按发布序、来源 ordinal、response ordinal 排序。没有指向自身的质疑时只用基本目标。目标记录 exact claim_id/response_id，最多 4 条；剩余质疑可见但不是强制目标，并单独显示未回应覆盖。

目标分配保证的是“被指定评议”，不是“必定得到评议”。成员失败后不修改同阶段快照、不临时重分配已经开始的输入；实际未回应目标由服务生成 coverage。需要补评时只能下一阶段按新快照或克隆追加轮次。不得把部分失败后的实际覆盖说成完整。

### 7.3 专用 MCP 通道

只暴露 read_evidence、search_evidence、submit_result 三个工具。调用通道使用不可猜测的 attempt token，宿主将 token 绑定到完整身份、fence、工具版本和可见别名表。token 仅通过受控启动通道交付，不进入提示词、日志、URL 或结果；过期或控制 fence 改变立即失效。工具入参不能指定 room/speaker/attempt 或切换身份。

submit_result 入参为 submission_id 和 result。result 是按 phase.kind 判别的联合类型：MemberResultV1 用于 proposal/critique/abstain，ModeratorResultV1 的 kind=synthesis 用于主持，禁止跨类提交。submission_id 为 attempt 内短字符串；相同 ID+规范 payload hash 返回原 receipt；同 ID 不同 hash 返回 submission_conflict。schema 或引用错误返回机器可读 field_errors，不写 valid candidate。每 attempt 最多 3 次不合格提交；超过即 invalid。首次合格提交 sealed，后续完全相同提交幂等，其他提交返回 result_already_sealed。receipt 表示 candidate 已暂存，不代表 turn accepted。

工具需要可靠完成该次请求；若连接丢失且候选写入情况不明，按提交幂等查询 receipt，不把“没收到回复”理解为未写入。整个 prompt 正常结束并满足第 8 节完成边界后，服务才在第 10 节事务中接纳。无合格 submit、取消、超时、非正常 stopReason 均不接纳。首版不从最终 assistant 文本抽 JSON。

### 7.4 成员 schema

MemberResultV1：kind=proposal|critique|abstain；summary 为非空文本；claims 为 1…20 条（abstain 为 0）；responses 为 0…30 条；open_questions 最多 20；position_changes 最多 20。默认完整规范 JSON 不超过 8 KiB，可在创建时上调至硬上限 64 KiB，但必须同步通过上下文预检。abstain 必须给 reason，状态为 abstained，不能算 valid。

claim 字段为 local_key、text、evidence_aliases、confidence=low|medium|high。response 字段为 target_claim_alias 或 target_response_alias（二选一）、stance=support|challenge|clarify|revise、priority=normal|critical、text、evidence_aliases。position_change 字段为 own_prior_claim_alias、new_local_claim_key、reason、trigger_response_aliases。局部 claim key 唯一；宿主生成稳定 claim ID。own_prior_claim 必须归同一 speaker 且已发布；new_local_claim_key 必须在本结果存在。回应 claim 的 message 由服务推导，杜绝 message/claim 错配。

别名只在该 phase/attempt 可见映射中解析，存储与 API 使用完整 ID。至少一个 response 覆盖每个强制目标，不许自我 support 充当交叉评议。模型自报 confidence 不是校准概率；priority 不是宿主认定的真实严重程度。

### 7.5 主持 schema

主持提交 recommendation、alternatives、consensus_items、disagreements、risks、decision_requests。每个结论引用至少一个有效 message/claim/evidence 别名，无证据推断明确标 inference。coverage 完全由服务生成，不接受模型填写成功/缺席人数。

consensus_item 包含 agreement_level=explicit_agreement|compatible_positions|unresolved。explicit_agreement 必须列 supporter aliases 和对应有效 support response；只允许已发布且支持同一目标的不同 speaker，无回应不能算同意。此检查证明存在显式支持边，不自动证明两句话语义等价。服务若校验失败返回契约错误，不能自行把未经支持的共识标签升级为事实。风险和少数意见的完整性由质量验收检查。

## 8 会话完成边界和上下文预算

### 8.1 有序收取

每个 connection incarnation 的 ACP update 与 prompt response 有一个有序 ingress 序列，不能由独立异步 callback 无序更新预览。专用 MCP 是另一条传输，不能凭本地 seq 假装两条链天然全序。同一 binding 最多一个 in-flight prompt。

prompt response 在 ACP 链上成为完成标记后，actor 持 gate 原子关闭该 attempt 的新 MCP 准入，记录 completion_pending，并取得已准入工具 handler 的集合；随后在 gate 和 DB 事务之外等待该集合及此前 ACP update 处理完成。不会等待尚未准入或尚未收到的工具请求；迟到请求返回 attempt_closed。集合收敛后，正常完成且恰有一个 sealed candidate 才发 RuntimeTurnCompleted，否则 no_submission/invalid。适配器资格须证明：成功 submit_result 的 receipt 已返回给 CLI 后，CLI 才可发 normal prompt completion。不能持 actor/gate 等 MCP 或 ACP 回包，防止互相等待。

ACP update 以 sessionId 关联，不能天然提供 attempt_id；适配层将连接当前 turn_generation 绑定到 attempt。取消允许尾部 update，不能收到 cancel ACK 就认定文本排空。[S13] 完成边界之后出现归属不明的 update，记录 protocol_violation 并退役 binding，不归给下一 attempt。首版即使不复用会话也必须通过这一测试，防止提前验证。

### 8.2 默认新建会话

每个 attempt，包括重试，使用新的 binding 和规范化公共输入。正常结束后也关闭并回收进程；不缓存空闲 CLI。因此首版空闲进程上限为 0，活动进程数受 C 和未完成清理共同约束。成员身份不会随 binding 重建而改变。

正常结束却 schema 不合格时，可以在同一 prompt 内利用工具 field_errors 修正；若该 prompt 已结束，最多额外一次修复 attempt，重新注入同一快照和有界错误摘要。只有未来单独认证 live_prompt_reuse、完成边界、上下文游标和工具累计量后，才可采用同 session 追加修复；首版不依赖这一优化。

冷恢复仅重建规范上下文。保留 resume_existing 能力描述，但不开放原 CLI session 恢复。未来若启用 strict_resume，必须定义 session 身份证明；“响应未带 ID”不能自动宣布无损恢复，应使用认证的等价证明或重建，不能修改通用 ACP gate 来假定所有合法响应必带 ID。[S7]

### 8.3 三份清单

phase_snapshot 是公共可见性清单，包含配置版本、问题/插话版本、来源 manifest、已发布 message ID/hash、有序成员、强制目标、policy 和输出/工具配额，采用规范 JSON 哈希。deadline、资源排队和单调时钟不进入 context_hash。

delivery_manifest 是某 attempt 实际提交的字节清单：公共视图 hash、角色 hash、prompt/template/schema/tool 版本、输出配额、模型配置、binding、前置游标（首版为空）、完整 prompt hash/bytes。它不把“大家公共视图相同”表述成整个模型上下文完全相同。

context_state 记录 fresh/known/unknown、已交付 prompt 字节、工具返回累计量、CLI 隐藏上下文限制和压缩信号。首版 fresh 消除跨阶段私有历史累计，但同 turn 的 CLI 工具循环仍须由认证 profile 限制。无法证明交付上界的适配器不能标为 bounded_context_delivery。

### 8.4 开始前容量检查

首版不调用隐形摘要模型，也不做可能丢失关键 claim 的自动压缩。先计算整个计划中最大完整公共前缀。令 S 为每份成员结果的规范输出字节上限，P=N×(1+R)；综合的结果正文上界为 P×S。3/2 配置若 S=64 KiB，仅结果正文就有 576 KiB；不是完整输入上界。

完整上界还必须加上问题、角色、插话最大总额、schema/tool 描述、序列化封装/转义、证据摘录、该 turn 的工具返回额度及生成预留。规范 prompt 使用 UTF-8 字节串拼接已编码对象，构造器提供 exact_bytes 和 verified_token_upper_bound；不得把字符数直接当 token。adapter 必须给出可信上下文容量及 token 上界算法，或返回 capacity_unknown 并拒绝真实开始。对于真实已冻结内容可精确计数，未来结果使用 S 的保守上界。

用户插话总额在创建时预留（默认 16 KiB），接受插话前验证剩余额度，不追加未经预检的无界文本。MCP 工具返回累计超额直接拒绝继续读取。每个后续阶段实际构造后再核验上界；若实现与预检不一致，报 context_contract_violation，停止准入，而不是静默截断。

完整前缀策略下，若结果平均 S 字节，R 轮评议的累计结果阅读量约为 N²×S×R(R+1)/2。这是容量增长模型，不是费用预测。大 N/R 需缩小资料/输出配额、选合格更大上下文适配器或减少轮次；不能只凭 ACP 尝试数公式准入。

## 9 时限 预算和资源

### 9.1 尝试单位

B=N×(1+R)+1 是基础 ACP prompt 尝试数，不是底层模型计费请求数。默认最大尝试数为 B+floor(0.4B)：3/2 是 10/14，7/5 是 43/60。一次 ACP prompt 可包含模型请求和工具循环；服务只硬限制自己的 prompt 准入、工具配额、时长和结果大小，不能将不受控的 CLI 内部重试宣称为已限制。[S13]

每 logical turn 每 revision 最多 2 个准入尝试；失败、取消、超时及 uncertain 的已准入尝试均消耗次数。reserved 且可证明未入队的不消耗，但 launch failure 受独立有界启动计数：每 turn/revision 最多 2 次 launch_start，失败和取消也计数；每 attempt 最多一次启动。达到启动上限即 turn=failed，不因未准入 prompt 而重新分配无限 attempt，也不能在底层无限 fallback。进入 admitting 后崩溃不能退回“未消耗”。

可选重试前，事务保留当前阶段未首次准入的 slot、后续每阶段 N 个首次尝试和主持 1 个尝试。首次尝试队列优先于重试，均按 ordinal 排序。用户重开是新 revision，其全部首次尝试也消耗房间总额；不能因新 revision 复位总预算。

### 9.2 时间账户

配置默认 launch_timeout=30 秒、prompt_timeout=180 秒、validation_timeout=5 秒、cleanup_target=10 秒（含 cancel 宽限 5 秒与强制回收）。默认槽预算 T=225 秒。launch/initialize 和正常验证都计入阶段墙钟；prompt 的 180 秒自本地最终准入起算。超时等于上限时不能接纳结果。

讨论阶段默认执行预算 D=2×ceil(N/C)×T，为每个 slot 的首次尝试和一次修复留出时间容量；综合默认 2T。房间建议活跃预算为 (1+R)×D+2T。3/2/3 由原来的 15 分钟改为 30 分钟，表示允许的最长预算，不是预期耗时。用户可减少，但不得小于首次尝试批次及后续必要阶段的最低额度；UI 明示压缩后哪些修复没有时隙。

总活跃时间为各运行阶段占用的 room 单调墙钟区间之和，不是并发 attempt 时长相加。包含启动、生成、验证、取消和清理；排队及 paused 不计。清理即使预算耗尽也必须执行，超额记 cleanup_overrun，禁止据此继续新 prompt。预算限制准入和主动取消，不能保证远端计算在精确毫秒停止。

运行期间用 monotonic clock，不以系统 UTC 判定竞争；UTC 仅用于审计展示。阶段剩余量和房间剩余量在每个业务事务及不超过 1 秒的 checkpoint 持久化。checkpoint 使用预扣下一个计时片：仅在预扣提交后允许继续受控执行；存储失败立即撤销准入并清理。正常停顿退款确知未用的片段；崩溃后的预扣片不退款。重启不恢复旧绝对墙钟 deadline，也不把关闭期间计为新增讨论时间；未知远端运行单列风险。

### 9.3 容量和公平

首版最多一个 active room；由 service 全局原子 active-room permit 强制实现，所有 start/resume/retry_synthesis/restart 共用一个分配器，不允许各 room actor 分别先查后占。permit 在本地运行及 orphan 清理期间保留，只有确认本地静止才能释放。另一个 start 返回 capacity_limited，不自动创建跨房间 FIFO 队列。paused 房间在清理完成后释放 active room 资格。C 是该房间同时活动 slot 的上限；resume 需 min(C,剩余可执行 slot 数) 个 permit，一次全部取得或一个不占，不逐个等待。

permit 覆盖启动到本地进程树清理确认，不能 prompt 结束就释放。运行期额外进程、MCP helper、scratch 和资料对象分别有配置上限，preflight 展示空间需求。每 binding 的私有日志/scratch 有字节配额；超限终止该 attempt。终态和正常结束的 binding 都回收进程，scratch 仅在诊断已固化后可清理；不可变资料与消息仍保留。

### 9.4 最小计量

首版 UI 展示已准入 ACP 尝试数、room 活跃时长、attempt 时长、可确认的输出 token、未知项数量。不显示虚构总费用；费用不完整时为 unknown，不把 null 显示成 0。不建立四级“质量评分”的复杂计费体系。

保存原始 measurement 时必须包含 unit、source、scope=request|turn|session、semantics=incremental|cumulative|context_occupancy、counter_id/epoch、dedupe_key、value/null、observed_at。累计计数只算同一 counter 的正向差额，回退或重置另起 epoch；context_occupancy 不计入收费 token。不能可靠归属的值保留为 unattributed，不强行摊给本次 prompt。晚到测量只能由当前 service 的独立计量入口写旧 attempt 记录，不能复活 room 或修改 accepted/published；UI 查询计量视图时单独刷新。

## 10 持久模型和事务

### 10.1 表和不可变对象

以下为新增逻辑表，最终 migration 可合并物理表，但不能丢失字段语义：

- rooms：principal、status、config_ref、revision、run_epoch、boot_epoch、current_phase_id、active_control_id、last_seq、remaining_active_ms、blocked_reason、result_quality
- speakers/participants：稳定身份、ordinal、角色、provider_ref、模型配置快照；主持独立 speaker
- phases：index、revision、status、snapshot_ref/hash、expected、quorum、remaining_ms、closing_set_ref、published_seq
- turns：phase_id、speaker_id、status、accepted_attempt_id、admitted_attempt_count
- bindings：speaker、generation、incarnation、external_session_id、policy/certificate_ref、context_state、state、retire_reason
- attempts：turn、binding、fence、dispatch_state、state、admitted_at、finished_at、finish_reason、prompt/delivery hash、cleanup_state、diagnostic_ref、residual_remote_work
- submissions：attempt、submission_id、payload_hash、candidate_ref、validation_errors、receipt；同 attempt 最多一个 sealed candidate
- messages/claims/responses/position_changes：规范结果及稳定关系；message body 不可变，发布/作废关系另存 membership 版本
- source_manifests/evidence/message_evidence：第 6 节的不可变版本、归属和发布水位
- user_inputs：文本、mode、accepted_seq、target_phase_index、applied_phase_id/seq、state
- control_operations：operation_id、kind、target_phase/revision、requested_epoch、step、status、blocked_reason、superseded_by、result_ref、budget_reservation
- commands：principal_id、api_major、request_id、canonical_request_hash、method、room_id、status=processing|completed、immutable_response、operation_id
- events/projection_versions/page_manifests：第 12 节的持久业务投影与固定分页
- budget_reservations/measurements/diagnostics：预算预留、已知计量和有界失败资料

唯一约束：participants(room,ordinal)、phases(room,index,revision)、turns(phase,speaker)、attempts(turn,attempt_no)、messages(attempt)、claims(message,local_key)、events(room,seq)、commands(principal,api_major,request_id)、submissions(attempt,submission_id)。部分唯一索引限制 turn 最多一个 active/uncertain attempt。所有外键同时检查 room 一致性；speaker_id 在主持路径也必填。

去掉 room owner lease、notification outbox、dispatch outbox、approvals 和可写工具副作用表。事件日志是通知源；attempt.dispatch_state 是持久调度意图，不因为删了 outbox 就删去“发送结果未知”的状态。

### 10.2 命令幂等

可信入口计算 canonical hash，包含 api_major、method、room、全部有效参数与默认值，不含凭据。在短事务中插入 processing 记录并执行同步决策；同键同 hash：processing 返回 command_in_progress，completed 返回原始受理响应；同键不同 hash 返回 idempotency_conflict。唯一冲突后查询既有行，不当作未知错误。

长操作不保持命令事务：提交 control_operation 后将命令 completed，响应为“已受理”及 operation_id。后续完成通过事件或 operation 查询取得。重放原响应不伪装为当前状态；当前 revision/status 从 get 或事件获取。崩溃留下 processing，恢复检查是否已有 operation/事务效果，确定完成或确定未执行后才继续；不能仅凭 request hash 推测动作。

### 10.3 原子接纳

Room actor 持 admission gate，执行短 SeaORM 事务，不声称仓库已有 BEGIN IMMEDIATE。可选的显式写锁实现须在数据访问层另行验证；正确性依靠 actor 序列、条件更新和 DB 约束。

事务重新核对 room=running、phase=running、current fence、当前 turn、单调 deadline 尚未到、normal finish+ingress barrier、sealed candidate 以及证据可见性。随后在同一个事务里：插入 message、claims、responses、position_changes、全部 evidence links；将 attempt=accepted、turn=valid；结算已准入预算；更新控制 revision；写不可变 projection 与 durable event。任何一步失败全部回滚，不能留下 accepted message 却没有 claims 的半条结果。

candidate 可在完成前持久化，但 candidate 不是 accepted，也不参加 quorum。preview 不作为失败资料唯一来源。DB 提交前不能向 UI 或 MCP 声称结果已接纳；事务失败不重发 prompt。

### 10.4 关闭和发布

全部 slot 终结或计时到期时，关闭事务 CAS running→closing，冻结 closing_set={accepted message IDs, slot outcome, context_hash, fence}，并为未结束运行写取消意图。关闭后不再接纳迟到结果。清理屏障确认受控进程树、mailbox、工具请求和有序 ingress 静止；首版所有 binding 退役。

发布事务重查 active control 和 fence。无覆盖控制时，Q 足够则为冻结集合写不可变 published membership，发布 evidence，更新 phase 与 coverage，并创建下一 phase ready；Q 不足则 phase_failed。按 ordinal 发布，不依赖完成速度。房间 completed 只有主持结果已发布且本地清理确认才可提交。

SQLite 事务不能等待模型、网络或进程退出。DB busy 只重试事务；存储不可用时停止准入，已运行进程仍执行清理。不能发出无持久依据的 completed。

## 11 用户控制和恢复

### 11.1 状态

room：draft、ready、running、pausing、paused、recovering、stopping、stopped、failed、completed。stopped/failed/completed 不原地复活；synthesis 失败是 paused+blocked_reason=synthesis_failed，而不是房间 failed。阶段：ready、running、closing、published、failed、superseded。attempt：reserved、launching、admitting、admitted、streaming、validating、accepted、invalid、failed、timed_out、interrupted、uncertain；cancelling/cleanup 为独立运行标记。

accepted 与 published 分离。用户立即暂停可能消耗已准入 attempt，甚至耗尽某 turn 的 2 次上限；剩余房间总额不代表该 turn 一定可继续。UI 显示继续将补几个 slot，以及哪些 slot 已不能重试。

### 11.2 持久控制操作

ControlOperation 的 kind 为 pause、stop、restart_current、retry_synthesis、recover；step 为 requested、revoking、cleaning、applying、done、blocked。控制受理事务保存确切目标 phase/revision、预算预留、输入、epoch 与后续动作，不能仅存 request hash。

同一 room 同时最多一个未完成的非 stop 控制。相同 request 幂等；不同 restart 在另一个控制进行中返回 control_in_progress，不排队、不无声合并。stop 可覆盖任何控制，旧操作标 superseded_by；其他操作不能覆盖 stop。每步 CAS 到下一步；新 phase 唯一键和 operation.successor_phase_id 保证恢复只创建一次 revision。

### 11.3 插话与重开

next_phase 接受文本并给 accepted_seq，当前快照不变；下个阶段创建事务按 accepted_seq 应用并记 applied_seq。当前 synthesis 返回 no_next_phase，不接受永远不会应用的文本。与阶段创建竞争时由 actor 顺序决定，响应包含实际 target_phase_id/index。

restart_current 在破坏进度前先在同一控制事务中验证并预留 replacement 当前阶段全部首次尝试、以后必要阶段及最低时间额度；不足直接拒绝，不撤销现有工作。通过后才增加 run_epoch、撤销工具 token/未入队工作、写操作并取消运行。清理后旧当前 revision 标 superseded，新 revision 使用此前 published 前缀、旧公共输入和新插话；旧 staged 内容只留审计。

若后续发生不可预见的资源或资格问题，操作为 blocked，UI 给重新 preflight、克隆或停止的具体出口；不能显示可无条件成功的“继续”。首版不提供静默追加预算。已发布阶段永不回滚。

### 11.4 暂停 继续和综合重试

pause 立即增加 run_epoch，禁止新准入，取消未完成回答；已 accepted 的 slot 保留，未完成输出丢弃。清理后才显示 paused。按钮文案为“立即暂停，保留已完成结果”；确认信息显示会丢弃进行中的回答和已耗次数。

resume 不改 phase_snapshot，补仍可执行的缺失 slot；C 变化写新配置版本但不改语义输入 hash，重新验证剩余时限是否够新的批次数，不自动补时。因崩溃暂停须 recovery_consent=true。若剩余 slot 不足以达 Q，返回 cannot_reach_quorum，提供 restart（如额度允许）、克隆或停止。

主持失败后保持所有 published 阶段，paused+synthesis_failed。retry_synthesis 显式预留 1 次尝试及足够时间，创建 synthesis 的下一 revision，仅调用主持，公共输入 hash 保持一致；可在重试前插话并改变 hash，但这属于 restart_current。房间总额不足时只能克隆或停止，不暗中加价。

### 11.5 closing 恢复矩阵

| 持久状态 | 清理后的唯一后继 |
|---|---|
| closing 无控制 | 按冻结集合继续发布或阶段失败，不重跑已完成成员 |
| closing 加 pause | 保留冻结集合进入 paused；resume 只继续发布判定，不重新补缺失 slot |
| closing 加 restart_current | 旧集合绝不发布；完成一次 supersede 并创建一个 successor revision |
| closing 加 stop | 只收敛 stopped，绝不追加 completed |
| running 后崩溃 | 清理旧运行，保留 accepted，进入 paused+recovery_required，不自动新调用 |
| 已 published 下一阶段 ready 后崩溃 | 保留发布；paused 等用户继续，不能重复发布 |
| stopping 后崩溃 | 继续清理到 stopped，无恢复运行路径 |
| control step 已做但回包丢失 | 用 operation_id、唯一 successor 和幂等响应恢复，不再创建第二个操作 |

恢复处理 closing 时，当前持锁 service 在事务中将既有 closing_set 的发布决策绑定到新的 boot_epoch 和 recovery_operation，并保留原 closing fence 作为不可变来源。随后依据新 owner/fence、当前覆盖控制及新的 cleanup proof 授权发布；不要求旧 closing fence 等于新 boot_epoch，也不允许旧 runtime 借此写回。

closing 的本地确定性发布可以在恢复时完成；这不启动付费工作。普通 running 的恢复不沿用旧 UTC deadline，不自动消耗新的模型费用。

### 11.6 uncertain 与活性

uncertain 表示是否准入/完成未确知，不等于本地进程永远活着。恢复先按 incarnation 清理并证明本地 mailbox、进程树和工具通道静止，才能释放 active 唯一占位。远端仍可能计算时保留 unknown_remote_outcome 和 residual_remote_work；在用户确认恢复费用风险后可以新 attempt，不能要求提供商证明一个本来无查询接口的结果，否则无法恢复。

活性承诺有明确前提：存储可用、资源可用、本地清理已证明且无需用户确认时，actor 必须推进到下一稳定状态；禁止永久留在 closing/recovering。若前提不成立，持久 blocked_reason、最后尝试时间及下一可操作出口。清理监视器有界重试；无法证明停止则持续阻止新准入并要求人工处理，不伪装清理成功。

## 12 持久事件和固定分页

### 12.1 选择完整版本化投影

首版选择“不可变业务投影引用”，不把仅含 ID/hash 的失效通知称为可 fold 日志。每次影响客户端业务状态的事务递增 room.seq，并写一个 ProjectionV1 版本和一个 projection_committed 事件。ProjectionV1 含该水位的 room/status/config、participants/speakers、phase/turn/attempt 摘要、binding 描述、控制操作、输入、预算、coverage，以及不可变消息/证据清单引用；不含进程句柄、秘密和 token。

事件 envelope 必填 schema_version、event_id、room_id、seq、resulting_revision、run_epoch、boot_epoch、occurred_at。payload 必填 projection_id、projection_hash、cause、changed_entities。cause 是版本化闭集：create、config、start、attempt、accept、close、publish、input、control、recovery、terminal。一次事务产生一个完整投影，多个语义变化放在 changed_entities 中，不产生中间不一致视图。

投影 JSON 存数据库不可变表；消息正文和较大对象通过 immutable object ID+hash 获取，不读“当前最新版”代替旧对象。客户端 reducer 对事件取出并校验同 room/seq/hash 的投影后，原子替换业务视图。该选择牺牲一些存储换取简单正确性，首版限制总结果数、总事件数和配额；不将每个 token 写成投影。

可测试不变量为 fold(snapshot_at_L, events_(L,H]) = projection_at_H，比较上述业务投影的规范 JSON hash。revision 和 run_epoch 即使不变化也在每帧给出。未知 projection schema、未知投影 cause 或缺失对象时停止增量处理并要求兼容升级/重新同步，不能忽略后仍推进 seq。未来只有显式 non_projecting 的扩展才能安全跳过。

### 12.2 不可变分页清单

每个投影引用 MessageManifestV1，保存按发布序、phase ordinal、speaker ordinal 排列的 (message_id, body_hash, membership_version, visibility_at_H)；另有 staged 列表。membership 本身不可变。snapshot/read 返回 projection_id 与 high_water_seq=H，后续 messages 请求按 manifest_id+offset 读取固定正文。不能每次用当前表 visibility 重新筛选。

cursor 服务端签名，绑定 principal、room、projection、manifest、offset 和分页版本。首版 manifest 与房间历史一起保留，不依赖长 SQLite 读事务；超过响应字节预算分页。若以后增加保留期限，过期返回 resync_required，不静默换 H。restart 后读取旧清单仍返回旧 revision 对应成员关系与正文。

SQLite WAL 的读事务快照只在事务生命周期内稳定；业务参数 at_seq 并不能自动让后续事务读到历史状态。[S14] 因此采用上述不可变清单，而非伪时态查询。

### 12.3 订阅握手

WebSocket 新增 roundtable_attach {subscription_id,room_id,since_seq?,projection_hash?,protocol_version} 与 detach。SubscriptionHub 先注册授权后的有界缓冲，再在一致性读事务取得 H。冷路径返回 H 的 snapshot 引用；热路径仅当 L 的 projection hash 匹配且日志完整时返回 (L,H] 的分页 replay，不同时要求合并 snapshot。

热 replay 的 cursor 固定 L/H/subscription_generation；客户端所有 replay 页处理完成前不应用 >H 的实时帧。完成后丢弃缓冲中 ≤H 的重复，再顺序应用 >H。缺口、断线、对象不匹配或溢出均重新握手。事件从 DB 日志按 seq 拉取，内存通知只是唤醒；通知丢失不能丢掉提交。无需 notification outbox 或 delivered 标志。

Tauri 使用窗口定向 emitter 或 IPC channel，只给绑定 subscription 的可信窗口投递；WebSocket 只给授权 socket 的 room subscription 投递。两者禁止调用 legacy emit_event，也不经过全局 broadcaster 的圆桌正文路径。复用 ACP 的原始 update、permission 和 lifecycle 同样接私有 sink；不能仅过滤派生圆桌事件却仍让 emit_with_state 的原始内容进入全局流。attach/detach 不拥有 room 生命周期。

### 12.4 临时预览

preview envelope 必填 subscription、room、speaker、attempt、incarnation、run_epoch、phase_revision。单帧 chunk_seq，合并批次使用 first_chunk_seq/last_chunk_seq；reset 带 baseline_seq。迟到旧 incarnation 的帧直接丢弃，accepted 后用规范结果替换预览并关闭该 attempt 预览。

可以在 30–50 毫秒窗口合并 delta，但不改变文本顺序。慢客户端先丢预览；仍溢出则 detach/resync，持久 DB 日志不丢。只展示可公开 assistant 内容和工具状态，不把思考文本或自由 token 解析为最终结果。

## 13 API 权限和界面

### 13.1 统一命令形状

所有 HTTP 调用均为 POST /api/<command>，与 Tauri invoke(command,args) 共用 snake_case DTO。无 GET/PATCH 路由假设，无路径参数。下列为完整首版命令闭集；读命令也沿用 POST：

| 命令 | 主要输入和效果 |
|---|---|
| roundtable_preflight | 完整配置与 source_refs；返回能力、证书、容量、ACP 尝试数和阻断原因，不启动模型 |
| roundtable_create | request_id、config；创建 draft，返回 room 和 projection |
| roundtable_update_draft | room_id、request_id、expected_revision、完整 config；仅 draft |
| roundtable_start | room_id、request_id、expected_revision；重新检查证书/资料/容量并受理运行 |
| roundtable_get | room_id、可选 projection_id；返回当前或指定不可变投影 |
| roundtable_list | workspace_id、cursor、limit；当前操作者可见房间列表 |
| roundtable_pause | room_id、request_id、expected_revision、reason；立即中断并清理 |
| roundtable_resume | room_id、request_id、expected_revision、concurrency 可选、recovery_consent；仅可恢复状态 |
| roundtable_stop | room_id、request_id、expected_revision、force_latest 默认 false；立 fence 并清理 |
| roundtable_interject | room_id、request_id、expected_revision、text、mode=next_phase 或 restart_current |
| roundtable_retry_synthesis | room_id、request_id、expected_revision；仅 synthesis_failed，预留额度后只重试主持 |
| roundtable_events | room_id、after_seq、through_seq、cursor；固定水位的授权重放 |
| roundtable_messages | room_id、manifest_id、cursor；按固定清单读取不可变正文 |
| roundtable_evidence | room_id、evidence_id；仅授权的人类查看已允许的证据 |
| roundtable_operation | room_id、operation_id；返回持久控制进度和原始结果 |
| roundtable_clone | room_id、request_id、expected_revision、carry_published_context、config_override 可选；创建新 draft，不自动运行 |
| roundtable_attach 和 detach | Tauri 命令和 WebSocket 同义帧；观察者订阅，不改变运行状态 |

config 必含 topic、workspace/source snapshot 引用、participants、moderator_ordinal、strategy={type:phased_rounds,version:1,critique_rounds:R}、concurrency:C、strict_snapshot_v1、budgets、超时、输入/输出字节配额。provider_ref 引用已有配置，不能提交任意二进制、shell、环境变量或凭据；服务器回显实际模型和 effort，不静默替换。

改显示名仅在 draft 的完整配置更新中提供；运行后显示名冻结。首版不提供房间删除、独立导出、批量管理或审批命令，界面也不出现这些按钮。后续如增加必须定义授权、事件、历史和 GC 语义，不能靠旁路写数据库。

### 13.2 响应和错误

mutation 响应含 request_id、accepted、operation_id 可空、room_id、revision、run_epoch、last_seq、status。accepted 仅表示命令已持久受理；paused/stopped/completed 必须等对应持久投影确认。请求号统一作用域为 (principal_id,api_major,request_id)，客户端重试不能随意换 ID。

统一错误字段 code、message、retryable、current_revision、details。400 invalid_argument；401 unauthenticated；403 forbidden；不可访问或不存在的 room 对外统一 404；409 revision_conflict、idempotency_conflict、command_in_progress、control_in_progress、invalid_state；422 policy_unenforceable、capability_unqualified、context_too_large、capacity_unknown、insufficient_budget、cannot_reach_quorum、no_next_phase；429 capacity_limited；503 storage_unavailable/runtime_unavailable。command_in_progress 通过同键查询或重试获取原响应。

读取分页默认 100 项，最大 500 项/1 MiB，以先达到的上限为准；单对象超页限制返回独立对象引用，不截断 JSON。配置/问题/插话、源文件、日志、工具结果各自有上限，preflight 统一展示。schema 拒绝重复 JSON key、未知必需枚举、非有限数、非预期字段和深度超限；先限字节，再解析。

### 13.3 授权和安全展示

每个命令、projection/message/evidence fetch 和订阅都调用同一 authorize_room。第一版是单操作者实例，不声称多租户隔离。所有有效 operator 客户端共享该操作者的房间操作权；知道 room UUID 不授予权限。Web 沿用已有 Bearer 与 WebSocket token subprotocol；token 不进 URL。CORS/Origin/TLS 的服务级配置沿用既有安全责任，不把全服务 CORS 重构塞入圆桌工程，但缺少合格远程部署保护时不开放远程圆桌。

模型、README、工具输出和别的成员结果都是不可信文本。前端安全 Markdown 或纯文本渲染，禁 raw HTML/script、javascript/data 执行链接和自动远程图片；外链仅允许受控 scheme，证据跳转走服务器授权端点。不能因为后端字段名叫“已验证证据”就放开浏览器资源加载。

### 13.4 体验

创建页展示 N/R/C、主持身份、模型服务、精确资料范围、Q、尝试上限、时长预算、隔离资格和费用未知。预设三人两轮可改；N=2 的脆弱性明确展示。

运行页以稳定 ordinal 卡片展示 role、模型/effort、等待/生成/验证/清理/缺席；C<N 明示“同时运行 C/N”。已到 Q 仍有成员运行时写“等待其余成员”，不让人误以为卡住。圆桌阶段和 slot 完成数独立于 Simple workflow 任务进度，不恢复最新 UI 已移除的 DAG。

预览标“生成中，不是最终结果”。结果可以跳到 claim、response、实际版本证据。暂停说明损失、综合失败的“只重试主持”、恢复费用风险和阻断出口必须可见。键盘操作、文字状态、适度 aria-live、RTL 与既有语言体系兼容。

## 14 耐久性 诊断和清理

### 14.1 DB 与 blob

逐连接初始化并测试 foreign_keys、busy_timeout、WAL 和选定同步等级；不能仅在 pool 对象 execute 一次后假定全部连接相同。首版明确区分进程崩溃恢复和断电耐久性。若界面对“已完成”作断电持久承诺，写连接需 FULL 等可验证配置并完成文件系统故障试验；它是全库相关变更，单独评估性能与兼容性。

不可变 blob 顺序：临时文件写完并 fsync → 同卷原子 rename → fsync 父目录 → 校验可读 hash → 提交 DB 引用。任一步失败不能提交引用；不支持相应耐久语义的文件系统不得宣称断电可靠。DB 内保存的小正文不另造文件窗口。

GC 只删除确认无 DB、projection、page manifest、source manifest、diagnostic 或备份引用的孤儿，使用至少 24 小时宽限，不能删除刚创建尚待 DB 提交的对象。备份先固定 DB 水位与对象清单，再复制并校验所有被引用对象；恢复先校验所有对象存在/hash，缺失则历史只读且阻止运行，不能用当前文件替代。迁移失败不启动圆桌，不修改旧 delegation 语义。

### 14.2 失败资料

每个 attempt 保存有界诊断：finish reason、错误分类、validation field_errors、已收到 assistant 原文的前/后片段（合计最多 64 KiB）、总字节数、完整流 hash（可取得时）、截断标志、ingress 水位和 cleanup 结果。不保存秘密 token 或思考内容；发现秘密先脱敏并记 redaction 标志。日志不替代规范 message。

诊断与消息使用同一 room 授权；默认保留到该房间以后被明确删除，配额不足时创建/preflight 失败或有界截断诊断，不能无提示删除证据。运行 scratch 与不可变历史分开，正常退出只清理 scratch。首版没有用户删除 API，不因此无限接收新数据；总存储配额是准入条件。

### 14.3 核心不变量

I1 每 phase revision/speaker 最多一份 accepted；当前 phase 唯一。I2 控制和结果写入经过当前 actor/fence；迟到计量只有受限归档入口。I3 published 正文与历史 membership 不可变。I4 后续输入只来自冻结快照与已发布数据。I5 接纳的关系、预算、projection 和 event 同事务。I6 stop 提交后无新增本地准入。I7 cleanup 确认前不复用 permit 或不明 binding。I8 room.seq 连续递增且投影可重放。I9 资格或隔离不明时不运行。I10 在明确活性前提成立时，中间状态最终前进，否则显示具体阻断。

## 15 自由讨论扩展边界

底座保留 strategy type/version、稳定 speaker、不可变可见视图、SchedulingIntent、预算准入、事件和控制操作。第一版只有 phased_rounds/v1，不提前实现通用图引擎或另一套 Python 调度器。未来 strategy 只能给意图，不能绕过所有权、隔离、结果契约或 fence。

自由讨论需要单独设计语义消息触发、触发去重、每消息最大回应者、每成员串行 mailbox、公平队列、冷却、因果深度、总发言数和停止条件。响应输入冻结到 trigger_seq，不能将实时 token 当触发；没有运行但仍有排队意图不算完成。达到预算时应保留分歧，不能制造共识。

扩展 N/R 只改变有界策略参数，不需要更换底座。自由讨论则必须先取得交互质量和资源数据，再冻结自己的触发/终止协议。首版 scope 减少不授权以后自动增加工具副作用或扩大资料范围。

## 16 验收计划和明确反例

以下是待实施的验收，不是本次已运行结果。每个 T 编号同时供评审处理登记引用。

T01 参数化：N=2/3/7、R=0/1/2/5、C=1/2/N，基础次数符合公式；7/5/2 可运行或被明确容量拒绝，不因写死常量出错。

T02 最终准入：用两个可实现的调度检验原反例。A 在取得 gate 前挂起，先提交 stop 再恢复，旧 attempt 必须拒绝。B 持 gate 并完成检查后、try_enqueue 前挂起，发起 stop；stop 必须等 gate，恢复后入队/释放 gate，然后 stop 才能提交。这证明“stop 先提交但旧 enqueue 成功”的顺序不可达，不能把测试写成持 gate 时等待 stop 提交的死锁。已有准入的 residual 风险保留；队列满没有准入，崩溃处于 admitting 不重发。

T03 所有权：两个应用进程共享目录，只有一个 RoundtableService 获锁；automation lock 不代替证明。模拟旧进程存活、PID 重用和 bootstrap 尚未返回，清理按 incarnation，不误杀/漏杀。

T04 只读资格：在首个精确 OS/CLI/版本组合下，测试全局 full-access、原生 shell/fs、绝对路径、symlink、HOME、额外 MCP、秘密读取、网络目的地及子进程。一次性副本外访问必须被强制拒绝；失败不列为支持。

T05 专用连接：仅出现 roundtable 三工具，permission 自动拒绝；不创建询问卡、反馈、浏览器、委托或普通侧栏记录；启动期间 discovery 与后续 import 也不能漏入。

T06 结果通道：错误别名、跨 attempt token、重复 key、错 phase kind、伪造身份、过大输出、无 submit、重复 submission 及不同 payload 冲突。合格 candidate 在取消或 abnormal finish 后不能变 accepted。

T07 完成边界：延迟最后一条 ACP update 或已经准入的 MCP handler，即使 prompt response 已到也不能提前 RuntimeTurnCompleted；跨传输的未准入迟到 submit 必须 attempt_closed；尾部不能归给下一 attempt；违反有序边界退役连接。

T08 阶段可执行性：两份 summary 非空但 claims=[] 的 proposal 不得让 critique 成功启动；abstain 不计 Q；有效 proposal 均分配不同 speaker；成员失败只降低实际回应覆盖，不改冻结目标。

T09 隔离与 barrier：同阶段互不可见，Q 达到不提前关，deadline 后结果不接纳；按 ordinal 发布；N=2 一人失败不假装成功；主持不把缺席当支持。

T10 原子接纳：在 message/claims/responses/evidence/budget/event 每步注入 DB 失败；不存在半条 accepted。accepted 已提交但 UI 丢包，重放后不再次调用成员。

T11 控制恢复：closing 加 pause/restart/stop 后各处崩溃；严格走矩阵；两个不同 restart 返回 control_in_progress；同请求仅一个 successor；stop 覆盖后不发布旧集合。

T12 重开预算：不足在破坏进度前拒绝；并发预算变化与预留同事务；连续暂停会消耗 per-turn 额度且 UI 给明确出口；综合失败只调用主持，不重复 proposal/critique。

T13 时间：default 3/2/3 确有修复时隙和总预算，C 降低重新验算；排队不扣活跃时长，并发不重复算墙钟；时间跳变无影响；checkpoint 失败停止准入；超时清理继续并显示 overrun。

T14 事件投影：任意 L/H，fold 与 H snapshot hash 一致；draft/config/C/控制/binding/revision 均恢复；未知投影事件停止，不能跳过推进水位。

T15 分页订阅：在 H 取清单后 restart，旧页保持 H；热 replay 多页期间积累 >H 事件，完成前不应用；模拟重复、乱序、buffer 溢出、drop wake、重连；预览批次范围不误报缺口。

T16 授权和泄漏：未授权 socket、completion scope、另一 room token 不得读/订阅/取证据；圆桌 body 和原始 ACP update/permission/lifecycle 均不进入 legacy 全局广播；Tauri 只到目标窗口；恶意 HTML/链接/图片不得执行或自动外发。

T17 快照：同 base_commit 不同 dirty/untracked 字节生成不同版本；创建中改文件明确报不稳定；证据定位到副本实际字节；原工作区变化不漂移；staged peer 证据不可见。

T18 上下文：完整上界包含结果、问题、插话、转义、证据、工具和输出预留；容量不足在付费前拒绝；新 binding 没有前轮隐藏历史；禁止将短追加文本当作 live context 已裁剪。

T19 耐久：每条 DB 连接配置、disk full、blob rename/目录 fsync/DB commit 窗口、缺失对象、备份恢复、GC 引用均测试；普通单元测试不能替代断电 VM/文件系统试验。

T20 计量：累计值去重、counter reset、occupancy 不计费、不完整数据为 unknown；终态旧 epoch 迟到 usage 只能更新历史 measurement，不能影响调度或复活状态。

T21 资源和活性：all-or-none permit、min(C,remaining)、首次优先、无空闲进程泄漏；本地清理已证且 DB/资源可用时不能卡 closing；无法清理时有可解释 blocked 状态，不伪成功。

T22 讨论质量：固定同资料任务比较单模型、R=0、R=2，隐去模式信息供评审；预埋事实错误、反例和重要少数风险，记录发现率、新增无依据结论、关键质疑实质回应和少数意见保留，同时报告额外 attempts/耗时。门槛在试验前制定，样本、评分和失败案例留档；不能仅以 JSON 合法或评价者喜好宣布圆桌更好。

## 17 里程碑和冻结门

M0a 协议确认：确认第 1 节推荐决策、DTO/错误闭集、完整 projection、控制矩阵、准入线性化、预算公式及安全档。输出契约 fixtures 和反例计划。通过只表示可开展受限验证，不表示真实 ACP 已可启用。

M0b 首适配器可行性验证：选择一个明确 OS/CLI/版本组合；验证隔离先于 spawn、专用 MCP ready、最终结果提交、更新归属、取消与进程回收、无普通侧栏导入。产出证书或明确失败原因。可以与小型 fake runtime 骨架并行，但不得在这条链路未证明前全面铺开产品实现。不得为通过试验默默放宽安全档。

M1 最小纵向控制链：先以 fake runtime 完成 reserve→admit→submit→accept→close→publish、stop 竞态、持久恢复和定向订阅。出口为 T01–T03、T06–T15、T17–T21 的适用协议测试通过。fake runtime 不取代真实隔离验收。

M2 首个真实双端闭环：接入唯一已认证组合，至少多个独立成员 binding 与独立主持完成参数化讨论，桌面/Web/remote 使用同一服务。通过首适配器的安全、结果、取消、失败和 UI 验收。无需在 MVP 前先要求三种品牌全部通过。

M3 小范围开启和质量验证：默认关闭的 feature flag，按证书逐组合开放，完成 T22；观察 invalid、uncertain、取消耗时、DB 错误和未知计量。回滚阻止新准入并安全停止已有运行，历史仍可读。增加品牌/平台需要新增证书，不沿用品牌级准入。

M4 后续能力：仅在实测需要时考虑 live reuse、strict resume、更多资料入口、多 active room 或自由讨论。每项重新定义能力与确认边界，不列为首版隐含任务。

冻结前仍有三项真实约束：产品需接受第 1 节取舍；首平台隔离实现尚未选定/验证；上下文和质量门槛必须由首适配器与固定任务试验确定。不存在“只剩 UI 参数，不妨碍全面开工”的结论。本次没有执行这些里程碑。

## 18 版本变化

相对 v1.0 的主要替换：

- 用一个受控本地准入边界代替“发送前再查 fence”的口头保证
- 用 roundtable MCP 的受限身份、候选提交和完成屏障代替自由文本 JSON 抽取与 UUID 抄写
- 新增 Roundtable 连接用途、明确内部会话登记/发现过滤；仍复用原 ACP 进程设施
- 严格区分一次性资料副本与真正隔离，不把当前适配器不足推成架构不可能
- 用数据目录专用所有权、boot_epoch、actor、attempt 调度状态和持久事件取代房间租约及两套 outbox
- 用完整版本化业务投影和固定清单使 replay、旧分页与 revision 传播可检验
- 补全 ControlOperation、closing 恢复、预算预留及综合单独重试
- 将修复时间同时计入阶段与总预算；默认新建会话，开始前检查整个计划上下文上界
- 资料按实际 dirty/untracked 字节版本化，coverage 由宿主生成，质量与结构验收分开
- 把首适配器资格提前，允许先做受限 fake runtime 验证，暂缓复杂审批、跨房间调度和自由讨论

## 19 评审处理登记

登记规则：V1 是 review1.txt，V2 是 review2.txt，V3 是 review3.txt；相同发现合并为一个 F 编号，并列出全部来源。接受表示问题和修订方向成立；部分接受表示限定论断或只采纳适合首版的部分；不采用表示建议不进入本版，并给出原因。每项都有规范落点和验收。原稿章节号只用于说明评审证据，不再构成有效规范。

F01 核心架构和身份分层。来源 V1 总结、V2 保留项、V3 第一节。接受：独立 Rust 服务、无需 Python、真实父 MCP 关联、accepted/published、固定快照、独立主持、ordinal、固定 Q 和 unknown 费用均保留。证据为原稿 §3–7 与 [S3][S12]；本稿 §1/4/7/9/10/15；验收 T01/T09/T10。

F02 只读资格排在控制面之后。来源 V1 阻断1、V2 必改1、V3 P0-4/交付顺序。接受：至少一个真实组合是启用可行性门；fake runtime 可以并行验证，不能将真实适配器当最后产品名单选择。证据原稿 M1/M2 顺序和 [S5][S6]；本稿 §5/17；验收 T04/T05/T07。

F03 一次性副本作为较低安全档。来源 V1 阻断1、决策1，V2 必改1、最后决策。部分接受：采用副本作为资料版本和隔离材料，不采用“副本即保护原项目”或普通对话风险档。无限制 CLI 能用绝对路径访问原文件，permission deny 也拦不住无需询问的操作。现有 containment 注释不证明新增 OS 隔离不可能。本稿 §1/5/6；验收 T04。

F04 Codex/Grok 与 Windows 风险。来源 V1 阻断1、V2 必改1。部分接受：[S6] 确认 Codex 模式语义和 Grok 全局 permission 配置；seatbelt/landlock 不是 Windows 隔离实现，不能据此断言 Windows 永无可用方案。所有组合保持 not_tested，覆盖全局配置旁路；本稿 §3/5；验收 T04。

F05 新增 Roundtable 用途和隐藏侧栏。来源 V1 阻断3/决策2、V2 必改3。接受需求、限定实现：仅加 enum 不够，需 policy 分解、内部登记和全发现路径过滤。[S4] 显示 purpose、registry 与 hidden generation 是不同控制点。本稿 §5；验收 T05。

F06 普通用途必然注入 delegation。来源 V1 阻断3、V2 必改3。部分接受风险、不接受绝对表述：HostToolsPolicy::Agent 已抑制 delegation，但其他组和原生配置仍需关。[S5]；本稿 §3/5；验收 T05。

F07 另建 service_session 运行时过重。来源 V1 阻断3、V2 必改3。接受复用建议：spawn 已支持无父连接和显式 emitter，但仍需新的服务 owner、gate 和 launch wrapper，不能零改动直接当安全入口。[S3]；本稿 §3/4；验收 T02/T03。

F08 JSON 抽取和完整 UUID 易错。来源 V1 阻断2、V2 必改2。接受：首版强制 schema MCP submit，提示词短别名映射，最终文本只预览。MCP 是本版选择，不声称唯一理论方案。证据原稿 §6.3/6.4；本稿 §7；验收 T06。

F09 关闭宿主 fs 却要求可见证据读取。来源 V1 阻断2、V2 必改2。接受：专用 read/search 工具走独立 MCP，不广告 ACP fs；CLI 原生读不能冒充已登记证据。[S5] 与原稿 §14；本稿 §6/7；验收 T04/T06/T17。

F10 主持 preview 缺 speaker_id。来源 V2 规格首项。接受：原稿主持 participant 可空但 preview 必填不一致；本稿所有帧统一 speaker，participant 可空。落点 §4/12；验收 T06/T15。

F11 接纳事务遗漏 claims 回复边与 evidence。来源 V2 事务项。接受：原稿 §10.2 仅明确 message；本稿 §10.3 同事务包含全部语义关系与投影；验收 T10。

F12 幂等处理中状态和 principal。来源 V2 事务项、V3 局部问题。接受：scope 统一 (principal,api_major,request_id)，持久 processing、immutable response、operation，桌面/Web 同一目录操作者。证据原稿 §9/附录不一致；本稿 §4/10/13；验收 T10/T11/T16。

F13 默认 180 秒没有可靠修复余量。来源 V1 重要项3、V2 时限项、V3 P1-3。接受问题、限定断言：早期 invalid 仍可能重试，不是绝对无重试；没有预留完整修复槽。同步修改阶段与总预算，默认上限 30 分钟；本稿 §9；验收 T13。

F14 同会话修复。来源 V2 时限项。部分接受：同 prompt 内工具错误可修复；结束后的同 session 修复暂不采用，需完成/上下文认证。首版新 binding 重试。原稿 §6.4 和上下文反例；本稿 §7/8；验收 T06/T07/T18。

F15 HTTP 形状不兼容。来源 V1 重要项1、V2 HTTP 项。接受：[S8] 为 POST /api/<command>；本稿 §13 完整命令统一该形状；验收 T16 的双端契约对照。

F16 emit_event 全局泄漏。来源 V1 重要项2、V2 事件项。接受：[S8] 确认全窗口与 Web 镜像；本稿 §12 独立订阅和定向投递，纳入 M1；验收 T15/T16。

F17 ResumeExistingOnly 省略 ID。来源 V2 恢复项。部分接受：源码确实接受省略，但 ACP load/resume 响应本来可能无 ID，不是通用 bug；首版只重建，未来 strict identity 必须有独立证明。证据 [S7][S13]；本稿 §8；验收 T07/T18。

F18 快照算法和输入上界。来源 V2 快照项、V3 P1-2/P1-5。接受：576 KiB 只是 9×64 KiB 的结果正文，不是含证据/插话/包装的完整上限。定义实际字节 manifest 和全计划 preflight；本稿 §6/8；验收 T17/T18。

F19 每房间租约 两套 outbox 和 BEGIN IMMEDIATE。来源 V1 过度设计、V2 单进程项。部分接受简化：保留 attempt 持久 dispatch 与 uncertain；专用 OS 锁+boot_epoch+actor 取代租约。现有 automation lock 只保护 engine，不能当全库单进程证明；SeaORM 事务不因示意伪码自动成为 IMMEDIATE。[S10][S11]；本稿 §4/10/12；验收 T02/T03/T10。

F20 交互审批 工具副作用账本 跨房间 FIFO 和保留槽。来源 V1 过度设计、V2 首版削减。接受本版移除：严格只读、三工具和一个 active room，保持 N/R/C 参数化。没有审批表不意味着未知工具放行；本稿 §1/5/9/10；验收 T01/T04/T21。

F21 暂停文案与取消语义不符。来源 V1 决策3、V2 暂停项、V3 P1-3。接受：选立即暂停并明示丢弃未完成回答、消耗尝试与恢复限制。不是已获产品批准的选择；本稿 §1/11/13；验收 T11/T12。

F22 综合失败只能全场重跑。来源 V2 综合项/最后决策。接受：保持 published 输入，synthesis_failed 为可操作 paused，新增 retry_synthesis 只跑主持。原稿 §6.5/8；本稿 §11/13；验收 T12。

F23 两人 Q=2 的脆弱性。来源 V2 quorum 项。接受展示建议，不降低 Q；本稿 §7/13；验收 T09。

F24 四级 usage 和跨重启墙钟期限。来源 V2 首版削减。接受简化，保留单位/范围/语义避免错误相加；用持久剩余量与预扣 monotonic 片，崩溃后暂停。原稿 §7.2/17；本稿 §9/11；验收 T13/T20。

F25 CORS 属于整个服务。来源 V2 首版削减。接受范围边界，仍要求实际远程部署保护和每房间授权；本稿 §13；验收 T16。

F26 连接池 PRAGMA。来源 V1 附带问题。部分接受：确认缺少每连接保证，不声称已经证明四条连接配置错误或数据损坏。列独立基础设施前置验证；本次不修代码。[S11]；本稿 §14；验收 T19。

F27 检查到 enqueue 的竞态。来源 V3 P0-1。接受：共用 gate 覆盖持久 fence 提交与非阻塞本地准入，工具入口也覆盖；stop 前准入的残余远端工作另计。原稿 §4.2/15.4 反例；本稿 §4；验收 T02。

F28 fold 事件载荷不闭合。来源 V3 P0-2。接受：配置 hash/消息 ID 不足以重建；选择 immutable ProjectionV1，完整版本取数后替换。原稿 §10.3/12/附录；本稿 §12；验收 T14。

F29 at_seq 分页不是历史视图。来源 V3 P0-2。接受：[S14] 事务快照不等于后续查询时态；使用固定 membership 清单和 body hash；本稿 §12.2；验收 T15。

F30 未知投影事件不可跳过。来源 V3 P0-2。接受：未知 projecting schema/cause 停止，不能 seq 正确而业务错误；本稿 §12；验收 T14。

F31 控制意图和 closing 恢复。来源 V3 P0-3。接受：持久 ControlOperation 与矩阵、同请求幂等、第二 restart 拒绝、stop 覆盖、唯一 successor；本稿 §10/11；验收 T11。

F32 预算不足导致无出口 paused。来源 V3 P0-3。接受并加强：破坏进度前原子预留而非仅查询余额；后续不可预见不足为明确 blocked，克隆/停止出口；本稿 §9/11；验收 T12。

F33 活性缺少约束。来源 V3 P0-3/第五节。接受：本地清理、存储/资源可用且无待确认时必须推进；其余明确阻断。远端不可知不等于本地永远活跃；本稿 §11/14；验收 T21。

F34 资格绑定版本且隔离先于 spawn。来源 V3 P0-4。接受：证书具体到 OS/二进制/adapter/隔离/配置/policy，漂移失效；本稿 §5；验收 T04。

F35 prompt 响应不等于 update 排空。来源 V3 P0-4。接受：[S13] 支持通知关联和取消尾部边界；运行层必须有有序 ingress 和完成屏障，首版不开放未认证复用；本稿 §8；验收 T07。

F36 空 claims 通过 Q 后下一阶段无合法目标。来源 V3 P1-1。接受：proposal/critique 至少一条 claim，abstain 不计 Q；本稿 §7；验收 T08。

F37 目标覆盖和后续质疑。来源 V3 P1-1。部分接受：确定性环分配与精确 incoming response 目标，保证分配不保证失败成员完成；不在进行中的快照重分配。最多 4 个必答目标，其余可见并如实列未回应；本稿 §7；验收 T08/T22。

F38 裁剪公共文本不能裁剪 live session。来源 V3 P1-2。接受：分开公共、交付、实际上下文三清单，首版每 attempt 新 binding，全前缀上界在开始前核验；本稿 §8；验收 T18。

F39 N² 与 R² 的累计阅读增长。来源 V3 P1-2。接受为完整前缀假设下的容量模型，非测得费用；不只计算 B；本稿 §8/9；验收 T18。

F40 ACP prompt 不是计费请求。来源 V3 P1-3。接受：[S13] 和原稿 B 公式；UI 名称修正，CLI 内部循环边界如实声明；本稿 §9/13；验收 T01/T20。

F41 usage 的 scope semantics 和去重。来源 V3 P1-3。接受：session 累计与 occupancy 不能混成 turn 增量计费；本稿 §9.4；验收 T20。

F42 总活跃时间定义及连续暂停。来源 V3 P1-3。接受：room 墙钟、包含/排除项、清理 overrun、per-turn 消耗都明确；本稿 §9/11；验收 T12/T13。

F43 permit 原子获取 余下 slot 首次优先。来源 V3 P1-4。接受：all-or-none、min(C,remaining)、首次优先；跨房间公平延期，不声称 FIFO+aging 已完成防饥饿证明；本稿 §9；验收 T21。

F44 空闲进程 scratch 与终态回收。来源 V3 P1-4。接受：首版空闲进程上限 0，活动 permit 直到清理，存储独立配额；本稿 §8/9/14；验收 T21。

F45 未提交内容和 evidence 发布归属。来源 V3 P1-5。接受：实际字节 manifest、owner_attempt、staged_phase/revision、published_seq；base_commit 仅来源元数据；本稿 §6；验收 T17。

F46 额外读取与扩大来源的差别。来源 V3 P1-5。接受原则并减范围：首版没有额外读取审批，工具只在固定 manifest 内；新文件必须克隆，不改当前 context_hash；本稿 §1/6；验收 T04/T17。

F47 机械 coverage 和共识依据。来源 V3 P1-6。接受：服务计算 coverage，显式共识绑定不同 speaker 的支持边；不把引用合法等同语义支持；本稿 §7；验收 T09/T22。

F48 产品质量与基线比较。来源 V3 P1-6。接受：固定任务比较单模型/R0/R2，关键错误/质疑/少数风险和消耗并列；本稿 §16 T22/17；尚无试验结果。

F49 revision 传播。来源 V3 局部问题。接受：每个 durable envelope 有 resulting_revision，config/C/binding 变化有投影，不能让控制按钮靠猜；本稿 §12；验收 T14。

F50 失败原文不能依赖预览。来源 V3 局部问题。接受：有界不可变诊断、截断/hash/错误/授权/保留；本稿 §14；验收 T06/T19。

F51 删除 导出 改名 API 缺口。来源 V3 局部问题。接受范围缺口，暂不提供删除/独立导出/运行后改名；draft 改名由 update_draft 完整定义；本稿 §13；验收命令闭集与 UI 一致。

F52 DB 与 blob 整体耐久。来源 V3 局部问题。接受：原稿已有文件 fsync/rename，但需父目录 fsync、可读校验、引用完整备份与 GC 顺序；本稿 §14；验收 T19。

F53 旧 epoch 与迟到计量。来源 V3 局部问题。接受：仅当前 service 的专用计量入口可归档，旧 runtime 不获得业务写入豁免；本稿 §9/14；验收 T20。

F54 前端不可信内容。来源 V3 局部问题后段。接受：安全 Markdown、scheme 白名单、禁自动远程资源及证据授权；本稿 §13；验收 T16。

F55 反例回归替代泛泛测试。来源 V3 第五节全部八个场景。接受：gate 竞态 T02、fold T14、H 清单 T15、closing+控制 T11、空 claims T08、延迟 update T07、预算不足 T12、dirty 快照 T17；另以 T21 检验活性。

F56 缩范围而不削弱基础不变量。来源 V3 第六节/结论。接受：首个真实资格前置，fake runtime 先建立协议证据，减少平台/入口/恢复能力；自由讨论仅保留接缝。原稿 M0–M5；本稿 §15/17。

F57 现有代码核对清单与证据强度。来源 V1 “17 条/9 准确/3 部分/5 遗漏”、V2 首段。部分接受可核对内容：父 MCP、workflow、恢复 gate、空 roots、companion readiness、completion scope、事件环、清理和新 session 去重分别有代码依据；不把未提供 canvas 的数量当成已审计清单。静态可复用不等于圆桌安全验证通过；本稿 §2/3/20；验收由上述对应 T 项承担。

## 20 固定源码和协议依据

S1 基线及 UI 差异。本文静态目标为下列固定提交；原始 checkout 的 abd3c674 与之分开说明。
https://github.com/icannotwait/MyCodeBuddy/commit/2587471a0ead1c5be4afda1ae3ff27bdea80606c
https://github.com/icannotwait/MyCodeBuddy/commit/4d513c7308d5594ba9f64d7b498dfb2d29fe788e
https://github.com/icannotwait/MyCodeBuddy/commit/993b6323a7d4f29768a97c5606a7da7dadd68e34

S2 独立修复线。tree 与本地 4e4d327 相同，本文不假定已合入主线。
https://github.com/icannotwait/MyCodeBuddy/commit/6a66c0a2757052cf4647164b2f6c3b44e1f986ec

S3 ACP 服务启动 预分配 incarnation 和会话去重。
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/acp/manager.rs#L3552-L3633
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/acp/manager.rs#L3690-L3839

S4 连接用途 隐藏会话登记 权限拒绝和用途特例。
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/auto_title/types.rs#L147-L165
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/auto_title/internal_sessions.rs#L19-L185
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/acp/connection.rs#L10268-L10301
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/acp/connection.rs#L5445-L5475

S5 宿主工具与 delegation 抑制 读写能力以及空根含义。
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/acp/host_tools_policy.rs#L1-L109
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/acp/connection.rs#L5674-L5687
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/acp/file_system_runtime.rs#L42-L175

S6 Codex 模式说明和 Grok 全局 permission 映射。
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/commands/acp.rs#L4216-L4258
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/commands/acp.rs#L4260-L4348

S7 现有恢复语义与品牌 rollout gate。
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/acp/session_attach.rs#L1-L134
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/acp/delegation/capability.rs#L10-L46

S8 Web 命令传输 全局事件桥和认证边界。
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src/lib/transport/web-transport.ts#L250-L266
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/web/event_bridge.rs#L411-L433
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/web/ws.rs#L81-L128
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/web/auth.rs#L13-L147

S9 有限连接重放环。
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/acp/event_stream.rs#L10-L45

S10 automation engine 的独占锁 范围仅为 engine。
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/automation/engine.rs#L224-L281

S11 SQLite 池初始化和 PRAGMA。
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/db/mod.rs#L56-L126

S12 父 MCP 关联与旧 workflow 业务模型。
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/acp/delegation/broker.rs#L9734-L9766
https://github.com/icannotwait/MyCodeBuddy/blob/2587471a0ead1c5be4afda1ae3ff27bdea80606c/src-tauri/src/acp/delegation/workflow/types.rs#L372

S13 ACP 官方 v1 schema 与 prompt turn。2026 年 10 月 3 日读取；它们是当前在线协议资料，不等于所有已安装 adapter 已支持的功能。
https://agentclientprotocol.com/protocol/v1/schema
https://agentclientprotocol.com/protocol/v1/prompt-turn

S14 SQLite 官方隔离说明。读事务快照与业务时态查询必须区分。
https://www.sqlite.org/isolation.html
