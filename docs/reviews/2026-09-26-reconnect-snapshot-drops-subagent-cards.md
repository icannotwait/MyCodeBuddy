# 重连后子会话卡片显示不正确

状态：已修复。

修复：snapshot 投影不再按 tool call id 的字典序静默丢掉 live message 仍引用的调用。委派类调用（`delegate_to_agent` / `continue_delegation` / `get_delegation_status`，以及带 `codeg.delegation` 的调用）优先保留，其余引用按时间从新到旧填到 payload 上限；数字上限 128 只再裁剪没有被引用的调用。放不下的引用记入 `truncation.omitted_live_tool_refs`。前端 hydrate 不再把悬空 `tool_call_ref` 丢掉，而是留占位；`omitted_tool_calls > 0` 或仍有悬空引用时会读取磁盘，并且不会用 in-flight hide 把能补齐缺口的持久化回合整段藏住。磁盘上已有的工具卡按 id 并回去，避免和 live 各画一份。内存里的 session 仍是全量，投影可以有损，但有损时 UI 会标明并补齐。

下文是修复前的定位，数字和代码路径仍是 2026-09-26 会话 66 的现场记录。

结论：子会话文件和父会话的磁盘 transcript 都是完整的。重连或重新打开进行中的父会话时，界面用的是 live snapshot 的有损投影。这一轮 tool call 超过 128 个之后，投影按 tool call id 的字典序只保留 128 个；前端把对不上的 `tool_call_ref` 直接丢掉。冷启动又因为已经有 `liveMessage` 而不再读磁盘，所以界面上的委派卡会缺、会错位，正在跑的子会话也可能从线程里消失。

本文描述的是会话 66（Worktree subagent-driven 0.32.2 rewrite adaptation）在 2026-09-26 的现场，以及对应的代码路径。数字来自当时的 `acp_list_connections`、该连接的 session snapshot，以及 `get_folder_conversation` 对会话 66 和子会话 67 的解析结果。

## 用户看到的现象

父会话仍在同一轮回复里（用户消息是「继续」，后端状态是 `prompting`）。重连或通过 UI 重新打开之后：

- 线程里的 `delegate_to_agent` / `continue_delegation` 卡片不完整。
- 若干已经完成的子会话无法再从父线程点进去。
- 当时正在执行的 Task 7（子会话 84）不在父线程的工具卡片里。
- 子会话自己的 transcript 并没有丢。直接按会话 id 读磁盘，内容是对的。

`acp_list_connections` 里这条连接的对外 status 当时是 `connecting`，但 session snapshot 的 status 是 `prompting`，`event_seq` 停在 56701。内容错乱不是「连接还停在 connecting、事件没到」，而是 prompting 状态下的 snapshot 投影本身就不完整。

## 现场数据

父连接对应会话 66，live message 从 `2026-09-26T09:41:09.851Z` 开始，一直没有 `TurnComplete`。

| 来源 | 结果 |
| --- | --- |
| 内存里的 live message | 173 个 `tool_call_ref` |
| snapshot 的 `active_tool_calls` | 128 条 |
| `truncation.omitted_tool_calls` | 45 |
| 磁盘 `get_folder_conversation(66)` | 9 个 turn，16 次 `delegate_to_agent` |
| 当前 assistant turn `grok-turn-8` | 457 个 block，时间 `2026-09-26T09:41:11Z` |
| 子会话 67 | 2 个 turn，237 次 tool call，委派 meta 正确 |

被投影丢掉、因而前端不会画出的委派调用：

| tool call id 后缀 | 工具 | 子会话 | 磁盘上的状态 |
| --- | --- | --- | --- |
| `…9d4f-60` | `delegate_to_agent` | 72 | completed |
| `…c9bf96-77` | `delegate_to_agent` | 74 | completed |
| `…a75bcc6-86` | `continue_delegation` | 73 | completed |
| `…f10848-112` | `delegate_to_agent` | 77 | completed |
| `…acc5c350-139` | `delegate_to_agent` | 79 | completed |
| `…13514f4-147` | `delegate_to_agent` | 80 | completed |
| `…4b078e8-22` | `delegate_to_agent` | 82 | completed |
| `…f667b2-42` | `delegate_to_agent` | 84 | running（Task 7） |

45 条 omissions 里还包括 3 次 `get_delegation_status`。其余是普通工具调用。留下的 128 条是 id 字典序最小的那一批，不是「最近 128 次」，也不是「所有委派调用」。所以有的子会话还在，有的中间回合和当前回合一起消失，看起来像随机坏掉。

`active_delegations` 里当时仍有子会话 84。它只表示「这个委派还在跑」，不能把已经从 `active_tool_calls` 里裁掉的启动卡补回来。已完成的子会话本来就不会留在 `active_delegations` 里，卡片一旦被裁掉，snapshot 里就没有任何入口再指向它们。

## 原因

一条链路，三处各自正确、叠在一起就错。

### 1. Snapshot 按 id 序截断 tool call，不截断引用

`SessionState.active_tool_calls` 是 `BTreeMap<String, ToolCallState>`（`src-tauri/src/acp/session_state.rs`）。`BTreeMap` 按 key 升序遍历。

默认投影上限是 128 个 tool call、3 MiB payload：

```rust
// SnapshotLimits::default
tool_calls: 128,
payload_bytes: 3 * 1024 * 1024,
```

`SessionState::to_snapshot_with_limits` 对 map 做 `.values().take(limits.tool_calls)`，然后把差额写入 `truncation.omitted_tool_calls`。这是有损投影，不改内存里的 session：测试明确断言投影之后源 `active_tool_calls` 仍然是全量。后端自己还拿着全部 173 条。

同一函数里的 `project_live_message` 会把 live message 里的每一个 `ToolCallRef` 原样放进 snapshot。正文里的 173 个引用都在，旁边的 tool 表只有 128 条。多出来的引用变成悬空 id。

128 是条数上限，和 3 MiB 的正文预算是两套限制。这次会话 66 的缺口是条数（`omitted_tool_calls = 45`）。即便将来把条数放大，超大 input/output 仍可能被 payload 预算截成 `[truncated]`，那是另一个显示损坏，不是这次卡片消失的原因。

`omitted_tool_calls` 会序列化进 snapshot，但前端没有任何读取点。用户看不到「这一轮还有 45 个工具没画出来」。

### 2. 前端把悬空引用当成不存在

`src/lib/snapshot-denormalize.ts` 的 `denormalizeBlock` 在 `tool_call_ref` 分支里用 `toolMap.get(wire.tool_call_id)`。map 里没有就返回 `null`，`denormalizeLiveMessage` 再把 `null` 滤掉。

注释写的是「下一次 tool_call 事件会把它建回来」。这对还在增量推送的新调用成立。重连 hydrate 不会重放已经进过 snapshot 的历史 `tool_call`。被裁掉的调用没有下一次事件，卡片就一直不存在。

委派卡的 `meta.codeg.delegation`（子会话 id、状态）挂在 `ToolCallState` 上，不在 ref 本身上。ref 被丢掉等于这张卡的身份和结果都没了。

### 3. 冷启动把这份不完整的 live 当成整轮回复，不再读磁盘

`src/stores/conversation-runtime-store.ts` 的 `fetchDetail` 在 session 已经有 `liveMessage`、`optimisticTurns` 或 `localTurns` 时直接 return。注释是 “Skip fetch if session has active data (ongoing conversation)”。它假定 live 缓冲就是这一轮的完整内容。

重连时序是：

1. attach 的 `onSnapshot` 先 `HYDRATE_FROM_SNAPSHOT`，写入截断后的 `liveMessage`。
2. `recoverAfterSnapshotHydrate` 只有在「冷 hydrate 时 store 里已经有残留 live runtime」或 background revision 变化时才 `refetchDetail(..., { preserveLive: true })`。一次全新打开，store 是空的，两个条件都不成立。
3. 会话页随后调用 `fetchDetail`。此时 `liveMessage !== null`，函数返回，磁盘 tail 根本不会请求。

于是 UI 唯一的数据源就是那份 128 条的投影。磁盘上那份 457 block 的 `grok-turn-8` 不会出现。

`computeHistoricalTimeline` 里还有一句对应的假设：hide 持久化中的 partial reply 是安全的，因为 “the live stream carries the full reply (the attach snapshot is built atomically and includes it)”。在 tool call 超过 128 之后，这句话不成立。会话 66 这次没有走到这条 hide 分支（见下文「这次没有发生的事」），但只要 `in_flight_user_turn_id` 对上了，已加载的磁盘副本也会被藏起来，界面同样只剩截断 live。

## 为什么刚好是这些子会话

tool call id 形如 `call-<uuid>-<序号>`。`BTreeMap` 比的是整串字符串，UUID 的十六进制比末尾序号权重大得多。`take(128)` 保留的是字典序最小的 128 个 id，和调用先后无关。

所以：

- 较早完成、但 id 排序靠后的子会话会消失。
- 当前这一发 `call-eccee5aa-…-42`（子会话 84）因为 `eccee5aa` 排序靠后，也在被丢掉的 45 条里。
- 排序靠前的委派调用还在，线程看起来像「有的子 agent 能看，有的不能」。

子会话 84 自己的 snapshot 当时大约 54 个 tool call，低于 128，它自己的 live 投影不是这次截断的受害者。打不开的是父线程上的入口，不是 84 的会话文件。

## 磁盘为什么是对的

Grok 把这一轮写进了会话文件。`get_folder_conversation` 走解析器，不走 snapshot 投影：

- 会话 66 有完整的 9 个 turn。
- 16 次 `delegate_to_agent` 都在，每张卡的 `meta.codeg.delegation` 指向上面的子会话 id。
- 抽查子会话 67：2 个 turn、237 次 tool call，内容可解析。

丢失只发生在「进行中的连接 → snapshot 投影 → 前端 denormalize」这条 UI 路径。Turn 结束后 `liveMessage` 清掉，同一轮改从磁盘渲染，这些卡片会重新出现。这也能解释成「重连之后、这一轮还没结束时才看不对」。

## 这次没有发生、但容易和它对上的路径

这些路径会让「重连后内容不对」更严重，但会话 66 当时的父页面不是被它们弄丢的。

**进行中的磁盘副本被藏起来。** 非委派会话在 `detail.in_flight_user_turn_id` 能对上当前 user turn、且 `liveMessage` 还在时，`computeHistoricalTimeline` 会藏起该 prompt 之后的持久化 assistant turn，避免和 live 拼成双份。会话 66 的 user turn `grok-turn-7` 时间戳是 `2026-09-26T09:41:06Z`，live message 从 `09:41:09Z` 开始。后端的 recency gate 要求持久化 user turn 不早于 pending message 的 `started_at`，这次对不上，`in_flight_user_turn_id` 为空，hide 没有触发。若 detail 已经在内存里（断线前标签一直开着），重连后会是「完整磁盘 turn + 截断 live turn」两段拼在一起，而不是只剩截断 live。全新打开则连磁盘都不取，只剩截断 live。

**委派子页的 `liveOwnsActiveTurn`。** 只读子页会把 live 当成当前一轮的权威回复，并剥掉最后一个 user turn 之后的持久化 assistant 内容。注释已经改成只剥当前一轮，不再从第一个 assistant turn 起把更早的回合都删掉。子页自己的 tool call 不超过 128 时，当前一轮的 live 投影是全的；超过之后，子页会碰到和父页一样的洞。会话 66 的投诉点是父线程，不是这条。

## 后果

- 父线程不能当作这一轮的完整操作记录。委派是否发出、子会话 id、完成状态都会缺。
- 已完成且 id 落在截断区的子会话，没有卡片，也不在 `active_delegations` 里，UI 没有入口。
- 仍在跑的子会话可能只剩状态栏/委派列表里的一条记录，线程里没有对应的启动卡和后续 `get_delegation_status`。
- 已经显示出来的卡片是字典序的一个子集，不能靠「还在的那些」推断时间线。
- 内存 session 仍是全量的。下一次针对某个被丢掉 id 的增量 `tool_call` / `tool_call_update` 可以只把那一条补回来，其余 omissions 不会因此恢复。

## 修复时要保持的不变量

本文不改代码。若要修，投影和前端不能再假设「snapshot 里的 live message 等于这一轮的全部 tool call」：

- 条数上限不能按 `BTreeMap` 的 key 序静默丢调用。至少委派类调用，或当前 turn 里被 `tool_call_ref` 指到的调用，必须能在投影里对上；放不下就明确分页或让 UI 改读磁盘，而不是删 ref 对应的卡。
- `denormalizeBlock` 不能在「没有后续事件」的 hydrate 路径上把悬空 ref 当成可忽略。
- `fetchDetail` 不能因为存在 `liveMessage` 就跳过磁盘。live 只覆盖它真正带齐的那一部分；`omitted_tool_calls > 0` 时必须把持久化 turn 拉回来，并且不能被 in-flight hide 整段藏住。
- `omitted_tool_calls` 如果仍然可能大于 0，UI 应把它当成可见的不完整信号，而不是只留在 snapshot 字段里。
