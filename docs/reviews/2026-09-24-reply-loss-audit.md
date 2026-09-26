# 回复丢失链路审计（2026-09-24）

> 状态更新：下面保留的是修复前审计记录（基线 `093f5a4a`），其中“仍有问题”“当前失败”“未修改生产代码”均指审计阶段。用户随后授权的 F1–F12 及评审相关修复现已完成；全量前端 623 个套件、10,110 项通过、15 项跳过，原始 7 个复现、构建及后端定向验证通过。最终状态与边界见[修复验收记录](reply-loss-audit/fix-verification.md)。

## 结论

**仍有类似问题的触发路径，之前的局部修复没有封住整类问题。** 本次沿事件接收、流式缓冲、完成转存、持久化回读、历史合并、视图生命周期检查，保留了 **6 个可复现缺陷、7 个失败场景**，另列出 6 项需要补充交互或后端测试的代码风险。

这里的“丢失”主要指回复被前端覆盖、隐藏、错误退役，或没有从流式区正确转入历史；不能把它们等同于 transcript 文件被删除。部分情况可通过重新加载持久化历史恢复，但未持久化的内容不能依赖这个补救。

会话 5095 的上一轮完整回复仍在 Codex transcript 和会话详情中。上一轮修复解决的是一种已复现的过期 in-flight 标记路径；缺少当时的前端状态快照，不能断言它就是 5095 的唯一原因，也不能把本报告其他缺陷全部归因到那一次事故。

本轮是审计：新增复现材料和报告，没有继续修改生产代码。工作区原有的 runtime-store 修复及测试保留，桌面应用未重新构建、重启或部署。

## 1. 回复如何到达界面

一条回复会经过多次所有权交接，而不是只存在于一个消息数组里：

1. 用户发送后，runtime store 写入乐观用户消息。
2. ACP 收到事件，按连接与序号处理；文本可能先进入合并队列，再进入连接级 liveMessage。
3. Provider 向 runtime 发布 liveMessage；开启增量渲染时，还维护独立的 live transcript 渲染投影。
4. 完成事件把 liveMessage 转成 localTurns，暂时充当历史回复。
5. 后端读取代理 transcript，解析成 detail.turns，异步回传前端。
6. 前端对齐持久化历史与本地消息，决定覆盖、保留、退役及隐藏哪些内容。
7. 主标签、只读抽屉、画布等组件消费这些共享状态；卸载、重挂载也会影响状态的生命周期。

链路涉及 DB 会话 ID、负数 runtime ID、连接 ID、实时消息 ID、解析器消息 ID。并且“收到完成事件”“转入本地历史”“完整回复落盘”是三个不同时间点。当前缺陷主要出现在这些交接处。

## 2. 已通过测试复现的缺陷

以下 P1/P2 是本次建议修复顺序，不表示所有用户都会遇到。测试调用真实 store/provider 逻辑，模拟网络及事件时序；不是桌面端端到端操作录像。

| 编号 | 优先级 | 触发条件                                             | 已观察结果                                 |
| ---- | ------ | ---------------------------------------------------- | ------------------------------------------ |
| F1   | P1     | 完整持久化回复先进入 detail，较短 live 副本随后完成  | 完整结论被短副本遮住，补读仍不恢复         |
| F2   | P1     | 手动刷新未返回时，新一轮已发送并完成                 | 旧刷新结果删除新一轮本地回复               |
| F3   | P2     | 无用户消息的续答，与其他轮回复恰好共享文本前缀       | 续答被误判为已持久化而退役                 |
| F4   | P1     | 普通回读未返回时，只读侧收到新 live 回复             | 旧详情清掉新用户消息和 live 回复           |
| F5   | P1     | A 已在前一帧显示，下一帧连续收到 A 完成和 B 整轮事件 | B 的完成被拒绝，B 没有进入本地历史         |
| F6   | P1     | 文本已接受但尚未 flush，此时发生序号缺口并恢复快照   | 进行中快照造成重复；已完成快照造成文本丢失 |

### F1：完整历史已经读到，仍会被较短本地副本遮住

位置：[历史合并](/D:/MyCodeBuddy/src/stores/conversation-runtime-store.ts:5975)、[持久化增长判断](/D:/MyCodeBuddy/src/stores/conversation-runtime-store.ts:1110)、[完成后补读](/D:/MyCodeBuddy/src/stores/conversation-runtime-store.ts:7084)。

复现顺序：

1. live 只有 `Checking.`。
2. 一次 preserveLive 回读先取得 `Checking. Final: all passed.`，且仍带本轮 in-flight 标记。
3. live 完成，较短副本进入 localTurns，并覆盖已对齐的完整持久化分组。
4. 完成后补读取得同一份完整 transcript。
5. `richer` 要求新的 detail 相比缓存 detail 有增长；完整内容早已缓存，因此不满足增长条件。

测试推进 30 秒后仍只显示 `Checking.`，缺少最终结论。这不是“等落盘即可”的问题：完整内容已经读到了。

修复方向：判断持久化内容是否足以替换本地副本，应比较**同一轮的持久化内容与本地内容**，不能把“比上次 detail 更长”作为必要条件；历史合并也不能无条件让短本地分组遮住完整持久化分组。

### F2：旧手动刷新可以删除刷新期间新完成的回复

位置：[reloadDetail](/D:/MyCodeBuddy/src/stores/conversation-runtime-store.ts:6787)、[FETCH_DETAIL_SUCCESS](/D:/MyCodeBuddy/src/stores/conversation-runtime-store.ts:3056)。

复现：发起旧历史的手动 reload → 发新消息 → 收到完整新回复 → completeTurn → 旧 reload 返回。

`reloadDetail` 使用 `authoritative: true` 提交，绕过普通交互保护，并清理本地缓冲。请求代次可以排除另一次读取，却没有随新用户轮次一起失效。新回复从“已显示”变成“消失”，只剩旧历史。

该时序具备 UI 可达性：手动刷新与发送入口没有以 detailLoading 互斥。

修复方向：读取开始时捕获当前交互代次；提交时如果出现了新用户轮、续答或新 live 内容，禁止破坏性覆盖。不能仅判断请求返回时是否仍在 streaming，因为新轮可能已经完成。

### F3：文本前缀相同不代表同一轮

位置：[turnGroupPersistedCoversLocal](/D:/MyCodeBuddy/src/stores/conversation-runtime-store.ts:1141)、[retireCoveredLocalTurns](/D:/MyCodeBuddy/src/stores/conversation-runtime-store.ts:1467)。

复现：09:00 的旧历史 → 10:00 assistant-only 续答 `OK` 完成 → 回读加入 11:00 不同用户任务及回复 `OK, different task completed.`。

覆盖判断在文本前缀成立后提前返回；对于没有用户消息的本地分组，没有足够的同轮身份约束。退役路径可直接比较最后两个分组，因此删掉了原来独立的 `OK` 续答。复现刻意使用不同时间，排除了相同时间戳导致的误对齐。

修复方向：先证明属于同一逻辑轮次，再判断内容覆盖。文本长度、前缀或“最后一条”都不足以单独证明身份。工具、思考等非正文块也需要纳入覆盖条件，不能只靠正文长度概括完整性。

### F4：旧普通回读可以清掉新观察到的 live 回复

位置：[refetchDetail](/D:/MyCodeBuddy/src/stores/conversation-runtime-store.ts:6534)、[isActivelyInteracting](/D:/MyCodeBuddy/src/stores/conversation-runtime-store.ts:3085)、[画布重入调用](/D:/MyCodeBuddy/src/components/canvas/canvas-conversation-surface.tsx:458)。

复现：普通 refetch 挂起 → appendViewerUserTurn → setLiveMessage → 旧的、不含 in-flight 标记的详情返回。

交互保护依赖 awaiting_persist、preserveLive 或后端标记，没有把新出现的实际 liveMessage 本身作为保护依据。结果新用户消息和新回复均被清掉。主发送路径有 awaiting_persist 保护，不代表只读观察或内部续答也有相同保护。

后续新事件或重新回读可能恢复显示，但不能保证即时恢复。建议和 F2 统一修复读取提交边界，而不是逐个调用点补 preserveLive。

### F5：同一批事件的完成判断读取了旧轮次状态

位置：[admitTurnComplete](/D:/MyCodeBuddy/src/contexts/acp-connections-context.tsx:5791)、[批次预处理](/D:/MyCodeBuddy/src/contexts/acp-connections-context.tsx:6049)。

复现只改变原有集成用例的分帧方式：

- 帧 1：A prompting、A 文本。
- 帧 2：A complete、B 用户消息、B prompting、B 文本、B complete。

帧 2 预处理 B complete 时，完成准入仍读取全局 runtime 中的 A，而不是已经沿本帧事件推进到 B 的轮次状态。B 的完成被拒绝，但对应序号已消费。最后只有 A 进入 localTurns，B 仍滞留在 live/prompting；后续 prompting 重置缓冲时又有丢失风险。

原用例把两轮全部放入初始空状态的一帧可以通过，拆成以上两帧就失败。这说明覆盖“批量处理”还不够，必须覆盖不同批次边界。

修复方向：完成准入使用随已接受事件推进的轮次身份，同时保留已有的错会话、旧完成事件保护。

### F6：序号缺口恢复绕过了文本队列 flush

位置：[handleSequenceGap](/D:/MyCodeBuddy/src/contexts/acp-connections-context.tsx:8572)、[流式快速接收路径](/D:/MyCodeBuddy/src/contexts/acp-connections-context.tsx:8825)。

文本可能已经推进 accepted/applied 序号，但还只在合并队列里。普通 attach 快照路径会先 flush；gap 恢复直接 HYDRATE_FROM_SNAPSHOT，没有执行同样的交接。

两个可复现结果：

- 队列中有 `hello `，恢复的 prompting 快照已经包含同一段；后续 flush 再追加，变成 `hello hello `。
- 队列中有 `hello `，恢复的完成快照是 connected 且 liveMessage 为 null；后续 flush 被 out-of-turn 保护丢弃，canonical 消费者没有收到这段文本。

修复方向：复用已有的 flush → hydrate 顺序，统一快照入口。还要明确完成快照之前已接受文本如何交接给本地历史，不能仅凭序号推进就认定内容已安全交付。

## 3. 代码审查发现的其他风险

以下已定位到具体分支，但本轮没有为每项新增独立失败测试，不能与上面 7 个运行结果混算。

### F7 / P1：只读抽屉卸载删除共享 runtime

[LiveTranscriptView cleanup](/D:/MyCodeBuddy/src/components/message/live-transcript-view.tsx:153) 无条件调用 [removeConversation](/D:/MyCodeBuddy/src/stores/conversation-runtime-store.ts:7853)，会删除整个会话 runtime，并取消相关读取与同步。

已有会话主标签和抽屉可能使用相同正数会话 ID。关闭抽屉因此会影响另一个仍在显示该会话的消费者；回读只能补回已持久化内容。现有抽屉测试验证了“关闭就删除”，没有证明多个消费者并存时安全。

建议把共享数据释放绑定到最后一个消费者离开或明确关闭会话，而不是任意一个视图卸载。

### F8 / P1：已绑定草稿跨分组重挂载后换了 runtime ID

[初始化 effectiveConversationId](/D:/MyCodeBuddy/src/components/conversations/conversation-session-surface.tsx:437) 使用 `conversationId ?? buildVirtualConversationId(...)`，没有复用标签里保留的 runtimeConversationId。

草稿首次运行使用负 ID，绑定 DB 后仍保留这个 runtime。分组迁移造成真实 remount 时，新的组件却会选正数 DB ID；旧组件为支持迁移而保留的本地回复还在负 ID 下，新组件没有继续读取它们。

建议复用已有稳定 runtime ID；若必须迁移，先完成状态转移再切换消费者。补测应验证迁入端回复，而不只是迁出端没有断连。

### F9 / P2：增量渲染模式下，只读视图没有对应投影 sink

[只读视图注册](/D:/MyCodeBuddy/src/components/message/live-transcript-view.tsx:122) 只有 canonical sink；[Provider](/D:/MyCodeBuddy/src/contexts/acp-connections-context.tsx:7418) 没有 transcript sink 就不更新增量投影；[消息列表](/D:/MyCodeBuddy/src/components/message/message-list-view.tsx:1870) 在增量模式下依赖该投影显示 live 内容。

因此只读抽屉独立显示时，runtime 有内容不代表渲染投影有内容。主会话 surface 注册了两类 sink，不能用主界面的测试证明只读界面也正常。

**这是 opt-in 风险，不是默认开启功能。** [streaming_performance.rs](/D:/MyCodeBuddy/src-tauri/src/acp/streaming_performance.rs:34) 的 legacy/default 配置关闭相关增量开关；不能直接认定会话 5095 命中了此路径。

### F10 / P2：多个只读消费者共用 key，后注册者覆盖前者

[registerLiveSinks](/D:/MyCodeBuddy/src/contexts/acp-connections-context.tsx:8350) 对 contextKey 使用 Map.set；只读桥接使用 connectionId 作为 key。第二个独立 viewer 覆盖第一个后，关闭第二个只会删掉当前注册，不会恢复第一个。

范围是多个独立只读宿主；主 surface 使用 tabId，同一个宿主只允许单个抽屉也会降低触发概率。需要消费注册具备独立身份及对称注销，或显式限制共享连接只能有一个桥接者。

### F11 / P2：后端生产完成入口没有 legacy 入口的 external_id 兜底绑定

[生产 internal 事件入口](/D:/MyCodeBuddy/src-tauri/src/acp/lifecycle.rs:221) 将完成事件交给 handle_turn_complete_internal；另一条 [legacy 完成分支](/D:/MyCodeBuddy/src-tauri/src/acp/lifecycle.rs:808) 有 bind_live_external_id 兜底。相关 Grok/Antigravity 测试走后者，不能证明前者执行了同样的修复。

条件是 SessionStarted/ConversationLinked 先前未成功绑定，完成时记录的 external_id 仍为空。此时状态可以完成，但之后找不到对应 transcript。详情读取有按目录、时间寻找 stale session 的补救，不过受时间窗口和候选歧义限制，并非必然成功。

正常绑定成功的会话不受此条件影响。应先给真实生产入口补后端测试，再决定如何复用绑定逻辑；本轮未运行 Rust 编译或测试。

### F12 / P2：gap 快照请求短暂失败会拆掉本地观察状态

[gap recovery catch](/D:/MyCodeBuddy/src/contexts/acp-connections-context.tsx:8652) 明知异常不等于连接死亡，仍清除本地 canonical/alias，并以 MAX_SAFE_INTEGER 恢复 ingestor，丢弃待恢复序列。

这避免了错误自动重连，却同时失去本地缓冲和后续事件路由。已有拒绝快照测试期望删除入口，没有验证已显示回复应保留。建议把“暂时不能恢复订阅”和“可以销毁已接受内容”分开处理。

## 4. 为什么多次修复后还会出现

不是所有旧修复无效，而是它们保护的入口和时序不一致：

- **身份与完整性混在一起。** 文本前缀、长度和时间戳被用来猜测“同一轮且已经完整”，对续答、重发及相同开头不可靠。
- **请求代次主要防读取覆盖读取。** 没有统一防止旧读取覆盖新产生的消息；新轮已经完成后，单看当前 streaming 状态也不够。
- **同一操作存在多条入口。** attach 与 gap 都恢复快照，但只有一条先 flush；legacy 与 internal 都处理完成，但兜底逻辑不同。
- **视图寿命与数据寿命混在一起。** 抽屉卸载删除共享数据，组件 remount 改变 runtime 身份。
- **测试缺少时序排列。** 正常事件序列、单消费者和单入口测试很多，但把落盘、完成、下一次发送、旧请求返回的顺序交换，就能绕过现有保护。

上一轮 in-flight 标记修复以及现有“完成后补读”“内部续答补读”“错会话完成保护”都有作用，但不能推出上述交接都安全。

## 5. 建议的修复顺序与验收条件

不建议为每个页面再加一组独立兜底，也不需要整体重写。按 systematic-debugging 定位共同边界，按 ponytail 原则优先复用已有代次、轮次身份和 flush 机制。

1. **先收紧 store 的破坏性提交与退役边界（F1–F4）。** 同轮身份成立且内容确实覆盖，才能退役本地副本；旧读不能删除读请求之后生成的内容。复用现有 generation 设施，避免再平行创建一套状态机。
2. **修事件事务交接（F5–F6）。** 按帧内已接受边界推进完成身份；统一快照恢复的 flush/hydrate 顺序。
3. **修共享生命周期（F7–F10）。** 保持 runtime key 稳定，明确最后一个消费者释放状态的边界，补齐只读增量投影。
4. **补后端真实入口与恢复失败保护（F11–F12）。** 先补针对性测试，不把 legacy 测试当生产路径覆盖。

核心验收约束：

- 一段已接受的回复，除用户明确删除外，只能被**同一轮且覆盖其内容**的版本替换。
- 同一有序事件序列，无论切成几帧，最终历史与完成状态应一致。
- 异步旧读无权删除读取之后出现的新轮、续答或完成回复。
- 关闭一个消费者不得破坏另一个消费者的回复。
- 同一个标签重挂载后应继续看到同一个 runtime。

回归矩阵至少覆盖：owner / viewer / 内部续答；持久化早于和晚于 complete；请求返回前新轮开始和已完成；跨帧多轮边界；gap prompting / completed 快照；主标签与抽屉并存；草稿绑定后跨分组 remount；增量渲染开关两种模式。

建议给 promotion、detail commit、retirement、隐藏分支补轻量诊断：记录会话/runtime/连接/轮次 ID、序号、代次、决策原因及块数/长度，不记录正文。现有 frontend-turn-trace 偏发送和首字显示，缺少最终交接与丢弃决策，是历史事故难以唯一归因的原因之一。

## 6. 复现材料与验证结果

材料在 [reply-loss-audit](/D:/MyCodeBuddy/docs/reviews/reply-loss-audit/repro.test.ts)。从仓库根目录执行：

```powershell
pnpm exec vitest run --config docs/reviews/reply-loss-audit/vitest.config.ts
pnpm exec vitest run --config docs/reviews/reply-loss-audit/acp-variants.config.ts
```

- 第一条运行真实 runtime store 的 4 个用例，**4 个失败**，分别对应 F1–F4。
- 第二条复用现有 ACP 集成夹具，只在内存中变换事件投递时序和快照恢复入口，**3 个失败**，对应 F5 与 F6 的两个分支。未修改原 ACP 测试或生产源码。
- 这 7 个测试断言的是期望行为，当前退出码 1 正是缺陷复现结果，不是验证通过。它们不进入默认测试收集范围。
- ACP 变体没有生成 source map，失败堆栈行号可能偏移；定位以用例名称和本报告的生产源码链接为准。

本轮其他验证记录：

- 相关 15 个测试套件：996 项中 995 通过，1 项超过默认 5 秒超时。
- 一次审计配置合并意外扩大了收集范围，已跑完全量前端测试：10,060 通过、15 跳过、5 失败。失败为当时加入的 4 个 runtime 审计用例，以及同一个 live-transcript-view 超时；之后已修正配置，只收集指定审计用例。
- `pnpm test src/components/message/live-transcript-view.test.tsx --testTimeout=20000` 单独诊断重跑：2 项通过，较慢一项约 6.1 秒。未修改默认超时配置，不能把之前全量结果描述成全绿，也未据此认定为产品逻辑缺陷。
- 上一轮的定向测试、lint 和 Next 构建通过属于上一轮记录，本轮没有重新构建桌面应用或验证部署。

## 7. 已有保护与结论边界

已检查的有效保护包括：取消后的 reconciliation 有 provider turn fence 和 cancelGeneration；空的持久化取消回复不会直接丢掉本地正文；EventIngestor 处理连续序号、重复与缺口；完成事件有错会话保护；后台回读有水位及前缀保护。普通标签切换也存在 keep-alive，不应把所有卸载都描述成必然丢失。

后端在状态锁内捕获 completion sidecar、关键事件 FIFO 能保护完成状态及 broker 时序，但不等于 durable 的完整回复备份。

本轮没有发现并证明虚拟列表本身删除回复，没有进行浏览器尺寸/滚动测量，也没有穷举所有代理解析器和所有平台的端到端时序。F7–F12 仍需对应测试确认触发边界。上述限制不影响“仍会出现类似问题”的结论：F1–F6 已由可运行的失败用例直接证明。
