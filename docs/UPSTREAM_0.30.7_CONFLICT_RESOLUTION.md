# main 合入 Codeg v0.30.7：冲突解决手册

> 实际合并提交：`4e2e46281955b242d45017ec5a514848f8622a4f`（ours `173b71d5298a013631942bd42c4100033948ea58`，theirs `932ac543532c86186a851ffada83dc1ccab9515c`）
> 共同祖先：`3ebdfed1d7c0b71d71880a3d2e0f8e09545feae1`
> ours 不是计划里的 `17f5e6ff`：那是合入前最后一个产品提交；实际第一父是其后的表征测试提交 `173b71d52`
> 合入对象：`v0.30.7`（`932ac543`，合入时的 `upstream/main`）
> 其后还有一次编译/测试收口 `5aeb660cb`。本文描述的是 **merge 当时怎么收冲突**，不要用 HEAD 当合并提交，也不要从 2026-09-11 试合入表抄动词。

本文只回答一件事：**每一个冲突块该怎么收**。按类给统一配方，再落到文件；错位块给「这一格的口令 + 功能迁到真正的臂」，而不是整文件 `--ours` / `--theirs`。

---

## 1. 合入事实

| 项 | 值 |
|---|---|
| Ours | `173b71d5298a013631942bd42c4100033948ea58` `test: pin 0.30.7 sync invariants before the upstream merge` |
| Theirs | `932ac543532c86186a851ffada83dc1ccab9515c` (`upstream/main` = `v0.30.7` at merge time) |
| 共同祖先 | `3ebdfed1d7c0b71d71880a3d2e0f8e09545feae1` |
| 实际合并提交 | `4e2e46281955b242d45017ec5a514848f8622a4f`（`git merge --no-ff`，两父） |
| 打开合并时的工作树清点 | **93** 个冲突文件、**291** 个 `<<<<<<<` 文本块（与试合入同数，但数字来自打开后的 live recount，不是试合入表） |
| add/add | `src/lib/notification.test.ts`（无 base） |
| 上游新增 `src/` + `src-tauri/src/` | **72**（`service.rs`、`mcp_service`、DeepSeek catalog、wallpaper market、Gitea、canvas path migration、term-keybar、image-diff、file-viewer-drawer 等） |
| 旧 `src-tauri/src/bin/codeg_server.rs` | **没有**复活；服务器入口仍是 `server_bin/main.rs` |

`<<<<<<< HEAD` = MyCodeBuddy / DrawCode fork（含已合的 ~0.30.0 与 fork 功能）。
`>>>>>>> v0.30.7` = 上游 Codeg。

配方描述的是 **`173b71d52` ⊕ `932ac543`** 落到 `4e2e46281` 的收法。

---

## 2. 全局原则

沿用 `docs/UPSTREAM_SYNC.md`，本轮再钉死：

1. **品牌 / 发行身份永远 ours。** 版本号只升数字，后缀与仓库元数据不动。
   写成 `0.30.7-mycodebuddy.1`（`package.json` / `Cargo.toml` / `Cargo.lock` 的 `codeg` package / `tauri.conf.json`）。不要收成光秃 `"0.30.7"`。
2. **OpenClaw 保持删除。** 上游会整段加回 `AgentType::OpenClaw`、`McpAppType::OpenClaw`、`ALL_MCP_APPS` 行、设置页 `open_claw`。所有「ours 空、theirs 写 OpenClaw」的块：**丢 theirs**。自动合并漏进来的 OpenClaw 臂也要删。i18n 里 theirs-only 的 gateway 文案可以留（不是一等运行时接线）。
3. **功能两边都留，适配而不是二选一。** 上游 0.30.1–0.30.7 的产品能力要进来（mid-turn `blocks`、`DelegationService`/`Ping`、codeg-mcp 服务、DeepSeek 0.9.0 catalog、Gitea、backup snapshot、画布/壁纸）；fork 的 CompanionLease、`continue_delegation` / `complete_work`、六工具父目录、会话弹出、ToolWatchdog、`richContentState`、`eventIngestorRef` 也要留。冲突形态几乎都是「同一锚点两边各插了一套 API」，正确动作是 **MERGE_BOTH 或 REWRITE**。
4. **Registry pin：谁新用谁。** CodeBuddy 取上游 **2.149.0**，`route.rs` 的 `PINNED_CODEBUDDY_VERSION` 跟随。不要停在 fork 的 2.148.0。
5. **Decision 8：** `BrokerMessage` 只加 `Ping`（不要 `TaskProgress` / `CreateAutomation` / `CreateWorkTask`）；生产路径走 `DelegationService::new` / `install` / `start`；`DelegationListener::new` 保持 **7 参**（不要 `tasks` / `authoring`）。

四字口令：

| 口令 | 含义 |
|---|---|
| **TAKE_OURS** | 整块丢上游 |
| **TAKE_THEIRS** | 整块丢 fork |
| **MERGE_BOTH** | 两套符号都留下，接好类型 |
| **REWRITE** | 以上游（或 fork）为底，把另一侧的符号手工迁回去 |

父目录 6 工具（`CompanionFeatures.delegation = true` 的默认 `tools/list`）按名断言：

1. `delegate_to_agent`
2. `register_simple_workflow`
3. `continue_delegation`
4. `resume_delegation`
5. `get_delegation_status`
6. `cancel_delegation`

`tools.len()==4|5` 全部改掉。不要给 `CompanionFeatures` 加 `tasks` / `automations` / `taskboard`。

---

## 3. 先拍板的四处结构分叉

这四处不定，后面 80+ 文件会来回改。错位块的逐块口令在 **C**。

### 3.1 OpenClaw：不恢复

- Ours：内建注册表、parser、MCP 枚举、设置页里没有 OpenClaw。
- Theirs：`registry` 加回 `openclaw@2026.9.3`、`ALL_MCP_APPS` 多一行、`mcp-settings` 的 `open_claw` 标签、`parsers/openclaw.rs`。
- **决议：TAKE_OURS（拒绝恢复）。** `AgentType` / `McpAppType` 没有 `OpenClaw`。`build_agent_parser` **不要** OpenClaw 臂。`ALL_MCP_APPS` 写成 **14** 元、`Pi` 垫底。设置页只留 `pi` 为 scan-only。

### 3.2 委托协议：continue + resume 并存，父目录恰好 6 个工具

二者**不是别名**。`continue_delegation` = 同一子会话上的新一代（可带新 task）；`resume_delegation` = 同一 `task_id`、无新 task 文本。`complete_work` 不进这 6 个。

Decision 8 只把上游的 **`Ping`** 和 **`DelegationService`（bind / accept_loop / start）** 接进来。不要把 theirs 的 taskboard / automation 工具接到 `allows_legacy_tool`。

### 3.3 `Steer` 带 `blocks`；详情面板继续是薄包装

上游 mid-turn 从 `Steer { text, reply }` 变成 `Steer { blocks, reply }`。`submit_feedback(conn_id, text, blocks)`。前端 `onSteer?(text, blocks?)` 进 `conversation-session-surface.tsx`，**不要**把 2040 行 theirs 面板贴进已经 `return <ConversationSessionSurface>` 的 `conversation-detail-panel.tsx`。

### 3.4 错位冲突：这一格 TAKE_OURS，功能迁到真正的臂

Git 把两段无关代码对齐到同一对 marker。在这一格 `TAKE_THEIRS` 会把函数插进错误的 match 臂 / 错误的文件。详见 C。

---

## 4. 按类怎么收（93 文件 / 291 块）

### A. Identity — 4 文件 4 块

统一结果：`0.30.7-mycodebuddy.1`，仓库字段保持 MyCodeBuddy。

| 文件 | 块 | 决议 | 配方 |
|---|---|---|---|
| `package.json` | 1 | MERGE_BOTH | `"version": "0.30.7-mycodebuddy.1"`，保留 ours 的 `license` / `repository` / `homepage`。不要收成 `"0.30.7"`。 |
| `src-tauri/Cargo.toml` | 1 | MERGE_BOTH | `version = "0.30.7-mycodebuddy.1"`。下面已自动留下 MyCodeBuddy `repository`/`homepage`。 |
| `src-tauri/Cargo.lock` | 1 | MERGE_BOTH | 仅 `[[package]] name = "codeg"` 那一行改成 `0.30.7-mycodebuddy.1`。不要 `checkout --theirs` 整文件。 |
| `src-tauri/tauri.conf.json` | 1 | MERGE_BOTH | 保留 `productName`/`mainBinaryName` = `DrawCode`，`identifier` = `app.mycodebuddy`，`version` = `0.30.7-mycodebuddy.1`。 |

旁路（无冲突块、但身份要一起改）：`install.ps1` 与各语言 README 的 `v0.30.7-mycodebuddy.1` 示例；`docs/UPSTREAM_SYNC.md` 的 `sync/codeg-0.30.7`。收完跑 `pnpm test:release`。

---

### B. Registry / MCP / parsers（含 `parsers/pi.rs` 与自动合并进来的 `ALL_MCP_APPS` OpenClaw 行）— 9 文件 28 块

**总策略：** pin 谁新用谁；OpenClaw 一律删；Pi 是第 14 个 MCP app，不是一等可勾选 agent。

#### `registry.rs` 7 块

| 块 | 决议 | 配方 |
|---|---|---|
| 1 | MERGE_BOTH | **保留** `distribution: codex_distribution()` 与 `CODEX_CLI_RUNTIME_DEFAULT_ENV`。helper 升到 `codex-acp@1.10.0` / `cmd: "codex-acp"` / `node_required: Some("20.0.0")`。禁止贴 theirs 的 inline `AgentDistribution::Npx` + `env: &[]`。 |
| 2 | **TAKE_OURS** | **整段丢掉** `AgentType::OpenClaw => … openclaw@2026.9.3`。 |
| 3 | TAKE_THEIRS | CodeBuddy **2.149.0** / `@tencent-ai/codebuddy-code@2.149.0`。 |
| 4 | TAKE_THEIRS | Grok 注释跟 1.0.25 live probe；pin 仍是 `1.0.25`。 |
| 5 | TAKE_THEIRS | Antigravity URL assert `agy_acp_server_1.1.1`。 |
| 6 | TAKE_THEIRS，再删 OpenClaw | Cline `3.0.61` + CodeBuddy `2.149.0` 断言留下。删 OpenClaw Node-floor 注释和 `assert_npx_version(AgentType::OpenClaw, …)`。 |
| 7 | TAKE_THEIRS | OpenCode **1.18.30**。 |

同文件自动合并的钉保持：Claude `0.75.1`、Gemini `0.59.0`、Hermes `0.21.1`、Kimi `0.42.0`、Pi `0.0.33`、Cursor `2026.09.02-c22c1a3`、DeepSeek `0.9.0`、Qoder `1.1.49`、Antigravity `1.1.1`。fork-only 的 Cursor 断言若还停在 `2026.08.31-4057e58`，改到表上的 commit，否则 pin 测试和条目打架。

`route.rs`（无冲突）：`PINNED_CODEBUDDY_VERSION = "2.149.0"`，跟着 registry。

`models-dev.json` #1 — **TAKE_THEIRS**（整文件 checkout theirs；JSON 数组，不要手合）。

#### `commands/mcp.rs` 3 块 + 自动合并债

| 块 | 决议 | 配方 |
|---|---|---|
| 1 | 删 ours | 丢掉残留的本地 `let all_apps = [ … 13 … ]`。下面走 `ALL_MCP_APPS`。 |
| 2 | TAKE_THEIRS | `None => ALL_MCP_APPS.to_vec(),` |
| 3 | REWRITE | `fn local_mcp_readers() -> [LocalMcpReader; 14]`；Antigravity 留下；**Pi 最后**；无 OpenClaw reader。 |

自动合并会把 `McpAppType::OpenClaw` 加回 `ALL_MCP_APPS`。收完必须是这 **14** 个、`Pi` 垫底，并删 `all_mcp_apps_is_exhaustive` 的 OpenClaw 臂，加 `assert_eq!(ALL_MCP_APPS.len(), 14)`。DeepSeek SSE 守卫 `Codex | DeepSeek` 留下。

#### `mcp-settings.tsx` 2 块

| 块 | 决议 | 配方 |
|---|---|---|
| 1 | TAKE_THEIRS，再删 OpenClaw | `SCAN_ONLY_APP_LABELS` 只留 `pi: "pi"`。接进 `appLabel`。 |
| 2 | TAKE_THEIRS | `visibleApps` + `hiddenLegacyApps` carry-forward。`APP_OPTIONS` 仍是可勾选的 13（无 `pi` checkbox、无 OpenClaw）。 |

#### parsers

| 文件 | 块 | 决议 | 配方 |
|---|---|---|---|
| `parsers/mod.rs` | 1 | REWRITE | DeepSeek 会话根用 fork 的 `resolve_deepseek_sessions_root_for_runtime_env` 作为唯一 `agent: "deepseek"` 源（加 `deepseek-attachments`）。不要再叠一份静态 `agent: "deepseek"`。无 `parsers/openclaw.rs`，无 OpenClaw `build_agent_parser` 臂。 |
| `parsers/claude.rs` | 1 | TAKE_THEIRS | compaction helpers（`CONTEXT_CONTINUATION_PREFIX`、`/compact` divider）。**只留一份** prefix helper。 |
| | 2 | MERGE_BOTH | 保留 `*pending_autonomous_origin = None` + `last_slash_command_prompt_id`；接 `/compact` divider。合成消息补 `reasoning_effort: None`。 |
| | 3 | TAKE_THEIRS | resume-replay / compaction 测试。 |
| `parsers/codex.rs` | 1–2 | MERGE_BOTH | `is_wait \|\| is_list` + `build_collab_list_input` + fork wait status / `native_team_wait_input`。 |
| | 3–4 | TAKE_THEIRS | 测试 import 并集；不要和 marker 后已有的名字重复。 |
| `parsers/pi.rs` | 6 | REWRITE | **以上游为底**，回植 fork 隐藏：`session_name` 包 `visible_title`；user text 先走 `visible_user_text` 再判空。helper **定义在** `parsers/mod.rs`、**调用在** `pi.rs`。两套测试都留，并回挂 `hides_codeg_terminal_context_from_history_and_title`。fixture `parentId: "mc1"` TAKE_THEIRS。注释里提 `parsers::openclaw` 只是 sibling 形状，不是恢复 OpenClaw。 |
| `codex_model_catalog.rs` | 1 | MERGE_BOTH | 11 models + `gpt-6-astra`；fork 的 `gpt-5.6-sol` / `fallback_base_slug` 断言留下。 |

`parsers/cursor.rs` 未冲突：`stale Cursor ACP id must recover` 已在，不要重写。

---

### C. ACP core（`connection.rs` / `manager.rs` / `lib.rs` 错位块；`grok_model_specs` 改名）— 21 文件 61 块

本类是本轮最难的 Rust。**禁止**对 `connection.rs` / `manager.rs` / `lib.rs` 整文件 `--ours` / `--theirs`。

#### 错位块（实际用的口令）

Git 把无关代码对齐到同一对 marker。**这一格**的口令是 TAKE_OURS（或 fork-base KEEP），功能迁到真正的臂。

| 文件 | 块 | 两侧体量 | 这一格实际口令 | 功能迁到 |
|---|---|---|---|---|
| `connection.rs` | **#12 / #13** | 18/124 与 2/370 | **TAKE_OURS，再 port** | 保留 Grok terminal-poll tick（`// Always tick for Grok …`）和它后面的两道闭合括号。**不要**把 370 行 theirs `tokio::select!`（AIR stop-reason / 内联 `SetMode` / `Steer { blocks }`）贴到 tick 末尾。`Steer { blocks, reply }`、`authRequired`→`turn_failure`、AIR `SessionFailure` 接到已有的 `finalize_bound_prompt_response` + `cmd_rx` / `start_ancillary_command`。 |
| `manager.rs` | **#1 / #3 / #4 / #5** | 374/27、105/5、76/14、963/38 | **REWRITE（fork-base）** | 骨架留 ours：shared-session bootstrap / admission / `spawn_agent_with_attach_mode` / `disconnect_with_origin` / watchdog。把 `DrainingChild` / `park_draining` / `DRAIN_GRACE` / `prune_reaped` / `_restore_guard` / `live_or_draining_agent_names` 嵌进去。 |
| `manager.rs` | **#7 / #10 / #11 / #12** | 10/174、11/100、9/139、7/591 | **REWRITE（test-base / 上游测试为底）** | #7 留 `disconnect_by_owner_window_and_operation`，插入 `disconnect_by_agent_type`（fork take/lease + theirs drain sweep）。#10 TAKE_THEIRS 原生 `submit_feedback` 身体（`turns_completed` + `Steer { blocks, reply }`），签名仍是 `LaneSender<ConnectionCommand>`。#11/#12 接上游 drain / native-steering 测试，删掉还在匹配 `Steer { text, reply }` 的 HEAD 残段。 |
| `lib.rs` | **#6** | 1/493 | **TAKE_OURS** | 继续 `production_tauri_commands!(generate_production_invoke_handler)`。**禁止**贴 493 行上游 `generate_handler![…]`（fork 靠宏避免桌面误编 server 命令）。把缺失的上游路径登记进宏。 |
| `conversation-detail-panel.tsx` | **#3**（三块都一样） | 13/2040（另 2/36、1/74） | **TAKE_OURS** | 文件已经是薄 `ConversationTabView` → `ConversationSessionSurface`。把 2040 行 theirs 贴进来会留下一堆未用 hook，再仍返回 surface。ask-selection / `imageRoot` / `onSteer` 迁到 `conversation-session-surface.tsx`。 |
| `acp-connections-context.tsx` | **#3** | 9/869 | **TAKE_OURS** | 留 `eventIngestorRef` / `pushMapped`。**不要**把 869 行内联 `switch` 贴进 ingestor 路径。`notifyDesktop` / `feedback_submitted`→`STEERING_MESSAGE` / `EMPTY_STEERED_MESSAGE_IDS` 迁到 `prepareMappedEnvelope`。 |

`manager.rs` #2 / #6 不是错位，但是同一文件的配套口令：#2 MERGE_BOTH（留 `continuation_store`，用 `Self::new()` + timeout 接 `with_spawn_handshake_timeout`，不要用缺 fork 字段的 theirs 结构体字面量）；#6 MERGE_BOTH（留 `.collect()` + `take_connections_for_disconnect`，drain 放进 helper，不要内联 `map` drain）。

#### `connection.rs` 其余 15 块 — REWRITE

| 块 | 决议 | 配方 |
|---|---|---|
| 1 | MERGE_BOTH | `use std::pin::Pin` **和** `AtomicBool` / `Ordering`。 |
| 2 | MERGE_BOTH | `ResolvedShellSpec` + `emit_with_state_gated`。 |
| 3 | TAKE_THEIRS | 上游 steer-injector 文档（`PromptInputBlock` / idle `NoActiveTurn`）。 |
| 4 | TAKE_THEIRS | 上游 AIR async-task stop 文档。 |
| 5 | TAKE_THEIRS | `build_steer_params(session_id.0.as_ref(), blocks)`。 |
| 6 | TAKE_THEIRS | `pub fn locate_codeg_mcp_binary()`（给 `mcp_service` 用）。周围的 fork load-failure recovery 不要动。 |
| 7 | MERGE_BOTH | `.on_receive_notification(handle_auth_status_update)` **写进** fork 的 `.connect_with(agent, { route_bootstrap_tx; async move \|cx\|` 包装。 |
| 8–9 | TAKE_THEIRS | 调用改成 `grok_model_specs` / `parse_grok_model_specs`；`pi_startup_banner` 在 `attach_session` 前写。 |
| 10 | TAKE_THEIRS | gateway-error → `Refusal`；3 参 `turn_failure_error_event(..., empty)`。 |
| 11 | MERGE_BOTH | **两个** stop-reason 臂都留：`"auth_required"` **和** `"empty"`。 |
| 14 | MERGE_BOTH | idle `ConversationInput::Command` 用 fork 形状 + `blocks: _`。 |
| 15–16 | MERGE_BOTH | Codex `codex_subagent_thread` / Terminal settle **和** fork `status`。 |
| 17 | MERGE_BOTH（append） | 接上游两条 Codex 测试；marker 下的 fork 测试整段留。 |

每个残留的 `grok_effort_specs` / `parse_grok_effort_specs` **改名**为上游名字。类型仍是 `GrokEffortSpec`。Antigravity 的两条 codeg-mcp skip 字符串（ready-lease / inject）和 `AgentType::Antigravity` skip 臂必须还在。

#### `manager.rs` / `commands/feedback.rs`

见上面的 fork-base vs test-base。#8/#9 文档 TAKE_THEIRS（`blocks` / native-path）。

`commands/feedback.rs` #1 — **MERGE_BOTH**：留 `db` / `manager` `State` 和 `ensure_connection_delegate_interactive`；加上 `blocks` 并原样转发。不要做两参 shim。Web handler 已自动带 `params.blocks`。

每个 `submit_feedback("…", "…".into())` 补第三个实参 `None` 或 `Some(draft)`。

#### `lib.rs` 其余块 + 双运行时接线

| 块 | 决议 | 配方 |
|---|---|---|
| 1 | MERGE_BOTH | 留全部 fork alias，**只加** `deepseek_settings as deepseek_settings_commands,`。 |
| 2 | MERGE_BOTH | `ProcessStart::now()` 然后 `apply_webview_rendering_override`，再 `init_desktop`。 |
| 3 | TAKE_OURS | 留 `reject_removed_completion_protocol_configuration` + `window_diagnostics::initialize`。 |
| 4–5 | REWRITE（一次安装） | 留 fork 早期 broker + **7 查找** 的 `new_with_workflow_emitter`（`wait_cancel` + `workflow_emitter`）。不要收上游后段 8 参或 `tasks` / `authoring`。把 `listener.run(socket_path)` 换成 `DelegationService::new` / `install` / `start`（`socket_path.clone()`）。`apply_persisted_terminal_shell_config` 改名为 `apply_persisted_terminal_settings`。 |
| 6 | **TAKE_OURS** | 见错位表。宏补齐上游路径（mcp_service 三件套、DeepSeek catalog、`background_market_*`、gitlab/gitea token、Antigravity 登录、backup snapshot 集……）。`backup_inspect` 不要登记（函数没了）。 |

`server_bin/main.rs` #1 — 同一配方：7 参 listener 收口后 `DelegationService::new` / `install` / `start`。不要 `listener.run(socket)`，不要重建 `bin/codeg_server.rs`。

`commands/mod.rs` #1 — MERGE_BOTH：留 `pub mod delegate_access;`，加 `pub mod deepseek_settings;`。`pub mod mcp_service;` 已在则不要再写一次。

`web/router.rs` #1 — MERGE_BOTH：七条都留（四条 fork delegation-profile + 三条 mcp-service）。DeepSeek catalog 与 `background_market_*` 路由确认与宏同名。

#### `grok_model_specs` 改名与其余 ACP / 运行时

| 文件 | 块 | 决议 | 配方 |
|---|---|---|---|
| `session_state.rs` | 1、3、4 | MERGE_BOTH | 留 fork 的 provider/generation fence 字段；加 `pub turns_completed: u64`；两端 initializer 都写 fence + `turns_completed: 0`；终局清 `active_provider_turn_id` **并** `saturating_add(1)`。 |
| | 2 | TAKE_THEIRS | 字段改名 `grok_model_specs`（类型仍 `GrokEffortSpec`）+ `pi_startup_banner: None`。本文件不许再出现 `grok_effort_specs`。 |
| `acp/types.rs` | 1 | MERGE_BOTH | 留 fork import（Continuation / shared-session / ToolWatchdog）；`PromptInputBlock` 加 `PartialEq, Eq`。 |
| `fork.rs` | 1 | MERGE_BOTH | 两条注释叠在 `claude_falls_back_to_the_fingerprint_with_no_agent_id`。 |
| `background_watch.rs` | 1–2 | MERGE_BOTH | `record_text` 保持生产 `pub(crate)`（不要单独 `#[cfg(test)]`）。episode 留 `autonomous_origin`，`initiator_text` 用上游 `&str` 调法。 |
| `commands/acp.rs` | 1 | MERGE_BOTH | follow-latest 先 `first_spec` 再 pin；两次失败都 `annotate_npm_bootstrap_failure`。 |
| | 2 | TAKE_THEIRS | Hermes `Some("hermes-agent@0.21.1")`，名字仍用 fork 的 `package_index`。`enabled: setting.map(...).unwrap_or(false)` 必须出现两次（status + list），不要 `unwrap_or(true)`。 |
| `antigravity_login.rs` | 1 | TAKE_THEIRS | 接 sign-out slot 测试（`claim_slot_as` / `SIGN_OUT_BUDGET`）。stderr scrape 已在文件里。 |
| `lifecycle.rs` | 1 | MERGE_BOTH | 留 fork CAS / `mark_awaiting_reply` / `persist_live_external_id`。把 `auth_required` 加进 TurnComplete CAS。不要换成无条件 `update_status`。 |
| `terminal/manager.rs` | 4 | MERGE_BOTH | `ResolvedShellSpec` + `TerminalSnapshot`；实例同时有 `owner_operation_id` 与 `scrollback`。测试两套 helper 都留。不必 import 未用的 `ShellFamily`。 |
| `db/migration/mod.rs` | 1 | MERGE_BOTH | 加 `mod m20260907_000001_canvas_node_path;`（list 里可能已有）。留 `install_for_historical_completion_fixture`。 |
| `commands/conversations.rs` | 1 | MERGE_BOTH | 选中导入测试用 in-place restore 名；整夹 `whole_folder_import_still_never_resurrects_a_deleted_conversation` 留下。`#707` worktree-parent 环已在未冲突区。 |
| `event_bridge.rs` | 1 | TAKE_THEIRS | `CHAT_AUTHORING_SETTINGS_CHANGED_EVENT` 与 `DELEGATION_SETTINGS_CHANGED_EVENT` 跟其它 settings 事件放一起。**删掉**后面那份重复的 chat-authoring 常量。 |
| `web/handlers/canvas.rs` | 1 | TAKE_THEIRS | `canvas_delete_node_core` 传 `&state.terminal_manager`。 |
| `work_task_service.rs` | 1 | TAKE_THEIRS | `Preparing` 并进 live-status 集。 |
| `tests/api_integration.rs` | 1 | MERGE_BOTH | **追加**上游 161 行（turn-window / mcp_service / DeepSeek catalog）。marker 上的 fork 测试不要动。 |

---

### D. Delegation surface — 8 文件 18 块

**总策略：** 6 工具目录不动；Decision 8 只加 `Ping` + `DelegationService`；settings 写入器只翻 `enabled`。

| 文件 | 块 | 决议 | 配方 |
|---|---|---|---|
| `delegation/mod.rs` | 1 | MERGE_BOTH | 留全部 fork 模块，**只加** `pub mod service;`。TAKE_OURS 会把刚进来的 `service.rs` 变成孤儿。 |
| `delegation/transport.rs` | 1 | MERGE_BOTH | SessionInfo 后的九个 fork `BrokerMessage` 变体全留；**只追加** `Ping`。不要 `TaskProgress` / `CreateAutomation` / `CreateWorkTask`。`client_ping` 已自动合并。 |
| `delegation/listener.rs` | 1 | REWRITE | 留 `install_status_release_decision_gate` 与 **7 参** `DelegationListener::new`。`run` = `bind` + `accept_loop`（各一份）。Unix `bind`：stage + rename，**不要**先 unlink 目标。`accept_loop` 调 fork 的 `spawn_connection`，不要 `tokio::spawn(serve_one)`。 |
| | 2 | MERGE_BOTH | `Ready` 早退后的第一臂是 `Ping => { "ok": true }`（不碰 broker/DB/token）。Call / CancelTask / ResumeTask 仍 `report_response`。 |
| `delegation/types.rs` | 1 | MERGE_BOTH | 留 fork `from_err_cause_code_tests`；接上游 `error_codes_are_stable…`。必须有 `DelegationError::ChildAuthRequired` + `from_err` → `"child_auth_required"`（否则 `lifecycle` 的 `auth_required` 映射编不过）。 |
| `commands/delegation.rs` | 8 | MERGE_BOTH | 留 fork mutation / catalog / `set_delegation_settings_live`。接 `DELEGATION_SETTINGS_CHANGED_EVENT`、`DELEGATION_WRITE_LOCK`、`set_delegation_enabled_core(conn, broker, emitter, enabled)`。**写入器只 upsert `delegation.enabled`**。`into_broker_config()` 会清 profiles：enabled writer 先拷活着的 profile map。 |
| `web/handlers/delegation.rs` | 1 | MERGE_BOTH | 留 fork profile/catalog/bundle；settings/bundle 写时 emit settings-changed。 |
| `delegation-settings.tsx` + test | 3+1 | MERGE_BOTH | UI：fork catalog/profiles/bundle + theirs 远程 enable / pending-edit。测试两套都留；远程用例打 `setDelegationBundle`。 |

`service.rs` 是上游新增、未冲突。测试里的 `make_service` 必须改成 **7 参** `DelegationListener::new`（broker、tokens、leases、parent、feedback、questions、session_info）。删掉 `WorkTaskToolAccess` / `ChatAuthoringAccess` Stub。生产 `DelegationService::new(listener, socket)` 不要加参。

`companion.rs` 未冲突：默认目录仍是那 6 个名字。`mcp_service` 对 slug `"delegation"` 调 enabled writer，后两个 popover 写的是 `chat_authoring` 旗，不是 `CompanionFeatures.tasks`。

---

### E. Backup / import / notification / forge — 14 文件 57 块

| 文件 | 决议 | 配方 |
|---|---|---|
| `import_service.rs` 13 | MERGE_BOTH | 留 fork `collect_visible_local_summaries` / `DeletedPolicy`（整夹 Skip、点选 Restore）。接 `ImportResult.restored` + `ImportOutcome::Restored` + 软删恢复测试。`#707` 环检测不在本文件（在 `conversations.rs`）。给上游测试补 `timed_summary` / `find_row` / `external_rows`。`commands/conversations.rs` 的 `ImportOutcome::Restored` 臂必须穷尽，否则编不过。 |
| `backup/core.rs` 10 | MERGE_BOTH | 接 `LiveRoots` / plaintext / packing progress；**留** `inspect_backup_core` 并让 `source.rs` 真正调用它（不要只留 dead fn，clippy `-D warnings` 会打）。 |
| `backup/restore.rs` 8 | MERGE_BOTH | extract progress + `apply_pending_restore` via `LiveRoots` + `managed_sections`。 |
| `backup/external.rs` 3 | MERGE_BOTH | `ExternalPack` / SQLite / scratch；fork 的 DeepSeek `add_external_sources_with` 与 `obtain_plaintext_zip` 留下。 |
| `backup/mod.rs` 2、`handlers/backup.rs` 3 | MERGE_BOTH | source_id + prepared zip + connections dispatch。 |
| `commands/notification.rs` 4 | MERGE_BOTH | 两端文档 + `OnceLock`；macOS 先 `resolve_notification_identity` 再 fork click-wait；接 `open_system_notification_settings`；两套测试都留。 |
| `src/lib/notification.ts` 2 | MERGE_BOTH | fork `sendSystemNotification`（hidden-document、camelCase、`getTransport`）+ theirs permission / `deliverSystemNotification` / identity / open-settings。 |
| `notification.test.ts` 1（add/add） | MERGE_BOTH | **两套拼起来**。撞名标题加 `fork` / `upstream` 前缀。一份 `./transport` mock。 |
| `forge/auth.rs` 1 | TAKE_THEIRS | 流式 probe + `PROBE_BODY_CAP` + `/api/v1/version`（Gitea / Forgejo）。 |
| `forge/deliver.rs` 3 | TAKE_THEIRS | Gitea create/get pull + `git_failure_message`。 |
| `forge/mod.rs` 2 | MERGE_BOTH | `/api/v1` suffix + `ForgeProvider::parse("Gitea")`。`gitea.rs` 是新增文件，直接留下。 |
| `general-settings.tsx` 2 | MERGE_BOTH | **两段都渲染**：先 `ConversationExperienceSettingsSection`（ours），再 `DesktopNotificationSettingsSection`（theirs），然后 sound / delegation / tools。 |
| `general-settings.test.tsx` 3 | MERGE_BOTH | mock 并集；fork 终端壳用例 + GeneralSettings 挂载/colorize/保存壳。 |

---

### F. Frontend（Decision 7 进 `conversation-session-surface.tsx` + ingestor；`api.ts` 持有 Decision 8 类型）— 27 文件 72 块

**总策略：** 详情面板三块 TAKE_OURS；mid-turn / 桌面通知 / steered ids 全部进 surface 与 `prepareMappedEnvelope`。`api.ts` 出 MCP 服务客户端；`types.ts` 不出 `CodegMcpServiceStatus`。

#### 会话表面 / 输入 / 连接层

| 文件 | 块 | 决议 | 配方 |
|---|---|---|---|
| `conversation-detail-panel.tsx` | 1–3 | **TAKE_OURS** | 见 C 错位表。合并后约 740 行，`ConversationTabView` 只转 `ConversationSessionSurface`。 |
| `conversation-session-surface.tsx` | 无冲突 | REWRITE（端口） | 在 `onForkSend` 旁加 `onSteer` / `steerChannel`；传 `imageRoot`、`steeredMessageIds`；`FeedbackNotesDisplay` 接 expired/resend/dismiss。 |
| `use-session-feedback.ts` | 1–3 | MERGE_BOTH | 留 `interactionLocked` / `onDelegateViewerOnly`。`steerAvailable = connectionId && (toolAvailable \|\| nativeSteering)`；`canSubmit` 再加 `!interactionLocked && isPrompting`。`steer(text, blocks?)` 调已自动合并的 `submitSessionFeedback`。 |
| `message-input.tsx` | 4 | MERGE_BOTH | 留 `onForkSend` + `mutationLockedNow()`；`onSteer` 扩成 `(text, blocks?)`；接 Clock/Zap `steerChannel` 与 0.30.5 `#672` `ComposerTokenAction`（Open link / mailto / Open file）。 |
| `chat-input.tsx` 2、`conversation-shell.tsx` 1 | MERGE_BOTH | 同上：fork 锁 + 加宽 `onSteer`。 |
| `message-input.test.tsx` | 1 | MERGE_BOTH | fork api mock + theirs sonner/attachments；补 workspace-action / grok-scope，让 token 菜单能挂 `useOpenLinkOrFile`。 |
| `acp-connections-context.tsx` | 1 | MERGE_BOTH | 接 `notifyDesktop` / `playEventSound` import（watchdog 仍 `sendSystemNotification`）。 |
| | 2 | MERGE_BOTH | 留 fork `rollbackLiveMessageAttempt`，**加上** `EMPTY_STEERED_MESSAGE_IDS`。 |
| | 3 | **TAKE_OURS** | 见 C。869 行 switch 不进 `eventIngestorRef`。 |
| | 4 | MERGE_BOTH | 留 fork cursor feed-back `onReplay`，外包音效/桌面通知抑制。`feedback_submitted`（仅 `delivered`）→ `STEERING_MESSAGE`，且走 `writableConnections`（theirs 的 `new Map` 返回会被丢掉）。五条本地 `fix(acp)`（remap / `turn_complete` / stale `external_id`）留下。 |
| `acp-connections-context.test.tsx` | 4 | MERGE_BOTH | 四块并集。`bb96c8b7` 自动进来；ours 67 行侧的五条 local fix 必须还在。 |

#### `api.ts` / `types.ts`

| 文件 | 块 | 决议 | 配方 |
|---|---|---|---|
| `api.ts` | 1 | MERGE_BOTH | 在本文件导出 `CodegMcpServiceState` / `getCodegMcpServiceStatus` / `startCodegMcpService` / `setCodegMcpToolGroup`。**同时**留 fork 的 `getDelegationProfileCatalog` / `setDelegationProfiles` / `setDelegationBundle`。`submitSessionFeedback(..., blocks?)` 已自动合并，不要改签名。 |
| `types.ts` | 1 | MERGE_BOTH | `ApiKeyUpdate` + `ConversationExperienceSettings` + 三条 settings-changed 事件（experience / `chat-authoring-settings://changed` / `delegation-settings://changed`）+ `AcpPromptContext`。`rg CodegMcpServiceStatus src/lib/types.ts` 必须为空。 |

#### 工作区 / tab / 时间线 / markdown / 文件

| 文件 | 块 | 决议 | 配方 |
|---|---|---|---|
| `workspace-context.tsx` | 12 | MERGE_BOTH | 留 `OpenFileSettleResult`、incarnation、settle/failOpenTab、maximize、fork 脏关闭对话框。接 `OpenFileOptions.background` / `index`、`seedLoadingTab`。关其它/全部走 `batchCloseSlots`；`closeFileTabsByIdsNow` 必须 **两参** `snapshotFileTab(closing, slot)`。不要收 `window.confirm`。 |
| `workspace-context.test.tsx` | 1 | MERGE_BOTH | **整份 fork 套件留下**，后面再接 `describe("reopening a closed file tab")`。脏确认关其它/关全部要能按原 `index` 重开。 |
| `tab-store.ts` | 7 | MERGE_BOTH | 留 `Promise<boolean>` + popout/detach/flush。`opts?: { index?: number }`；新 tab `insertTab` 再 evict。`closeOtherTabs` 用 `batchCloseSlots`。 |
| `conversation-runtime-store.ts` | 3 | MERGE_BOTH | 留 `TurnGroup` / `computeTimeline` / `appendCanonicalStreamingTurns`。加 `StreamingGroup`（不要复制 `getJoinedChunks`）。persist strip 留 `persistedWithLocalReplacement`。对 historical 先 `suppressPersistedSteeredPrompts` 再接 streaming。 |
| `message-list-view.tsx` | 6 | MERGE_BOTH | fork overlays / Grok / `useIncrementalLive` + theirs `isLiveTurnId` / `forkPointUnnamed` / `MarkdownImageProvider`。`storedImageRoot` 从 workspace store 取。`onForkFromHere` 要有可解析 `forkTurnId`。 |
| `message-list-view.test.tsx` | 1 | MERGE_BOTH | `isThreadTail: false` + fork `sourceTurns`（含 `autonomous_origin`）。 |
| `message.tsx` | 2 | MERGE_BOTH | 留 wrapper / `MATH_FENCE_PAD` / autolink；加 `remarkLocalImages` + `markdownLocalImageComponents`。Grok 作用域不要让 `remarkLocalImages` 吞掉 `images/*` 会话引用。 |
| `rehype-allow-codeg.ts` + test | 2+2 | MERGE_BOTH | 留 `grokSessionImages` + harden rewrite；接 span 属性白名单与本地图测试。 |
| `remark-file-uri-links.ts` | 1 | TAKE_THEIRS | 头注释改到 `remarkLocalImages` / image destinations。 |
| `link-safety.tsx` | 3 | MERGE_BOTH | 留 workspace/Grok extras；**继续导出** `canOpenLinkOrFile`（composer token 菜单用）。不要换成 `useOpenFileTarget`。 |
| `file-reference-actions.tsx` + test | 1+2 | MERGE_BOTH | `resolveGrokSessionImage` + download API；两套 mock/用例。 |
| `file-workspace-panel.tsx` | 1 | **TAKE_OURS** | 返回 `Promise<OpenFileSettleResult>`，不是 `Promise<unknown>`。 |
| `use-open-file-tabs-watch.ts` | 1 | **TAKE_OURS** | 与 HEAD 相同即可；fork options + settle 联合类型留下。 |
| `aux-panel-file-tree-tab.tsx` | 1 | MERGE_BOTH | 内层 download 菜单；`DesktopDropDirContext` 仍包住并在菜单后关闭。根菜单也要有 ignored-files checkbox。 |
| `terminal-view.tsx` | 2 | TAKE_THEIRS | `applySnapshot` deps = `[terminalId, workingDir, shell, initialCommand, attach]`。 |

`status-bar.tsx` 已自动合并：`StatusBarMcp` 在移动端和桌面右侧集群都要挂上（本轮落在 `StatusBarUpdate` **之后**）。`message-list-image-root.test.ts` 的 `SURFACES` 必须指向 `conversation-session-surface.tsx`。

---

### G. i18n — 10 文件 51 块

每个 locale 同一配方。`en.json` 是结构真源（5 块）；`pt.json` 是 6 块，深合并后键集仍与 `en` 对齐。

从 index 三路（`:1:` / `:2:` / `:3:`）做 JSON 深并集，不要把「为了让 Vite 能 collect 而 TAKE_OURS 的工作区预览」当成合并结果。

| 侧 | 留下 |
|---|---|
| Ours-only | `ToolWatchdogSettings` / `ToolWatchdogBanner` / `ConversationPopout` / `statusAwaitingReplyBadge` / workflow / pop-out / CLI-runtime |
| Theirs-only | canvas file/terminal、wallpaper market、`DesktopNotificationSettings`、codeg-mcp 状态栏、DeepSeek 0.9.0 文案、mid-turn steer、OpenClaw **gateway 文案**、backup snapshot |
| 三路同键 | ours==base → theirs（Gitea、Linux rendering、Codex launch-only mid-turn）；theirs==base → ours（DrawCode、fork 终端/ACP 壳）；两边都改的 `GeneralSettings.sectionDescription` → theirs（“notifications”） |

`en.json` 五块形态：bundled+`latestChannel`；`sectionDescription`；shell/colorize/rendering；`sessionModel`+`mcp`；terminal `confirmClose*`+`keybar`。

每个文件合法 JSON、只有一个收尾 `}`、无重复键、无 `<<<<<<<`。十个 locale **同一套叶键**（本轮 5493）。`ScienceSettings.description` 写 `DrawCode` 不写 `codeg`。`ConversationPopout.runtimeRestartRequired` 用 DrawCode 字符串。

不要 `TAKE_THEIRS`：会删掉看门狗和弹出窗。不要 `TAKE_OURS`：画布 / 壁纸 / codeg-mcp / 桌面通知变 key。

---

## 5. 建议施工顺序

不要按字母，按编译依赖（也是本轮实际落地顺序）：

1. **机械身份** — A + `UPSTREAM_SYNC.md` 示例 + `pnpm test:release`。
2. **Registry / MCP** — B 的 pin 与 OpenClaw 扫除（含自动合并的 `ALL_MCP_APPS`），`route.rs` 跟随 2.149.0。
3. **Decision 8 骨架** — `mod.rs` 加 `service` → transport 只加 `Ping` → listener 拆 `run`/`bind`/`accept_loop` → `lib.rs` / `server_bin` 换 `DelegationService`（#6 只收 ours）→ `service.rs` 测试改 7 参。
4. **ACP 改名与外围** — `session_state.grok_model_specs` + `turns_completed` → commands/acp Hermes → event_bridge 去重常量。
5. **`connection.rs` 再 `manager.rs`** — 先 #12/#13 TAKE_OURS 再 port；再 fork-base drain、test-base native steer；最后 `feedback.rs` 加 `blocks`。
6. **lifecycle / terminal / parsers** — CAS + `auth_required`；`pi.rs` 上游为底回植隐藏；DeepSeek 唯一 restore key。
7. **Backup / import / notification / forge** — `inspect_backup_core` 要有调用点；`ImportOutcome::Restored` 穷尽。
8. **委托 settings** — enabled-only writer + `ChildAuthRequired`。
9. **前端** — **先** surface / ingestor，**再**对详情面板 #3 TAKE_OURS；`api.ts` 最后收 Decision 8 客户端。
10. **i18n** — 十个 locale 深并集 + `messages.test.ts`。
11. **`pnpm test:release` + 定向 vitest + 能链接时再 `cargo test --lib --features test-utils`**（先窄后宽）。

### 最容易「看起来解决了、文件其实坏了」的块

| 文件 | 为什么 |
|---|---|
| `connection.rs` #12/#13 | 错位：370 行 select 会插进 Grok tick |
| `manager.rs` #1/#3/#4/#5 vs #7/#10/#11/#12 | 前半是 fork 骨架，后半才是上游测试；整文件 `--theirs` 会丢掉 admission/lease |
| `lib.rs` #6 | 展开 `generate_handler!` 会吞掉 fork IPC |
| `conversation-detail-panel.tsx` #3 | 2040 行贴进已经是 thin wrapper 的文件 |
| `acp-connections-context.tsx` #3 | 869 行 switch 会拆进 `eventIngestorRef` |
| `ALL_MCP_APPS` OpenClaw 行 | 自动合并会让 `AgentType` 编不过 |
| `companion.rs` tools.len | 4 或 5 都表示掉了一个工具 |
| `DelegationListener::new` | 8 参（tasks/authoring）对不上 fork |
| `enabled` writer | 整表 upsert 会把 profiles 写成空 |
| `inspect_backup_core` | 只留函数不调用，clippy 红 |
| `types.ts` 里的 MCP 类型 | Decision 8 客户端属于 `api.ts` |

---

## 6. 绝对不要做的事

- `git checkout --theirs` 整份 `connection.rs` / `manager.rs` / `lib.rs` / `acp-connections-context.tsx` / `conversation-detail-panel.tsx` / `workspace-context.tsx`。
- 把 2040 行 theirs 贴进 `conversation-detail-panel.tsx`（正确入口是 `conversation-session-surface.tsx`）。
- 在 `connection.rs` #12/#13 或 context #3 这些**错位格**收 theirs。
- 用 theirs 的展开 `generate_handler!` 替换 `production_tauri_commands!`。
- 用计划里的 `17f5e6ff` 当 ours，或用 HEAD 的 `5aeb660cb` 当合并提交。
- 为了让 `tools.len()==4` 或 `==5` 变绿而删掉 `continue_delegation` 或 `resume_delegation`。
- 给 `CompanionFeatures` 加 `tasks` / `automations` / `taskboard`，或把 `Ping` 以外的 BrokerMessage 任务变体接进来。
- 恢复 OpenClaw，或保留 `build_agent_parser` / `ALL_MCP_APPS` 的 OpenClaw 臂。
- 把版本收成上游的 `0.30.7`（发行与更新器会指错仓库）。
- 把 `CodeBuddy` 停在 2.148.0，或只改 `registry.rs` 不改 `route.rs`。
- 重建 `src-tauri/src/bin/codeg_server.rs`。

---

## 7. 收完后的验证

按 `AGENTS.md` 收窄范围，不要一上来全量：

```text
pnpm test:release
pnpm exec vitest run src/i18n/messages.test.ts \
  src/components/conversations/conversation-session-surface.test.ts \
  src/components/chat/message-input.test.tsx \
  src/contexts/acp-connections-context.test.tsx \
  src/contexts/workspace-context.test.tsx
cd src-tauri && cargo test --lib --features test-utils -- acp::sync_invariants::
cd src-tauri && cargo test --lib --features test-utils -- tools_list_exposes_six_delegation_tools
cd src-tauri && cargo test --lib --features test-utils -- production_tauri_registry_contains_repaired_commands
```

硬核对：

- 身份四处都是 `0.30.7-mycodebuddy.1`；`docs/UPSTREAM_SYNC.md` 只有 `sync/codeg-0.30.7` / `0.30.7-mycodebuddy.1`。
- `PINNED_CODEBUDDY_VERSION` 与 registry 都是 `2.149.0`。
- `AgentType::OpenClaw` / `McpAppType::OpenClaw` / `open_claw:` 字段不存在。
- `tools/list` 正好 6 个父工具名。
- `DelegationListener::new` 仍是 7 参；`lib.rs` 没有内联 `tauri::generate_handler![`。
- `connection.rs` 的 Grok tick 后面不是 370 行 `select!`；`Steer {` 带 `blocks` 不带 `text`。
- `src-tauri/src` 里没有 `grok_effort_specs`。
- 详情面板仍是 thin wrapper；`SURFACES` 指向 `conversation-session-surface.tsx`。

手测（有 UI 时）：

1. DrawCode 名称、图标、更新器 URL 仍是 MyCodeBuddy。
2. 智能体列表 **没有** OpenClaw；Pi 可扫不可当一等勾选项。
3. `@` 委托子会话可打开；取消后 `resume_delegation` 仍是同一 `task_id`。
4. mid-turn steer 能带附件 blocks；弹出窗 / 看门狗设置还在。
5. 无限会话画布、壁纸市场、Gitea 探活、backup snapshot 能走。
6. 状态栏有 codeg-mcp 服务指示。

---

## 8. 块数对照（验收用）

| 类 | 文件 | 块 | 默认口令 |
|---|---:|---:|---|
| A Identity | 4 | 4 | MERGE_BOTH → `0.30.7-mycodebuddy.1` |
| B Registry / MCP / parsers | 9 | 28 | pin TAKE_THEIRS；OpenClaw TAKE_OURS；`pi.rs` REWRITE |
| C ACP core | 21 | 61 | REWRITE；错位格 TAKE_OURS 再 port；`lib.rs` #6 TAKE_OURS |
| D Delegation surface | 8 | 18 | MERGE_BOTH；listener REWRITE；Ping only |
| E Backup / import / notification / forge | 14 | 57 | MERGE_BOTH；forge 探活 TAKE_THEIRS |
| F Frontend | 27 | 72 | 详情面板 / context #3 / file-workspace TAKE_OURS；其余 MERGE_BOTH |
| G i18n | 10 | 51 | MERGE_BOTH（抄 en 键集） |
| **合计** | **93** | **291** | |

实际合并已落在 `4e2e46281955b242d45017ec5a514848f8622a4f`；后续若重放合并，按第 5 节顺序清冲突后再提交。不要把 `5aeb660cb` 记成合并提交。
