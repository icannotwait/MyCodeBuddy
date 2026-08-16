# AGENTS.md

This file provides guidance to Code Agent when working with code in this repository.

## 项目概述

Codeg（Code Generation）是一个多智能体编码工作台，它将多个智能体（Claude Code、Codex CLI、OpenCode、Gemini CLI、Cline 等）统一到一个工作区中，支持会话聚合和多智能体协作，支持桌面安装，服务器/Docker 部署。

## 技术栈

- **桌面运行时**: Tauri 2（Rust 后端 + webview 前端）
- **服务器运行时**: 独立 Rust 二进制（Axum HTTP + WebSocket）
- **前端**: Next.js 16（静态导出模式）+ React 19 + TypeScript（strict）
- **样式**: Tailwind CSS v4 + shadcn/ui（radix-maia 风格）
- **国际化**: next-intl
- **数据库**: SeaORM + SQLite
- **包管理器**: pnpm

## 代码检查与测试（任务完成后进行必要的检查）

### 前端

```bash
pnpm eslint .                  # lint
pnpm test                      # vitest 全跑（CI 用同一条命令）
pnpm test:watch                # 开发时增量重跑
pnpm test:coverage             # 覆盖率报告（输出到 coverage/index.html）
pnpm build                     # 静态导出构建
```

### 后端 Rust（在 `src-tauri/` 目录下执行）

```bash
# 桌面模式（默认 feature）
cargo check
# 日常快测：只运行库单元测试，跳过二进制与集成测试链接
cargo test --lib --features test-utils
# 单个集成测试目标：只替换相关 tests/*.rs 的文件名
cargo test --test delegation_session_reuse_integration --features test-utils
# 最终回归、CI 或明确要求时：运行库、二进制与全部集成测试
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings

# 服务器模式
cargo check --no-default-features --features server --bin codeg-server
cargo test --no-default-features --features server --bin codeg-server --lib
cargo clippy --no-default-features --features server --bin codeg-server --lib -- -D warnings

# codeg-mcp 协作伴生进程（多智能体委托）
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings

# 解析器快照评审（输出变化时）
cargo insta review
INSTA_UPDATE=auto cargo test --features test-utils     # 自动写新 .snap
```

`cargo test some_test_name` 只过滤最终执行的测试函数，仍会预先编译所有选中的
测试目标，不能替代 `--lib` 或 `--test` 来缩小构建范围。日常开发必须选择能证明
改动的最窄目标，完整回归留到分支完成阶段。

普通 dev/test 构建默认关闭 Rust 调试信息和增量编译，以限制每个 worktree 的
`target` 体积。确认 Cargo/rustc 已退出后，可在仓库根目录显式清理当前 worktree：

```bash
cargo clean --manifest-path src-tauri/Cargo.toml --target-dir src-tauri/target
```

### 低内存 Rust 开发（在仓库根目录执行）

仅在明确受低内存约束时使用以下 opt-in 命令：

```bash
# 4 GiB 机器的日常 Rust 反馈：只检查共享核心，不启用 Tauri
pnpm rust:check:low-memory
# 仅在改动对应运行面时执行；桌面检查仍可能超过 4 GiB
pnpm rust:check:desktop:low-memory
pnpm rust:check:server:low-memory
pnpm rust:check:mcp:low-memory
# 有更高可用内存或足够页文件时，才运行精确单测
pnpm rust:test:low-memory -- acp::codex_goal::tests::clear_with_no_open_goal_is_a_noop -- --exact
```

低内存配置将 Cargo 编译任务和测试线程限制为 1，并关闭增量编译和调试信息，
但首次冷编译仍可能较慢。`--exact` 只过滤测试运行，不会缩小编译目标；当前
Windows 整库测试程序包含 4,028 个测试，单个 `rustc` 在低内存配置下实测峰值
仍约 7.55 GiB。因此 4 GiB 机器默认只运行共享核心 check，Rust 单测与完整回归
交由 CI 或更高内存机器执行；足够大的系统页文件可能有帮助，但不作成功保证。

## 架构

### 双模式运行

项目通过 Cargo feature flags 支持三种二进制：

- **`codeg`**（`tauri-runtime`，默认）：完整桌面应用，包含 Tauri 窗口管理、系统通知、自动更新等
- **`codeg-server`**（opt-in `server` feature，`--no-default-features --features server`）：独立服务器模式，仅编译 Axum HTTP API + WebSocket。默认不编此 bin（桌面发行不附带），避免杀软将远程监听进程误判为远控
- **`codeg-mcp`**（无 feature）：per-launch stdio MCP 伴生进程，被注入到代理 CLI 的 MCP 配置中，向 LLM 暴露**异步**子智能体委托工具。

### 共享核心

- **`app_state.rs`** — `AppState` 共享状态结构，两种模式通过 `EventEmitter` 枚举区分事件发射方式
- **`web/event_bridge.rs`** — `EventEmitter::Tauri(AppHandle)` 或 `EventEmitter::WebOnly(Arc<WebEventBroadcaster>)`
- **`web/router.rs`** — Axum 路由，接受 `Arc<AppState>`
- **`web/handlers/`** — HTTP API 端点，全部使用 `Extension<Arc<AppState>>`

### Rust 后端（`src-tauri/src/`）

后端负责读取和解析本地文件系统上的代理会话文件：

- **`app_state.rs`** — 共享状态（db、连接管理器、终端管理器、事件广播器）
- **`models/`** — 共享数据结构
- **`parsers/`** — 每个智能体一个解析器
- **`commands/`** — 业务逻辑，`_core` 函数供两种模式共用，`#[tauri::command]` 函数仅桌面模式
- **`web/`** — Axum HTTP API + WebSocket + 静态文件服务 + 认证中间件
- **`acp/`** — Agent Client Protocol 连接管理
- **`db/`** — SeaORM + SQLite

### 前端（`src/`）

#### 核心库（`lib/`）

- **`transport/`** — Transport 抽象层（自动检测 Tauri/Web 环境切换 `invoke()`/`fetch()`）
- **`adapters/`** — AI 响应到组件渲染的适配器
- **`types.ts`** — Rust 模型的 TypeScript 镜像
- **`api.ts`** — 主 API 客户端
- **`tauri.ts`** — Tauri API 封装

#### 国际化（`i18n/`）

- 支持 10 种语言：英语、简体中文、繁体中文、日语、韩语、西班牙语、德语、法语、葡萄牙语、阿拉伯语
- 使用 next-intl 框架，消息文件存放在 `i18n/messages/`

### 数据流

桌面模式：前端 `invoke()` → Tauri 命令 → 业务逻辑 → 返回数据
服务器模式：前端 `fetch()` → Axum HTTP API → 同一业务逻辑 → 返回 JSON
实时通信：后端事件 → EventEmitter（Tauri 事件 / WebSocket 广播）→ 前端

### 条件编译约定

- `#[cfg(feature = "tauri-runtime")]` — 仅桌面模式编译（Tauri 窗口、通知、`tauri::State` 参数等）
- `#[cfg_attr(feature = "tauri-runtime", tauri::command)]` — 函数始终可用，仅在桌面模式标记为 Tauri 命令
- `_core` 后缀函数 — 接受普通引用参数（`&AppDatabase`、`&EventEmitter`），供 Web handlers 和 Tauri 命令共用

## 关键约束

- **仅支持静态导出**：`next.config.ts` 设置 `output: "export"`，不支持动态路由（`[param]`），必须使用查询参数替代
- **路径别名**：`@/*` 映射到 `./src/*`，导入写法为 `@/lib/utils`、`@/components/ui/button`
- **服务器部署**：通过环境变量配置（`CODEG_PORT`、`CODEG_HOST`、`CODEG_TOKEN`、`CODEG_DATA_DIR`、`CODEG_STATIC_DIR`）
- **Docker 支持**：多阶段构建（Node.js + Rust），支持 `docker-compose` 一键部署

## 代码风格

- Prettier：无分号、尾逗号（es5）、2 空格缩进、80 字符宽度
- ESLint：next/core-web-vitals + typescript + prettier
- TypeScript：strict 模式，启用 `noUnusedLocals` 和 `noUnusedParameters`
- Rust：2021 edition，使用 `thiserror` 定义错误类型

## 长命令与 wait 策略（Codex code-mode）

后台 `exec` / `wait` 在命令未结束时会把控制权交回模型；每次短轮询都会带着
完整上下文再唤醒一轮 LLM。空输出不等于卡住——`cargo test` 等常在结束前几乎
无增量输出。

### 必须遵守

- 优先使用默认：`yield_time_ms` **默认 20000**；够用时不要显式传更小值。
- 长任务用**一次较长 wait**，禁止高频短轮询：
  - `cargo build` / `cargo test` / `cargo clippy` / 全量套件：**30000–60000**
  - `pnpm test` / `pnpm build` / 大体量安装：**30000–60000**
  - 未知长命令：至少 **20000**，优先 **30000**
- **禁止**用 `yield_time_ms=1000`（或任何小于 **10000** 的值）去轮询编译/测试，
  除非命令本身预期数秒内结束且需要近实时输出。
- 仍在 running 时再次 wait，应保持**相同或更大**的 `yield_time_ms`，不要降到 1s。

### 实操建议

1. 冷编译场景先做小范围 `cargo check` / 定向编译，再跑长测试。
2. 优先窄过滤器（单测 / `--exact` / 模块级 filter），尽量落在首段 yield 窗口内完成。
3. 仅在要主动取消 cell 时使用 `terminate: true`。

说明：Codex 对 code-mode `wait` **不走 PreToolUse 改写**（源码 opt-out），因此
本策略是避免 1s 高频唤醒的软约束，无法被 hook 硬拦截。
