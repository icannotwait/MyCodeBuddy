# main 合入 Codeg v0.31.1：冲突解决手册

> 本文描述的是 **`sync/codeg-0.31.1` 上 `git merge --no-ff upstream/main` 当时怎么收冲突**。
> 不要用后续编译/测试收口提交当合并提交。

`<<<<<<< HEAD` = MyCodeBuddy / DrawCode fork（`0.31.0-mycodebuddy.1`）。
`>>>>>>> upstream/main` = 上游 Codeg `v0.31.1`。

四字口令仍是 **TAKE_OURS / TAKE_THEIRS / MERGE_BOTH / REWRITE**。错位块给「这一格的口令 + 功能迁到真正的臂」，而不是整文件 `--ours` / `--theirs`。

---

## 1. 合入事实

| 项 | 值 |
|---|---|
| Ours（merge 第一父链） | `42ab3b83b` `sync: Codeg v0.31.0 as 0.31.0-mycodebuddy.1`（`origin/main`） |
| Theirs | `ac3a336c7`（`upstream/main` = tag `v0.31.1`） |
| 共同祖先 | `aace536fe9e38575a8973ed83435199ecc2700c6`（`v0.31.0`） |
| 上游提交（5，全是 browser + release） | `883e12d30` ask-once eval 设置；`31e558716` pane 贴边；`70255f6ae` 空 tab 自有页面；`67250e517` Windows 最大化/还原；`ac3a336c7` Release 0.31.1 |
| 打开合并时的工作树清点 | **17** 个冲突文件、**27** 个 `<<<<<<<` 文本块（与 `git merge-tree origin/main upstream/main` 同数） |
| add/add | 无 |
| 上游新增 | `src-tauri/src/browser/blank_page.rs`、`src/lib/browser/blank-page-theme.ts(+test)` |

---

## 2. 全局原则（本轮再钉死）

1. **品牌 / 发行身份永远 ours。** 版本写成 `0.31.1-mycodebuddy.1`（`package.json` / `Cargo.toml` / `Cargo.lock` 的 `codeg` package / `tauri.conf.json`）。不要收成光秃 `"0.31.1"`。`productName` / `mainBinaryName` = `DrawCode`，`identifier` = `app.mycodebuddy`。
2. **OpenClaw 保持删除。** 本轮上游没有把 `AgentType::OpenClaw` 加回来。i18n 里既有的 gateway 文案继续留。
3. **父目录恰好 6 个工具：** `delegate_to_agent`、`register_simple_workflow`、`continue_delegation`、`resume_delegation`、`get_delegation_status`、`cancel_delegation`。上游 `tool_schema.json` 的 2 行 diff 只改了 `browser_eval` 描述；fork 目录没有这个工具，整文件 **TAKE_OURS**。不要把上游 4 工具 + browser 目录贴进 companion。
4. **browser 能力接上游。** pane 贴边、空 tab 主题页、Windows `hold_resize_hook`、eval 一次询问（设置里选，而不是每段代码一次）走自动合并 + 下面的 MERGE_BOTH。`lib.rs` 继续 `production_tauri_commands!`，只把新命令登记进宏。

---

## 3. 按类怎么收（17 文件 / 27 块）

### A. Identity（4 文件 / 4 块）

| 文件 | 决议 | 配方 |
|---|---|---|
| `package.json` | MERGE_BOTH | `"version": "0.31.1-mycodebuddy.1"`；保留 ours `license` / `repository` / `homepage`。上游只改了 version。 |
| `src-tauri/Cargo.toml` | MERGE_BOTH | `version = "0.31.1-mycodebuddy.1"`。`windows` crate 的 `Win32_UI_WindowsAndMessaging` 已自动合并（`hold_resize_hook` 需要）。 |
| `src-tauri/tauri.conf.json` | TAKE_OURS + 版本 | DrawCode / `mainBinaryName` / `app.mycodebuddy` / `0.31.1-mycodebuddy.1`。 |
| `src-tauri/Cargo.lock` | MERGE_BOTH | 只改 `[[package]] name = "codeg"` 的 version 为 `0.31.1-mycodebuddy.1`。无新 crate，不必 `generate-lockfile`。 |

### B. Delegation schema（1 文件 / 1 块）

| 文件 | 决议 | 配方 |
|---|---|---|
| `src-tauri/src/acp/delegation/tool_schema.json` | TAKE_OURS | 六工具 + `complete_work` 目录不动。上游唯一改动是 `browser_eval` 描述（「每一次都问」→「可能问，取决于设置」）。fork 的 `allows_tool` 不暴露 `browser_*`，把这段贴进数组会污染目录且对 `tools/list` 无收益。agent 侧 eval 文案走 `browser_tools.rs`（已自动合并）和 i18n。 |

`tools/list` 断言仍是 `tools.len()==6`，名字仍是上面那六个。

### C. lib / Windows shim（2 文件 / 2 块）

| 文件 | 决议 | 配方 |
|---|---|---|
| `src-tauri/src/lib.rs` | TAKE_OURS + 迁命令 | 1/555 式错位：**不要**贴上游展开 `generate_handler!`。继续 `.invoke_handler(production_tauri_commands!(generate_production_invoke_handler))`。把 `browser_commands::browser_set_blank_page_theme` 插进宏，紧挨 `browser_set_sign_in_user_agent`（与上游登记位置一致）。 |
| `src-tauri/src/browser/shim/windows.rs` | MERGE_BOTH | 以上游 maximize/restore `hold_resize_hook` 为底（body 已自动合并）。import 并集：留 fork 的 `COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG`（截图路径在用），加上游的 `ICoreWebView2Controller` + `COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC`。 |

### D. i18n ×10（20 块）

十个 locale **JSON 三路深并集**（ours==base → theirs；theirs==base → ours；两边都改且 ours 含 DrawCode/MyCodeBuddy → ours；两边都改且路径是 browser 文案 → theirs）。

每个 locale 叶键集 **6018 → 6022**。本轮 theirs-only 4 键、browser 两边都改 3 键：

| 键 | 决议 |
|---|---|
| `BrowserSettings.evalApprovalTitle` / `Hint` / `Ask` / `Silent` | TAKE_THEIRS（新设置文案） |
| `AgentTools.browserEval.short` / `hint` | TAKE_THEIRS（一次询问改到设置） |
| `Browser.agent.eval.everyTime` | TAKE_THEIRS（指向 Built-in browser 设置） |
| `LoginPage.*` / `WebServiceSettings.*` 等 DrawCode 产品句 | TAKE_OURS |

冲突形态是「ours 空、theirs 在文件中段再插一份 `AgentTools` / `BrowserSettings`」：那是键位错位，不是缺键。不要把第二份对象贴进去。从 `HEAD` / `MERGE_HEAD` / merge-base 做深并集，写回原文件。

---

## 4. 自动合并、没有冲突块、但仍要核对的上游能力

这些文件 git 自己收了，不要在第 3 节当成冲突块：

- `blank_page.rs` / `blank-page-theme.ts`：空 tab 用应用色，不再是系统白页。
- `layout.tsx` / `resizable.tsx` / `browser-surface-host.tsx`：页面贴到 pane 边缘。
- `browser-eval-confirm.tsx` / `browser-prefs.ts` / `browser-settings.tsx`：`evalApproval` = `ask` \| `silent`。
- `commands/browser.rs`：`browser_set_blank_page_theme`。
- `Cargo.toml` `Win32_UI_WindowsAndMessaging`：给 `hold_resize_hook` 用。

---

## 5. 最容易「看起来解决了、文件其实坏了」的块

| 文件 | 为什么 |
|---|---|
| `lib.rs` 展开 `generate_handler!` | 吞掉 fork IPC 宏（delegation / config-sync / 冷重连命令） |
| `tool_schema.json` `--theirs` | 丢掉 `continue_delegation` / `register_simple_workflow`，`tools.len()` 不再是 6 |
| i18n 整段贴第二份 `AgentTools` | 重复键，JSON 后写覆盖前写，DrawCode 句被冲掉 |
| `windows.rs` 整段 `--theirs` import | 丢掉 `FORMAT_PNG`，截图路径编不过 |
| Identity `--theirs` | 版本变成 `0.31.1`，identifier 变回 `app.codeg` |

---

## 6. 绝对不要做的事

- 用 theirs 的展开 `generate_handler!` 替换 `production_tauri_commands!`。
- 把上游 browser 工具目录收进 `tool_schema.json`。
- 恢复 OpenClaw，或把版本收成 `0.31.1`。
- 在 rustc 1.83 上 `generate-lockfile`（vendored `sacp-tokio` 要 edition 2024 / rustc ≥ 1.85）。本轮锁文件只钉 version，没有重生。

---

## 7. 收完后的验证

```text
pnpm test:release
pnpm eslint .
pnpm test
pnpm browser:agent:check
pnpm browser:agent:types
# Rust：对齐 .github/workflows/test.yml
cd src-tauri
cargo fmt --check
cargo test --features test-utils
cargo test --no-default-features --features server --bin codeg-server --lib
cargo clippy --all-targets --features test-utils -- -D warnings
cargo clippy --no-default-features --features server --bin codeg-server --lib -- -D warnings
```

硬核对：

- 身份四处都是 `0.31.1-mycodebuddy.1`。
- `productName` / `mainBinaryName` = `DrawCode`，`identifier` = `app.mycodebuddy`。
- `AgentType::OpenClaw` / `McpAppType::OpenClaw` 不存在。
- `tools/list` 正好 6 个父工具名。
- `lib.rs` 没有生产路径内联 `tauri::generate_handler![`（宏内部那一次除外）。
- 无 `<<<<<<<` / `=======` / `>>>>>>>`。

---

## 8. 合入后验证收口（不是冲突块）

`<<<<<<<` 清零之后、`pnpm test:release` / `cargo fmt --check` 才暴露：

| 缺口 | 决议 |
|---|---|
| `scripts/release-policy.test.mjs` 仍钉 `0.31.0-mycodebuddy.1` / `sync/codeg-0.31.0` | 改成 `0.31.1-mycodebuddy.1` / `sync/codeg-0.31.1` |
| `install.ps1` 与各 README 的 `.\install.ps1 -Version` 示例 | 跟身份一起升到 `v0.31.1-mycodebuddy.1` |
| `blank_page.rs` `is_hex_colour` 折行 | `cargo fmt`（上游文件在我们的 rustc/rustfmt 下不过 `--check`） |
| `windows.rs` MERGE_BOTH import | `cargo fmt` 收成 rustfmt 折行 |
