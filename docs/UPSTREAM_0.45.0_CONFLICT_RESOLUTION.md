# main 合入 Codeg v0.45.0：冲突解决手册

> 本文描述的是 `cursor/sync-codeg-0.45.0-9255` 上 `git merge --no-ff upstream/main` 当时怎么收冲突。
> 不要用后续编译/测试收口提交当合并提交。

`<<<<<<< HEAD` = MyCodeBuddy / DrawCode fork（合入前 `0.35.1-mycodebuddy.1`，`35b654224`）。
`>>>>>>> upstream/main` = 上游 Codeg `v0.45.0`（`bd88b9dac`）。

四字口令仍是 **TAKE_OURS / TAKE_THEIRS / MERGE_BOTH / REWRITE**。错位块给「这一格的口令 + 功能迁到真正的臂」，而不是整文件 `--ours` / `--theirs`。

---

## 1. 合入事实

| 项 | 值 |
|---|---|
| Ours（merge 第一父） | `35b654224` `superpowers 7.0.1`（`origin/main`，版本 `0.35.1-mycodebuddy.1`） |
| Theirs | `bd88b9dac`（`upstream/main` = tag `v0.45.0`，`# Release version 0.45.0`） |
| 共同祖先 | `e21eb6b38`（tag `v0.35.1`） |
| 打开合并时的工作树 | **253** 个未合并路径、约 **1469** 个 `<<<<<<<` 文本块。类型：双方都改、我们删了（`install.sh`、`parsers/openclaw.rs`）、上游删了 9 个 UI 文件、少量 add/add |
| 空侧 / 完全相同 / 纯版本号 | 先机械收掉 **115** 块（空侧取非空侧，OpenClaw 空侧丢弃；两边文本相同取 ours；只差版本或品牌的改成 `0.45.0-mycodebuddy.1` + DrawCode）。其余块两边都改过，不能靠「一边是另一边的超集」自动收 |
| 版本结果 | `0.45.0-mycodebuddy.1` |

上游相对共同祖先大约 10 个小版本。上一轮 fork 的冲突决议在第一父链上，不是上游的祖先，所以 0.35.1 上已经手动收过的行会再次冲突。

---

## 2. 全局原则（本轮再钉死）

1. **品牌 / 发行身份永远 ours。** 版本写成 `0.45.0-mycodebuddy.1`（`package.json` / `src-tauri/Cargo.toml` / `src-tauri/Cargo.lock` 的 `codeg` package / `src-tauri/tauri.conf.json` / `install.ps1` / README 安装示例）。不要收成光秃 `0.45.0`。`productName` / `mainBinaryName` = `DrawCode`，`identifier` = `app.mycodebuddy`。仓库、主页、更新 endpoint 仍是 `https://github.com/icannotwait/MyCodeBuddy`。更新公钥用 fork 的，不用上游 `spacering-net` / `app.codeg` 那把。
2. **默认 Tauri 配置 `bundle.createUpdaterArtifacts` 保持 `false`。** 发布 overlay 仍是 `true`（`scripts/release-policy.mjs` 这么断言）。`requireSignedVersion` 接上游。`macOSPrivateApi` 不留：Tauri 2.12 不再要这个 cargo feature。
3. **OpenClaw 保持删除。** 不恢复 `AgentType::OpenClaw`、`parsers/openclaw.rs`、`open_claw`。MCP 应用列表长度 19，不含 OpenClaw。Kimi 共享 `auth` 不再被已删除的 OpenClaw reader 覆盖，扫描传入空 owner map。
4. **`codeg-server` 入口留在 `src/server_bin/main.rs`。** 不要收成上游的 `src/bin/codeg_server.rs`（Tauri CLI 会扫 `src/bin/`，桌面包装进远程监听进程）。Feature：`server = []`，`server-bin = ["server"]`。`--features server` 和 `--features server-bin` 都能编这个二进制。监听进程只 bind 一次。
5. **Fork 行为留着，上游能力接进来。** Roundtable、委托准入 / runtime gate、codeg-boot、Superpowers、冷重连、Grok settle、Windows NSIS 发布、签名、许可证文件都留。上游新能力要接：ACP 3.4（`unstable_session_fork` + `unstable_end_turn_token_usage`；session notices / compaction 已是 schema 1.11 稳定面，不要开回旧 feature）、`codeg-cli` / `cli-bin`、pdfjs 的 postinstall、`@viz-js/viz`、Tauri 2.12。
6. **`lib.rs` 继续 `production_tauri_commands!`。** 不要用上游展开的 `generate_handler!` 换掉它。新命令登记进宏。已经不存在的 `opencode_install_plugins` / `opencode_uninstall_plugin` 删掉；OpenCode 插件命令是 `opencode_list_plugins` / `opencode_add_plugin` / `opencode_remove_plugin`。
7. **锁文件不手合。** `pnpm-lock.yaml` 取上游后再 `pnpm install`。`Cargo.lock` 取上游后再 `cargo generate-lockfile`，`codeg` 包版本由清单写成 `0.45.0-mycodebuddy.1`。

---

## 3. 按类怎么收

### A. 身份与清单

| 文件 | 决议 | 配方 |
|---|---|---|
| `package.json` | MERGE_BOTH | 版本 `0.45.0-mycodebuddy.1`；留 `license` / `repository` / `homepage`。依赖版本取上游（Tauri 2.12、Tiptap 3.32、Vitest 4、mermaid 1.0.3）。留 fork 的 `licenses:generate`、`test:release`、`release:check`、`tauri:before-build:fast`、`tauri:build:fast`。`server:build` / `server:dev` 仍是 `--features server`。`postinstall` 取上游，多拷贝 pdfjs。`@tiptap/markdown` 升到 `3.32.0`（fork 仍 import）。`@streamdown/code` 留着。 |
| `src-tauri/Cargo.toml` | MERGE_BOTH | 以上游清单为底。版本、license、repository、homepage 用 fork。`test-utils` = fork 的 `tauri?/devtools` + `tauri?/test`，再加 `computer-helper`、`server`、`server-bin`、`mcp-bin`、`cli-bin`。留 `unmerged-upstream-helpers`。`cli-bin = ["dep:clap"]` 和 `codeg-cli` 二进制接上游。`codeg-server` 的 path 仍是 `src/server_bin/main.rs`，`required-features = ["server"]`。ACP `3.4.0`。tokio 补上 fork 在用的 `time`。`roundtable-protocol`、`pulldown-cmark`、`anyhow`、`unicode-normalization`、`similar`、`notify-rust` 留。`kill_tree` 仍钉 `=0.2.4`，`[patch.crates-io]` 仍指向 `vendor/kill_tree`。hmac / ring / boon / jsonc-parser / hyper-util 接上游。 |
| `src-tauri/tauri.conf.json` | TAKE_OURS + 上游增量 | DrawCode / `app.mycodebuddy` / `0.45.0-mycodebuddy.1` / NSIS / 许可证资源 / fork 更新 endpoint 与公钥。`externalBin` 加上 `binaries/codeg-cli`。`createUpdaterArtifacts: false`。`targets` 仍是 `nsis`。 |
| `src-tauri/Cargo.lock` / `pnpm-lock.yaml` | 重生 | 见第 2 节第 7 条。 |
| `.gitattributes` | MERGE_BOTH | `src-tauri/experts/**` 和 `src-tauri/science/**` 都 `text eol=lf`。冲突标记本身会被 git 当成属性名，先收这个文件。 |
| `install.sh` | REWRITE | Fork 在发行身份提交里删过它。上游安装器接回来，仓库改成 `icannotwait/MyCodeBuddy`。二进制名仍是 `codeg-server` / `codeg-mcp` / `codeg-cli`。 |
| `install.ps1` | MERGE_BOTH | DrawCode 桌面说明、`v0.45.0-mycodebuddy.1`、MyCodeBuddy 的 raw URL。扫描列表含 `codeg-cli`。 |
| `README.md` 与 `docs/readme/*` | MERGE_BOTH | 下载链接留 MyCodeBuddy / DrawCode。安装示例版本改成 `v0.45.0-mycodebuddy.1`。从源码构建补上 `codeg-cli`（`--features cli-bin`）。文档站链接仍是 fork 原来就有的 `docs.codeg.app`。 |
| `.github/workflows/release.yml` | TAKE_OURS + `codeg-cli` | Windows NSIS 发布工作流。服务器 job 用 `--features server`，并打包、签名 `codeg-cli`。不收回上游的 Apple 签名、Docker Hub、Linux/macOS 桌面 job。 |
| `Dockerfile` | MERGE_BOTH | 服务器 feature 仍是 `server`，入口仍是 `src/server_bin/main.rs`，接上游镜像/构建更新。 |

### B. i18n ×10

十个 `src/i18n/messages/*.json` **不要按冲突块贴**。从 `HEAD`、`upstream/main`、merge-base `e21eb6b38` 做 JSON 深并集：

- 键只在一边：ours 相对 base 没改且上游删了 → 接受删除；ours 改过且上游删了 → 留 ours；ours 删了且是 OpenClaw → 继续删；否则取上游。
- 叶值：ours==base → theirs；theirs==base → ours；两边都改且 ours 含 `DrawCode` / `MyCodeBuddy` / `mycodebuddy`，或路径像 Roundtable / Superpower / Boot / Grok / Admission → ours；其余两边都改 → theirs。

本轮叶键是并集：base 5990，ours 6730，theirs 6724，合并后 **7464**。英文没有「同一键两边都改」的叶。繁体中文 `RemoteWorkspace.*` 有 11 个键两边都译过，取上游措辞。

### C. 上游删掉的 UI

| 文件 | 决议 |
|---|---|
| `mode-selector.tsx`、`model-option-picker.tsx`、`session-config-selector.tsx`（及 test） | 接受删除。输入框改用 `SessionSettingsPicker`。 |
| `delegated-sub-thread.tsx`、`delegation-status-group-card.tsx`、`resumed-delegation-card.tsx`（及 test） | TAKE_OURS。`content-parts-renderer.tsx` 仍 import 它们。 |

### D. 委托 / ACP

- `DelegationMatchKey` 留 fork 枚举：`Delegate { correlation_id, agent_type, task, working_dir }` 和 `Continue { correlation_id, target_task_id, task }`。不要收成上游的 `{ target }` 结构体。
- `commands/conversations.rs` 的列表、详情、统计、侧栏仍走 `InternalAgentSessionRegistry`，并加载 per-agent env。`delete_conversation_with_cleanup_core` 同时接自动标题协调器和 `ConnectionManager`（先取消标题，再 `disconnect_by_conversation_id`）。`get_folder_conversation_outline_core` 的参数是 `(conn, registry, conversation_id, before_index)`。Web handler 必须把 `state.internal_sessions` 传进去。`get_sidebar_data_core` 后面不要多一个 `}`。
- `DelegationListener` 三个构造函数都接 `results: Arc<dyn ResultToolAccess>`。桌面和 `codeg-server` 传 `ConnectionManagerResultLookup`，测试传 `NoResultTool`。`new` 必须把 `results` 继续传给 `new_with_wait_cancel`，不能在半路丢掉。
- `TokenEntry` 多了 `agent_hosts_tools`。字面量要带这个字段，或者 `..TokenEntry::legacy(...)`。
- `spawn_agent_connection` 最后一个参数是 `cli_root: Option<RootSeed>`。普通共享会话传 `None`。`fork_session` 的顺序是 `(cx, session_id, agent_type, cwd, terminal_meta, mcp_servers, claude, fork_point)`，不要把 `mcp_servers` 和 `terminal_meta` 对调。
- Codex ACP 版本留 fork 的 **2.1.1**（roundtable profile 钉在这版），不收上游 2.6.0。
- Companion 工具目录是 fork 那组，再加上游的 assistant 与 `submit_result`。
- `QuestionSpec` 同时有 `recovery` 和 `allow_other`。
- `send_prompt_linked_for_delegation` 增加 `can_spawn_child`、`prebound_child`、`brief`，返回 `AcceptedDelegationPrompt`。
- 后台监视 `spawn_for_policy` / `spawn_if_claude` 增加 `projects_dir`。

### E. 前端状态

`presentBackendError(translate, code, message, details, agentLabel)` 定义在 `acp-connections-context.tsx`，返回 `{ route, text, reason, evidence }`。`handleMappedEvent` 用上游事件路径，旁边留 fork 的 `handleDesktopDeliveryFailure`。`completeTurn` 同时做 fork 的 settled-turn hydrate 和上游的 unseen-turn resync。

### F. 应用壳

- `server_bin/main.rs` 留在原路径。委托监听器用 `stack.*` 加上 `EngineWorkTaskTools`、`DbChatAuthoring`、`NoBrowserTabs`、computer 匹配、`ConnectionManagerResultLookup`。
- `models/agent.rs` 十九个变体，无 OpenClaw。线上保留名只新增 `kiro`；`zcode` / `trae` / `command_code` / `minimax_code` 仍是合法自定义 id。
- 专家技能取上游 Superpowers 7.0.2 的脚本路径和 `git -C`，不删 fork 自己的技能说明。

---

## 4. 最容易「看起来解决了、文件其实坏了」的块

| 位置 | 为什么 |
|---|---|
| 身份文件整文件 `--theirs` | 版本变成 `0.45.0`，identifier 变回 `app.codeg`，更新地址指向上游 |
| `lib.rs` 展开 `generate_handler!` | 吞掉 fork 的 `production_tauri_commands!` |
| `codeg-server` 收成 `src/bin/codeg_server.rs` | 桌面 NSIS 会打进远程监听进程 |
| i18n 按冲突块拼接 | 同一对象写两遍，后写覆盖前写，DrawCode 句子被冲掉 |
| `parsers/openclaw.rs` 用 `--theirs` 加回来 | OpenClaw 必须保持删除 |
| `delete_conversation_with_cleanup_core` 只留一边 | Web 同时传协调器和 `connection_manager` |
| `get_folder_conversation_outline_core` 漏 `registry` | 详情加载走内部会话过滤，漏参编不过 |
| `DelegationListener` 函数体写了 `results` 但签名没有 | 构造函数半路把参数丢了 |
| `fork_session` 参数顺序按冲突块原文贴 | `terminal_meta` 和 `mcp_servers` 对调 |
| `commands/conversations.rs` 多一个 `}` | 后面的命令函数被挡在模块外 |
| 锁文件手合 | 58 个 pnpm 冲突块和 Cargo 依赖图对不上，必须重生 |

---

## 5. 绝对不要做的事

- 把版本收成 `0.45.0`，或把 `productName` 收成 `codeg`。
- 恢复 OpenClaw。
- 用 rebase 改已经发布的 MyCodeBuddy 历史。本轮是 `git merge --no-ff`。
- 把上游 browser / 桌面发布矩阵塞回 Windows-only 的 `release.yml`。
- 在冲突标记还在时跑 `cargo check` 或 `pnpm test`，把失败当成代码错。

---

## 6. 冲突清零之后，为了编过又改的地方

这些不是冲突标记，是标记清掉之后编译器和测试桩对不上的地方。下一轮如果再碰到同样的签名，按这里收。

- 工作区文件搜索只留上游的索引 IPC（`workspace_file_search`）。fork 的会话身份搜索不再是 Tauri 命令。
- Web 认证把 `&Request` 拆成同步的 `prepare_admission` 和 `finish_admission`。`&http::Request` 不能跨 `.await`。
- `SetConfigOption` 走 `set_config_option_for_agent`，Kiro / Hermes / Grok 各走自己的通道，mode 成功之后仍同步文件系统外部访问。
- `codeg-mcp` 的 `--custom-agents` 照旧收下但忽略。委托目标表不从启动参数长大。重复的本地 `parse_args` 删掉，解析以 `parse_companion_args` 为准。
- Grok 子代理卡片没有从上游半移植过来。当前解析结果没有 `spawn_call_ids` / `subagents`。这是已知缺口，不是已经接上的功能。
- 测试桩按现行签名补齐：控制通道、`AcceptedDelegationPrompt`、`CompanionLeaseRegistry`、`TokenEntry::legacy`、`QuestionSpec.allow_other`、`DelegationRequest.assistant`、`DelegationLink.assistant`、`CompanionFeatures.result`、`CompanionContext.custom_agents`、`ActiveDelegationState` 的助手字段、`PermissionResolved` 的 `option_id` / `origin`、`ConnectionCommand::Prompt.internal_suffix`。`async_main` 测试传入 `None`。
- 只被测试调用、生产路径已经换成别的实现的辅助函数，标了 `#[allow(dead_code)]`，避免 `cargo clippy -D warnings` 把库目标判红。没有把 `package.homepage` 删掉：它和 `repository` 都是 fork 身份，Cargo 只是提示重复。
- 发布测试里的版本改成 `0.45.0-mycodebuddy.1`，CI 矩阵含 `cli`，发布用的 `tauri.release.conf.json` 把 `binaries/codeg-cli` 留在 sidecar 列表里。

## 7. 收完后的验证

冲突标记清零、`git diff --diff-filter=U` 为空之后，这几条是过的：

```text
pnpm test:release
# 146 pass

cd src-tauri && cargo check --lib --features test-utils
cd src-tauri && cargo check --no-default-features --features server --bin codeg-server
cd src-tauri && CARGO_BUILD_JOBS=1 cargo check --tests --lib --features test-utils --keep-going
```

最后一条把库测试、集成测试和带测试的二进制都做了类型检查，退出码 0。这台机器把完整测试二进制链接起来时会被 OOM 杀掉（`SIGKILL`），所以没有把 `cargo test --lib --no-run` 的链接当成通过。`cargo check --tests` 不链接，能证明类型和签名对得上。

发布政策断言以 `pnpm test:release` 为准。sidecar 在 check 时缺真实二进制、写出 0 字节占位，是预期警告，不是失败。
