# Codeg 崩溃、逻辑与严重性能审查（终稿）

日期：2026-08-20  
范围：`src-tauri/src`（Rust 桌面/服务器/MCP）、`src`（前端；不含 `src-tauri/vendor`）。  
方法：Wave 0 全量模式扫描 + Waves 1–6 深审 + 对照调用链/默认规模复核。不改代码、不跑 `cargo test` / Insights；性能项均为 **静态推断**。

**P0 复核（2026-08-20）：** 无开放 P0。生产路径 `panic!` 几乎全在 `#[cfg(test)]`；agent JSON 解析走 skip/`map_err`；鉴权与 upload jail 默认 fail-closed。

**P1 复核（2026-08-20）：** 初审 8 条 P1。保持 P1 **5**，降 P2 **3**（桌面 Web 的 ChatChannel 分身、连接 map 锁跨 await、session/load 无界 drain）。

---

## Counts

| 严重度 | 崩溃 | 逻辑 | 性能 | 合计 |
|---|---:|---:|---:|---:|
| P0 | 0 | 0 | 0 | **0** |
| P1 | 0 | 4 | 1 | **5** |
| P2 | 0 | 6 | 11 | **17** |
| **开放合计** | **0** | **10** | **12** | **22** |

按波次（复核后开放）：

| 波次 | P1 | P2 | 关闭（本波猎项） | 开放 |
|---|---:|---:|---:|---:|
| Wave 1 Core 生命周期 | 0 | 3 | 11 | 3 |
| Wave 2 ACP 运行时 | 2 | 7 | 13 | 9 |
| Wave 3 Web/WS/传输 | 0 | 1 | 8 | 1 |
| Wave 4 终端/MCP/委托 | 1 | 1 | 10 | 2 |
| Wave 5 前端热路径 | 2 | 2 | 13 | 4 |
| Wave 6 更新/备份/频道 | 0 | 3 | 12 | 3 |
| **合计** | **5** | **17** | — | **22** |

严重度口径：

- P0：预期日常使用可进程崩溃、死锁或持续不可用
- P1：日常路径很可能错/残留进程/明显 hitch
- P2：热路径模式错误，规模上去后会爆，当前默认规模可能还能忍

无开放 P0。仍为 P1 的五项：

1. `[P1][逻辑] CleanupGuard 按 connection_id 删除替换连接 — connection.rs:541`
2. `[P1][逻辑] Connecting 期用户断开不 abort 8MiB driver — manager.rs:6099`
3. `[P1][逻辑] ACP terminal/create 子进程在退出后残留 — terminal_runtime.rs:333`
4. `[P1][逻辑] preserveLive 详情回拉复制已 promote 的回合 — acp-connections-context.tsx:3888`
5. `[P1][性能] 每个 live 事件对全部 tool raw_input JSON.parse — conversation-runtime-store.ts:1257`

---

## 总表（复核后，仅开放项）

| Sev | 类 | 标题 | 位置 | Wave |
|---|---|---|---|---|
| P1 | 逻辑 | CleanupGuard 按 connection_id 删除替换连接 | `src-tauri/src/acp/connection.rs:541` | 2 |
| P1 | 逻辑 | Connecting 期用户断开不 abort driver | `src-tauri/src/acp/manager.rs:6099` | 2 |
| P1 | 逻辑 | ACP `terminal/create` 子进程退出后残留 | `src-tauri/src/acp/terminal_runtime.rs:333` | 4 |
| P1 | 逻辑 | `preserveLive` 详情回拉复制已 promote 的回合 | `src/contexts/acp-connections-context.tsx:3888` | 5 |
| P1 | 性能 | 每个 live 事件对全部 tool `raw_input` JSON.parse | `src/stores/conversation-runtime-store.ts:1257` | 5 |
| P2 | 逻辑 | 桌面内嵌 Web 的 ChatChannelManager / WebServerState 是新实例 | `src-tauri/src/web/mod.rs:817` | 1 |
| P2 | 逻辑 | Supervised Docker SIGTERM 不 drain ACP 子进程 | `src-tauri/src/supervise.rs:59` | 1 |
| P2 | 性能 | 递归 workspace watch 几乎不过滤 `node_modules`/`target` | `src-tauri/src/workspace_state/mod.rs:1385` | 1 |
| P2 | 性能 | 连接 map 锁跨 SessionState `.await`（pop-out / rebind） | `src-tauri/src/acp/manager.rs:7483` | 2 |
| P2 | 性能 | `session/load` replay drain 无总时限 | `src-tauri/src/acp/connection.rs:5978` | 2 |
| P2 | 性能 | 工具 input 分片在写锁下 O(n²) 重解析 | `src-tauri/src/acp/session_state.rs:2089` | 2 |
| P2 | 性能 | 邮箱满时每个事件 spawn 无界 overflow waiter | `src-tauri/src/acp/internal_bus.rs:173` | 2 |
| P2 | 性能 | Snapshot 克隆无上限 live 工具图（含多 MB 图） | `src-tauri/src/acp/session_state.rs:189` | 2 |
| P2 | 逻辑 | Default attach 忽略 agent 返回的不匹配 session id | `src-tauri/src/acp/session_attach.rs:101` | 2 |
| P2 | 性能 | 过期 Cline/Gemini `external_id` 回退全量 list | `src-tauri/src/commands/conversations.rs:1521` | 2 |
| P2 | 性能 | UI 线程 `JSON.parse` 无上限 WS snapshot | `src/lib/transport/web-transport.ts:492` | 3 |
| P2 | 逻辑 | `codeg-mcp` 父进程 PID 轮询在 Windows 上可遇 PID 复用 | `src-tauri/src/acp/delegation/parent_watcher.rs` | 4 |
| P2 | 性能 | Owner `localTurns` 在整个开 Tab 会话内只增不减 | `src/stores/conversation-runtime-store.ts:2378` | 5 |
| P2 | 性能 | 展开的 Read/Write 卡片按行挂 DOM | `src/components/message/content-parts-renderer.tsx:1497` | 5 |
| P2 | 性能 | Telegram/Weixin HTTP/JSON 错误无退避，可能空转 POST | `src-tauri/src/chat_channel/backends/telegram.rs:273` | 6 |
| P2 | 逻辑 | Lark `connect_async` 无超时、不响应 shutdown | `src-tauri/src/chat_channel/backends/lark.rs:295` | 6 |
| P2 | 逻辑 | git credential 日志打印完整 remote（可能含 token） | `src-tauri/src/git_credential.rs:532` | 6 |

---

## 崩溃

本轮 **无开放崩溃项**。生产 `unwrap`/`expect`/`panic!` 密度高的文件（`connection.rs`、`manager.rs`、`lifecycle.rs`、`continuation/store.rs`）复核后主体在测试模块。agent JSONL 坏行 skip。Mutex poison 多处 `into_inner` 恢复。

残留：Release 构建里 `run_connection` 仍在单条 8MiB OS 线程上；更深的 select 嵌套可能再次撑破栈（已有 `recursion_limit` + 专用线程缓解，见 `lib.rs:8`、`connection.rs` 大栈注释）。不升 P2：当前缓解在默认路径有效。

---

## 逻辑

### [P1][逻辑] CleanupGuard 按 connection_id 删除替换连接 — `src-tauri/src/acp/connection.rs:541`

**场景：** 同一 `connection_id` 上 teardown-then-respawn（Codeg-route `AllowedFallback`：`manager.rs:2398` 先 `teardown_unexposed_attempt` 再插入替换；用户取消 Connecting 后立刻重连）。`ConnectionCleanupGuard::drop` 总是 `spawn`：先按 incarnation 清 lease，再 `connections.lock().await.remove(&connection_id)`，**没有** incarnation CAS。

**为什么错：** `disconnect_with_origin` 已经用 incarnation CAS 摘掉旧条目（`manager.rs:6083-6088`）。旧 driver 稍后 Drop，迟到的 remove 会把 **新** `AgentConnection` 删掉。UI 以为已连上，map 已空，子进程可能仍在。

**建议：** 与 disconnect 相同的 incarnation CAS；map 里已是更新 incarnation 则 no-op。

**默认规模：** 一次共享会话的路由 fallback 或「连上即取消再连」。不是每次启动，窗口是 Drop 任务与插入替换的交错。

---

### [P1][逻辑] Connecting 期用户断开不 abort driver — `src-tauri/src/acp/manager.rs:6099`

**场景：** 冷启动 agent，Initialize 最长 60s（`connection.rs:5401`）。用户取消/关会话。

**为什么错：** 生产 disconnect 摘 map 后只 `timeout(200ms, control_tx.send(Disconnect))`，**从不** `task_abort.abort()`。`control_rx` 要到 `run_conversation_loop` 才 select；Initialize 是裸 `timeout(60s, send_request)`。未暴露尝试的 `teardown_unexposed_attempt` **已经** abort-first（`manager.rs:3669-3672`），用户断开没有走这条。UI 已断开，8MiB 线程 + CLI 继续跑；立刻重连会再起一个 agent。

**建议：** 与 unexposed teardown 一样：先 abort driver，Disconnect 只作尽力 drain。

**默认规模：** 桌面连上即取消。多一个 CLI 最多约 60s；重试则两个。

---

### [P1][逻辑] ACP `terminal/create` 子进程退出后残留 — `src-tauri/src/acp/terminal_runtime.rs:333`

**场景：** Agent 用 ACP `terminal/create` 拉起 `npm run dev` / `cargo test` / watcher。用户退出 Codeg。这些进程是 **Codeg 的子进程**，不是 agent CLI 的孩子。

**为什么错：** 模块明确「tokio drop 不杀进程，codeg 从不设 `kill_on_drop`」。退出路径 `lib.rs:1691-1700` 只 `TerminalManager::kill_all()`（UI PTY）+ `disconnect_all`（500ms 后 `kill_tree` **agent PID**）。`release_all_for_session` 只在 connection loop 来得及 unwind 时运行；`kill_command` 只保证上报时限，owner 任务随进程退出会 drop 未杀的 `Child`。Windows 父进程退出不杀子进程。

**建议：** `kill_on_drop(true)`；quit 时扫 `TerminalRuntime` 全表；或把 ACP 终端放进 Job Object / 同一 kill tree。

**默认规模：** 每个 ACP 会话都会 `terminal/create`。一条长驻命令 + 退出即中招。Docker/服务器 SIGTERM 路径更短（见下条 P2）。

---

### [P1][逻辑] `preserveLive` 详情回拉复制已 promote 的回合 — `src/contexts/acp-connections-context.tsx:3888`

**场景：** `background_activity` 且 `detail_refetch` 时 **总是** `refetchDetail(..., { preserveLive: true })`，含回合已 idle。`FETCH_DETAIL_SUCCESS`（`conversation-runtime-store.ts:2066`）保留 `localTurns`（id 如 `live-{cid}-{msg}`），同时用 parser id 替换 `detail.turns`。

**为什么错：** `dedupeTimelineByRoleAwareId` 只合并 **相同 role+id**。Owner 会话 `liveOwnsActiveTurn` 为 false，**不会**剥离已持久化的 assistant。时间线变成两条 assistant，随后 `mergeConsecutiveAssistantTurns` 拼成一个气泡、内容重复。代码自己也承认 promote→refetch 窗口会短暂双份（`:4458-4460`）；`detail_refetch` 把窗口变成「后台任务落地后的确定性路径」。

**建议：** 仅 `status === prompting` 时 `preserveLive`；owner 在 watermark 覆盖后丢掉已 promote 的 `localTurns`；或按 transcript offset 而不是 live id 去重。

**默认规模：** 1 个流式 agent 打 Task/后台类工具。纯文本回合打不中。

---

### [P2][逻辑] 桌面内嵌 Web 的 ChatChannelManager / WebServerState 是新实例 — `src-tauri/src/web/mod.rs:817`

**场景：** 桌面打开 Web 服务（设置或 `auto_start`）。HTTP 走第二份 `AppState`。`connection_manager` / `terminal_manager` / delegation 都 `clone_ref`，`chat_channel_manager` 却是 `default_chat_channel_manager()`（`ChatChannelManager::new()`），`web_server_state` 是注释写「handlers 不用」的 placeholder。

**为什么错：** HTTP `connect`/`disconnect`/`get_status` 看不到桌面已连的 Telegram/Lark。`get_web_server_status` / `stop_web_server` 读 placeholder（`running=false`），HTTP 停不掉真 listener。

**建议：** `app.state::<ChatChannelManager>().clone_ref()`；status/stop 用桌面那份 `WebServerState`。

**默认规模：** 缺 metadata 时 `auto_start` 为 false，不是首次启动必经。Web 服务一旦打开则 100% 错。二次复核由 P1 降 P2。

---

### [P2][逻辑] Supervised Docker SIGTERM 不 drain ACP 子进程 — `src-tauri/src/supervise.rs:59`

**场景：** 默认镜像 `CMD ["codeg-server", "--supervise"]`。`docker stop` 给 PID 1 SIGTERM。

**为什么错：** Supervisor 转发 SIGTERM 后，worker 带着 `TERMINATING` 退出即 `process::exit(0)`。worker 的 `axum::serve` 没有 `with_graceful_shutdown`，到不了桌面那条 `disconnect_all`。ACP CLI 被 reparent，再被容器拆掉。DB 行可停在 `in_progress`，启动有 `reconcile_running_on_startup`（fail-closed），所以不是持续不可用。

**建议：** worker 捕 SIGTERM → `disconnect_all` 再退出；supervisor 先 reap 孤儿。

**默认规模：** Docker 是服务器默认。一次在途 agent 即中。不升 P1：启动 reconcile 能恢复可用性。

---

### [P2][逻辑] Default attach 忽略 agent 返回的不匹配 session id — `src-tauri/src/acp/session_attach.rs:101`

**场景：** 日常重开（`SessionAttachMode::Default`）。resume/load 返回非空 id ≠ 会话 `external_id`。`ResumeExistingOnly` 会拒；Default 仍 `Emit { expected }`，connection 用 **旧** id 建 `NewSessionResponse`（`connection.rs:5727`）。

**为什么错：** 线上 session 是 B，DB/lifecycle 仍是 A。省略 `sessionId` 按设计算匹配（常见 ACP）；**带着**不同 id 回来才是洞。

**建议：** `actual` 存在且不同时，Default 应拒绝或改写身份，与 ResumeExistingOnly 一致。

**默认规模：** 多数 agent 省略 `sessionId` 则不中；返回新 id 的 agent 会错。

---

### [P2][逻辑] `codeg-mcp` 父进程死亡 backstop 在 Windows 上可遇 PID 复用 — `src-tauri/src/acp/delegation/parent_watcher.rs`

**场景：** 默认注入 `codeg-mcp --parent-pid`。看门狗每 2s `OpenProcess(pid)`。Codeg 被 End Process / 崩溃后，Windows 可能复用该 PID，companion 把陌生人当父进程，永不退出。

**为什么错：** ready-lease socket 能看见 Codeg 死（`wait_until_closed`），但该任务不在 stdin/`select!` 里。之后依赖 agent stdin EOF（agent CLI 若也活着则失败）或这次 PID 轮询。正常退出仍会 `kill_tree` agent（通常带上 companion）。这是 **崩溃 / End Process / hung-agent** 路径。

**建议：** 把 lease EOF 放进同一 `select!`；Windows 用 wait 句柄而不是轮询 PID；启动时打开父进程句柄并一直持有。

---

### [P2][逻辑] Lark `connect_async` 无超时、不响应 shutdown — `src-tauri/src/chat_channel/backends/lark.rs:295`

**场景：** `fetch_ws_url` 有超时/可中止；随后 `connect_async(&ws_url)` 不在与 `shutdown_rx` 的 `select!` 里，也无超时。握手挂住则任务停在 Connecting，`stop()` 打不断。

**建议：** `timeout` + `select!` shutdown；失败进 Error 并退避。

**默认规模：** 一个 Lark 频道。飞书通常很快；卡住会粘住。

---

### [P2][逻辑] git credential 日志打印完整 remote — `src-tauri/src/git_credential.rs:532`

**场景：** `try_inject_for_repo` 在 warn/info 打 `git remote get-url` 原文（`skipping non-HTTPS URL` / `injecting credentials for {}`）。可以是 `https://<token>@host/...`。

**为什么错：** token 进日志。stdout 上的 `password=` 是 git credential 协议，没问题。生产 unwrap 在测试里。

**建议：** 日志只打 host/path，strip userinfo。

---

## 性能

凡性能项均为 **静态推断**。默认规模：1 用户、1–3 个打开会话、流式 agent。

### [P1][性能] 每个 live 事件对全部 tool `raw_input` JSON.parse — `src/stores/conversation-runtime-store.ts:1257`

**规模假设：** 一次编码回合里已完成 1+ 个 Write/Edit，payload 数十到数百 KB；随后文本 token 继续流。

**场景：** `registerLiveSinks` → `SET_LIVE_MESSAGE`（增量 UI 关着也走）。每次：`kimiTodoWriteEntries` 对每个 `tool_call` `JSON.parse`（`plan-parse.ts:210`）；`inferLiveToolName` → `isCodexCollabInput` + `inferFromInput` → 再 `JSON.parse`（`tool-call-normalization.ts:229`）。`computeTimeline` / `syncToolGroups` 再走一遍。`LiveTurnStatsBanner` 用新 `{ type: "tool_call", info }` 包一层，打穿本该跳过的 WeakMap。

**为什么卡：** Write 大文件之后，**每个后续 token** 对同一份大 JSON 解析多次。不是「每帧」，但是流式主线程 hitch。日常写代码路径。

**建议：** 按 `tool_call_id` + `raw_input` 身份缓存 parse 结果；banner 不要克隆出新对象；Kimi 探测先看短前缀/title 再 parse。

---

### [P2][性能] 递归 workspace watch 几乎不过滤重目录 — `src-tauri/src/workspace_state/mod.rs:1385`

**场景：** 打开文件夹默认 `wants_tree_git=true`，`notify` `RecursiveMode::Recursive`。通道有界（2048）+ `try_send` + overflow 旗，**不是**无界 mpsc。忽略列表几乎只有 `__pycache__` 和部分 `.git` / tmp。`node_modules`、`target`、`.next` 仍灌 watcher。overflow 最多每 1.5s 合成一次 flush（`git status` + 深度 2 树，树走 `spawn_blocking`）。

**建议：** watch 时按 gitignore / 已知重目录跳过。

**默认规模：** 每个打开的根。默认一个 JS/Rust 仓通常可忍；`npm install` 时 hitch。

---

### [P2][性能] 连接 map 锁跨 SessionState `.await` — `src-tauri/src/acp/manager.rs:7483`

**场景：** 流式回合中 pop-out / rebind owner 窗口。`rebind_*` 持 `self.connections.lock()` 同时 `state.write().await`。`emit_with_state` 持该写锁做 `apply_event`。不是经典 A-B/B-A 死锁（emit 不拿 map），但是进程级 ACP hitch。

**建议：** map 锁下只 clone `Arc`，放下锁再改 state；rebind 用 incarnation CAS。

**默认规模：** 少数连接 + 一个流式父。二次复核由 P1 降 P2：主窗口日常不走 pop-out。

---

### [P2][性能] `session/load` replay drain 无总时限 — `src-tauri/src/acp/connection.rs:5978`

**场景：** resume 缺失/失败但 load 成功（Cline/自定义/无 resume；Claude 通常走 resume 跳过）。`while timeout(100ms, session.read_update())` 抽干历史通知。100ms 只结束 **更新间隔**，不是总工作量。大 JSONL replay 堵住 8MiB 连接线程，UI 迟迟不到 Connected。

**建议：** 限制条数/墙钟；失败则闭到 snapshot/parser 历史。

**默认规模：** 长会话在 load 型 agent 上重开。二次复核由 P1 降 P2：默认 Claude/Codex resume 不走这条。

---

### [P2][性能] 工具 input 分片在写锁下 O(n²) 重解析 — `src-tauri/src/acp/session_state.rs:2089`

**场景：** Agent 对流式 `raw_input` 大 Write。每片 `push` 再 `join("")` + `serde_json::from_str`，且 `emit_with_state` 持写锁。一次给完整 JSON 则没事；大文件多片段会爆。

**建议：** 运行缓冲 + 对象闭合后再 parse；限制 chunk 数。

---

### [P2][性能] 邮箱满时每个事件 spawn 无界 overflow waiter — `src-tauri/src/acp/internal_bus.rs:173`

**场景：** SQLite 卡住；worker 邮箱（64）或 critical 车道（1024）或 broker-tool（1024）满。TurnComplete/SessionStarted 在 Full 时 `tokio::spawn` waiter，critical send **无超时**。有意避免丢 CAS，但 N 个卡住连接 × M 个 critical 事件任务无界。桌面默认几个会话通常装得下。

**建议：** 每个 mailbox 一个 overflow waiter，或有界 overflow 队列 + 指标。

---

### [P2][性能] Snapshot 克隆无上限 live 工具图 — `src-tauri/src/acp/session_state.rs:189`

**场景：** 回合中途 WS/桌面 attach。`to_snapshot` 克隆全部 `active_tool_calls`（含 base64 图）、整份 `live_message`、全部 failure/tombstone map。`session_failures` / `tool_watchdog_max_versions` 连接存活期间不剪。

**建议：** snapshot 省略或封顶 image bytes；tombstone LRU。

**默认规模：** 一次生图/大截图工具 → attach hitch。与 Wave 3 前端 parse 叠加。

---

### [P2][性能] 过期 Cline/Gemini `external_id` 回退全量 list — `src-tauri/src/commands/conversations.rs:1521`

**场景：** 打开 Cline/Gemini 行，文件对不上 `external_id`。`ConversationNotFound` → 同一 `spawn_blocking` 里 `parser.list_conversations()` 再 `get_conversation`。不在 UI 线程，但线程池一条线程扫完整店。

**建议：** 按 cwd+time 索引，不要全量 parse。

---

### [P2][性能] UI 线程 `JSON.parse` 无上限 WS snapshot — `src/lib/transport/web-transport.ts:492`

**场景：** 每个入站 WS 文本帧在 `onmessage` 同步 `JSON.parse`（`remote-desktop-transport.ts` 同构）。无字节上限、无 worker。Attach snapshot 带无界 `live_message` + `active_tool_calls`（文档已写多 MB 图）。冷 attach、lag 重附、`__ready__` `reattachAll` 都会走。

**默认规模：** 一个浏览器 tab / 一个远程桌面窗。普通编码回合远小于多 MB；生图路径才是放大。不升 P1。

---

### [P2][性能] Owner `localTurns` 在整个开 Tab 会话内只增不减 — `src/stores/conversation-runtime-store.ts:2378`

**场景：** `completeTurn` 故意不 refetch。`conversation://changed` 只同步纯 viewer。历史时间线 = windowed `detail.turns`（约 120）**加上**本会话每一次 promote。有虚拟化，不是冻结；长编码会话把整段 in-session transcript 留在 Zustand。

**建议：** watermark 覆盖后丢掉 local；或按 round 封顶。

---

### [P2][性能] 展开的 Read/Write 卡片按行挂 DOM — `src/components/message/content-parts-renderer.tsx:1497`

**场景：** `FileContentLines` `content.split("\n")` 后每行一个 div。外壳 `max-h-[420px]`，整表仍 commit。折叠会卸 body。用户点开大 Write/Read 才中。

**建议：** 窗口虚拟化，或只渲染可见行。

---

### [P2][性能] Telegram/Weixin HTTP/JSON 错误无退避 — `src-tauri/src/chat_channel/backends/telegram.rs:273`

**场景：** `getUpdates` 只在传输 `Err` 时睡 5s。HTTP 409/401/429（仍是 `Ok(resp)`）、`ok:false`、JSON 失败会立刻再 POST。Weixin（`weixin.rs:716`）parse 失败同样不睡。

**建议：** 非 200 / 无 `result` 走指数退避；409 当冲突，停掉重复 poller。

**默认规模：** 一个 bot。第二 poller、吊销 token 或 HTML 错误页会空转直到 API 恢复。

---

## 已关闭（猎项，复核后不开放）

### Wave 1 — Core

| 项 | 结论 |
|---|---|
| Mutex poison 在 shutdown unwrap 崩溃 | 多数 `into_inner`；`WebServerState.lock().unwrap()` 临界区只赋 `Option`/`String`，不够日常 panic |
| workspace 无界 channel | 容量 2048 + `TrySendError::Full` |
| Supervise PID 复用杀错进程 | Unix 在重启延迟前把 `WORKER_PID=0`；Windows 不按 pid kill |
| 二次 GUI 启动双开 | Release `tauri_plugin_single_instance` 置顶 |
| EventEmitter 漏 ACP emit / 订户泄漏 | 全局 `acp://event` 有意去掉；WS `Lagged` 已处理 |
| SQLite 日常 SQLITE_BUSY 锁死 | WAL + `busy_timeout=5000` + max 5 |

### Wave 2 — ACP

| 项 | 结论 |
|---|---|
| 生产 `unwrap` 解析 agent JSON 进程崩溃 | parse 走 `map_err` / skip line |
| Idle sweep 杀掉 Prompting / 权限等待 / 后台 | `sweep_idle` 跳过这些；shared Ready 有 lease + blocker |
| ResumeExistingOnly 附到错误 session | 不匹配则拒 |
| Parser 遇坏 JSONL panic | Claude/Codex/Grok/Pi skip |
| 请求线程同步扫巨大 JSONL | `list_conversations_core` 走 `spawn_blocking` |
| `active_delegations` 只增不减 | live set；`DelegationCompleted` 清除 |
| ACP `fs/*` 无界读 | 16MiB 文件 / 2MiB 响应上限 |
| `prompt_hydration` 无界 base64 | 合计 64MiB + 并发 2 |
| transcript 无限 `continues_from` | `MAX_CONTINUATION_DEPTH = 512` |
| Debug `run_connection` 栈溢出 | 8MiB 专用线程；`recursion_limit` 只影响编译 |

### Wave 3 — Web / 传输

| 项 | 结论 |
|---|---|
| WS/文件路由鉴权绕过 | `/api/*`（四条公开除外）与 `/ws/events` 都要 token；空 token fail-close |
| files/upload_jail 路径穿越 | 文件名消毒、Unix `O_NOFOLLOW`、canonicalize `starts_with` jail |
| WS 无界 outbound / 慢客户端内存 | `OUTBOUND_CAPACITY=64`；Lag → Detach |
| 重连风暴 / WS 泄漏 | 单例 `WebTransport`；teardown 先摘 handler；退避 1s–32s |
| 危险端点无鉴权 | backup/files/terminal/ACP 在 token 层后 |
| 压缩炸弹 | 只压响应；SSE/zip 排除 |
| socket inherit fd 泄漏 | bind 后 `FD_CLOEXEC` / 清 `HANDLE_FLAG_INHERIT` |
| 默认 `CODEG_HOST=0.0.0.0` 等于未鉴权 RCE | LAN 可达，但 **必须** 非空 token（未设则生成并打 stderr）。持 token = 完整主机操作者，是产品模型 |

### Wave 4 — 进程 / 委托

| 项 | 结论 |
|---|---|
| PTY UTF-8 unwrap panic | 生产 `from_utf8_lossy` |
| `listener.rs` `unbounded_channel` | **仅测试** |
| broker `Mutex` 跨 await | `std::sync::Mutex` 字段不跨 `.await` |
| depth 无界 DB 负载 | `compute_depth` 饱和在 `depth_limit+1`（默认 1） |
| 委托 wait 无界 / 双 complete | `wait_ms=0` 显式；CAS first-write-wins |
| 父断连后 **running** 子永远跑 | `cancel_by_parent` drain + settle cancel |
| tool_watchdog 误杀 | 产品默认 `enabled: false` |
| officecli 无界 spawn | `MAX_CONCURRENT_WATCHES=32` + `kill_on_drop` |
| `commands/mcp.rs` 进程泄漏 | 只配配置/市场，不 spawn |

### Wave 5 — 前端

| 项 | 结论 |
|---|---|
| 无虚拟化回退 | `MessageListView` 总走 `VirtualizedMessageThread`；历史约 120 条窗口 |
| Zustand 重渲染冻死（P0） | 历史 adapter 缓存；默认 legacy 流式重绘 live 行是有意的 P4-off |
| `conversation-runtime-store` `setInterval` 泄漏 | 命中是 `setTimeout`；`removeConversation` 会取消 |
| 重连丢/重事件（除 preserveLive） | stale replay 有挡；sequence-gap recovery 在 |
| subscribe 不退订 | live-transcript / ticker / ACP keepalive 有 disposer |
| 每个 chunk 主线程高亮巨大流式文本 | Streamdown code plugin `idle`；高亮 LRU 128 / 8MiB |
| 关 Tab 泄漏会话 | keep-alive 卸已关 tab；hung-agent `pendingCleanup` 算残留不升 |
| `use-token-output-speed` 定时器 | 生产未用 |
| composer 100ms polling | 仅 streaming-perf replay；unmount 清 |

### Wave 6 — 周边

| 项 | 结论 |
|---|---|
| 更新跳过校验 | 归档 minisign；fork HTTP perform/restart/rollback 拒 |
| 更新路径穿越 | tar `sanitize_entry_path`；zip `enclosed_name` |
| 安装 unwrap 崩溃 | 生产路径测试里；`canonicalize` 是 `unwrap_or` |
| 恢复时覆盖正在跑的 DB | stage + 标记；swap 在开 DB 前 `apply_pending_restore_on_startup` |
| 入站 webhook 无鉴权 | 无入站 webhook 监听器 |
| 调度器忙等 | 60s sleep |
| auto_title 76 个 unwrap | `#[cfg(test)]` 之后 |
| 标题路径阻塞 HTTP | async reqwest，30s timeout |
| document_translate 失控 | 容量 1、480s、输出上限 |
| Pets marketplace 无超时 | 8s connect + 30s 总计 |

---

## P1 复核（2026-08-20）

口径同参考文档：叙事路径走不到则关闭；机制成立但「全量 / 每帧 / 日常必崩」被放大则降级。只读对照上表引用行。

### 降为 P2（3）

| 条目 | 依据 |
|---|---|
| 桌面 Web ChatChannel 分身 | 机制 100% 错，但 `auto_start` 默认 false，不是首次启动日常路径 |
| 连接 map 锁跨 await | pop-out/rebind；默认几个连接，主窗口日常不够「明显 hitch」 |
| session/load 无界 drain | Claude/Codex 默认 resume 跳过；Cline/自定义/无 resume 才走 |

### 仍为 P1（5）

1. CleanupGuard 无 incarnation CAS：与 disconnect 的 CAS 不对称；fallback/取消重连窗口真实。
2. Connecting 断开不 abort：与 `teardown_unexposed_attempt` abort-first 对照成立；Initialize 最长 60s。
3. `terminal/create` 退出残留：无 `kill_on_drop`；quit 只杀 UI PTY + agent PID；Windows 父死子活。
4. `preserveLive` 双份回合：`detail_refetch` 在 idle 后仍 `preserveLive: true`；owner 不去重不同 id。
5. live 事件重复 JSON.parse：Write 之后每个 token 解析同一大 payload；编码日常路径。

未从 P2 升 P0/P1。

---

## Wave 0 密度（短）

扫描根：`src-tauri/src`（516 `.rs`）、`src/`（前端；不含 vendor）。权重：崩溃/逻辑 1.0；HOT 文件内性能 3.0；冷路径性能 0.3。测试 `panic!`/`unwrap` 不计入开放项。

Rust 密度（`unwrap`/`expect`/`panic!` 命中，含测试）：ACP `connection.rs` / `manager.rs` / `lifecycle.rs` / `continuation/store.rs` / `background_watch.rs` 最高；其次 backup、upload_jail、auto_title、window_diagnostics。深读后测试占比极大。

`unsafe` 命中少，集中在 `supervise` 信号处理、`socket_inherit`、`parent_watcher`、backup 归档、`upload_jail`。

前端：`conversation-runtime-store.ts` 定时器/流式归约最密；`virtualized-message-thread.tsx` 有 `addEventListener`；`setInterval` 多数在 settings/pet/测试。

HOT 深读队列（非缺陷）：`acp/connection.rs`、`acp/manager.rs`、`acp/lifecycle.rs`、`acp/session_state.rs`、`acp/delegation/broker.rs`、`web/auth.rs`、`web/ws.rs`、`stores/conversation-runtime-store.ts`、`contexts/acp-connections-context.tsx`。

Wave 0 **不开放** P0/P1。

---

## 覆盖与剩余风险

**已覆盖：** Wave 0 两棵根模式扫描；Wave 1 AppState/关机/DB/supervise/watch；Wave 2 ACP 连接/会话/parser 调用方；Wave 3 鉴权/WS/jail/前端 transport；Wave 4 终端/MCP/委托/watchdog/office_watch；Wave 5 runtime store/虚拟列表/live parse；Wave 6 更新/备份/频道/翻译/auto_title/pets。

**未深审 / 剩余风险：**

- `src-tauri/vendor/codex-acp`（计划：范围外）。
- 委托 `broker.rs` 全文（约 3 万行）；只采样 spawn/wait/cancel/parent-end。
- `src/**/*.test.ts(x)` 只当契约证据，不深审测试夹具。
- sqlx 池连接是否每条都拿到 `busy_timeout`（无 in-tree `after_connect`）。
- `git_credential` 从 askpass helper 对同一文件开第二 SQLite。
- 无 Job Object 时，崩溃且无 `ExitRequested`：agent CLI + ACP 终端可比 Codeg 活得更久。
- 默认 desktop 流式 flag 是 **legacy**（增量/延迟除非 `CODEG_*` env）。该路径更慢是有意的，不记 P0。
- 无运行时 trace：性能全是静态推断。

建议修复顺序：CleanupGuard incarnation CAS → Connecting 断开 abort driver → quit 纳入 `TerminalRuntime` / `kill_on_drop` → `preserveLive` 仅 prompting + owner 去重 → live tool JSON 按 id 缓存。
