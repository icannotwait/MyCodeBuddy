# Codeg

[![Release](https://img.shields.io/github/v/release/icannotwait/MyCodeBuddy)](https://github.com/icannotwait/MyCodeBuddy/releases)
[![License](https://img.shields.io/github/license/icannotwait/MyCodeBuddy)](./LICENSE)
[![Tauri](https://img.shields.io/badge/Tauri-2.x-24C8DB)](https://tauri.app/)
[![Next.js](https://img.shields.io/badge/Next.js-16-black)](https://nextjs.org/)
[![Docker](https://img.shields.io/badge/Docker-ready-2496ED)](./Dockerfile)

<p>
  <strong>English</strong> |
  <a href="./docs/readme/README.zh-CN.md">简体中文</a> |
  <a href="./docs/readme/README.zh-TW.md">繁體中文</a> |
  <a href="./docs/readme/README.ja.md">日本語</a> |
  <a href="./docs/readme/README.ko.md">한국어</a> |
  <a href="./docs/readme/README.es.md">Español</a> |
  <a href="./docs/readme/README.de.md">Deutsch</a> |
  <a href="./docs/readme/README.fr.md">Français</a> |
  <a href="./docs/readme/README.pt.md">Português</a> |
  <a href="./docs/readme/README.ar.md">العربية</a>
</p>

Codeg (Code Generation) is a multi-agent coding workspace. It brings multiple agents (Claude Code, Codex CLI, OpenCode, Gemini CLI, Cline, Hermes Agent, CodeBuddy, Kimi Code, Pi, Grok Build, Cursor, DeepSeek Harness, etc.) into one workspace, supporting conversation aggregation and multi-agent collaboration, with desktop installation plus server/Docker deployment.

![gallery](./docs/images/workspace-light.png#gh-light-mode-only)
![gallery](./docs/images/workspace-dark.png#gh-dark-mode-only)
It aggregates your sessions from every supported agent CLI into one searchable workspace, and lets a main agent delegate to sub-agents of other types within a single task. Work you'd rather not sit through goes on a to-do board instead — each task in its own branch, running unattended, waiting for your review before it lands. Codeg runs as a desktop app, a standalone server, or a Docker container, with native iOS and Android clients for when you're away from your desk; fifteen agents come built in, and you can register any other ACP-compatible agent yourself.

## Sponsors

<table>
  <tr>
    <td align="center" width="220">
      <a href="https://www.compshare.cn/?ytag=GPU_YY_git_codeg" target="_blank"><img src="./docs/images/compshare.png" alt="Compshare" width="160" /></a><br/>
      <strong><a href="https://www.compshare.cn/?ytag=GPU_YY_git_codeg">Compshare (UCloud)</a></strong>
    </td>
    <td>Thanks to Compshare for sponsoring this project! Compshare is UCloud's AI cloud platform, offering cost-effective monthly and pay-as-you-go agent Plan subscriptions for Chinese models, starting at just ¥49/month. It also provides stable officially-proxied access to overseas models. Supports Claude Code, Codex, and API integrations. Enterprise-ready with high concurrency, 24/7 technical support, and self-service invoicing. Users who sign up via <a href="https://www.compshare.cn/?ytag=GPU_YY_git_codeg">this link</a> receive ¥5 in free platform credits!</td>
  </tr>
  <tr>
    <td align="center" width="220">
      <a href="https://sui-xiang.com/register?aff=JPFCRHHBE8HE" target="_blank"><img src="./docs/images/sui-xiang.jpg" alt="随想AI中转站" width="200" /></a><br/>
      <strong><a href="https://sui-xiang.com/register?aff=JPFCRHHBE8HE">随想AI中转站</a></strong>
    </td>
    <td>Thanks to 随想AI中转站 for sponsoring this project! 随想AI中转站 is a reliable and efficient API relay provider, offering relay services for Claude, Codex, Gemini, and more. New accounts earn ¥0.5 in test credit with each daily check-in after <a href="https://sui-xiang.com/register?aff=JPFCRHHBE8HE">signing up</a>; top-ups are credited 1:1 — no subscription, pay as you go. Multi-route redundancy, cross-region disaster recovery, and automatic failover keep long-lived SSE connections uninterrupted.</td>
  </tr>
  <tr>
    <td align="center" width="220">
      <a href="https://hezu.ink/sign-up?aff=0wVz" target="_blank"><img src="./docs/images/hezu-ink.jpg" alt="合租巴士" width="200" /></a><br/>
      <strong><a href="https://hezu.ink/sign-up?aff=0wVz">合租巴士</a></strong>
    </td>
    <td>Thanks to 合租巴士 for sponsoring this project! 合租巴士 is a reliable and efficient AI relay platform, offering highly stable relay for mainstream models such as Codex and Claude Code. Top-ups are credited at a transparent 1:1 ratio, with Codex rate subsidies as low as 0.08. <a href="https://hezu.ink/sign-up?aff=0wVz">Join the group via the official website to get $5 in trial credit</a>.</td>
  </tr>
  <tr>
    <td align="center" width="220">
      <a href="https://onehop.ai/platform/login?ref=CODEG&utm_source=github&utm_medium=readme_sponsor&utm_campaign=codeg&utm_content=sponsor_cta" target="_blank"><img src="./docs/images/onehop.jpg" alt="OneHop" width="120" /></a><br/>
      <strong><a href="https://onehop.ai/platform/login?ref=CODEG&utm_source=github&utm_medium=readme_sponsor&utm_campaign=codeg&utm_content=sponsor_cta">OneHop</a></strong>
    </td>
    <td>Thanks to OneHop for sponsoring this project! OneHop gives Codeg users one OpenAI-compatible API key for hundreds of leading models, including GPT, Claude, Gemini, DeepSeek, Kimi, and Qwen. Switch models without managing multiple provider accounts or repeatedly changing your code, and pay only for what you use. <a href="https://onehop.ai/platform/login?ref=CODEG&utm_source=github&utm_medium=readme_sponsor&utm_campaign=codeg&utm_content=sponsor_cta">Sign up through Codeg</a> to receive $1 in credit, then join the OneHop community and participate in the welcome activity for an additional $5 — up to $6 in test credit in total.</td>
  </tr>
</table>

> Want to become a Codeg sponsor? [Reach out to us by email.](mailto:itpkcn@gmail.com)

## Main Interface

![Codeg Light](./docs/images/workspace-light.png#gh-light-mode-only)
![Codeg Dark](./docs/images/workspace-dark.png#gh-dark-mode-only)

## Multi-Agent Collaboration

![Codeg Light](./docs/images/collaboration-light.gif#gh-light-mode-only)
![Codeg Dark](./docs/images/collaboration-dark.gif#gh-dark-mode-only)

## Office Workflow

![Codeg Light](./docs/images/office-light.png#gh-light-mode-only)
![Codeg Dark](./docs/images/office-dark.png#gh-dark-mode-only)

## Highlights

- **Conversation Aggregation** — import sessions from all supported agents into one unified workspace
- **DeepSeek Harness** — install and launch it from the agent list, with skills, MCP, delegation, experts, Office tools, and session history. Needs Node 22.
- **Multi-Agent Collaboration** — within a single session, the main agent delegates to sub-agents of different types (e.g. Claude Code calling Codex, Gemini) to jointly complete a task, each running as an independent session
- Parallel development with built-in `git worktree` flows
- **Project Boot** — visually scaffold new projects with live preview
- **Office Documents** — create, analyze, proofread, and edit `.docx` / `.xlsx` / `.pptx` through the bundled `officecli` toolset, with live in-tab preview that refreshes as the agent edits
- **Scientific Research** — bundled science skills (hypothesis generation, experimental design, statistics, visualization, critical appraisal, literature search) any agent can invoke, managed per-agent
- **Automations** — save a composer setup as a reusable automation that runs headlessly, on a cron schedule or on demand
- **Chat Channels** — connect Telegram, Lark (Feishu), iLink (Weixin) and more to your coding agents for real-time notifications, full session interaction, and remote task control
- MCP management (local scan + registry search/install)
- Skills management (global and project scope)
- Git remote account management (GitHub and other Git servers)
- Web service mode — access Codeg from any browser for remote work
- **Optional standalone server** — opt-in `codeg-server` (Docker or source build with `--features server`) for self-hosted browser access; **not** shipped in desktop GitHub Releases
- **Docker support** — local builds with `docker compose up -d`, with custom token, port, and volume mounts for data persistence and project directories
- Runtime Logs — a live in-app log viewer with filtering and per-module log levels
- Integrated engineering loop (file tree, diff, git changes, commit, terminal)

## Supported Agents

| Agent        | Environment Variable Path             | macOS / Linux Default                 | Windows Default                                       |
| ------------ | ------------------------------------- | ------------------------------------- | ----------------------------------------------------- |
| Claude Code  | `$CLAUDE_CONFIG_DIR/projects`         | `~/.claude/projects`                  | `%USERPROFILE%\\.claude\\projects`                    |
| Codex CLI    | `$CODEX_HOME/sessions`                | `~/.codex/sessions`                   | `%USERPROFILE%\\.codex\\sessions`                     |
| OpenCode     | `$XDG_DATA_HOME/opencode/opencode.db` | `~/.local/share/opencode/opencode.db` | `%USERPROFILE%\\.local\\share\\opencode\\opencode.db` |
| Gemini CLI   | `$GEMINI_CLI_HOME/.gemini`            | `~/.gemini`                           | `%USERPROFILE%\\.gemini`                              |
| Cline        | `$CLINE_DIR`                          | `~/.cline/data/tasks`                 | `%USERPROFILE%\\.cline\\data\\tasks`                  |
| Hermes Agent | `$HERMES_HOME/state.db`               | `~/.hermes/state.db`                  | `%USERPROFILE%\\.hermes\\state.db`                    |
| CodeBuddy    | `$CODEBUDDY_CONFIG_DIR/projects`      | `~/.codebuddy/projects`               | `%USERPROFILE%\\.codebuddy\\projects`                 |
| Kimi Code    | `$KIMI_CODE_HOME/sessions`            | `~/.kimi-code/sessions`               | `%USERPROFILE%\\.kimi-code\\sessions`                 |
| Pi           | `$PI_CODING_AGENT_SESSION_DIR`        | `~/.pi/agent/sessions`                | `%USERPROFILE%\\.pi\\agent\\sessions`                 |
| Grok Build   | `$GROK_HOME/sessions`                 | `~/.grok/sessions`                    | `%USERPROFILE%\\.grok\\sessions`                      |
| Cursor       | `$CURSOR_CONFIG_DIR/chats`            | `~/.cursor/chats`                     | `%USERPROFILE%\\.cursor\\chats`                       |
| DeepSeek Harness | `$DSH_HOME/sessions`                  | `~/.dsh/sessions`                     | `%USERPROFILE%\.dsh\sessions`                     |

> Note: environment variables take precedence over fallback paths.

Not on the list? Add it yourself. Pick any agent from the public ACP registry or paste its distribution JSON, and Codeg installs it, checks it can launch, and treats it like a built-in — it shows up in the picker, takes `@` delegation and skills, and gets its conversations recorded and searchable even when the agent keeps no history of its own. → [Custom Agents](https://docs.codeg.app/guide/custom-agents)
Claude Code · Codex · Gemini · OpenClaw · OpenCode · Cline · Hermes · CodeBuddy · Kimi Code · Pi · Grok · Cursor · DeepSeek Harness · Qoder · Google Antigravity

## 🪟 Split View

One tab strip isn't always enough. Right-click a conversation tab to split the view **right** or **down**, as many times as you like: two panes side by side, a stack of three, a grid. Each group is a workspace of its own — its own tabs, its own header, its own new-conversation button — so Claude Code can refactor in one pane while Codex reviews a diff in the next.

Drag a tab from one group into another and its session keeps streaming through the move; drag the divider between two groups to change how they share the space. Your layout is remembered per workspace, drafts included: reopen Codeg and the split comes back, with the text you never sent still in the box.

![Splitting the conversation area into a grid of tab groups](./docs/images/split-light.gif#gh-light-mode-only)
![Splitting the conversation area into a grid of tab groups](./docs/images/split-dark.gif#gh-dark-mode-only)

<details>
<summary><h2>Project Boot</h2></summary>

Create new projects visually with a split-pane interface: configure on the left, preview in real time on the right.


### What it does

- **Visual Configuration** — pick style, color theme, icon library, font, border radius, and more from dropdowns; the preview iframe updates instantly
- **Live Preview** — see your chosen look & feel rendered in real time before creating anything
- **One-Click Scaffolding** — hit "Create Project" and the launcher runs `shadcn init` with your preset, framework template (Next.js / Vite / React Router / Astro / Laravel), and package manager of choice (pnpm / npm / yarn / bun)
- **Package Manager Detection** — automatically checks which package managers are installed and shows their versions
- **Seamless Integration** — the newly created project opens in Codeg's workspace right away

Currently supports **shadcn/ui** project scaffolding, with a tab-based design ready for more project types in the future.

</details>

<details>
<summary><h2>Chat Channels</h2></summary>

Connect your favorite messaging apps — Telegram, Lark (Feishu), iLink (Weixin), and more — to your AI coding agents. Create tasks, send follow-up messages, approve permissions, resume sessions, and monitor activity — all from your chat app. Receive real-time agent responses with tool-call details, permission prompts, and completion summaries without ever opening a browser.

### Supported Channels

| Channel        | Protocol                    | Status   |
| -------------- | --------------------------- | -------- |
| Telegram       | Bot API (HTTP long-polling) | Built-in |
| Lark (Feishu)  | WebSocket + REST API        | Built-in |
| iLink (Weixin) | WebSocket + REST API        | Built-in |

> More channels (Discord, Slack, DingTalk, etc.) are planned for future releases.

Telegram forum supergroups can also use [Telegram topic mode](docs/chat-channels/telegram-topic-mode.md) to bind each topic to a separate Codeg session.

</details>

<details>
<summary><h2>Office Documents</h2></summary>

Work with Word, Excel, and PowerPoint files as a first-class workflow. The bundled **officecli** toolset lets your agents create, analyze, proofread, and edit `.docx`, `.xlsx`, and `.pptx` documents — and you can preview the result right inside Codeg.

### What it does

- **Create & Edit** — generate new documents or modify existing `.docx` / `.xlsx` / `.pptx` files, including charts, tables, and formatting
- **Analyze & Proofread** — inspect document structure, surface formatting issues, and proofread content
- **Live Preview** — open a `.docx` / `.xlsx` / `.pptx` in a file tab and it renders inline, refreshing automatically as the agent edits — backed by a long-lived `officecli watch` server (reverse-proxied and capability-authenticated so it works in web and standalone-server deployments)
- **Quick Actions** — the welcome page offers Coding, Office, and Scientific Research tabs that drop the matching skill invocation and a prompt template into the composer with one click; a skill that isn't enabled for the selected agent shows a lock badge linking to where you can turn it on
- **Office Tools settings** — a dedicated settings page installs `officecli` and manages its document skills through a skill-by-agent matrix: toggle any (skill, agent) pair, flip a skill across all agents or every skill for one agent, and apply bulk changes at once

</details>

<details>
<summary><h2>Scientific Research</h2></summary>

Turn any agent into a rigorous research assistant. Codeg bundles a curated set of MIT-licensed **scientific-research skills** — from ideation to analysis to write-up — that install into the shared central skill store and link into whichever agents you choose, exactly like the expert and office toolsets.

### What it does

- **Curated skills** — hypothesis generation, experimental design, statistical power, statistical analysis, exploratory data analysis, scientific visualization, critical appraisal, peer review, citation management, scholar evaluation, paper lookup, and AI schematics
- **Quick Actions** — the welcome page's Scientific Research tab drops the matching skill invocation plus a localized prompt template into the composer with one click
- **Science settings** — a dedicated settings page manages the skills through a skill-by-agent matrix, with badges flagging skills that need an API key or a Python environment

</details>

<details>
<summary><h2>Automations</h2></summary>

Turn any composer setup — agent, model, prompt, working directory, and options — into a reusable **Automation** that runs without opening the UI.

### What it does

- **Save once, reuse** — capture a fully-configured composer as a named, reusable automation
- **Scheduled or on demand** — run it on a cron schedule or trigger it manually whenever you need it
- **Headless execution** — automations run in the background and create real sessions you can open in the workspace at any time, then return you straight to the workspace when you start one

</details>

<details>
<summary><h2>Quick Start</h2></summary>

### Requirements

- Node.js `>=22` (recommended)
- pnpm `>=10`
- Rust stable (2021 edition)
- Tauri 2 build dependencies (desktop mode only)

Linux (Debian/Ubuntu) example:

```bash
sudo apt-get update
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  patchelf
```

### Binaries

Codeg ships three Rust binaries from a single workspace:

| Binary         | Role                                                                                                         | Build                                                                                          |
| -------------- | ------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------- |
| `codeg`        | Tauri desktop app (window, tray, updater)                                                                    | `pnpm tauri build` (release) / `pnpm tauri dev` (dev)                                          |
| `codeg-server` | Opt-in standalone HTTP + WebSocket server (self-host only; gated by Cargo feature `server`)                  | `pnpm server:build` / `pnpm server:dev` (passes `--features server`)                           |
| `codeg-mcp`    | Per-launch stdio MCP companion that surfaces the `delegate_to_agent` tool to agent CLIs (multi-agent collab) | `pnpm tauri:prepare-sidecars` (auto-invoked by `tauri dev` / `tauri build`)                    |

`codeg-mcp` must sit next to its parent binary at runtime — installers, the Docker image, and the Tauri sidecar bundler all place it next to `codeg` / `codeg-server`. Source builds and custom layouts can override the lookup with the `CODEG_MCP_BIN=/abs/path/codeg-mcp` env var. If the companion is missing, delegation is skipped (a single warning is logged) and the rest of the agent session keeps working.

### Development

```bash
pnpm install

# Frontend only (Next.js dev server, no Rust)
pnpm dev

# Frontend static export to out/
pnpm build

# Full desktop app (Tauri + Next.js, builds codeg-mcp sidecar automatically)
pnpm tauri dev

# Desktop release build (bundles codeg-mcp as externalBin)
pnpm tauri build

# Optional standalone server (opt-in; not in desktop releases)
pnpm server:dev
pnpm server:build                  # release binary at src-tauri/target/release/codeg-server
                                   # (requires Cargo feature `server`)

# Build the codeg-mcp companion explicitly (for the host triple)
pnpm tauri:prepare-sidecars        # output: src-tauri/binaries/codeg-mcp-<triple>

# Skip sidecar prep when iterating on the frontend and you don't need delegation
CODEG_SKIP_SIDECAR=1 pnpm tauri dev

# Lint
pnpm eslint .

# Frontend tests (vitest)
pnpm test
pnpm test:watch
pnpm test:coverage

# Rust checks (run in src-tauri/)
cargo check                                                     # desktop (default features; no codeg-server bin)
cargo check --no-default-features --features server --bin codeg-server  # server mode
cargo check --no-default-features --bin codeg-mcp               # MCP companion
cargo clippy --all-targets --features test-utils -- -D warnings

# Rust tests
cargo test --features test-utils                                # desktop (incl. integration)
cargo test --no-default-features --features server --bin codeg-server --lib  # server mode
cargo insta review                                              # accept parser snapshot updates
```

#### Low-memory Rust development

Run these opt-in commands from the repository root:

| Command                                               | Scope                                                                                             |
| ----------------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| `pnpm rust:check:low-memory`                          | Shared Rust library without Tauri; the recommended 4 GiB daily check                              |
| `pnpm rust:test:low-memory -- <test-path> -- --exact` | One exact shared-core test at runtime; compilation still builds the complete library test harness |
| `pnpm rust:check:desktop:low-memory`                  | Desktop library, including Tauri                                                                  |
| `pnpm rust:check:server:low-memory`                   | Server library and binary                                                                         |
| `pnpm rust:check:mcp:low-memory`                      | MCP companion binary                                                                              |

The alternate Cargo configuration limits compilation and test execution to
one job/thread and disables incremental state and debug information. It is
opt-in, so normal Cargo commands and CI are unchanged. The first invocation
can still be slow because it may need a cold build.

On the current Windows codebase, even one filtered unit test first compiles a
single harness containing all 4,028 library tests. The low-memory profile
reduced its observed `rustc` peak from roughly 12.2 GiB to 7.55 GiB, but the
test-name filter only changes execution after compilation. A 4 GiB machine
should therefore use `rust:check:low-memory` for daily Rust feedback and leave
Rust tests to CI or a higher-memory machine; a large system page file may help
but is not guaranteed. The desktop check and Tauri development can also exceed
4 GiB.

When enough memory is available, an exact test can be run with:

```bash
pnpm rust:test:low-memory -- acp::codex_goal::tests::clear_with_no_open_goal_is_a_noop -- --exact
```

> Tip: when you have a fresh `codeg-mcp` build under `src-tauri/target/release/` and want to point a manually-launched `codeg-server` at it without reinstalling, export `CODEG_MCP_BIN=$(pwd)/src-tauri/target/release/codeg-mcp`.

### Server Deployment (opt-in self-host)

GitHub Releases ship **desktop DrawCode (NSIS)** plus signed standalone
`codeg-server` archives (same layout as upstream):

| Platform   | Asset |
| ---------- | ----- |
| Linux x64  | `codeg-server-linux-x64.tar.gz` (+ `.sig` / `.sha256`) |
| Linux arm64 | `codeg-server-linux-arm64.tar.gz` |
| macOS x64  | `codeg-server-darwin-x64.tar.gz` |
| macOS arm64 | `codeg-server-darwin-arm64.tar.gz` |
| Windows x64 | `codeg-server-windows-x64.zip` |

Each archive contains `codeg-server`, the `codeg-mcp` companion, `web/` static
assets, and license files. The binary is still gated by Cargo feature `server`
and is intended for deliberate self-hosting (long-lived HTTP listener).

#### Remove a leftover Windows install

```powershell
.\uninstall-server.ps1
# or:
irm https://raw.githubusercontent.com/icannotwait/MyCodeBuddy/main/uninstall-server.ps1 | iex
```

Windows operators can install from the release zip:

```powershell
.\install.ps1 -Version v0.27.0-mycodebuddy.1
```

#### Option 1: Docker

```bash
docker compose up -d
```

Docker Compose builds the image locally from this repository (enables Cargo
feature `server` in the image build). The multi-stage build (Node.js + Rust →
slim Debian runtime) includes `git` and `ssh` for repository operations. Data
is persisted in the `/data` volume. You can optionally configure token, port,
and project-directory mounts in `docker-compose.yml`.

#### Option 2: Build from source

```bash
pnpm install && pnpm build          # build frontend
cd src-tauri
cargo build --release --bin codeg-server --no-default-features --features server
cargo build --release --bin codeg-mcp --no-default-features    # delegation companion
CODEG_STATIC_DIR=../out ./target/release/codeg-server          # codeg-mcp is picked up as a sibling
```

If you keep the two binaries in separate directories, set `CODEG_MCP_BIN=/abs/path/to/codeg-mcp` so the runtime can still find the companion; without it, multi-agent delegation is silently disabled.

#### Source-built Linux/macOS upgrades

```bash
git pull
pnpm install && pnpm build
cd src-tauri
cargo build --release --bin codeg-server --no-default-features --features server
cargo build --release --bin codeg-mcp --no-default-features
# Stop the service, redeploy both binaries and the web assets, then restart it.
```

Source-built Linux/macOS deployments can upgrade by pulling source, rebuilding,
and redeploying the server, companion, and static web output. Prebuilt release
archives (`codeg-server-linux-x64.tar.gz` and siblings) are also available for
operators who prefer not to compile locally; signed self-update uses the same
assets when enabled on a running server.

Docker deployments upgrade by pulling source and rebuilding/recreating the
container, for example
`git pull && docker compose up --build -d --force-recreate`.

#### Configuration

Environment variables:

| Variable                       | Default                | Description                                                                                                                                                                                                                                                                                                                                                                                                                      |
| ------------------------------ | ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `CODEG_PORT`                   | `3080`                 | HTTP port                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `CODEG_HOST`                   | `0.0.0.0`              | Bind address                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `CODEG_TOKEN`                  | _(random)_             | Auth token (printed to stderr on start)                                                                                                                                                                                                                                                                                                                                                                                          |
| `CODEG_DATA_DIR`               | `~/.local/share/codeg` | SQLite database directory (also roots `uploads/`, `pets/`)                                                                                                                                                                                                                                                                                                                                                                       |
| `CODEG_STATIC_DIR`             | `./web` or `./out`     | Next.js static export directory                                                                                                                                                                                                                                                                                                                                                                                                  |
| `CODEG_MCP_BIN`                | _(unset)_              | Absolute path to the `codeg-mcp` companion. Overrides the default sibling-of-executable + `PATH` lookup. Use this for source builds or custom layouts where the companion lives outside the server's install directory.                                                                                                                                                                                                          |
| `CODEG_SKIP_SIDECAR`           | _(unset)_              | Frontend-only convenience for `pnpm tauri dev` / `pnpm tauri build` — when `1`, skips building the `codeg-mcp` sidecar. Delegation is disabled in that build; ship-quality artifacts must leave it unset.                                                                                                                                                                                                                        |
| `CODEG_UPLOAD_MAX_TOTAL_BYTES` | _(unset)_              | Hard cap on total bytes resident under `<data dir>/uploads/`. Plain decimal byte count (e.g. `10737418240` for 10 GiB). Unset, `0`, or an unparseable value disables the cap and prints a startup line so the posture is visible. The cap is enforced within a single `codeg-server` process — horizontally-scaled deployments sharing one `uploads/` volume need external coordination (file lock, Redis, reverse-proxy quota). |
| `CODEG_UPLOAD_QUOTA_STRICT`    | _(unset)_              | When truthy (`1` / `true` / `yes` / `on`), abort startup with exit code 2 if `CODEG_UPLOAD_MAX_TOTAL_BYTES` is set to an unparseable value, instead of fail-open with a WARN. Use this when your security policy requires "configured quota must be effective".                                                                                                                                                                  |

</details>

<details>
<summary><h2>Architecture</h2></summary>

```text
Next.js 16 (Static Export) + React 19
        |
        | invoke() (desktop) / fetch() + WebSocket (web)
        v
  ┌─────────────────────────┐
  │   Transport Abstraction  │
  │  (Tauri IPC or HTTP/WS) │
  └─────────────────────────┘
        |
        v
┌─── Tauri Desktop ───┐    ┌─── codeg-server ───┐
│  Tauri 2 Commands    │    │  Axum HTTP + WS    │
│  (window management) │    │  (standalone mode)  │
└──────────┬───────────┘    └──────────┬──────────┘
           └──────────┬───────────────┘
                      v
            Shared Rust Core
              |- AppState
              |- ACP Manager
              |- Parsers (conversation ingestion)
              |- Chat Channels
              |- Git / File Tree / Terminal
              |- MCP marketplace + config
              |- Office Tools (officecli) + Automations
              |- SeaORM + SQLite
                      |
              ┌───────┼───────┐
              v       v       v
  Local Filesystem  Git   Chat Channels
    / Git Repos    Repos  (Telegram, Lark, iLink)
```

</details>

## Privacy & Security

- Local-first by default for parsing, storage, and project operations
- Network access happens only on user-triggered actions
- System proxy support for enterprise environments
- Web service mode uses token-based authentication

## Community

- Scan the QR code below to join our WeChat group for discussions, feedback, and updates

<img src="./docs/images/weixin-light.jpg#gh-light-mode-only" alt="WeChat" width="240" />
<img src="./docs/images/weixin-dark.jpg#gh-dark-mode-only" alt="WeChat" width="240" />

- Thanks to the [LinuxDO](https://linux.do) community for their support

## Acknowledgments

- MyCodeBuddy is a fork of the original [Codeg](https://github.com/xintaofei/codeg) project.
- [ACP](https://agentclientprotocol.com) — the Agent Client Protocol (ACP) is the foundation that enables Codeg to connect with multiple agents
- [Superpowers](https://github.com/obra/superpowers) — powers Codeg's expert skills module
- [OfficeCLI](https://github.com/iOfficeAI/OfficeCLI) — powers Codeg's Office documents workflow
- [scientific-agent-skills](https://github.com/K-Dense-AI/scientific-agent-skills) — powers Codeg's Scientific Research skills (MIT-licensed subset)

## License

Apache-2.0. See [LICENSE](./LICENSE).

Installed desktop bundles include the Apache-2.0 license, modification
attribution, and generated third-party notices under their resources directory.
