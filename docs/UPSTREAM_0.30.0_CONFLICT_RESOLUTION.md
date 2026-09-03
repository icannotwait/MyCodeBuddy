# main 合入 Codeg v0.30.0：冲突解决手册

> 实际合并提交：`8d8b691c`（ours `0972665f`，theirs `30e45706`）
> 原试合入基线：`5790c076`；实际 ours 在其后另有 127 个 fork 提交
> 合入对象：`v0.30.0`（`30e45706`，与当前 `upstream/main` 同提交）

本文只回答一件事：**每一个冲突块该怎么收**。按类给统一配方，再落到文件；巨型块给「上游为底 + 回植 fork」而不是逐行抄。

---

## 1. 试合入事实

| 项 | 值 |
|---|---|
| Ours（实际 merge HEAD） | `0972665f` Merge resolved upstream Codeg v0.27.0 baseline |
| 原试合入基线 | `5790c076`；实际 ours 在其后增加 127 个提交 |
| 实际已含上游点 | `0870d330`（v0.27.0 标签之后、含 PR #546） |
| Theirs | `v0.30.0` = 0.28.0 + 0.28.1/0.28.2 + 0.29.0 + 0.30.0 |
| 增量提交 | 171（`0870d330..v0.30.0`） |
| 增量文件 | 463 files，+74050 / −5921 |
| 冲突文件 | **92**（91 个文本文件 + 1 个二进制 `icon.icns`） |
| 冲突块 | **293 个文本块**（另 1 个二进制冲突） |

`<<<<<<< HEAD` = MyCodeBuddy / DrawCode fork（含已合的 ~0.27 与 fork 功能）。
`>>>>>>> v0.30.0` = 上游 Codeg。

原 assess 试合入少了 127 个 fork 提交，不能直接代表实际 main。本文数字和块表已按 `8d8b691c` 的 remerge 结果修订；配方描述的是 **`0972665f` ⊕ `30e45706`**。

---

## 2. 全局原则（先于任何一块）

沿用 `docs/UPSTREAM_SYNC.md`，本轮再加三条操作定义：

1. **品牌 / 发行身份永远 ours。** 版本号只升数字，后缀与仓库元数据不动。
   写成 `0.30.0-mycodebuddy.1`（`package.json` / `Cargo.toml` / `Cargo.lock` 的 `codeg` package / `tauri.conf.json`）。
2. **OpenClaw 保持删除。** assess 的 `registry.rs` 已无 `AgentType::OpenClaw`；0.30 会整段加回来。所有「ours 空、theirs 写 OpenClaw」的块：**丢 theirs**。不要为了「少冲突」把 OpenClaw 接回来。
3. **功能两边都留，适配而不是二选一。** 上游 0.28–0.30 的产品能力要进来；fork 的 CompanionLease、`continue_delegation` / `complete_work`、会话弹出、ToolWatchdog、Windows 路径归一、DrawCode 选择器禁用守卫、`richContentState` 也要留。冲突形态几乎都是「同一锚点两边各插了一套 API」，正确动作是 **MERGE_BOTH 或 REWRITE**，不是整块 `--ours` / `--theirs`。

四字口令：

| 口令 | 含义 |
|---|---|
| **TAKE_OURS** | 整块丢上游 |
| **TAKE_THEIRS** | 整块丢 fork |
| **MERGE_BOTH** | 两套符号都留下，接好类型 |
| **REWRITE** | 以上游（或 fork）为底，把另一侧的符号手工迁回去 |

---

## 3. 先拍板的三处结构分叉

这三处不定，后面 80+ 文件会来回改。

### 3.1 OpenClaw：不恢复

- Ours：内建注册表、parser 和设置页里没有 OpenClaw。通用 custom-agent 兼容注释及 skill 模板示例可以保留，它们不是内建运行时接线。
- Theirs：`registry` 加回 `openclaw@2026.8.1`、`supports_mcp: false`、设置页整段 UI、`parsers/openclaw.rs`、`build_agent_parser` 的 OpenClaw 臂。
- **决议：TAKE_OURS（拒绝恢复）。** `openclaw.rs` 已按删除自动消失；`AgentType` 没有 `OpenClaw`。`parsers/mod.rs` 里若已有 `build_agent_parser`，**只删它的 OpenClaw 臂**，不要整函数推倒重写。设置页 `open_claw`、registry 臂、`mcp.rs` OpenClaw 合并、`from_wire("open_claw")` 一律不接。接任何一侧都会对不上当前枚举。

### 3.2 委托协议：continue + resume 并存，父目录恰好 6 个工具

| 侧 | 工具 / 能力 |
|---|---|
| Fork | `delegate_to_agent` / `continue_delegation` / `get_delegation_status` / `cancel_delegation` / `register_simple_workflow`；`complete_work`（子侧）；`CompanionLease`；orchestration；session reuse |
| 0.30 | 四件套：`delegate` / `status` / `cancel` / **`resume_delegation`**（同一 `task_id` 断点续跑）；`@` mention；`agent_mentions` |

二者**不是别名**，工作树未冲突的 `broker.rs` 里两套实现都在：

- `continue_delegation` = 同一子会话上的**新一代**（新 `correlation_id`、可带新 task 文本）
- `resume_delegation` = **同一 `task_id`**、无新 task 文本；仅 interrupted（取消/ stranded in_progress）可走，否则 `not_resumable`
- `complete_work` = 子侧语义收口，**不进**下面这 6 个，也不替代 resume

默认父目录 `tools/list`（`CompanionFeatures.delegation = true`）必须是这 **6** 个，按名断言：

1. `delegate_to_agent`
2. `register_simple_workflow`
3. `continue_delegation`
4. `resume_delegation`
5. `get_delegation_status`
6. `cancel_delegation`

`tools.len()==4|5` 全部改掉。feedback+delegation 用例是 **7**（6 + `check_user_feedback`）。
**不要**把 theirs 的 `task_progress` / `create_automation` / `create_work_task` 接到 `allows_legacy_tool`：fork 的 `CompanionFeatures` 没有 `tasks` / `automations` / `taskboard` 字段。

不要用空 task 去调 `continue_delegation` 冒充 resume。resume 走 `resuming` 集合 + `find_resume_context_by_call_id` + 原 `parent_tool_use_id`。

### 3.3 连接启动：`delegation_lease` **和** `delegation_enabled` 都留

`CompanionInjection` 冲突后的代码已经在 `delegation_lease.take()`。只收 bool 会编不过；只收 lease 则 `prepare_agent_bound_prompt` / `@` 路由看不到开关。

- 两个字段都放进结构体并一起返回。
- 暴露门仍是 **`plan.expose_codeg_delegation`**（不可变 launch plan），不要改成 live broker toggle 当唯一门。
- 先 `leases.register` 再写 MCP 条目。
- `acp/mod.rs`：`autonomous_activity` + `agent_mentions` + `antigravity_login` 三个 `pub mod`。

### 3.4 错位冲突：这一格 TAKE_OURS，功能迁到真正的臂

Git 把两段无关代码对齐到同一对 marker。在这一格 `TAKE_THEIRS` 会把函数插进错误的 match 臂。

| 文件 | 块 | 这一格 | 功能迁到 |
|---|---|---|---|
| `connection.rs` | 14（354 行 stop-reason） | **TAKE_OURS**（空，结束 poll arm） | 真正的 prompt-response / StopReason 出口 |
| `connection.rs` | 15（Grok `set_config`） | **TAKE_OURS**（`suspend_no_active_turn`） | 已有的 ancillary `SetConfigOption` 路径 |
| `acp-connections-context.tsx` | 9（786 行 `handleMappedEvent`） | **TAKE_OURS**（结束 `handleDesktopDeliveryFailure`） | 现有 `applyMappedEnvelope` / mapped-event |

---

---

## 4. 按类怎么收（293 个文本块 + 1 个二进制冲突）

### A. 仓库卫生 · 1 文件 1 块

**`.gitignore` #1 — MERGE_BOTH**

```
# Generated codex-acp seed (built by src-tauri/scripts/stage-codex-acp.mjs)
src-tauri/resources/codex-acp-seed/

# `rustc foo.rs` with no -o writes its binary here. ...
/rust_out
```

丢任何一侧都会让 worktree 交付检查或 seed 脏仓复发。

---

### B. 版本清单 · 4 文件 4 块

统一结果：`0.30.0-mycodebuddy.1`，仓库字段保持 MyCodeBuddy。

| 文件 | 块 | 决议 | 配方 |
|---|---|---|---|
| `package.json` | 1 | MERGE_BOTH | `"version": "0.30.0-mycodebuddy.1"`，保留 ours 的 `license` / `repository` / `homepage`。不要收成光秃 `"0.30.0"`。 |
| `src-tauri/Cargo.toml` | 1 | MERGE_BOTH | `version = "0.30.0-mycodebuddy.1"`。下面已自动留下 MyCodeBuddy `repository`/`homepage`。 |
| `src-tauri/Cargo.lock` | 1 | MERGE_BOTH | 仅 `[[package]] name = "codeg"` 那一行改成 `0.30.0-mycodebuddy.1`。其余 lock 已自动合并，不要 `checkout --theirs` 整文件。 |
| `src-tauri/tauri.conf.json` | 1 | MERGE_BOTH | 保留 `productName`/`mainBinaryName` = `DrawCode`，`identifier` = `app.mycodebuddy`，`version` = `0.30.0-mycodebuddy.1`。 |

收完跑 `pnpm test:release`。

---

### C. 品牌资源 · 1 个二进制冲突

**`src-tauri/icons/icon.icns` — REWRITE**

二进制无法三方合并。以 DrawCode 品牌资源重导出，吸收 0.30 的 macOS Dock 尺寸修正；最终 blob 与 ours/theirs 都不同，不能记成 TAKE_OURS，也不能直接换上游 codeg 图标。

---

### D. i18n · 10 文件 20 块（同一配方抄十遍）

每个 locale 恰好 2 块，形态相同。以 `en.json` 为结构真源，其余 9 个按语言填字。

**块 1（`Folder` 对象尾）— MERGE_BOTH**

```json
"statusAwaitingReplyBadge": "<ours 译文>",
"canvas": "<theirs>",
"folderGroup": { ...theirs... }
```

先徽章、再 canvas、再 folderGroup。缺徽章会丢「待回复」；缺后两项画布和侧栏分组没有文案。

**块 2（文件尾）— MERGE_BOTH**

先完整保留 ours 的 `ToolWatchdogSettings` + `ToolWatchdogBanner` + 会话弹出（`cannotPopOutDraft` / `restartDrawCode` 等），**再**接 theirs 的整个 `"Canvas": { ... }`。JSON 最后只留一个 `}`.

不要 `TAKE_THEIRS`：会删掉看门狗和弹出窗文案。不要 `TAKE_OURS`：无限会话 UI 变 key。

---

### E. 委托 · 11 文件 43 块

**总策略：** 未冲突的 broker/listener 主体已经同时有 continue 与 resume。冲突块多数是 import、结构体字段、分发臂和测试。骨架用 ours，把 `resuming` / `ResumedSpawn` / `findByTaskId` / `isAffirmedResume` 补进去。

| 文件 | 块 | 决议 | 配方 |
|---|---|---|---|
| `delegation/broker.rs` | 1 | MERGE_BOTH | 留 `store` import；把 `ResumeDelegationRequest` 并进**已有** types import，不要第二份 types use。 |
| | 2 | MERGE_BOTH | 保留全部 fork map，在**同一 struct 闭合前**加 `resuming: HashSet<String>`。已合并的 `resume_delegation` 会 `insert`/`remove`，缺字段编不过。 |
| | 3 | MERGE_BOTH | 先 ours 的 typed `DelegationTaskStatus`，再 `task_status_from_row` 兜底旧行。 |
| | 4 | MERGE_BOTH | `accepted` + `ResumedSpawn`。 |
| | 5 | MERGE_BOTH | Task-8 / continue / complete_work 套件整段留；**后面另开** `// -- resume_delegation` 模块接 theirs ~495 行。helper 重名再改一次名。 |
| | 6 | REWRITE | 整段留 ours。theirs 那行 `contains(&"child-conn-2")` 接到块 5 最后一个 resume 测试末尾，不要放在这里。 |
| `delegation/companion.rs` | 1 | MERGE_BOTH | 留 fork 客户端；**只加** `client_resume_task_round_trip` + `BrokerResumeTaskRequest`。不要加 taskboard/automation 客户端。 |
| | 2 | MERGE_BOTH（窄） | `continue_delegation` **和** `resume_delegation` 都走 `self.delegation`。不要加 `task_progress` 等臂。 |
| | 3 | MERGE_BOTH | `complete_work` 与 `resume_delegation` 两条 match。resume 建立期要抓住 `external_handle`，cancel 才能拆掉重拉的子进程。 |
| | 4–6 | REWRITE | `tools.len() == 6`；保留 `register_simple_workflow` / `continue_delegation`；收 resume schema（只要 `task_id`，不能有 `task`）。 |
| | 7 | MERGE_BOTH | 整段 orchestration 套件留下；默认目录计数改 **6** 并断言 `resume_delegation`。 |
| | 8 | REWRITE | feedback+delegation → **7**。 |
| `delegation/listener.rs` | 1 | MERGE_BOTH | 留 fork import；加 `BrokerResumeTaskRequest` + `ResumeDelegationRequest`。`process_resume_task` 已在未冲突区。 |
| | 2 | MERGE_BOTH（手术） | **只加** `ResumeTask`。`CancelTask` 在 L664 已有，theirs 是重复臂。 |
| | 3 | MERGE_BOTH | fork 测试 import + `ResumedSpawn`。 |
| `delegation/spawner.rs` | 1 | REWRITE | 先把 `impl SpawnerError` **正确闭合**，再写 `ResumedSpawn`。marker 撕开了两边 impl，单取一侧括号会坏。两套 spawn（`spawn_resume_existing` / `spawn_for_resume`）都留。 |
| | 2 | MERGE_BOTH | continue 的 `resume_args` + resume 的 `resume_spawn_*` / `live_conversations`。 |
| `delegation/transport.rs` | 1–2 | MERGE_BOTH | 文档与两个 `client_*_round_trip` 都留。 |
| | 3 | REWRITE | theirs 的 resume/cancel 测试插进了 ours json 字面量。先还原完整的 orchestration `#[test]`，再在后面加两条 `#[tokio::test]`。 |
| `delegated-sub-thread.tsx` | 1–4 | **TAKE_OURS** | 这张卡继续用 fork chrome / `openDelegatedChildSession`。`DelegationCardRow` 给 `ResumedDelegationCard`，不要在这里降级。块 3 可留 `useSessionViewerHost()`。 |
| `delegation-context.test.tsx` | 1 | MERGE_BOTH | observation 套件 + `findByTaskId` 两条。 |
| `use-delegated-sub-session.ts` | 1 | MERGE_BOTH | 顺序：parent tool id → `selectPreferredDelegationBinding(childConversationId)` → **最后**才 `findByTaskId(fallbackTaskId)`。裸 task id 会扫到别的会话。 |
| `use-delegation-card-model.ts` | 1 | MERGE_BOTH | fork helpers + `isAffirmedResume`。 |
| | 2 | TAKE_OURS | theirs 会重复定义 hook、删掉 projection retain。 |
| | 3 | MERGE_BOTH | **不要**再插一次 hook。给已有的 `useDelegatedSubSession` 补 `fallbackTaskId: source.taskIdHint`。 |
| | 4–5 | REWRITE | 留 `sourceTaskId` + work-unit ticker；无 binding 且有 hint 时用 theirs 的 corroboration（同 `childConversationId` 或 `isAffirmedResume`）。`agentType` 回退：live → input → report → meta。 |
| `delegation-card.ts` + test | 1–9 + 1 | MERGE_BOTH | 字段并集；`interpretReport` 始终带 `{ durationMs, agentType, errorCode }`；resume 拒绝即使 status=completed 也要 `error_code === "not_resumable"`。 |

---

### F. ACP 运行时 · 11 文件 79 块（最大类）

#### `acp/mod.rs` #1 — MERGE_BOTH

```rust
pub mod autonomous_activity;
pub mod agent_mentions;
pub mod antigravity_login;
```

少 `autonomous_activity` 会断 Codex/Grok 自主回合；少后两个会断 `@` 委托和无浏览器 Antigravity 登录。

#### `background_watch.rs` #1 — MERGE_BOTH

import 并集：ours 的 `is_explicit_automation_marker` + theirs 的 `capture_title_record`。

#### `codex_model_catalog.rs`

| 块 | 决议 | 配方 |
|---|---|---|
| 1 | **TAKE_THEIRS** | 用 `EnumSpec { allowed, nullable }`。冲突后已有 `enum_spec_for` / `sanitized_override`。旧 `const ENUM_*` 留下会重复定义。 |
| 2 | **TAKE_THEIRS**（删 6 行） | ours 残留的 `strict_enum_for` 已经不存在。留着编不过，也会在 `enum_spec_for` 之后双重拒绝。 |
| 3 | TAKE_THEIRS | 文案改成 codex **0.147**；`len()==8` 若快照对不上就重数。 |

#### `connection.rs` 19 块 — 本轮最难的 Rust 文件，REWRITE

按块：

| 块 | 决议 | 配方 |
|---|---|---|
| 1 | MERGE_BOTH | `autonomous_activity::*` 与 `agent_mentions::append_agent_routes` 都 `use`。 |
| 2 | TAKE_THEIRS | `OsStr`/`to_string_lossy` 的 `~` 展开，修 Windows。 |
| 3 | REWRITE | `configured` 已是 `OsString`。`.join(ANTIGRAVITY_ACP_SUBDIR)` **只写一次**——冲突里和 marker 后都有，并集会变成 `antigravity-acp/antigravity-acp`。 |
| 4 | TAKE_THEIRS | AIR 长注释。不要广告 `agentFileChangeReport` / `nativeSubagentSessions`。 |
| 5 | **MERGE_BOTH** | `delegation_lease` **和** `delegation_enabled` 都留。冲突后已 `lease.take()`。 |
| 6 | REWRITE | 可抽 `inject_codeg_mcp_with_binary_locator`，但门仍是 launch plan。保留 `coordination_v1` / `CompanionRole` / `binding`。`companion_features_arg_for_agent` 可加 flags，不要换成只有 3 参的 theirs helper。 |
| 7 | MERGE_BOTH | 整段 ours lease + `role_arg`；`binary_path.clone()` 可以要。 |
| 8 | TAKE_THEIRS | 只加 `--custom-agents`。`--disabled-agents` 在冲突后已有，不要再查一遍。 |
| 9 | **MERGE_BOTH** | 返回两个字段。 |
| 10 | REWRITE | 留 `apply_initialized_connection_capabilities`，给它加上 `delegation_enabled`；无 companion 时 fail-closed。不要加 `AgentType::OpenClaw`。 |
| 11 | MERGE_BOTH | 留 `?` 校验 + `config_option_rejection`；emit 仍传 `agent_type`（Codex `ensure_codex_mode_option`）。 |
| 12 | TAKE_THEIRS | 注释改到 1.7.0。行为不变。 |
| 13 | **TAKE_THEIRS**（5/10 完成之后） | 走 `prepare_agent_bound_prompt`（内部已 Grok normalize + `append_agent_routes`）。不要再调一次 `normalize_grok_image_blocks`。 |
| 14 | **TAKE_OURS** | 错位：354 行不属于 poll arm。stop-reason 迁到真正的 TurnComplete 出口。 |
| 15 | **TAKE_OURS** | 错位：Grok set_config 不属于 `SuspendForDelegation`。迁到 ancillary 路径。 |
| 16 | MERGE_BOTH | Grok ask-id 测试 + Cursor `--force` 三态。 |
| 17–18 | REWRITE | 留 complete_work / v2 child 测试。`test_delegation_injection` 必须带 `CompanionLeaseRegistry`，theirs helper 现在缺 `leases`。 |
| 19 | MERGE_BOTH | 本夹具断言仍是 `["known", "auto_approve"]`（和上面的 json 一致）。theirs 后续 `AGENT_METHOD_NAMES` 等测试另接，不要把本夹具改成 `model`。 |

#### `manager.rs`

| 块 | 决议 | 配方 |
|---|---|---|
| 1 | MERGE_BOTH | **先** `strip_route_separator_from_prompt(&mut blocks)`，再算 `pending_mandatory_ids`。`send_prompt_inner` 保持 fork 的 8 个非 `self` 参数，包含 `mark_awaiting_reply`。 |
| 2 | **TAKE_OURS** | 继续 8 参调用。`register_mandatory_routes = delegation.is_none()`，并保留独立的 `mark_awaiting_reply`。theirs 3 参会让子任务装上父 profile 路由。 |

#### `registry.rs` 5 块

| 块 | 决议 | 配方 |
|---|---|---|
| 1 | MERGE_BOTH | **更新** `codex_distribution()` 到 `codex-acp@1.7.0`，**保留** `CODEX_CLI_RUNTIME_DEFAULT_ENV`（fork 默认关掉 CLI runtime）。把 theirs 1.6/1.7 注释拷进 helper。禁止直接贴 theirs 的 `env: &[]`。 |
| 2 | TAKE_OURS | **整段丢掉 OpenClaw。** |
| 3 | MERGE_BOTH | Cline 升到 **3.0.60**；**不要**加 OpenClaw 断言。 |
| 4 | TAKE_THEIRS | OpenCode **1.18.25**。 |
| 5 | TAKE_THEIRS | `uses_cursor_acp_backend` 测试。与 OpenClaw 无关。 |

同文件里未冲突、已自动升到 0.30 的钉（Hermes 0.21.0、CodeBuddy 2.143.0、Qoder 1.1.40、Grok 1.0.13）保持。

#### `session_state.rs` #1 — REWRITE

留 `visible_assistant_text`（已跳过 Thinking/Plan）+ `TurnCompletionSnapshot` + 清 `active_turn` / `active_turn_generation`。再接收 theirs 的**重复 TurnComplete 守卫**（`turn_in_flight || live_message.is_some()` 才清）。只收 theirs 会让 `active_turn` 粘住；只收 ours 会在 cancel 的第二次 TurnComplete 上抹掉好结果。

#### `commands/acp.rs` 4 块

| 块 | 决议 | 配方 |
|---|---|---|
| 1–2 | TAKE_THEIRS 为底 | 采用 1.7 的 preset 映射（`read-only` 不再等于只读沙箱，见上游长注释）。fork 的「profile-backed 不改 root」约束写进新注释，行为跟 theirs 走。 |
| 3 | TAKE_THEIRS | 新增 `initial_mode_for` 单测。 |
| 4 | TAKE_THEIRS | Hermes 钉到 `hermes-agent@0.21.0`。 |

#### `acp-agent-settings.tsx` 21 块 — 几乎全是「ours 空、theirs 加 Codex 旋钮」

| 块 | 决议 |
|---|---|
| 1–4、6–16、19 | **TAKE_THEIRS**：`default_mode_request_user_input`、`service_tier=fast`。块 16 还要闭合 `handleCodexSkillsChange`，ours 空会语法坏。 |
| 5 | MERGE_BOTH：`model_reasoning_effort` 臂 **和** `service_tier=fast` 臂都留。 |
| 17 | **TAKE_OURS**：只改 `envText: patchCodexCliRuntimeEnv(...)`。theirs 把 toml 同步体贴进这个 handler，`synced`/`nextToml` 不在作用域。 |
| 18 | **TAKE_OURS**：整行「启用 CLI runtime」留下，后面再接 skills/fast 行。 |
| 20 | **TAKE_OURS**：整段 `open_claw` UI 丢掉。 |
| 21 | **TAKE_OURS**：保留 Grok CLI proxy URL 的完整 label/input/hint；theirs 只剩 API key label，会丢 fork 配置项。 |

#### `acp-connections-context.tsx` + test · 22 块 — REWRITE

这是前端连接层的结构分叉：

| 概念 | Ours | Theirs | 合并后 |
|---|---|---|---|
| 归档恢复 | `loadErrorCode` | `loadErrorCommand`（可复制 `codex unarchive`） | **两个字段都留** |
| 保活注册 | `registerLiveSinks` | `registerLiveSurfaceKeys(source, keys)` | **两个 API 都留**（弹出窗靠 sinks；画布/非 tab 靠 surface keys） |
| 所有权 | pop-out claim/reclaim | `localOwnerKeyOf` / viewer 降级 | **ours 弹出所有权状态机 + theirs viewer/owner 判定** |
| 测试 | 根活动边界、watchdog 去重（2000+ 行） | 归档恢复、stuck-on-responding（1000+ 行） | **两套都留** |

块级速查：#1 TAKE_THEIRS（注释）；#2–6 MERGE_BOTH（`code`+`command` 成对；CLEAR 用 `writableConnections` 并清两个字段）；#7–8 MERGE_BOTH（两套 register）；**#9 TAKE_OURS**（错位，见 3.4；结束 delivery-failure，不要在这里贴 `handleMappedEvent`）；#10 REWRITE（keepalive 用 theirs deps，**下一 hook** 整段留 pop-out 效果）；#11 MERGE_BOTH（deps 并集）；#12 TAKE_THEIRS（`localOwnerKeyOf`，修「任务看客抢走 owner」）；#13 REWRITE（留 payload 校验 + `skipOrphanReattachTo`，所有权判定用 `localOwnerKey !== contextKey`）；#14–16 MERGE_BOTH（导出并集）。

测试：#1–3 import/mock/`beforeEach` 并集（`§` 渲染 **和** `tCalls`；`acpTouchConnection=true`）。#4–5 按 `describe` 边界拼接，不要在 `emitAcpEvent` 中间切开。#6 TAKE_THEIRS（stuck-on-responding / canvas keepalive）。

---

### G. Forge · 6 文件 10 块

0.30 的主题：自建 GitLab 探活（不再误当 GHE）、未知宿主显式报错、评论虚拟列表、`into_row` 去重。

| 文件 | 块 | 决议 | 配方 |
|---|---|---|---|
| `forge/auth.rs` | 1 | TAKE_THEIRS | 接 136 行宿主探活。ours 只是 `HostProfile {..}` 尾巴。 |
| `forge/deliver.rs` | 1 | TAKE_THEIRS | 写法等价，跟面板 composer 共用。 |
| | 2 | TAKE_THEIRS | 回写走共享写路径，避免链到 review-comment。校验逻辑若 ours 更严，收成辅助函数再调用共享写。 |
| `forge/github.rs` | 1 | TAKE_THEIRS | `r.into_row(is_pr)`。确认 `merged` 仍从 `pull_request.merged_at` 来。 |
| `forge/gitlab.rs` | 1、3–4 | TAKE_THEIRS | `into_row`；测试从 `ForgeComment.html_url` 取链。 |
| | 2 | REWRITE | 收单行 URL。**丢掉**冲突里那个 1 字段 `RawNote`——后面已有完整 `RawNote`，留下会遮蔽 `into_comment`。 |
| `forge/mod.rs` | 1 | MERGE_BOTH | 留 `NoAccount`，加上 `UnsupportedHost` / `WrongForge`。 |
| `clone-dialog.tsx` | 1 | MERGE_BOTH | `openFolderWithDraft`（ours）+ `joinFsPath` / `useAppWorkspaceStore`（theirs）。 |

---

### H. 工作任务 · 2 文件 10 块

`deliver_pr` **实现已经是 4 参**（`pr_title, draft, delete_worktree`）。`remove_worktree_and_branch` 已经是 4 参（`expected_tip`）。下面冲突几乎都是调用点还停在 3 参，ours 现在编不过。现有测试第四参默认 `false`，除非用例就是「交付时删工作树」。

| 文件 | 块 | 决议 | 配方 |
|---|---|---|---|
| `commands/work_task.rs` | 1 | TAKE_THEIRS | `.map_err(|blocked| DbError::Validation(blocked.message))`，保留结构化拒绝。 |
| `work_task/engine.rs` | 1–2 | MERGE_BOTH | import 并集：`build_session_runtime_env`、`get_folder_conversation_core`、`work_task::compact`。 |
| | 3 | TAKE_THEIRS | 删除工作树传 `expected_tip`。 |
| | 4–9 | TAKE_THEIRS | 所有 `deliver_pr(..., false)` 改成 `deliver_pr(..., false, false)`；块 8 整段收「空交付必须失败」的新测试。 |

---

### I. 解析器 · 2 文件 6 块

| 文件 | 块 | 决议 | 配方 |
|---|---|---|---|
| `parsers/claude.rs` | 1 | TAKE_THEIRS | 非 assistant/user 清 `pending_assistant_message_id`；接 `/rename` 标题。 |
| | 2 | MERGE_BOTH | 保留 ours `pending_autonomous_origin`；接 theirs `message.id` 合并键。 |
| | 3 | MERGE_BOTH | theirs thinking-fragment 合并（#494/#586）+ ours `UnifiedMessage` / autonomous origin。同一 `message.id` 的 thinking 必须并进一张卡。 |
| | 4 | TAKE_THEIRS | 698 行新夹具。过一遍，确认没有覆盖 autonomous 断言；缺则补。 |
| `parsers/codex.rs` | 1 | MERGE_BOTH | `AutonomousTurnOrigin` 与 `agent_mentions::{contains_only_internal_agent_routes, strip_internal_agent_routes}` 都 `use`。`@` 提及不能漏进气泡/侧栏标题。 |
| | 2 | MERGE_BOTH | 同一 normalized user text 同时走 fork 的 `visible_user_text` 和上游标题候选提取；不能为了标题恢复内部路由文本。 |

**自动合并后的债：** `build_agent_parser` **已经在** `parsers/mod.rs`，并且还带着 OpenClaw 臂——当前 `AgentType` 没有这个变体，**直接用会编不过**。做法：删 OpenClaw 臂；DeepSeek 在 history/import 路径上仍用 `DeepSeekParser::from_runtime_env` **覆盖** factory 的 `DeepSeekParser::new()`。Qoder/Antigravity 已在 `BUILTIN_AGENT_TYPES` 里。factory 外包 `RouteSanitized`。

---

### J. 文件夹 / DB · 4 文件 8 块

| 文件 | 块 | 决议 | 配方 |
|---|---|---|---|
| `folder_service.rs` | 1 | MERGE_BOTH | **整段保留** ours `normalize_folder_storage_path` / alias lookup（Windows `\\?\`）。紧接着放 theirs `next_sort_order()`（folder **和** folder_group 共享序号，分组才能和未分组穿插）。 |
| | 2–3 | REWRITE | `next_sort_order` + `is_open = matches!(mode, ForceOpen)`。theirs 的 `Set(true)` 会让 RegistrationOnly 也张开，破坏 fork 可见性。 |
| `import_service.rs` | 1–2 | REWRITE | 改用本地 `build_agent_parser`，但 `build_parser(agent, deepseek_env)` 仍要把 env 传进 DeepSeek。不要收成无参 `build_agent_parser(agent_type)` 而丢掉 DeepSeek 根。 |
| `web/handlers/folders.rs` | 1 | REWRITE | 完整留下 `close_folder_if_empty` **和** `list_folder_groups`。**丢掉**半截 `ReorderFoldersParams`——`commands/folders.rs` 里没有对应命令，marker 后的 `}` 会错关。`reorder_folders` 若还在宏里，那是另一处编译债，不是这块能修的。 |
| `db/migration/mod.rs` | 1–2 | MERGE_BOTH | 模块声明和迁移列表都保留 fork 的 turn/delegation migration，并按时间顺序接上上游 remote-header、folder-group、canvas migrations。历史 completion fixture 还要显式安装其依赖的 folder-group migration。 |

---

### K. Rust 接线 · 8 文件 21 块

| 文件 | 块 | 决议 | 配方 |
|---|---|---|---|
| `automation/engine.rs` | 1 | MERGE_BOTH | 留 `automation_root_title_admission`；收 `path_separator`。**不要**留 ours 那个只按 `/` 切的 `basename`——冲突后已有 Windows 版，会重复定义。 |
| `commands/conversations.rs` | 1 | REWRITE | 收 factory + 只留 `CodexParser`（`load_thread_name_index`）。DeepSeek 仍要具名 import。 |
| | 2–3 | REWRITE | factory 循环 + DeepSeek `from_runtime_env` 覆盖；保留 shared-filter `guard`。不要加 OpenClaw。 |
| | 4–5 | REWRITE | 留 `DelegationMetaSnapshot` + `synthetic_historical`；`task_preview` 继续 `None`（标题不是 task 文本）。`to_value` 之后可补 `agent_type` 给 resume 卡。 |
| | 6 | REWRITE | **两套 helper 都留**（fork collector + `find_task_id_in_value` / `parse_resume_task_id`）。raw TAKE_THEIRS 会把剩余 `_from_text` 身体留在 `inject_delegation_meta` 里，`trimmed` 未绑定。 |
| | 7 | REWRITE | 收 resume 绑定；留 fork 的歧义 id 拒绝和 durable-status 覆盖。 |
| | 8 | REWRITE | 留 guard / `reject_internal_detail` / DeepSeek env；改 factory；外层解构扩成 **6 元组**（含 `parsed_model`）。fallback `matches!` **不要**加 OpenClaw。 |
| | 9 | TAKE_THEIRS | transcript model 覆盖 DB。依赖 #8 的 6 元组。 |
| | 10 | REWRITE | 收测试，但不要断言 title-as-`task_preview`。 |
| `commands/mcp.rs` | 1 | TAKE_OURS | theirs 是 OpenClaw/Kimi 抢 `auth` 键的注释。OpenClaw 已删，不要把这条例外加回来。 |
| `commands/mod.rs` | 1 | MERGE_BOTH | **只加 `pub mod open_in;`**。`office_tools` 在文件后部 ours 已有，再收一次会重复定义。 |
| `lib.rs` | 1 | MERGE_BOTH | `use` 并集：保留 `conversation_popout`、`forge`、`tool_watchdog`；加上 `canvas as canvas_commands`、`open_in`。 |
| | 2 | **TAKE_OURS** | 继续 `production_tauri_commands!(generate_production_invoke_handler)`。**禁止**收 473 行展开列表（fork 靠宏避免桌面误编 server 命令）。把 canvas / open_in / 新 folder-group / 新 forge 命令 **登记进宏**。 |
| `logging/init.rs` | 1–3 | TAKE_THEIRS | 收 `clamped_backstops` 表（已含 `codeg_lib::logging=off`）。同步改 `build_env_filter` 若还把 `TARGET_BACKSTOPS` 当 `&str`。 |
| | 4 | REWRITE | 整段收 theirs，**删掉** marker 后残留的旧 `kill_tree` assert（会落到错误的测试里或双重闭合）。 |
| `models/mod.rs` | 1 | MERGE_BOTH | `RemoteWorkspaceHeader` / `ToHeaderMap` + 现有 `GitHub*` / `SystemLanguage` re-export。 |
| `web/router.rs` | 1 | MERGE_BOTH | **只收 canvas 九条路由。** `folder_links` 在约 715 行已有，再收会重复注册。`close_folder_if_empty` / `list_folder_groups` 已在冲突点之前。 |

---

### L. 聊天 UI · 18 文件 61 块

主题：0.29 回复折叠标题 + 0.30 `@` 路由/画布；fork 的弹出、watchdog、mermaid 密封、`autolinkLocalPaths`、选择器 disabled 守卫。

#### Markdown / Streamdown

| 文件 | 决议 | 配方 |
|---|---|---|
| `message-thread.tsx` | MERGE_BOTH | 留 fork 文档。**必须**收 `{ className, children, ...props }`：函数体用了裸 `children`，只收 ours 会未绑定。 |
| `message.tsx` #1–4 | MERGE_BOTH | **两套 API 都留：** `richContentState` + `autolinkLocalPaths`（ours，密封流式/本地路径）以及 `mode` / `parseIncompleteMarkdown`（theirs，修 `foo/*` `_meta` 被 remend）。默认 finished = static + 不 parse incomplete；live 仍走 streaming。memo 比较四字段。 |
| `message.test.tsx` | MERGE_BOTH | mock 同时接受 `remarkPlugins` 与 `mode`/`parseIncompleteMarkdown`。两边用例都留（密封 mermaid + remend 回归）。 |
| `content-parts-renderer.tsx` | MERGE_BOTH | props 并集：`autolinkLocalPaths` / `autolinkLocalPathParts` / `showThinking` / `parentConversationId` + `isStreaming`。传给 `MessageResponse` 时：streaming → `mode="streaming"`；autolink 仍按 ours 集合。块 4 TAKE_THEIRS（Codex 截断脚本说明）。 |

#### 选择器 / 输入框

| 文件 | 决议 | 配方 |
|---|---|---|
| `adapters.ts` | TAKE_THEIRS | 注释：可读 `@` 链接就是路由锚。 |
| `message-input.tsx` | MERGE_BOTH | 收 SelectorTooltip / 打开时压 hint；保留 fork 触发器结构。 |
| `mode-selector.tsx` / `model-option-picker.tsx` / `session-config-selector.tsx` | MERGE_BOTH | **保留 ours `disabled` 时强制 `setOpen(false)`**；包上 theirs `SelectorTooltip`。少守卫会在禁用时仍弹出。 |
| `sub-agent-overlay.tsx` | MERGE_BOTH | 保留 `openDelegatedChildSession`；行布局可收 theirs（一张卡、task 截断）。0.30「resume 只画一张卡」靠 `seenTaskIds`，见 message-list-view。 |

#### 会话面板 / 侧栏

| 文件 | 决议 | 配方 |
|---|---|---|
| `conversation-detail-panel.tsx` #1–3 | **TAKE_OURS** | 本文件在冲突后已经 `return <ConversationSessionSurface …>`。把 1700 行 theirs 贴进来会留下一堆未用 hook，再仍返回 surface，是坏的杂交体。**ask-selection / `loadErrorCommand` / `PiProjectTrustBanner` / 更严的 `ownFolderId` 迁到 `conversation-session-surface.tsx`。** 父面板未冲突区已有 `DelegationRouteMenu`、`copyTextFromMenu`、watchdog 投影。 |
| `session-details-content.tsx` | TAKE_OURS | 保留 `Folder.statusLabels` + `ToolWatchdogBanner`。 |
| `sidebar-conversation-card.tsx` | REWRITE | 以上游为底（hover 气泡、pin、子会话徽章）。菜单里 **加回** ours「弹出窗口」。`data-conv-key` 保留。 |
| `sidebar-conversation-card.test.tsx` | MERGE_BOTH | import 并集。**两套 describe 都留**：pop-out 菜单 + hover 气泡。`renderCard` 同时接 `isOpenInTab`/`mainTabCount` 与 `timeLabel`。 |
| `sidebar-conversation-list.tsx` | MERGE_BOTH | 保留 ours optimistic activity / FLIP；接 theirs git HEAD 标签、drag 目标、`recent-` key 前缀（块 3 TAKE_THEIRS）。 |

#### `message-list-view.tsx` 13 块 — REWRITE

这是聊天时间线的结构分叉。合并后一行必须同时具备：

- Ours：`LiveTranscriptRow` / `renderKind` / `showThinking` / `agentType` / `delegationIdentityIndex`+`runRecords` / autonomous origin
- Theirs：`CompletedTurnContent` + 回复折叠（`currentRound`/`roundOpen`/`foldEpoch`）/ `parseResumeTaskId` / `seenTaskIds` 去重 resume 卡

| 块 | 要点 |
|---|---|
| 1 | 两个 content 组件都 import。 |
| 2 | import 并集。 |
| 3–5 | `collectDelegationSources` 签名：保留 `parentConversationId`，加上 `seenTaskIds`。导出 `extractDelegationSources`（theirs 测试用）。 |
| 6–7、11–12 | `HistoricalMessage` props **并集**，deps 并集。 |
| 8 | 用户资源链接（ours）+ `CompletedTurnContent` 折叠（theirs）。 |
| 9–10 | `threadState` 可收；投影字段和 `lastAssistantReplying` 都要。 |
| 13 | 外层同时保留 `SessionViewerHost` / `GrokConversationProvider` 与 `SelectionActionBubble`。 |

测试：#1–3 MERGE_BOTH（import/夹具并集，保留 `autonomous_origin`）。#4 TAKE_THEIRS（in-flight→completed 缓存）。#5 MERGE_BOTH（ours interrupted outcome + theirs `extractDelegationSources`）。

#### `turn-stats.tsx`

| 块 | 决议 | 配方 |
|---|---|---|
| 1 | TAKE_OURS | `Plane`/`Timer`（生成/耗时）。 |
| 2 | MERGE_BOTH | 空行条件：有 Copy/Usage/CompletedAt/Jump **或** Model/Effort/Generation/Duration 才渲染。 |
| 3 | TAKE_OURS | 保留生成耗时按钮。0.29 把 duration 挪到折叠标题后，fork 统计条仍要模型/档位/生成。 |

---

### M. 前端状态 · 13 文件 29 块

| 文件 | 块 | 决议 | 配方 |
|---|---|---|---|
| `app-workspace-context.tsx` | 1 | MERGE_BOTH | `FolderCloseCause` + `FolderGroupChange` 都 import。 |
| `tab-context.tsx` | 1 | MERGE_BOTH | `closeTab(id, { recordForReopen? })`。弹出 detach 仍走现有 `detachTab`。`openDraft` 若 ours 返回 `Promise<boolean>` 则保留。 |
| `tab-context.test.tsx` | 1 | MERGE_BOTH | 两用例都留：folder recent agent + forced agent（ask-selection）。 |
| `workspace-context.tsx` | 1–3 | MERGE_BOTH | 脏文件确认 + `pushClosedTab`/`snapshotFileTab`（Ctrl+Shift+T）+ ours `finishOpenSettleClosed`。 |
| `use-connection.ts` | 1–4 | MERGE_BOTH | `loadErrorCode` **与** `loadErrorCommand` 一起暴露。 |
| `use-message-queue.ts` | 1–4 | MERGE_BOTH | `optimisticTurnId`（ours 乐观气泡）+ `adoptSendTimeMode`（theirs ask-selection）。options 并集。 |
| `api.ts` | 1 | MERGE_BOTH | 保留 `closeFolderIfEmpty` 文档/函数，**后面接** theirs 全部 `listFolderGroups` / create / rename / delete / set 分组 API。 |
| `selector-prefs-storage.ts` | 1 | MERGE_BOTH | 先 ours 过滤，再 `healLegacyValues`。 |
| `types.ts` | 1 | MERGE_BOTH | 保留 `HistoryWindowInfo`，**后面接** 整段 Canvas 类型。 |
| `app-workspace-store.ts` | 1–2 | MERGE_BOTH | activity 时钟 import + `forgetClosed*` + `FolderThemeColor`。 |
| | 3 | MERGE_BOTH | 保留 optimistic activity 时钟；接 closed-tab 删除戳。 |
| | 4 | TAKE_THEIRS | 分组删除戳，防 `fetchFolders` 把已删 band 刷回。 |
| `app-workspace-store.test.ts` | 1–2 | MERGE_BOTH | mock 并集：`listFolderGroups` / `getGitHead` / `openFolder` + 现有 list/get。 |
| `conversation-runtime-store.ts` | 1 | MERGE_BOTH | `background-agent` 保留 fork 的完整 helper import，并加入上游 `imageCardLabel`。 |
| | 2 | TAKE_THEIRS | `retainKey` 廉价键（长 thinking 性能，0.30 修复）。确认不影响 ours 窗口化/乐观 id。 |
| `tab-store.ts` | 1 | MERGE_BOTH | 签名并集：`detachTab`/`restoreDetachedTab`/`flushOpenedTabsSave` + `closeTab(..., {recordForReopen})`。 |
| | 2–3 | MERGE_BOTH | 关 tab 时：ours draft-leave 文件夹逻辑 + theirs `pushClosedTab`。 |
| | 4 | TAKE_THEIRS | `force` agent 压过 folder default（ask-selection）。 |

---

### N. 布局 · 1 文件 1 块

**`workspace-chrome-controller.tsx` #1 — MERGE_BOTH**

以上游「一条 keydown 里处理 close + 1–9 跳 tab + Shift+T 重开」为骨架，把 ours 的会话/文件 pane 关 tab 行为嵌进 `close_current_tab` 臂。不要只收 ours，会丢掉 0.29 快捷键。

---

## 5. 建议施工顺序

不要按字母，按编译依赖：

1. **机械（可当天清）**
   A+B+C+D + `commands/mod.rs` + `Cargo.lock` 版本行。
2. **接线**
   `lib.rs`（宏 + use）→ `router.rs`（只加 canvas）→ `models/mod.rs` → `logging/init.rs` → `api.ts`/`types.ts`。
3. **OpenClaw 扫除**
   `registry` #2、settings #20、`mcp.rs` #1。确认 `AgentType` 枚举、i18n、图标没有被自动合并加回来。
4. **文件夹**
   path normalize + `next_sort_order` + handlers MERGE_BOTH + store 分组。
5. **Forge / work_task / parsers**
   探活、调用点改 4 参 `deliver_pr`、Claude thinking 合并、**删掉** `build_agent_parser` 的 OpenClaw 臂。
6. **委托协议（先 Rust 后 TS）**
   transport → listener → spawner → companion → broker → card/hooks。默认 `tools/list` 先绿到 6 名。
7. **ACP connection / manager / registry**
   先改 `CompanionInjection`（块 5–10），再 `prepare_agent_bound_prompt`，再 1.7 catalog。错位块 14/15 只收 ours。
8. **前端连接与时间线**
   `acp-connections-context`（#9 错位只收 ours）→ `message-list-view` → **先改 `conversation-session-surface.tsx`，再对本文件块 3 TAKE_OURS** → 侧栏卡。
9. **选择器 / Streamdown / 设置旋钮**（CLI runtime 块 17–18 收 ours）
10. **`pnpm test:release` + 定向单测 + `cargo test --lib --features test-utils`**（先窄后宽）。

### 最容易「看起来解决了、文件其实坏了」的块

| 文件 | 为什么 |
|---|---|
| `connection.rs` #14/#15 | 错位，theirs 插进错误 match 臂 |
| `acp-connections-context.tsx` #9 | `handleMappedEvent` 会拆进 delivery-failure |
| `conversation-detail-panel.tsx` #3 | 1700 行贴进已经是 thin wrapper 的文件 |
| `conversations.rs` #6/#8 | leftover 函数体 / 元组宽度 |
| `folder_service.rs` #1、`folders.rs` handler | 一侧函数未定义；半截 Reorder params |
| `companion.rs` tools.len | 4 或 5 都表示掉了一个工具 |
| `spawner.rs` #1、`transport.rs` #3 | 括号/测试插进 json |
| `lib.rs` #2 | 展开 handler 会吞掉 fork IPC |
| `logging/init.rs` #4、`gitlab.rs` #2、`automation/engine.rs` #1 | leftover assert / 遮蔽 RawNote / 重复 basename |
| `build_agent_parser` OpenClaw 臂 | 当前 `AgentType` 编不过 |

---

## 6. 绝对不要做的事

- `git checkout --theirs` 整份 `connection.rs` / `broker.rs` / `acp-connections-context.tsx` / `message-list-view.tsx` / `conversation-detail-panel.tsx` / `lib.rs`。
- 把 1700 行 theirs 贴进 `conversation-detail-panel.tsx`（正确入口是 `conversation-session-surface.tsx`）。
- 在 `connection.rs` #14/#15 或 context #9 这些**错位格**收 theirs。
- 用 theirs 的展开 `generate_handler!` 替换 `production_tauri_commands!`。
- 为了让 `tools.len()==4` 或 `==5` 变绿而删掉 `continue_delegation` 或 `resume_delegation`。
- 用空 task 调 `continue_delegation` 冒充 resume。
- 恢复 OpenClaw，或保留 `build_agent_parser` 的 OpenClaw 臂。
- 把版本收成上游的 `0.30.0`（发行与更新器会指错仓库）。
- 把 `delegated-sub-thread.tsx` 收成 `DelegationCardRow`（那是 resume 卡的组件）。

---

## 7. 收完后的验证

按 `AGENTS.md` 收窄范围，不要一上来全量：

```text
pnpm test:release
pnpm exec vitest run src/i18n/messages.test.ts src/lib/delegation-card.test.ts src/contexts/delegation-context.test.tsx src/contexts/acp-connections-context.test.tsx src/components/message/message-list-view.test.tsx
cd src-tauri && cargo test --lib --features test-utils acp::delegation::broker::tests::resume_
cd src-tauri && cargo test --lib --features test-utils continue_delegation
cd src-tauri && cargo test --lib --features test-utils complete_work
cd src-tauri && cargo test --lib --features test-utils -- tools_list
```

`tools/list` 必须正好是那 6 个父工具名。4 或 5 都算失败。

手测：

1. DrawCode 名称、图标、更新器 URL 仍是 MyCodeBuddy。
2. 侧栏：文件夹分组 + 「待回复」徽章 + 弹出菜单。
3. 无限会话画布能打开、能拖。
4. `@` 一个智能体 → 子会话可打开，气泡里没有内部路由字。
5. 取消一个委托子任务 → `resume_delegation` 仍是同一 `task_id`、一张卡。
6. `continue_delegation` / complete_work / CompanionLease 仍能走通。
7. 工具看门狗设置页还在。
8. 智能体列表 **没有** OpenClaw。
9. 自建 GitLab 不再 410；未知宿主有明确错误。
10. 工作树有未提交改动时删除被拒。

---

## 8. 块数对照（验收用）

| 类 | 文件 | 块 | 默认口令 |
|---|---:|---:|---|
| A 仓库卫生 | 1 | 1 | MERGE_BOTH |
| B 版本清单 | 4 | 4 | MERGE_BOTH → `0.30.0-mycodebuddy.1` |
| C 品牌资源 | 1 | 二进制 | REWRITE |
| D i18n | 10 | 20 | MERGE_BOTH（抄 en） |
| E 委托 | 11 | 43 | MERGE_BOTH / REWRITE |
| F ACP 运行时 | 11 | 79 | REWRITE + OpenClaw TAKE_OURS |
| G Forge | 6 | 10 | 多数 TAKE_THEIRS |
| H 工作任务 | 2 | 10 | TAKE_THEIRS arity |
| I 解析器 | 2 | 6 | MERGE_BOTH |
| J DB/文件夹 | 4 | 8 | MERGE_BOTH |
| K Rust 接线 | 8 | 21 | 宏 TAKE_OURS，路由 MERGE |
| L 聊天 UI | 18 | 61 | MERGE_BOTH；详情面板 #3 TAKE_OURS |
| M 前端状态 | 13 | 29 | MERGE_BOTH |
| N 布局 | 1 | 1 | MERGE_BOTH |
| **合计** | **92** | **293 个文本块 + 1 个二进制冲突** | |

实际合并已落在 `8d8b691c`；后续若重放合并，按第 5 节顺序清冲突后再提交。
