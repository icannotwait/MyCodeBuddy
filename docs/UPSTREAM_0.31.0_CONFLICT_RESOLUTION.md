# main 合入 Codeg v0.31.0：冲突解决手册

> 本文描述的是 **`sync/codeg-0.31.0` 上 `git merge --no-ff upstream/main` 当时怎么收冲突**。
> 不要用后续编译/测试收口提交当合并提交。

`<<<<<<< HEAD` = MyCodeBuddy / DrawCode fork（`0.30.7-mycodebuddy.1` + 冷重连 / Grok settle / PR #16/#19）。
`>>>>>>> upstream/main` = 上游 Codeg `v0.31.0`。

四字口令仍是 **TAKE_OURS / TAKE_THEIRS / MERGE_BOTH / REWRITE**。错位块给「这一格的口令 + 功能迁到真正的臂」，而不是整文件 `--ours` / `--theirs`。

---

## 1. 合入事实

| 项 | 值 |
|---|---|
| Ours（merge 第一父链） | `7ca357398` `Merge pull request #16`（`origin/main` / `0.30.7-mycodebuddy.1`），其上先落了设计文档 `363779c19` |
| Theirs | `aace536fe9e38575a8973ed83435199ecc2700c6`（`upstream/main` = tag `v0.31.0`） |
| 共同祖先 | `932ac543532c86186a851ffada83dc1ccab9515c`（`v0.30.7`） |
| 打开合并时的工作树清点 | **83** 个冲突文件、**272** 个 `<<<<<<<` 文本块（与 `git merge-tree --write-tree origin/main upstream/main` 同数） |
| add/add | `message-queue-display.test.tsx`、`file-workspace-header.test.tsx`、`file-workspace-tab-bar.test.tsx`、`deep-link-bootstrap.test.tsx` |
| 旧 `src-tauri/src/bin/codeg_server.rs` | **没有**复活；服务器入口仍是 `server_bin/main.rs` + `required-features = ["server"]` |

---

## 2. 全局原则（本轮再钉死）

1. **品牌 / 发行身份永远 ours。** 版本写成 `0.31.0-mycodebuddy.1`（`package.json` / `Cargo.toml` / `Cargo.lock` 的 `codeg` package / `tauri.conf.json`）。不要收成光秃 `"0.31.0"`。`productName` / `mainBinaryName` = `DrawCode`，`identifier` = `app.mycodebuddy`。
2. **OpenClaw 保持删除。** `AgentType::OpenClaw`、`openclaw@` pin、`open_claw` slug、`parsers/openclaw.rs` 全部丢掉。i18n 里 theirs-only 的 gateway 文案可以留。
3. **父目录恰好 6 个工具：** `delegate_to_agent`、`register_simple_workflow`、`continue_delegation`、`resume_delegation`、`get_delegation_status`、`cancel_delegation`。不要收上游 4/5 工具目录，不要给 `CompanionFeatures` 加 `tasks` / `automations` / `taskboard`。`BrokerMessage` 只追加 `Ping`。
4. **Registry pin：谁新用谁。** CodeBuddy **2.155.0**，`route.rs` 的 `PINNED_CODEBUDDY_VERSION` 跟随。新智能体 `enabled` 默认 `unwrap_or(false)`，不要 `unwrap_or(true)`。
5. **冷重连 / Grok settle / PR#16#19 hydrate：fork-base REWRITE。** 错位块这一格 TAKE_OURS，功能迁到真正的臂。
6. **Cargo features MERGE_BOTH：** 保留 fork `server` / `test-utils = ["tauri?/devtools", "tauri?/test"]` / `unmerged-upstream-helpers`；采用上游 `browser-child` / `browser-smoke`。`Cargo.lock` 用 `cargo generate-lockfile` 重生，再钉 `codeg` 版本（需要 rustc ≥ 1.85，因为 vendored `sacp-tokio` 是 edition 2024）。

---

## 3. 按类怎么收（83 文件 / 272 块）

### A. Identity

| 文件 | 决议 | 配方 |
|---|---|---|
| `package.json` | MERGE_BOTH | `"version": "0.31.0-mycodebuddy.1"`；保留 ours `license` / `repository` / `homepage`、`test:release` / low-memory / `server:* --features server`；接上游 `browser:agent:*`。插件版本保留 fork 较新的 `plugin-dialog@^2.7.2` / `plugin-opener@^2.5.4`。 |
| `src-tauri/Cargo.toml` | MERGE_BOTH | `version = "0.31.0-mycodebuddy.1"`。features：fork `test-utils` + `unmerged-upstream-helpers` + `server`，加上游 `browser-child` / `browser-smoke`。`url` crate 注释 TAKE_THEIRS。bin 仍走 `server_bin/main.rs`。 |
| `src-tauri/tauri.conf.json` | MERGE_BOTH | DrawCode / `app.mycodebuddy` / `0.31.0-mycodebuddy.1`。 |
| `src-tauri/Cargo.lock` | REWRITE | 不 `checkout --theirs` 整文件。`cargo generate-lockfile` 后把 `[[package]] name = "codeg"` 钉成 `0.31.0-mycodebuddy.1`。 |
| `README.md` | TAKE_OURS + 端口 | 保留 fork 的 `<details>` 功能文档；不把上游 19 行 docs.codeg.app 目录贴进 Chat Channels 闭合标签。另加 URL scheme 小节。 |
| `docker-compose.yml` | TAKE_THEIRS | 接 `CODEG_BRIDGE_PORTS` / in-place upgrade 注释。 |

### B. Registry / parsers

| 文件 | 决议 | 配方 |
|---|---|---|
| `registry.rs` | REWRITE（theirs pins） | 以上游为底吃 pin，再删全部 OpenClaw 臂/测试。保留 `codex_distribution()` / `CODEX_CLI_RUNTIME_DEFAULT_ENV` / `is_reserved_builtin_id`。 |
| `route.rs`（无冲突） | 跟随 | `PINNED_CODEBUDDY_VERSION = "2.155.0"`。 |
| `opencode.rs` | REWRITE | 上游 deep-adapt 为底，回植 fork visibility / terminal-context 测试。 |
| `cline.rs` / `codebuddy.rs` / `gemini.rs` | MERGE_BOTH | 上游 connect/auto-approve / `$set.summary` + fork history-scan / `visible_user_text`。 |
| `parsers_snapshot.rs` | TAKE_THEIRS − OpenClaw | 新 OpenCode 快照留下；丢掉 OpenClaw snapshot/import。 |

落地 pin：Claude `0.79.0`、Codex `1.12.0`、Gemini `0.60.0`、Cline `3.0.62`、OpenCode `1.18.31`、Hermes `0.21.3`、CodeBuddy `2.155.0`、Kimi `2.0.2`、Grok `1.0.34`、Cursor `2026.09.15-d2fe57e`、Qoder `1.1.57`、DeepSeek `0.9.0`、Pi `0.0.33`、Antigravity `1.1.1`。

### C. Delegation

| 文件 | 决议 | 配方 |
|---|---|---|
| `tool_schema.json` | REWRITE fork-base | 六工具 + `complete_work` 目录不动。删 `open_claw`。接上游 `task_ids.minItems` 文案。 |
| `companion.rs` | REWRITE fork-base | CompanionLease + 六工具 `tools/list`。错位的 browser/authoring 大段丢。UDS 测试接 `/tmp` AF_UNIX budget。`tools.len()==6`。 |
| `listener.rs` | REWRITE | **7 参** `new`；stage+rename bind；`spawn_connection`。接 `Ping => {"ok": true}`、`SUN_PATH_CAP`、`short_socket_dir`。不要 8 参（tasks/authoring/browser）。 |
| `service.rs` | TAKE_THEIRS + 适配 | 生命周期测试留下；删 `WorkTaskToolAccess` / `ChatAuthoringAccess` stub；构造走 fork 7 参。 |
| `transport.rs` | MERGE_BOTH | fork `BrokerMessage` 变体全留，**只追加** `Ping`。 |
| `delegation_e2e_uds.rs` | MERGE_BOTH | 留 `fixture_started_at`；接 `socket_dir()` budget。 |
| `delegation_e2e_windows.rs` | TAKE_OURS | 拒收 8 参构造。 |
| `delegation-agent-defaults.tsx` | MERGE_BOTH | 无 `open_claw`；接 `loaded` + `useAgentVocabulary`。 |

### D. ACP connection + commands

| 文件 | 决议 | 配方 |
|---|---|---|
| `connection.rs` | REWRITE fork-base | 骨架留冷重连 / Grok settle / leftover-live。迁入：scratch isolation、AF_UNIX、install-dir PATH、ask-card auto-allow、elicitation peer、`codex_user_input_shape`、terminal service-watch。错位大段 select **不要**贴进 Grok tick。 |
| `commands/acp.rs` | MERGE_BOTH | **`enabled: setting.map(...).unwrap_or(false)` 必须出现两次**（status + list）。Hermes `0.21.3`；接 vendor-dir / Cline env / Kimi `.agents/skills`。 |
| `question.rs` | MERGE_BOTH | 上游 ask-card collapse/companion；fork grok/pi helpers。 |
| `terminal_runtime.rs` / `preflight.rs` / `binary_cache.rs` / `antigravity_login.rs` / `mod.rs` | MERGE_BOTH | Codex pin 1.12.0；binary cache 跟 `CODEG_HOME` / `CODEG_DATA_DIR`；mod 加 `scratch_dir` / `temp_reclaim` / `browser_tools` / `cursor_acp_retry_compat`。 |

### E. lib / server / 其余 Rust

| 文件 | 决议 | 配方 |
|---|---|---|
| `lib.rs` | MERGE_BOTH；1/555 **TAKE_OURS** | 继续 `production_tauri_commands!(generate_production_invoke_handler)`。禁止贴上游展开 `generate_handler!`。缺的 browser / deep-link / config-sync / close-behavior 命令登记进宏。`DelegationService::new/install/start`。 |
| `app_state.rs` | MERGE_BOTH | 留 `DelegationStack`；加 `browser`，两个构造都写。 |
| `server_bin/main.rs` | MERGE_BOTH | 仍是 server feature + `server_bin`；`stack.browser`。 |
| `web/router.rs` + handlers/mod | MERGE_BOTH | 留 fork delegation-profile；加 browser-tools / config-sync。 |
| `keyring_store.rs` | MERGE_BOTH | fork `CredentialState` + 上游 `secret_key` / `read_tokens_at*`。 |
| `work_task/engine.rs` + service | MERGE_BOTH | `chosen_base_branch` / ConversationId unlink on requeue。 |
| `terminal/manager.rs` | MERGE_BOTH | `owner_operation_id` + ResolvedShellSpec 测试。 |
| `office_watch/mod.rs` | TAKE_OURS | 保留 sleeper/reap 实现。 |
| 其余 commands / models / app_error | MERGE_BOTH | folders `open_no_follow`；close-behavior 类型；config-sync i18n keys。 |

### F. Frontend hydrate

| 文件 | 决议 | 配方 |
|---|---|---|
| `acp-connections-context.tsx` | REWRITE fork-base | 留 `eventIngestorRef` / `prepareMappedEnvelope` / 冷重连 / Grok / leftover-live / turn-complete。835/18 与 9/883 **错位 TAKE_OURS**，不要把 883 行 switch 贴进 ingestor。迁入 streaming flush、`setConnectPending`、`config_option_rejected` toast。 |
| `conversation-detail-panel.tsx` | TAKE_OURS | 薄包装。**禁止**贴 2187 行 theirs 面板。`onSteer` / `imageRoot` 已在 `conversation-session-surface.tsx`。 |
| `conversation-runtime-store.ts` | MERGE_BOTH | 留 TurnGroup / Grok overlay；接 `MARK_OUT_OF_TURN_CONTENT`。 |
| `use-connection*.ts` | MERGE_BOTH | 留 `sharedSession`；接 `connectPending` / `preparing`。 |
| `workspace-context.tsx` | MERGE_BOTH | 留 `OpenFileSettleResult` / 两参 `snapshotFileTab`；接 browser tabs / `background` / `index` / `batchCloseSlots`。不要 `window.confirm`。 |
| `workspace-context.test.tsx` | MERGE_BOTH | **整份 fork 套件留下**，再追加 `browser tabs`。 |
| `deep-link-bootstrap.tsx(+test)` | MERGE_BOTH | 上游 exactly-once handoff + fork workspace 接线。 |

### G. 其余前端

MERGE_BOTH 为主：`canOpenLinkOrFile` 继续导出；composer 留 `onForkSend` / `mutationLocked`，`onSteer` 加宽；queue-display 接 queue-steer；session-config 用智能体原生模型名；turn-stats 接 Qoder `hasTokenCounts`；`acp-agent-settings` 接 Cline 登录、**不加 OpenClaw 行**；`web-transport.ts` 留 fork completion-context，接 `parseJsonOrThrow`。

add/add 测试：两套拼起来（pause-control + click-to-insert；header snapshot + more-menu；tab-bar translate + browser add-tab）。

### H. i18n

十个 locale **JSON 三路深并集**（ours==base → theirs；theirs==base → ours；两边都改且 ours 含 DrawCode/MyCodeBuddy → ours）。叶键集对齐为 **6018**。OpenClaw **gateway 文案**保留。无 `<<<<<<<`，合法 JSON。

---

## 4. 最容易「看起来解决了、文件其实坏了」的块

| 文件 | 为什么 |
|---|---|
| `connection.rs` 错位大段 | 上游协议循环会插进 Grok tick |
| `lib.rs` 1/555 | 展开 `generate_handler!` 会吞掉 fork IPC 宏 |
| `conversation-detail-panel.tsx` 26/2187 | 薄包装被整页面板替换 |
| `acp-connections-context.tsx` 9/883 | 883 行 switch 拆进 `eventIngestorRef` |
| `companion.rs` / listener 8 参 | 丢掉 `continue_delegation` 或加上 taskboard |
| `commands/acp.rs` `unwrap_or(true)` | 新智能体被重新默认开启 |
| `Cargo.lock` 整文件 `--theirs` | 丢掉 fork 身份或 server feature 图 |
| `ALL_MCP_APPS` / `AgentType::OpenClaw` | 自动合并会让枚举编不过 |

---

## 5. 绝对不要做的事

- 整文件 `--theirs`：`connection.rs` / `lib.rs` / `companion.rs` / `listener.rs` / `acp-connections-context.tsx` / `conversation-detail-panel.tsx` / `workspace-context.tsx`。
- 恢复 OpenClaw，或把版本收成 `0.31.0`。
- 为了让 `tools.len()==4|5` 变绿而删掉 `continue_delegation` 或 `resume_delegation`。
- 用 theirs 的展开 `generate_handler!` 替换 `production_tauri_commands!`。
- 重建 `src-tauri/src/bin/codeg_server.rs`。
- 在 rustc 1.83 上 `generate-lockfile`（vendored `sacp-tokio` 要 edition 2024 / rustc ≥ 1.85）。

---

## 6. 收完后的验证

```text
pnpm test:release
pnpm lint
pnpm test
# Rust：对齐 .github/workflows/test.yml
cd src-tauri
cargo test --features test-utils
cargo test --no-default-features --features server --bin codeg-server --lib
```

硬核对：

- 身份四处都是 `0.31.0-mycodebuddy.1`。
- `PINNED_CODEBUDDY_VERSION` 与 registry 都是 `2.155.0`。
- `AgentType::OpenClaw` / `McpAppType::OpenClaw` 不存在。
- `tools/list` 正好 6 个父工具名。
- `DelegationListener::new` 仍是 7 参；`lib.rs` 没有生产路径内联 `tauri::generate_handler![`。
- 详情面板仍是 thin wrapper。

---

## 7. 合入后验证收口（不是冲突块，是错位功能）

这些是 `<<<<<<<` 清零之后、`pnpm test` 才暴露的 MERGE_BOTH 缺口。不要把它们写进第 3 节的「当时怎么收冲突」表。

| 缺口 | 决议 |
|---|---|
| `acp-connections-context.tsx` 迁了 `flushStreamingQueue` / `STREAM_FLUSH_*`，但没迁 `enqueueStreamingAction` | 在 `pushMappedEvents` 里对 `content_delta` / `thinking` 入队 + `EVENT_APPLIED`；其它事件先 `flushStreamingQueue` 再走 `EventIngestor`。不要复活上游 `handleMappedEvent` 当唯一分发器。 |
| `session-config-selector.tsx` 被上游 `selected?.name` 盖掉 | 回植 `configOptionDisplayLabel`，保留上游 `recommendedLabel` chip。 |
| `useConnectionLifecycle` 的 `preparing` | 在 session surface 上传 `preparing: isActive && awaitingHistoricalSessionId`。 |
| `WebTransport.call` 改 `res.text()` | 测试 mock 补 `text()`；fork completion-context 捕获留下。 |
| `useOpenFileTarget` 多了 `folderId` | 未设时不要把 `folderId: undefined` 传给 `openFilePreview`。 |
| General / tab-bar / MarkdownLink 测试 | fixture 并上 `BrowserSettings`；`Browser.tab.untitled`；`useOptionalWorkspaceActions` mock。 |
| `link-classify.ts` 抽走了 fork 的 Windows/`%3A`/bare-relative 解析 | 先对 raw href 切 `:line`；`stripLeadingSlashOnWindows` 看解码前是否真是盘符；bare relative 走共享 `isLocalPathLike`。 |
| `conversation-session-surface.tsx` 没迁上游 `queueSteerInFlight` | 薄包装仍 TAKE_OURS。把 click-to-insert hold + `handleQueueSteer` 接到真正的 flush 臂（fork 的 `waitingForSubagents` / lock / pause / `sharedSession` 闸门都留下）。依赖数组保持多行；layout 测试认 trailing-comma 最后一项。 |
| `prepareEventEnvelope` 把 streaming 变体拓宽成整份 `EventEnvelope` | 改成 generic `<T extends EventEnvelope>`；flush 窗口里用 `content_delta \| thinking` type guard，再读 `parent_tool_use_id`。 |
| `browserTabRecord` 缺 fork 的 `hasLoadedSuccessfully` | 冷开 browser tab 写 `false`（与 file-tab 工厂一致）。`BrowserWorkspaceTab` 继承 `FileWorkspaceTabBase`。 |
| `patchFileTabRef` 对 union `FileWorkspaceTab` 做 `{...tab,...patch}` | `applyFileTabPatch` 保 discriminant；patch 只叠共享字段。 |
| `antigravity_launch_env` 丢了上游 `scratch` 第二参 | 签名接 `Option<&Path>`，login spawn 传 `scratch.path()`；settings 测试传 `None`。 |
| fixture 缺 `show_thinking` / `recommended_value` | 测试构造补字段；`OpenCodeParser` 从 `super` 再 import。 |
| `folders::open_no_follow` 仅非 unix `doc_guest` 调用 | unix 实现 `#[allow(dead_code)]`，避免 server clippy `-D warnings`。 |
| `folders::is_within_workspace` 被 TAKE_OURS 丢掉 | 从上游回植：canonical starts_with **或** `folder_links::is_allowed`。`#[cfg(feature = "tauri-runtime")]`，server clippy 不要 `dead_code`。 |
| `strip_environment_details` 被上游 `trim()` 吃掉行首空格 | fork 用 `trim_end()` + `trim_start_matches(['\\r','\\n'])`，保留 indented mandatory-route。 |
| Cline `legacy_conversation` title 没走 `visible_title` | 先 `visible_title`；task 只剩 terminal context 时回落到第一条可见 user-turn。 |
| `TurnComplete(end_turn)` 用 `persist_live_external_id` 多打一条 upsert | 只 `bind_external_id`；CAS 后的 State patch 仍是唯一 `conversation://changed`。 |
