# Lifeline 机器隧道上的 CodeG 原生 Web

| 字段 | 值 |
| --- | --- |
| 标题 | Lifeline 机器隧道上的 CodeG 原生 Web |
| 作者 | （待填写） |
| 日期 | 2026-09-23 |
| 状态 | Draft — 信任模型已确认，待实现与端到端验收 |
| 修订 | 2026-09-24：三方评审后核实修订健康探测、流控、会话、登记与恢复流程；随后确认 `lifeline.woa.com` 网页登录鉴权绝对可信，已授权用户完整操作自己的机器，撤销令牌绝对保密的发布阻断。保留桌面进程、机器子域与出站隧道方案，不启动 `server_bin` |
| 仓库 | CodeG / MyCodeBuddy：`D:\MyCodeBuddy`（Tauri + Next.js；`/api` 与 `/ws/events` 由桌面进程里的 Rust web router 提供）。`src-tauri/src/server_bin/main.rs` 只是独立二进制，本设计不启动它。Lifeline：`D:\lifeline`（pnpm workspace：`packages/server`、`packages/agent`、`packages/web`、`packages/protocol`、`packages/cli`） |

## Overview

用户在 Lifeline 上完成浏览器登录并拥有某台机器之后，浏览器打开的是这台机器上的 **CodeG 原生静态页**（Next.js `output: "export"`，由已经在跑的桌面应用里的 Rust web router 提供），不是 Lifeline 的 `/console`。不另起 `codeg-server`。本模式桌面 web 服务只监听 `127.0.0.1`，机器通过出站连接连到公网 Lifeline。登录和连接协议不主动分发 `web_service_token`；已通过 Lifeline 鉴权且拥有该机器的用户可使用完整的 CodeG 编程能力，包括终端、文件访问、代理执行与备份。

这不是「假装成 Cursor，把 ACP 翻成 `session:full` / `command:*`」。桌面专有壳（宠物窗、原生文件对话框、在资源管理器中显示）不在本设计内；UI 就是现有 CodeG web mode。

选定的两个不能再打开的机制：

1. **URL**：一台机器一个源站，`https://m-<hostLabel>.<apex>/`。`<apex>` 是 `PUBLIC_ORIGIN` 的 **hostname**（DNS 名，不含端口）。非默认端口只出现在浏览器 URL 的 `:port`。`<hostLabel>` 是登记时生成的 16 位十六进制，不是原始 `agentId`。不用路径前缀。
2. **令牌**：浏览器 `localStorage["codeg_token"]` 里只放公开常量 `codeg-lifeline-session`。Lifeline 边缘在送进隧道之前删掉 `Authorization` 和 `codeg-token.*` 子协议。机器侧守护进程在打到 `127.0.0.1` 之前注入桌面应用已有的 `web_service_token`。`require_token` 仍然做字符串相等，不改比较逻辑。

浏览器能打开哪台机器，由 Lifeline 会话的 `userId` 与 `machines.owner_user_id` 是否相等决定。Office 沙箱通过该身份派生的单文档能力访问，见专节。

### 已确认的信任模型与令牌约束

`lifeline.woa.com` 网页端登录鉴权是绝对可信的身份入口。本设计复用该鉴权结果；通过鉴权并通过 `machines.owner_user_id === userId` 归属检查的用户，是该机器的完整 CodeG 操作者。不会为防止用户读取自己的令牌而增加受限执行身份、文件沙箱或削减终端、代理及备份能力。

`web_service_token` 的要求是**正常登录、连接和服务配置接口不主动分发真令牌**：浏览器连接使用哨兵，机器端注入已有令牌，页面 meta、localStorage、隧道控制字段和运行日志不写入它。文件内容、用户主动执行的命令输出及完整备份属于授权数据通道；其中出现本机令牌不构成越权，也不作为发布阻断。交接文件的 0600 / DACL 继续隔离其它 OS 用户，不承担隔离当前操作者的职责。

现有服务配置/状态接口会返回 token，HTTP `stop_web_server` 在桌面进程里会真的停服，而停服后 HTTP 无法重新启动。因此托管模式保留一项范围明确的接口约束：`get_web_server_status`、`get_web_service_config`、`update_web_service_config`、`start_web_server`、`stop_web_server` 由边缘与机器侧在转发前返回 403 `hosted_operation_not_allowed`，对应 Web 设置页提示在桌面管理服务，Tauri IPC 保留。这个限制用于避免无必要的凭据回显及误停、改坏承载当前连接的服务，不是对已登录操作者的安全隔离；不扩展到通用文件、终端、代理或备份 API，也不扫描这些响应体来删令牌。

可信登录仍需正确接入每条请求：机器归属、子域会话交接、HTTP/WS 有效期、Origin 与 Office 派生能力检查继续执行。这里验证的是已有身份能否访问目标机器，而不是重新建立一套用户鉴权。

信任模型已经确定，无需再次选择“完整 Web 能力或绝对保密”。发布取决于后文的路由、流控、恢复、兼容性和容量验收。

## Background & Motivation

### 今天的两条认证是断开的

Lifeline 把浏览器身份和机器凭据拆开。`packages/server/src/auth/provider.ts` 写明：agent 不走 `AuthProvider.verify`；机器令牌在 `identity-store.ts`，浏览器会话与机器凭据完全分离。

浏览器侧（`packages/server/src/auth/factory.ts`）：

- 显式 `AUTH_PROVIDER` 优先，否则 `AUTH_HEADER`，否则 `AUTH_PASSWORD`，否则 loopback 上落到 `none`，再否则 password。
- `none` 只能绑 loopback；私网还要 `AUTH_INSECURE_ALLOW=1`；公网 / `0.0.0.0` / `::` 拒绝。
- `trusted-header` 在非 loopback 上必须 `AUTH_TRUSTED_PROXY=1`，否则拒绝启动（头可以被直连客户端伪造）。
- password 的会话 cookie 名是 `lifeline_session`，`HttpOnly; SameSite=Strict; Path=/`，`Max-Age` 为 14 天（`SESSION_TTL_MS = 14 * 24 * 60 * 60 * 1000`）。HMAC 载荷是 `v1|<userId>|<exp>|<mac>`。password 模式下 `sendSessionCookie` 把 `userId` 写成字面量 `owner`（单租户）。`none` 的 `verify` 也返回 `{ userId: 'owner' }`。`trusted-header` 用网关头，最长 128。HTTPS 时加 `Secure`（`PUBLIC_ORIGIN` 以 `https://` 开头，或 `X-Forwarded-Proto: https`）。没有 logout 路由。

机器侧：

- `lifeline setup` 打开已登录的 `GET /cli-setup?redirect_uri=http://127.0.0.1:<port>/callback`。`isAllowedRedirectUri` 只允许 `http`、主机 `127.0.0.1`、路径 `/callback`、无 userinfo、无 hash。
- 一次性 code：`randomBytes(32).toString('hex')`，TTL `120_000` ms，用过即删，绑签发时的 `userId`（`cli-setup.ts`）。CLI 等回调最多 `180_000` ms。
- 兑换是 `POST /public/cli-setup/exchange`，body 为 `exchangeBodySchema`：`code`、`agentId` 均 `min(1)`，`hostname` 可选。`owner` 为空字符串直接 400。明文令牌 `randomBytes(24).toString('hex')` 只返回一次；库里存 `sha256`（`hashToken`）。同一 `agentId` + `owner` 的旧哈希在同一事务里 `revokedAt`。
- `agentId` 默认 `machine-<uuid>`（`cli/src/config.ts` 的 `newAgentId`）。`AGENT_ID` 可以覆盖，所以 **agentId 不是 DNS 安全字符集**。
- Agent 出站：`socket.io` namespace `/agent`，path `/agent-io`，`auth: { agentToken }`，只走 websocket，重连 1s→30s，连接 timeout `60_000` ms（`packages/agent/src/uplink.ts`）。
- Handshake（`agent-hub.ts` 的 `nsp.use`）：`resolveHandshake(socket.handshake.auth.agentToken)` → `{ owner, agentId }`。对不上就 `Unauthorized`。`agent:register` 上报的 id 必须等于令牌绑定的 id，否则 `agent:rejected` / `agent-mismatch`。`AgentDirectory.register` 再做所有权门：已有 owner 且不是同一人则拒绝（`owner-mismatch`），不替换、不交付数据。`canSee` 在传入 userId 时比较 `owner === userId`；省略 userId 会走内部免隔离分支，公网路径不得省略。
- 共享 `AGENT_TOKEN` 已不再被接受。

`/agent-io`、`/public/*`、`/healthz` 在 `public-paths.ts` 里免浏览器身份。其余路径，包括页面和 `/api/*`，要过 `gateHttp`。静态页来自 `clientDir()`：优先 `dist/client`（存在 `index.html` 或 `public/install.sh`），否则 `packages/web`。`GET /` 和看起来像页面的路径回 `index.html`（`relay.ts` 的 `serveIndex` / `looksLikePagePath`）。前端路由只有两个屏幕：`/` 与 `/console`（`packages/web/src/lib/routes.ts`）。

### CodeG web 客户端不认 cookie

桌面 web router 的 `require_token_with_completion_authorizations`（`src-tauri/src/web/auth.rs`，独立二进制也链接同一份代码）只认两件事，而且是 **字节级相等**：

- `Authorization: Bearer <进程令牌>`。桌面进程里这个字符串就是 `web_service_token`
- 或 `Sec-WebSocket-Protocol` 里的 `codeg-token.<base64url>`，用 `URL_SAFE_NO_PAD` 解码后相等

空的进程令牌直接 401 `Server token is not configured`（避免 `Bearer ` 空值把认证关掉）。Bearer 对上之后，若带 `x-codeg-completion-context`，还要过进程内 `CompletionAuthorizationRegistry`；对不上是 401 `Invalid completion context`。没有这个头则是 `server_operator`。失败正文是 `Invalid or missing token`。

客户端（web mode，`isDesktop()` 为假）：

- `src/lib/transport/index.ts` 用 `window.location.origin` 构造 `WebTransport`。`call()` 请求 `` `${origin}/api/${command}` ``，超时默认 `60_000` ms。
- `src/app/page.tsx`、`src/app/login/page.tsx`、`src/lib/api.ts` 的上传使用 `fetch("/api/...")` 或 `` `${window.location.origin}/api/...` ``。都是 **源站根上的绝对路径**，不是相对当前目录。
- `localStorage` 键名 `codeg_token`（`web-auth.ts`）。`/` 上没有值就 `router.replace("/login")`。`/api/health` 返回 401 时 **这一页** 会删掉键并去 `/login`。
- 应用内的 401 不立刻删键：`WebTransport.call` 和 `probeHealth` 只 `markUnauthorized()`。用户点 WebConnectionGuard 的 “Go to login” 才走 `redirectToCodegLogin()`（删键，`location.href = "/login"`）。探针超时 `8_000` ms。WS 断开本身不登出。
- 空令牌：`connectWs()` 返回 false；`probeHealth` 把空令牌当成 unauthorized。
- WS：`/ws/events`，子协议 `codeg-events` 加 `codeg-token.<base64url>`（`ws-auth.ts`）。服务端 `ws_handler` 用 `ws.protocols(["codeg-events"])` 只回选 `codeg-events`。第一条文本帧必须是 `{"channel":"__ready__","payload":null}`。客户端等它最多 `5_000` ms（`READY_TIMEOUT_MS`）。
- 重连退避 `1_000`→`32_000` ms。重连对话框迟 `4_000` ms 才出现；`unauthorized` 不迟滞。

`next.config.ts`：`output: "export"`。生产构建 `assetPrefix` 为 `undefined`（资源在 `/_next/...`）。没有 `basePath`。开发态 `assetPrefix` 才是 `http://<TAURI_DEV_HOST|localhost>:3000`。托管模式吃的是桌面 web router 上的静态导出，不是 `next dev`。

`build_router`（`router.rs`）把带认证的 API `nest` 在 `/api`，WS 单独挂 `/ws/events`，其余走 `ServeDir`，并把无扩展名路径重写成 `path.html`。静态页 **不** 经过 `require_token`。例外的无 Bearer API：`get_system_language_settings`、`workspace_download/{ticket}`、`backup_download/{ticket}`、`office-watch-proxy/{port}`。这些仍然只应该从机器源站、在 Lifeline 门通过之后到达。

`server_bin/main.rs` 是 **另一条** 进程，本设计不启动、不打包、不监督。它默认 `CODEG_HOST=0.0.0.0`，`CODEG_PORT=3080`，令牌走 `resolve_persisted_server_token`，数据目录默认 `default_data_dir()` = `dirs::data_dir()/codeg`（Windows 上是 `%APPDATA%\codeg`）。它在生成令牌时和绑定成功后各有一处明文 `eprintln`（约 230–236 行，以及约 763 行的 `[SERVER] Token:`）。自行运行这个二进制的操作者仍会看到这两行。本设计不靠改掉它们来保护令牌，也不从 stderr 或 `LOG_DIR` 里刮令牌。桌面默认构建甚至不产出这个二进制：`src-tauri/Cargo.toml` 的 `server` feature 不在 `default` 里，注释写明只给 Docker / 自建服务器用。

桌面应用已经在自己的进程里绑定同一套 router。自动启动在 `lib.rs` 里调用 `do_start_web_server_tauri`（`host` 传入 `None`）。`do_start_web_server_tauri` 与尚未被调用、但带同样默认值的 `do_start_web_server_with_state` 今天都是 `host.unwrap_or_else(|| "0.0.0.0".to_string())`。令牌由 `resolve_web_service_token` 解析：显式参数优先，否则 `app_metadata` 键 `web_service_token`（`WEB_SERVICE_TOKEN_KEY`），否则生成无连字符 UUID。解析结果在 bind 成功后由 `persist_web_service_config` 写回同一张表。库文件是 `init_database` 打开的 `{effective_data_dir}/{database_file_name()}`：`effective_data_dir` 来自 `paths::resolve_effective_data_dir(app.path().app_data_dir())`；`CODEG_DATA_DIR` 非空时用它，否则是 identifier `app.mycodebuddy` 的 Tauri `app_data_dir`（Windows `%APPDATA%\app.mycodebuddy`，macOS `~/Library/Application Support/app.mycodebuddy`，Linux `$XDG_DATA_HOME/app.mycodebuddy` 或 `~/.local/share/app.mycodebuddy`）。发布版文件名 `codeg.db`，debug 且 `tauri-runtime` 时是 `codeg-dev.db`。`journal_mode=WAL`，运行时连接池最多 5 条。`db/mod.rs` 写明：在已打开的句柄下换掉 `codeg.db` 会损坏它。这不是 `%APPDATA%\codeg`。本设计不运行共用该数据目录的第二个完整应用进程；ACP 会话也在这个桌面进程的 `ConnectionManager` 里，不在 `server_bin` 里。

### 为什么不能只加一张 cookie

`require_token` 不读 cookie。`localStorage` 为空时，`/` 去 `/login`，WS 不连。登录页把用户粘贴的字符串原样放进 `Authorization` 再发给 `/api/health`。所以：

- 只在浏览器放 Lifeline cookie、不改客户端：请求仍是 `Bearer ` 空或旧值，桌面 web router 401，用户进粘贴令牌的 `/login`。
- 把真 `web_service_token` 作为登录结果写入 `localStorage`：不符合“正常连接不主动分发真令牌”的约束。Lifeline 会话已确认身份，浏览器只需非空哨兵，不必再持久化这枚长期本机凭据。
- 在 Lifeline 上做反向代理、却让桌面 web 服务继续对公网监听：违反 loopback + 出站。而且现在的默认绑定就是 `0.0.0.0`。

必须同时规定：浏览器里存什么、谁改写、桌面 web router 仍检查什么。

### 现有 agent 通道不是字节管道

`/agent` 上的事件是 `agent:register`、`state:*`、`session:*`、`command:*`、`command:result`。`COMMAND_EVENTS` 是一份固定名单（发消息、批准、切 tab……），没有任意 HTTP/WS 帧。`SOCKET_MAX_HTTP_BUFFER_SIZE = 20 * 1024 * 1024`。ping 间隔 `25_000` ms，ping 超时 `120_000` ms，因为主线程上的 sqlite 同步加 `session:full` 会撑过默认 20s。

CodeG 侧的体积不是这个形状：

| 限制 | 值 | 位置 |
| --- | --- | --- |
| axum 默认 JSON body | 2 MiB | `router.rs` 注释 |
| `background_set` | 24 MiB | 同上，`DefaultBodyLimit::max(24 * 1024 * 1024)` |
| 附件上传 | 20 MiB + 64 KiB | `UPLOAD_MAX_BYTES` |
| `match_reference_regex` | 64 MiB | `MAX_REFERENCE_REGEX_HTTP_BODY_BYTES` |
| 工作区文件上传 | 默认不封顶 | `upload_workspace_file` 的 `DefaultBodyLimit::disable()` |
| 备份上传 | 默认不封顶，可用 `CODEG_BACKUP_UPLOAD_MAX_BYTES` | `backup.rs`，`DefaultBodyLimit::disable()` |
| 客户端丢弃的 WS 文本 | > 4 MiB 字符 | `MAX_WS_FRAME_CHARS` |
| 服务端 attach 帧上限 | 4 MiB | `MAX_ATTACH_FRAME_BYTES`；超出快照变成 `AttachError` / `SnapshotBudgetExceeded`，其它超限帧丢掉，不断 socket |
| axum WS（`ws_handler` 没改） | 消息 64 MiB，单帧 16 MiB | axum 0.8 `WebSocketUpgrade` 默认值 |
| WS 出站队列 | 64 条 `ServerMsg` | `OUTBOUND_CAPACITY` |
| 调用超时 | 默认 60 s；备份控制面 1 h | `WEB_CALL_TIMEOUT_MS` 只是默认值。更长的调用见隧道一节的空闲上限 |

把多兆字节快照和 64 MiB 请求塞进 socket.io 事件，会和 IDE 同步抢那条 20 MiB 缓冲和 120 s ping。本设计另开一条复用机器令牌、但不复用事件名的字节隧道。

Windows 是一等客户端。现有守护进程不是 Windows 服务：计划任务名 `Lifeline Agent`，`LogonType Interactive`、`RunLevel Limited`、触发器 `AtLogOn`，执行 `daemon.ps1` 的 `while ($true)` 监督循环。`RestartOnFailure` 实测不会在非零退出后续跑。安装命令是 `irm <origin>/public/install.ps1 | iex`，升级链用 `;` 而不是 `&&`（PowerShell 5.1 没有 `&&`）。

## Goals & Non-Goals

### Goals

- 登录仍走 Lifeline（password / trusted-header）。登记仍走现有 `lifeline setup`：浏览器登录、一次性 code、每机器令牌哈希。不另做一套 enroll。
- 登录成功后，选机器进入的是 CodeG 页面（`/workspace` 等静态导出），不是 `/console`。
- 信任 Lifeline 已验证身份，保留用户对自己机器的完整编程能力；登录与连接使用哨兵、机器侧注入，服务配置接口不主动回显真令牌。用户授权的文件、终端、代理输出和完整备份不做主令牌内容过滤。
- 这条隧道当公网面时，桌面 web 服务只绑 `127.0.0.1`。服务没在跑时用户看到机器离线，不另起 `server_bin`，也不改绑 `0.0.0.0` 补位。
- 只有 `machines.owner_user_id === 会话 userId` 的人能打开该机器。判定与 `resolveHandshake` / `canSee` 相同。
- 一条具体的请求路径，覆盖：文档加载、`transport.call`、WS attach、401、Lifeline 会话过期。
- Windows 沿用现有交互式计划任务；实现不假设 systemd 或常驻 SYSTEM 服务。

### Non-Goals

- 不改 `require_token` 的相等比较，不加 cookie 认证分支。
- 不把 ACP 翻成 `session:full` 或 `command:*`，不伪装 `ide=cursor`。
- 不做桌面宠物窗、原生文件对话框、reveal-in-explorer。额外端口的 `browser_bridge` 开发服务器预览也不在本隧道内：它用 `页面 hostname:bridgePort`，无法经单一机器源站到达。托管客户端显示不支持，不申请 bridge grant；不要发布 3081–3090 或自定义 bridge 端口补位。同源路径下的 Office 预览按专节处理。
- 不删除 `/console` 及其 IDE 中继。本设计只是不再把它当作 CodeG 的入口。Cursor/CodeBuddy CDP 上行继续走现有 `/agent`。
- 不把 password 模式做成多用户。今天签出的就是 `owner`。
- 不提供 logout / 服务端吊销。现有 cookie 也没有。机器 cookie 同样靠到期；已建立流也必须受此期限约束。
- 不把 `next dev` 或 Tauri 桌面窗接进隧道。
- Lifeline 的 `install.ps1` / `install.sh` 不携带、不安装、不监督 `server_bin`。守护进程不启动 `codeg-server`。原生页要的 `/api` 与 `/ws/events` 只来自已经在跑的桌面应用。

## Proposed Design

### URL 形状

```
https://<apex>/                  Lifeline 落地页、登录、/cli-setup、/machines 选择器
https://<apex>/go/<hostLabel>    同站相对路径，登录后的 next 只能是这种路径
https://m-<hostLabel>.<apex>/    这一台机器的 CodeG 源站（页面、/api、/ws、/_next）
```

`<apex>` = `new URL(PUBLIC_ORIGIN).hostname`，不是 `.host`。`URL.host` 在非默认端口时带端口（`example.com:8443`），不能放进 DNS。未设置 `PUBLIC_ORIGIN` 时机器网关拒绝启动（不能靠 `Host` 猜注册域）。`<hostLabel>` 匹配 `^[0-9a-f]{16}$`。URL 主机是 `m-` 加这 16 位，单个 DNS label。

启动时解析 `PUBLIC_ORIGIN`，不通过则拒绝监听机器主机，并在日志里打规范化结果（只有主机名和端口，没有秘密）：

- 拒绝带 userinfo 的值（`https://user:pass@example.com`）。
- 拒绝带 query、hash，或 path 不是空也不是 `/` 的值。公网协议只允许 `https:` 且 hostname 必须是 DNS 名，拒绝 IPv4/IPv6 字面量（不能给 `127.0.0.1` 拼机器子域）。本机开发仅允许 `http://localhost[:port]`，须验证 `m-<label>.localhost` 解析到 loopback；HTTP 开发 cookie 使用 `SameSite=Lax` 且不设 Secure，不声称它与公网 HTTPS 的同站模型相同。
- 记下 `apexHostname = url.hostname`、`apexPort = url.port`（默认端口时是空字符串）、`apexOrigin = url.origin`（非默认端口会含 `:8443` 这种后缀）。
- 日志一行：`gateway: public origin hostname=<apexHostname> port=<apexPort 或 default>`。

浏览器上的机器源站是：

```
${protocol}//m-${hostLabel}.${apexHostname}${apexPort ? ":" + apexPort : ""}/
```

证书和 DNS 通配符是 `*.<apexHostname>` 再加 apex 自己的 hostname。端口不进 DNS 标签，也不进证书名。`Host` 路由比较的是 hostname：先去掉 `:port`（机器主机不是带括号的 IPv6），再与 `m-<label>.<apexHostname>` 做不区分大小写的比较。`m-<label>.example.com` 与 `m-<label>.example.com:8443` 是同一台机器。

为什么不是路径前缀（`https://<apex>/m/<agentId>/...`）：

- 生产静态导出没有 `basePath`，`assetPrefix` 为空，HTML 引用根上的 `/_next/static/...`。`router.rs` 的 html 重写和 `nest("/api")` 也假设根。
- `WebTransport` 的 base 是 `origin`，不是 `origin + 目录`。`fetch("/api/health")` 和 `location.href = "/login"` 都回到源站根。前缀下的页面仍会打到 `https://<apex>/api/...`，打不到这台机器。
- 若只把页面放在前缀下、把 `/api` 与 `/_next` 留在根上，多台机器的资源会撞车，cookie 的 `Path=/m/<id>` 也附不到 `/api`。
- Next 的 `basePath` 是编译期常量，不能按机器变。运行时改每一个 `fetch` 等于把客户端改成前缀感知，同时还要改静态资源 URL。子域不需要改这些 URL。

为什么不是每台机器一个独立注册域：证书、DNS、和「从 apex 登录再跳回来」都更重。子域本身并不隔开机器。host-only cookie 只保证 A 的 cookie 不会被附到 B；B 自己的 cookie 仍会在同站请求里出现。隔离靠下面的 Origin 相等检查，不是靠「不设 Domain」。

`hostLabel` 不使用 `agentId`。label 在机器首次持久化 INSERT 时生成：8 字节 `randomBytes` 的 hex，唯一冲突则重试。`mintMachineToken` 今天只写 `machine_tokens`，不创建机器行；因此不能只在这个函数里“更新 host_label”。统一登记与改名规则见登记一节。

`m-<label>.<apexHostname>` 只有一层标签，能被 `*.<apexHostname>` 盖住。外层代理必须原样转发 `Host`，并把该通配符指到 **同一个** Lifeline 进程。

### Cookie 与谁可以打开机器

password 的 `lifeline_session` 保持 host-only，只挂在 apex。trusted-header 不签发这张 cookie，它由外部网关认证并注入身份头；不能把 password 的 cookie 与登录页当成所有 provider 的公共契约。两种模式均由 `/go/<label>` 进入机器源站。

每台机器用自己的 host-only cookie。载荷是 `hostLabel`，不是 `agentId`。`agentId` 不能进这条字符串：`AGENT_ID` 可以覆盖 `newAgentId()`，`exchangeBodySchema` 只要求 `min(1)`，值里可以有 `|`；`trusted-header` 的 `userId` 同样没有字符集限制，只拒绝空串和长度大于 128（`MAX_USER_ID_LENGTH`）。password 的 `parseSessionCookie` 用 `split('|')`，机器 cookie 不能抄那份格式。`agentId` 还会变：`AgentDirectory.adoptStaleSibling` 在同一 owner、同一 hostname、旧连接已断开时调用 `renameMachine`，改 `machines` 与 `machine_tokens` 的 id。`host_label` 不跟着改，所以 cookie 必须绑 label。

```
lifeline_machine=v1.<payload>.<hexHmac>; HttpOnly; SameSite=Strict; Path=/
```

`payload` 是下面这个 JSON 的 UTF-8 字节，再做无填充的 base64url。键顺序固定，没有多余空白，这样 HMAC 盖住的是中间这一段原文，校验时不必重新编码：

```json
{"v":1,"hostLabel":"<16 hex>","expMs":123,"userId":"<string>"}
```

`hexHmac` 是 HMAC-SHA256（密钥 `gateway_secret`）对 **payload 段的 ASCII** 的结果，小写 64 位十六进制。比较用恒定时间。不接受旧的 `v1|...|` 管道格式；解析失败就是 cookie 无效。

- 无 `Domain`。公网 HTTPS 使用 `Secure; SameSite=Strict`；仅 `http://localhost` 开发使用 `SameSite=Lax` 且无 Secure。拒绝重复同名认证 cookie，避免父域 cookie 覆盖选择歧义；HTTPS 新机器 cookie 使用 `__Host-lifeline_machine`（本文 `lifeline_machine` 是逻辑简称），不能接受无前缀别名。
- `gateway_secret` 是 32 字节，存在 sqlite 单行，不复用 password 的 `sessionSecret`（trusted-header 部署也要有密钥）。
- `VerifiedIdentity` 增加可选 `expiresAtMs`。password 在验证签名与有效期后返回真实会话期限；`/go` 签发时取 `min(expiresAtMs, now + 14 天)`，将此绝对期限与 `{userId, hostLabel}` 一起写入 bootstrap 记录，兑换时不得重新从零计 14 天。trusted-header / loopback none 没有可验证的上游会话期限，明确采用签发起 14 天上限，不承诺与外部 IdP 同时过期；如部署需要更短寿命，应由可信 provider 提供期限。
- 先检查版本、字段类型、有限安全整数 expMs 与当前时间，HMAC 输入/输出长度严格校验后恒定时间比较；非法或到期均无效。
- 解析之后：cookie 里的 `hostLabel` 必须等于当前请求 hostname 上的 label。再按 `host_label` 查 `machines`。`userId` 必须等于该行的 `owner_user_id`。只信 HMAC 不够（forget / 转主之后旧 cookie 要失效）。label 对不上就不要去查另一台机器。
- `renameMachine` 只改 `agentId`（以及 `machine_tokens.agentId`）。禁止更新 `host_label`。改名之后书签主机和已签发的 cookie 仍然有效，不必重签。
- `auth.kind === 'none'` 且绑定不是 loopback 时，机器网关全部 403。loopback 上的 `none` 仅供本机开发（`verify` 已经是 `owner`）。`AUTH_INSECURE_ALLOW` 下的私网 `none` 仍然 403。

host-only 与 `SameSite=Strict` **挡不住**同一浏览器里从机器 A 打到机器 B。`m-A` 与 `m-B` 是同一个 schemeful site，Strict 仍会在发往 B 的请求上带上 **B 自己的** host-only cookie。`fetch` 默认 `credentials: "same-origin"`，跨源默认不带 cookie；`credentials: "include"` 会带。`new WebSocket` 没有 CORS，握手总会带目标主机的 cookie，打开方能直接读帧。边缘随后让机器注入真正的 `web_service_token`，A 上的脚本就拿到 B 的 `server_operator` 通道。password 模式大家的 `userId` 都是字面量 `owner`，这是同一浏览器里的横向移动，不是理论上的跨站。

所以机器主机上还有一条 Origin 规则，并且 **不要** 调用 `checkOriginAgainstExpected`（`packages/server/src/auth/origin.ts`）。那个函数在设置了 `PUBLIC_ORIGIN` 时只接受 apex 这一个源，机器页的 `Origin` 是 `https://m-<label>.<apexHostname>[:port]`，套用它会把合法的 `/ws/events` 全部拒掉。它还允许缺 Origin，而浏览器的 WebSocket 握手不能缺。

除 `GET /__lifeline/bootstrap` 和下面限定的 Office 能力路由以外（bootstrap 的 HEAD 不消费 code，返回 405）：

- 请求带了 `Origin`：规范化（trim、去掉末尾 `/`）后必须 **精确等于** 这台机器的源站字符串（协议 + `m-<label>.<apexHostname>` + 非默认端口）。`http` 只在上面允许的 loopback apex 上出现。不等则 403，**不要** 打开隧道流。
- `GET /ws/events`（Upgrade）没有 `Origin`：403，不要升到 101。
- 其它请求没有 `Origin`：只允许有效 cookie 下的 GET/HEAD。若有 `Sec-Fetch-Site` 且不是 `same-origin`，仅允许顶层文档导航（`Sec-Fetch-Mode: navigate`、`Sec-Fetch-Dest: document`）；跨机器 iframe、script、img 等子资源请求在边缘拒绝。无 Origin 的非安全方法拒绝。普通 CodeG 文档加 `Content-Security-Policy: frame-ancestors 'self'`，禁止跨机器嵌入。

除下文 Office 能力前缀外，边缘删除所有上游 Access-Control-Allow-* / Access-Control-Expose-Headers，不自行添加 Allow-Credentials。普通 CodeG fetch 同源，不需要 CORS。Office opaque-origin 的例外仅作用于已验证的单文档能力路由，不扩展到一般 API/WS。

测试：在 `m-A` 的页面里 `new WebSocket('wss://m-B.<apex>/ws/events')` 不得升到 101（即使 B 的 cookie 会被浏览器附上，Origin 是 A）。B 自己的页面打开 `/ws/events` 可以 101。从 A `fetch` B 且 `credentials: "include"` 得到 403，不进隧道。

打开机器的判定，和 handshake 是同一身份：

1. apex 会话 `userId`（password 下就是 `owner`）通过 `canSee`。
2. 机器行的 `owner` 来自登记 code 上的 `userId`，handshake 再用令牌哈希取出同一个 `owner`。
3. 对不上的 bootstrap、`/go`、以及带了别人 cookie 的请求：404（不区分「没有这台机器」和「不是你的」，避免用 label 探资产）。label 本身不是能力凭证；没有 cookie 只会被送去登录。

`MachineInfo.connected` 今天表示 **IDE socket.io 还连着**（`listMachines` 里 `conn.socket?.connected`）。选择器不得复用这个字段当成 CodeG 可达。新增 `codegTunnelConnected` 与 `codegReady`。

### 令牌方案（sentinel + 机器侧注入）

公开常量，CodeG 与 Lifeline 写成同一个字符串：

```
CODEG_LIFELINE_SENTINEL = "codeg-lifeline-session"
```

它不是秘密。谁都可以知道。它唯一的作用是让现有客户端「空令牌就去登录 / 不连 WS」的分支有一个非空值。

桌面 web router 仍只检查令牌字符串相等。注入的是 `resolve_web_service_token` 返回的 `web_service_token`，不另造、不轮换。生成器的 32 位 hex 与哨兵不可能相等，但显式 override / 持久化配置可以人为设为哨兵，故仍需比较，相等则拒绝 ready。

#### 浏览器存什么

只在托管页面上。页面 `<head>` 里由边缘注入（见下），不含任何秘密：

```html
<meta name="codeg-access" content="lifeline">
<meta name="codeg-account-origin" content="<apexOrigin>">
<meta name="codeg-host-label" content="<16 hex>">
```

`getCodegToken()`（`src/lib/transport/web-auth.ts`）同步读 meta：三者都合法时，把 `localStorage["codeg_token"]` 写成哨兵并返回哨兵。不合法则保持今天的行为（读已有键，可能为空）。`page.tsx` 改为调用 `getCodegToken()`，禁止再直接 `getItem`，否则子组件 effect 顺序会在写入之前就把空键送去 `/login`。

`account-origin` 必须能被 `new URL` 解析，并且等于该 URL 的 `origin`。协议只接受 DNS 名的 `https:`，以及 hostname 为 `localhost` 的 `http:`。`host-label` 必须匹配 `^[0-9a-f]{16}$`，当前 `location.origin` 还必须等于由这两个值构造的机器源站；不接受任意外站 meta 作为跳转目标。

直接打开 `http://127.0.0.1:<port>` 的人没有这三枚 meta，继续在 `/login` 粘贴真令牌。桌面 Tauri 窗本身不走这条路径。

#### 谁改写

两段都改，防御不一致：

1. **Lifeline 边缘**（认证协议不要求持有 `web_service_token`；用户授权的数据通道按上述信任模型处理）在把请求放进隧道之前：
   - 删除 `Authorization`、`Proxy-Authorization`、`Cookie`（不把 `lifeline_machine` 送到机器上）。
   - 删除 `Sec-WebSocket-Protocol` 里的 `codeg-token.*`。保留 `codeg-events` 与否无所谓，机器会自己组子协议。
   - Open 帧带 `injectCodegAuth: true|false`。带了浏览器原始 Authorization 的 Open 视为协议错误，流 `Reset`，请求 502。失败闭合：边缘实现错了也不要靠「把哨兵原样转发」碰巧工作。
2. **机器守护进程**（交接令牌在内存中用于本地认证，不记录或作为登录结果交给浏览器）只对 `injectCodegAuth: true` 的流，在拨 `127.0.0.1:<port>` 时写上：
   - `Authorization: Bearer <web_service_token>`
   - WS 子协议：`codeg-events` 与 `codeg-token.<base64url(web_service_token)>`（`URL_SAFE_NO_PAD`，与 `auth.rs` / `ws-auth.ts` 一致）
   - 原样转发 `x-codeg-completion-context`（这是桌面 web router 自己签发的短生命周期能力，不是会话秘密；不转发会让 completion 变更全部 401）。
   - `injectCodegAuth: false`（静态页、以及边缘明确标成匿名的下载）不加 Bearer。

`require_token` 不改。相等失败的正文仍是现在那三句。

#### 边缘如何处理桌面 web router 的 401

| 来源 | 给浏览器的状态 | 正文 `code` | 客户端现有反应 |
| --- | --- | --- | --- |
| 没有合法 `lifeline_machine` | 401 | `lifeline_session_expired` | `page.tsx` 与 `call` / 探针都走登出。托管模式下登出目标是 apex，见下 |
| 隧道未连或 `codegReady === false`，且请求不是文档导航 | 503 | `machine_offline` | `probeHealth` 只在状态 **等于** 401 时 `markUnauthorized`。503 不删哨兵，退避重试 |
| 同上，但是文档导航 | 503 | 无 JSON。自包含 HTML | 不进隧道。`Cache-Control: no-store`。用户看到离线页，不是一段 JSON |
| 静态流配额用尽 | 503 | `static_saturated` | `Retry-After: 0`。不是离线。脚本或样式 503 浏览器不会重试，视为发布阻断 |
| `/api` 流配额用尽 | 503 | `machine_busy` | 只用于 API 池。不要拿这个 code 表示静态资源或整机离线 |
| `/ws/events` 流配额用尽 | 503 | `ws_saturated` | 不升 101。不是 `machine_offline` |
| 已注入真令牌，上游正文仍是 `Invalid or missing token` 或 `Server token is not configured` | 502 | `codeg_token_mismatch` | 与 503 一样不登出。这是机器配置错误，重新登录 Lifeline 修不好 |
| 已注入真令牌，上游正文是 `Invalid completion context` | 401，正文原样 | （纯文本，与今天相同） | 仍会 `markUnauthorized`。这是今天 loopback 上也存在的行为，不在这里重定义 |
| owner 不匹配 | 404 | 空 | 不进隧道 |

隧道 down 时禁止回 401。loopback connect 失败同样映射离线；注入令牌错误先回 502 并触发 down；服务配置禁止命令返回 403。这里的 5xx 不导致登出，但普通 `call()` 仍向调用者抛错，不代表操作会自动重试。

### 文档加载

```mermaid
sequenceDiagram
  participant B as Browser
  participant A as Lifeline apex
  participant E as Lifeline machine host
  participant T as Tunnel
  participant M as Machine daemon
  participant C as Desktop web 127.0.0.1

  B->>A: GET /login (无 lifeline_session)
  A-->>B: password 登录页
  B->>A: POST /api/login
  A-->>B: Set-Cookie lifeline_session (host-only, apex)
  B->>A: GET /machines
  A-->>B: 选择器 HTML (dist/client)
  B->>A: GET /go/<hostLabel>
  Note over A: canSee；签发 60s 一次性 bootstrap code
  A-->>B: 302 https://m-<label>.<apex>/__lifeline/bootstrap?code=...
  B->>E: GET /__lifeline/bootstrap?code=
  Note over E: 兑换即焚；核对 userId 与 owner
  E-->>B: 302 / 并 Set-Cookie lifeline_machine (host-only)
  B->>E: GET /workspace + Cookie lifeline_machine
  E->>T: Open kind=http path=/workspace injectCodegAuth=false
  T->>M: Open
  M->>C: GET /workspace
  C-->>M: text/html (ServeDir, 无 require_token)
  M-->>E: Head + Data
  Note over E: 非 /api 的 text/html：按 Content-Encoding 解压，注入 meta，以 identity 回传
  E-->>B: 合法 HTML，Content-Length 为注入后的长度
  Note over B: getCodegToken 同步写入哨兵
```

`/__lifeline/bootstrap` 与 `/__lifeline/offline` 不进隧道。bootstrap code 为 `randomBytes(32)` hex，TTL 60 s，绑定 `{userId, hostLabel, machineExpiresAtMs}`；校验当前 host label、owner、code TTL 和绝对会话期限后原子兑换即删。失败不种 cookie，不能跨机器兑换。所有跳转加 `Referrer-Policy: no-referrer` 与 `Cache-Control: no-store`；应用和外层代理日志均隐藏 query 中的 code。

机器主机的所有登录/会话恢复跳转统一是绝对 `apexOrigin + "/go/" + hostLabel`。`/go` 自己经过 apex 身份门：password 未登录时走现有 `onHttpDenied` 到 `/login?next=/go/<label>`；已登录则直接签发 bootstrap；trusted-header 交给外部网关。不要从机器页直接跳 `/login`：password 已登录时该路由会忽略 next 回 `/`，trusted-header 根本不注册它。password 的相对 next 校验必须同时用于服务器和页面，已登录的 `/login` 也应尊重合法 next，缺省 `/machines`。

机器主机是单独的路由，**不安装** password 的 `onHttpDenied`。那个钩子对页面 GET/HEAD 做的是相对 `redirect(\`${target}?next=${encodeURIComponent(req.url)}\`)`（`password.ts`）。用在机器主机上，Location 会落在 `https://m-<label>.<apexHostname>/login`，也就是 CodeG 的粘贴页，而不是 apex。

#### 什么叫文档导航

按这个顺序判断，后面的 503 分流用同一条规则：

1. `Sec-Fetch-Mode: navigate` → 文档导航。
2. 这个头存在且不是 `navigate`（`cors`、`no-cors`、`same-origin`、`websocket`）→ 不是文档导航。
3. 头缺失：仅当方法是 `GET` 或 `HEAD`，路径不以 `/api/` 开头，不是 `/ws/events`，且最后一段不含 `.`（与 `onHttpDenied` 把带点的最后一段排除出页面的启发式相同）。这样没有该头的 `GET /workspace` 仍是页面，`GET /_next/static/...js` 不是。

#### HTML 注入

`compression_layer()` 包住整个路由，包括 `ServeDir`（`router.rs` 末尾 `.layer(compression_layer())`）。`CompressibleContentType` 对所有 `text/*` 返回 true，单测包含 `text/html`；`text/event-stream`、`application/octet-stream`、zip、png、`font/woff2` 不压。阈值 `MIN_COMPRESS_BYTES = 32`。浏览器带 `Accept-Encoding: gzip, deflate, br` 时，生产 HTML 远大于 32 字节，隧道里的字节是压缩的。边缘如果不解压就扫描，找不到 `</head>`，三枚 meta 不会出现，`getCodegToken()` 退回粘贴登录。三条令牌失败正文都短于 32 字节，502 映射仍能看到明文；不要据此认为 HTML 也是明文。

决定：`Accept-Encoding` **继续原样转发** 给 loopback 上的桌面 web router。不要在机器侧为了注入而全局删掉它——`compression.rs` 的注释写明这层是为了压几十 MB 的 JSON。只对边缘 **准备改写** 的响应解压。

改写条件：路径不以 `/api/` 开头，且 `Content-Type` 去掉参数、转小写后是 `text/html`。`/api/*` 即使是 `text/html` 也不注入（`office-watch-proxy` 会回上游 HTML）。

改写步骤：

1. `Content-Encoding` 只接受缺省、`identity`、`gzip`、`deflate`、`br` 四者之一。多个编码、未知编码、或解压失败：503 不用，回 **502** `html_encoding`。不要把压缩字节当 HTML 交给浏览器，也不要静默不注入。
2. 解压后正文上限为 `TUNNEL_HTML_REWRITE_MAX_BYTES`（4 MiB，注入 meta 前），超出回 502 `html_encoding`。改写前预留完整 4 MiB 名额，每隧道最多 `TUNNEL_MAX_HTML_REWRITES = 2`；无名额立即 503 `html_rewrite_busy` 并 Reset 该流，不占着半篇等待。收齐缓冲独立于普通转发的 8 MiB 连接窗口，故两篇待 End 的 HTML 不会阻断控制帧或其它流。Window 只按解压器已接受的 **原始 Data 载荷字节数（压缩字节）** 归还，不能按解压字节数归还；输出每块检查 4 MiB 上限，解压器写队列满时暂停该流的生产者。`out/workspace.html` 当前为 380201 字节，但测试必须覆盖大于 1 MiB 的 HTML。
3. 在解压后的前 65536 字节内不区分 ASCII 大小写查找 `</head>`，插入三枚转义后的 meta。找不到或已有冲突 meta：502 `html_bootstrap`，不返回会诱导粘贴真令牌的半可用托管页。
4. 给浏览器的头：删除 `Content-Encoding`、上游 `ETag`、`Last-Modified`、`Content-Range` 和已失效的内容摘要；重算 `Content-Length`，`Cache-Control: private, no-store`。HTML 文档请求在边缘去掉 `If-None-Match` / `If-Modified-Since` / Range，确保得到可改写的完整 200，不能把旧缓存 304/206 当新托管页。HEAD 不收集或注入正文，去掉原长度与验证器，不凭空构造 body；204/304 等无正文响应不得进入收齐流程。
5. 边缘不改写的响应（JS、CSS、JSON、字体、下载、`/api`）原样转发 `Content-Encoding` 和 body。外层代理只有在响应 **没有** `Content-Encoding` 时才可以再压缩；改写后的 HTML 就是这种情况。

集成断言：带 `Cookie` 和 `Accept-Encoding: gzip` 的 `GET /workspace`，到达浏览器的 body 不是 gzip 魔数 `1f 8b`，是合法 HTML，且 `<head>` 里有 `codeg-access`、`codeg-account-origin`、`codeg-host-label`。

静态资源与页面不注入 Bearer。router 的 `ServeDir` 本来就不检查令牌。

### `transport.call`

```mermaid
sequenceDiagram
  participant B as Browser
  participant E as Lifeline machine host
  participant M as Machine daemon
  participant C as Desktop web router

  B->>E: POST /api/<command><br/>Authorization: Bearer codeg-lifeline-session<br/>可选 x-codeg-completion-context<br/>Cookie: lifeline_machine
  Note over E: 校验 cookie + owner + codegReady<br/>删除 Authorization 与 Cookie
  E->>M: Open kind=http injectCodegAuth=true<br/>path=/api/<command>?query<br/>headers 不含 Authorization
  M->>C: POST /api/<command><br/>Authorization: Bearer web_service_token<br/>completion 头原样
  Note over C: require_token 相等<br/>再查 completion registry
  C-->>M: 状态 + body
  M-->>E: Head + Data 流
  E-->>B: 原样状态；令牌不等被改写成 502
```

`call()` 的 URL 不用改：在机器源站上 `origin` 就是 `https://m-<label>.<apex>`。`src/lib/api.ts` 里写死 `window.location.origin` 的上传同样落到这台机器。

默认请求体累计上限 64 MiB，对齐 `MAX_REFERENCE_REGEX_HTTP_BODY_BYTES`。`backup_upload` 与 `upload_workspace_file` 在桌面默认不封顶，经隧道默认均限制为 64 MiB；有效 `ready.maxRequestBody` 可提高机器级上限，来源暂复用 agent 自身环境中的 `CODEG_BACKUP_UPLOAD_MAX_BYTES`，只接受正安全整数并在界面说明其同时影响两类上传。已知 Content-Length 超限在 Open 前 413；未知长度须边读边计数，越限时 413 + Reset 已开的流，取消 loopback 请求，不得承诺“全部超限请求都不开流”，也不得为预判而先收齐 64 MiB。响应累计不设上限。下载、上传与 SSE 保持流式；浏览器取消立即 Reset、取消上游和释放池。普通 HTTP 空闲 65 分钟。Office SSE 可能因此重连：保持原生 EventSource 自动重连，必须验证恢复；v1 不新增 SSE 心跳或无限寿命豁免。

边缘与机器共同校验请求目标，不能仅在边缘做：

- 将原始 pathname 与 query 分开；pathname 必须以单个 `/` 开始，无 scheme/authority、反斜杠、NUL 或控制字符，解码一次后再检查。解码失败回 400。
- 拒绝解码后的 `.` / `..` 段以及编码路径分隔符。不能靠 `posix.normalize` 后查 `..`：`/../../api/health` 会被归一成 `/api/health`，证据已消失。命令禁止名单在这个同一规范路径上匹配，避免别名绕过。
- 校验通过后 Open 仍携带原始编码的 pathname 与 query，不能解码整个 URL 再转发而改变 `%3F`、`%25` 或 cap 的含义。所有日志只记 pathname；query 可能含下载票据/Office cap，默认不记录。
- 机器只拨交接文件的 `127.0.0.1:<port>`，关闭 HTTP 客户端自动解压、自动跟随 Location 以及环境代理（否则可能把注入的 Bearer 发到别处）。Host 由本地目标生成，删除浏览器的 Forwarded / X-Forwarded-*，不使用请求 Host 做拨号。3xx 原样返回前检查 Location，禁止泄漏 loopback 内部地址或认证头。

Hop-by-hop 头按大小写不敏感处理：删除 `Connection` 及它点名的所有字段，再删除 `Keep-Alive`、`Transfer-Encoding`、`TE`、`Trailer`、`Upgrade`、`Proxy-*`；HTTP 长度与分块由每跳重建。两侧拒绝 CR/LF 注入、冲突的长度以及重复认证头。WS 握手头由各跳库生成。请求 Authorization、Cookie 及 token 子协议仍须剥离，响应 Set-Cookie 不从机器透传。保留 Accept-Encoding，普通响应的压缩字节原样传递。上游 CORS 默认剥除，只有下一节的窄例外。

#### Office 沙箱的派生能力

`office-preview.tsx` 刻意不设 `allow-same-origin`。其 fetch/EventSource 带 `Origin: null`、默认不带机器 cookie，JSON POST 先做无凭据 OPTIONS。不能靠全局放开 null Origin 或给 iframe 加同源权限解决。

保留预览功能的最小适配是复用现有单文档 `cap`，同时让边缘记录它由哪个机器会话授权：

1. 仅对已通过机器 cookie + owner 门的 `POST /api/start_office_watch` 的成功小 JSON 响应，边缘解析 `{port,cap}`（必要时先解压；解析缓冲上限 64 KiB，仍按原始字节归还窗口），登记内存映射 `(hostLabel,port,capHash) -> {userId,expMs,tunnelGeneration}`。不记录原 cap，不持久化；到期清除，同一 cap 重复登记不得延长旧授权的期限，需重新打开预览刷新。返回内容原样，不替换桌面的 cap。
2. 只有精确的 `/api/office-watch-proxy/<十进制端口>[/...]` 可用该映射替代机器 cookie：要求当前 label、owner、未过期授权和隧道代次全部匹配，且携带唯一 cap。机器仍以 `injectCodegAuth:false` 转发，桌面的 `validate_watch_cap` 再查活跃 watch/端口白名单。错误 cap 不可碰其它 API。
3. 该前缀允许 `Origin: null`，以及合法的本机源；有 Origin 的其它值拒绝。OPTIONS 仅回答此路由的窄预检（GET/POST/HEAD 与 Content-Type），不进隧道、不返回数据、不授予能力；实际请求必须过上述能力门。只在这个前缀返回 `Access-Control-Allow-Origin: *`，不加 Allow-Credentials。保留 iframe sandbox，普通 API/WS 的 Origin 规则不变。
4. 边缘重启、会话期限到达、owner 改变或隧道代次变化就失效，既有 SSE 同时关闭。用户重开预览重新获取授权。cap query 在两侧及外层代理访问日志中脱敏。预览 HTML 保持 `no-store`，不做 CodeG meta 注入。

浏览器级测试必须覆盖真实 sandbox iframe 的初次加载、SSE 刷新、编辑 POST 预检及失败 cap；不能只用手工带 cookie 的 fetch 代替。

### WebSocket attach

```mermaid
sequenceDiagram
  participant B as Browser
  participant E as Lifeline machine host
  participant M as Machine daemon
  participant C as Desktop web router

  B->>E: GET /ws/events Upgrade<br/>Sec-WebSocket-Protocol: codeg-events, codeg-token.<b64(哨兵)><br/>Cookie: lifeline_machine
  Note over E: 同 HTTP 门；剥离 codeg-token.*
  E->>M: Open kind=ws path=/ws/events injectCodegAuth=true
  M->>C: Upgrade /ws/events<br/>子协议 codeg-events, codeg-token.<b64(web_service_token)>
  Note over C: require_token 解码子协议并相等<br/>ws.protocols 只选中 codeg-events
  C-->>M: 101 Sec-WebSocket-Protocol: codeg-events
  M-->>E: Head status=101 该头
  E-->>B: 101 Sec-WebSocket-Protocol: codeg-events
  C-->>B: 文本 {"channel":"__ready__","payload":null}
  Note over B: 5s 内必须到达，否则 waitForReady 超时继续
  B->>C: 文本 ClientMsg（attach / detach / ping）
  C-->>B: 文本 ServerMsg（snapshot / replay / event / detached）
```

代理必须保住的，来自 `ws.rs` / `ws_attach.rs` / `web-transport.ts`：

- 选中的子协议是 `codeg-events`，不是 `codeg-token.*`。浏览器若收不到这个回选，握手失败，`onclose` 只有 1006，客户端只能靠随后的 `/api/health` 区分 401 和网络错误。
- 文本帧顺序不能并、不能换。第一条是 `__ready__`。attach 帧是 JSON 文本，`type` 字段；旧的全局广播是 `channel` 字段。两条共存。
- 客户端把大于 4 MiB 字符的帧跳过 `JSON.parse`，并用前 512 字符里的 `subscription_id` 调 `notifyOversizedFrame`。所以隧道不能静默丢掉「略大于 4 MiB」的帧。单条 WS 消息上限设为 **8 MiB**（高于应用层 4 MiB，低于 axum 默认 16 MiB 帧 / 64 MiB 消息）。再大则 **只** `Reset` 这条流，不断隧道。正常 attach 路径上 `serialize_server_msg` 已经不会发出大于 4 MiB 的快照。快照仍会越过 256 KiB 的隧道帧上限，所以一条 WS 消息必须能由多片 Data 组成，并且有明确的结束位。见隧道帧的 FIN。
- 应用层忽略 Binary。协议层 Ping/Pong 由每一跳自己的库处理（`ws.rs`：axum 处理 ping/pong），**不是** 隧道 Data，也不刷新 HTTP 空闲，也不因此给 `kind: "ws"` 加应用层空闲 Reset。应用层 ping 只在存在 `shared === true` 的订阅时每 `30_000` ms 发一帧（`web-event-stream.ts` 的 `syncSharedHeartbeat`）。普通 attach 可以远超过 60 秒没有任何 Data，这条流不得因此被 Reset。浏览器不会自己发 WebSocket 协议 Ping。边缘对浏览器这条 `/ws/events` 每 `TUNNEL_PING_INTERVAL_MS`（25_000 ms）主动发一次协议 Ping，浏览器自动 Pong；120 s 无 Pong 则关闭对应流并释放 WS 槽。这个间隔短于外层代理为这条连接准备的读超时。隧道上的 Ping 不转发成浏览器的 Ping，反过来也不把浏览器的 Pong 当成隧道 Data。边缘与浏览器之间、机器与 loopback 上的桌面 web router 之间，都 **不** 协商 `permessage-deflate`，避免一边压缩一边当文本转发。
- 大于 1 MiB 的消息不能等隧道 FIN 再交给浏览器。axum 把整条消息交给机器（attach 路径上限为 `MAX_ATTACH_FRAME_BYTES`，4 MiB；legacy 广播没有该输出保护），机器再切成 ≤ 256 KiB 的 Data。若边缘把这些片留到 FIN，并且要等浏览器 `onmessage` 才归还 Window，第一条超过 `TUNNEL_STREAM_WINDOW_BYTES`（1 MiB）的快照会在约 1 MiB 处停住，FIN 不到，后面的短消息也发不出。选定的做法是分片转发，不是在边缘收齐：
  - 每一片 Data 立刻写成对端 WebSocket 帧。该消息的第一片使用文本或二进制 opcode；若 FIN 为 0，这一帧的 WebSocket FIN 为 0。后续片是 continuation（opcode 0）。隧道 FIN 为 1 的那一片，对应 WebSocket 帧的 FIN 为 1。只有一片时，opcode 仍是文本或二进制，WebSocket FIN 为 1。
  - 机器到 loopback 的方向相同：隧道片一到就写成给桌面 web router 的 WebSocket 分片，不要等隧道 FIN 再写。
  - 浏览器 `onmessage` 仍得到一条消息。边缘不保留整条快照。8 MiB 是明确的隧道兼容性上限：legacy 全局广播不像 attach 有 4 MiB 输出保护，超限可能导致此 WS 断开；v1 保留 Reset，不承诺与直连的“跳过超大文本”完全相同。已有部分分片写出后不能丢剩余片再发送下一条。必须用超限 legacy 广播测试确认不影响其它流，持续触发则阻断发布并在源端另修输出预算，不能单纯提高隧道上限。
  - Window 在对端套接字收下这些字节时归还，不是在看到隧道 FIN、也不是在 `onmessage` 时才归还。套接字写缓冲满了就不归还，这是正确的反压。
- 出站队列只有 64 条。窗口压住时机器停止再切下一片；axum 已经交给机器的那一条消息可以留在机器内存里，直到窗口恢复。不要在边缘再缓冲整条快照。

客户端 `buildCodegWebSocketProtocols` 不用改：它会把哨兵编进子协议。边缘丢掉这段，机器换上真令牌。

### 401 与 Lifeline 会话过期

```mermaid
sequenceDiagram
  participant B as Browser
  participant E as Lifeline machine host
  participant A as Lifeline apex

  alt 隧道 down 或 codeg 未 ready，且是 /api 或 fetch
    B->>E: POST /api/health Bearer 哨兵
    E-->>B: 503 JSON machine_offline
    Note over B: 不删 codeg_token；退避重试 1s→32s
  else 隧道 down，且是文档导航
    B->>E: GET /workspace + 有效机器 cookie
    E-->>B: 503 自包含离线 HTML，不进隧道
  else cookie 缺失、过期或 HMAC 失败，且是 /api（owner 不匹配单独回 404）
    B->>E: POST /api/health
    E-->>B: 401 lifeline_session_expired
    Note over B: markUnauthorized；不立刻删键
    B->>E: 用户点 Go to login
    Note over B: redirectToCodegLogin 见 meta<br/>删哨兵
    B->>A: GET <apexOrigin>/go/<label>
    A-->>B: 登录后 302 /go/<label>，再走 bootstrap
  else 文档导航时 cookie 已死
    B->>E: GET /workspace
    E-->>B: 302 绝对 URL，apexOrigin/go/<label>
    Note over E: 不调用 onHttpDenied
  else 注入后 codeg 仍说令牌不对
    E-->>B: 502 codeg_token_mismatch
    Note over B: 重连框，不去登录
  end
```

托管模式下 `redirectToCodegLogin()`：

- 删 `codeg_token`。
- 若 meta 合法：`location.href = accountOrigin + "/go/" + hostLabel`。
- 否则保持今天：已在 `/login` 则返回，不然去 `/login`。

`/login` 页：发现合法 meta 就走同一 `/go/<label>` 跳转，不画粘贴框。托管认证依赖机器会话；浏览器粘贴的任意非空字符串都会被剥掉，机器仍注入真令牌，因此旧登录页并非“永远登不进”，而是一个误导性的凭据输入面。

`page.tsx` 对 `/api/health` 的 401 今天会删键并 `replace("/login")`。改为调用 `redirectToCodegLogin()`，这样托管模式去 apex，loopback 模式行为不变。

机器会话到期以其 `expMs` 为准；HMAC 错误或 owner 改变也使其无效。边缘给已建立的 HTTP/WS 流保留 `{hostLabel,userId,expMs}`，到期主动关闭（WS 用 1008 并释放槽位），不能只在 Upgrade/Open 时检查。owner 变更、forget、机器令牌轮换要关闭对应旧隧道及流；无全局 logout 功能不等于已建立连接可以无限续命。apex 重新认证后经 `/go/<label>` 换新机器 cookie。

### 隧道

不扩展 `/agent` 的 socket.io。那条连接继续给 IDE 投影用。新通道（公网连接只用经证书验证的 wss，不允许关闭 TLS 校验；仅 loopback 开发可 ws）：

- 路径：`/machine-io`，仅 apex 的 Upgrade 分派器接受，第一帧用机器令牌认证。裸 WebSocket 升级不经过 Fastify `gateHttp`，无需扩展 `isPublicPath`；普通 HTTP 到此路径返回 426，不暴露控制面。
- 传输：裸 WebSocket，子协议 `lifeline-machine-v1`。不是 socket.io。
- 鉴权：升级后 5 s 内第一条必须是文本 `{"op":"auth","agentToken":"<plaintext>"}`。服务器 `IdentityStore.resolveHandshake`（与 agent hub 同一函数，同一 sqlite）。成功回文本 `{"op":"auth-ok","agentId":"..."}`。失败关 1008。同一 `agentId` 的新隧道替换旧的，旧流全部 `Reset`；身份仍必须匹配机器当前 owner，已登记的隧道也要绑定令牌哈希以在轮换时关闭旧连接。
- auth/auth-ok 之后只接受二进制消息：一个 WebSocket message 恰好包含一个 10 字节头和 `length` 字节载荷，限制底层 `maxPayload` 为 10 + 256 KiB，拒绝尾随字节。所有类型都检查长度。stream 0 专用于 Control、连接级 Window、Ping/Pong；Control 载荷是 ready/codeg-down JSON，Ping/Pong 是配对的 8 字节 nonce。不能混用“文本消息带 stream id”的两种表示。

帧头 10 字节，大端：

| 偏移 | 字段 | 说明 |
| --- | --- | --- |
| 0 | `type` u8 | 1 Open，2 Data，3 Window，4 End，5 Reset，6 Ping，7 Pong，8 Head，9 Control |
| 1 | `flags` u8 | 见下表。非 Data 必须为 0 |
| 2 | `streamId` u32 | 边缘分配奇数，从 1 起。v1 机器不主动 Open。0 = 控制 |
| 6 | `length` u32 | 载荷字节数。单个隧道帧的 Data/Head/Open 载荷 ≤ `262144`（256 KiB）。`length` 更大是协议错误：断开 **整条** 隧道。这和「一条 WS 消息拼到 8 MiB」不是同一条规则 |

Open 载荷是 UTF-8 JSON，一帧装下（路径和头很小；装不下则 400，不要拆 Open）：

```json
{
  "kind": "http",
  "method": "POST",
  "path": "/api/health",
  "headers": [["content-type", "application/json"], ["x-codeg-completion-context", "<cap>"]],
  "injectCodegAuth": true
}
```

`kind: "ws"` 时没有 `method`，只接受 `/ws/events`。禁止 Open 携带 `authorization`（大小写不敏感）、Cookie、token 子协议；命令禁止名单与路径规则在机器端再次执行。流 ID 单调增加且不复用，u32 用尽前重建隧道；未知/已结束流的迟到帧丢弃，不得复活流。

Head 载荷（机器 → 边缘），同样是一帧 JSON：

```json
{ "status": 200, "headers": [["content-type", "application/json"]] }
```

WS 成功时 `status` 为 101，头里带 `sec-websocket-protocol: codeg-events`。HTTP 的 Head 必须在任何 Data 之前。

Data 的 `flags`：

| 位 | 掩码 | 含义 |
| --- | --- | --- |
| bit0 | `0x01` | WS 文本 opcode |
| bit1 | `0x02` | WS 二进制 opcode |
| bit2 | `0x04` | WS close |
| bit3 | `0x08` | FIN：本片是这条 WebSocket **消息** 的最后一片 |
| bit4–bit7 | | 必须为 0 |

- HTTP 的 Data：`flags` 必须为 0。HTTP body 是字节流，由 End 结束，没有消息边界，也不使用 FIN。非 0 只 `Reset` 这一条流。
- 非 Data 帧的 `flags` 必须为 0。`streamId != 0` 时只 `Reset` 该流；`streamId == 0` 时断开整条隧道（控制帧界坏了）。
- WS text/binary 的 opcode 位必须恰有一个；同一消息 opcode 不变，中间片 FIN=0，末片 FIN=1，允许空消息。发送使用现有 `ws.send(chunk, {binary, fin, compress:false}, callback)` 的分片接口，不能每片都用默认 FIN=true。close 独立为一帧、FIN=1、最多 125 字节，校验关闭码/UTF-8，不得分片或混用 text/binary 位。接收片就转给对端，不等 FIN；异常部分消息只 Reset 该流。
- 已转发字节超过 `TUNNEL_MAX_WS_MESSAGE_BYTES`（8 MiB）只 `Reset` 该流，不断隧道。单帧 `length > 262144` 或控制面/连接信用违规断开整条隧道，消息累计超限仅 Reset 流。

往返测试（PR 3）：一条 `4 * 1024 * 1024` 字节的文本消息（等于 `MAX_ATTACH_FRAME_BYTES`，大于 `TUNNEL_STREAM_WINDOW_BYTES`；FIN 只在最后一片），紧接着一条短文本消息。对端必须得到两条消息，而不是一个拼接缓冲。测试代码不得另外注入 Window。接收实现在每片被对端套接字（测试里是立即接受的 sink）收下时自己发 Window。若实现要等 FIN 才归还信用，发送方会在约 1 MiB 处停住，这条测试不得通过。另有一条大于 256 KiB 且小于 1 MiB 的消息，用来锁住分片边界，不能代替上面这条。8 MiB 上限只拆这一条流。

End：HTTP 两个方向各半关闭一次，双向 End 后释放；任何方向 Reset、浏览器 Abort 或 socket 关闭立即取消对应上游并释放池，操作幂等。WS close 完成后回收流，不等 HTTP 双 End；浏览器侧断开也要通知机器。Reset 为 u16 代码 + 最多 200 字节 UTF-8 原因。Window 为恰好 4 字节的正 u32 增量，不得使剩余信用超过初始上限，不接受整数溢出。

背压（按每条隧道、每个方向独立计算）：

- 每流初始 Data 信用 1 MiB；新增连接总信用 `TUNNEL_AGGREGATE_BUFFER_BYTES = 8 MiB`，使用 `Window(streamId=0)` 补充。发 Data 同时扣流信用和连接信用，消耗单位均为原始载荷字节；不能让 200 条流各自预读 1 MiB 后再声称总缓冲只有 8 MiB。
- 普通 HTTP / WS 在目标套接字接受该块且写缓冲未满时归还等量的两级信用；写回调/drain 表示可继续，不代表浏览器应用已消费。慢消费者只停自己的上游生产者；不能 pause 整条复用 socket，把其它流的 Window/End/Ping 一起堵住。控制帧不消耗 Data 信用，控制队列单独有界并优先调度，滥发控制帧关闭隧道。
- HTML 使用事先预留的独立 4 MiB 收齐空间（最多两份）；按解压器接受的原始字节归还两级信用。Office 授权 JSON 的 64 KiB 缓冲也单独计入解析预算。解压输出受自己的硬上限约束，不能混入压缩信用。
- 8 MiB 是等待转交的连接 Data 预算，不是进程总 RSS。HTML 最多 8 MiB、WS 库的入站完整消息、每跳 socket 缓冲和有限解压块还需分别计入测量。WS 库设消息上限且最多保留每流一条待分片消息，不能无限排队。
- 池满不排队，按下表返回 503；与流控信用耗尽不同，后者等待信用而不回 503。控制面始终能推进，双方同时大上传/下载不能因互等 Window 死锁。

| 池 | 常量 | 计入条件 | 超出时 |
| --- | --- | --- | --- |
| 静态 | `TUNNEL_MAX_STATIC_STREAMS = 160` | `kind: "http"`，方法 `GET` 或 `HEAD`，路径既不以 `/api` 开头，也不以 `/ws` 开头 | 503，JSON `static_saturated`，`Retry-After: 0`。不是 `machine_offline`，也不是离线 HTML |
| API | `TUNNEL_MAX_APP_STREAMS = 32` | 路径以 `/api` 开头的 HTTP，以及其它非静态 HTTP（含 POST） | 503，JSON `machine_busy`。只表示这个池满了 |
| WS | `TUNNEL_MAX_WS_STREAMS = 8` | `kind: "ws"`（产品上只有 `/ws/events`） | 503，JSON `ws_saturated`，不升 101 |

  静态产物复核：`out/workspace.html` 380201 字节，44 个 JS、3 个 CSS，加文档共 48 个 URL。三份 CSS 中两份分别引用 20 和 31 个唯一 woff2，总计 51，静态分析上界为 99（另计 favicon）；字体按实际使用与 unicode-range 按需加载，99 不是实测请求数，更不是峰值并发。删去原稿“两标签约 136、必低于 160”的保证。维持 160 配额，发布门同时要求单页资源上界 ≤128，以及真实冷缓存单/双标签、现有长下载/SSE/备份占槽时无 static_saturated 或 machine_busy。API 池也要测峰值，`call()` 不自动重试 503；失败则调整测得的容量后再发布，不凭总 URL 数放行。

- HTTP 流空闲上限是 `TUNNEL_HTTP_IDLE_MS = 65 * 60_000`（3_900_000 ms），**不是** 60 秒，也删除 `TUNNEL_STREAM_IDLE_MS`。计时从 Open 开始。任一方向的 Data 都重置它。Head、Window、隧道 Ping 不重置。到期只 `Reset` 这一条 HTTP 流。上限必须长于今天最长的客户端超时，这样先超时的是客户端自己的 `Abort`，而不是隧道。最长的是 `BACKUP_LONG_CALL_TIMEOUT_MS = 60 * 60_000`（`src/lib/api.ts`），用在 `exportBackupWeb` 的 `backup_create_ticket`、`prepareBackupSourceDesktop` / `prepareBackupSourceWeb` 的 `backup_prepare_source`、`stageRestoreDesktop` / `stageRestoreWeb` 的 `backup_restore_stage`、`scanExternalConflicts` 的 `backup_scan_external_conflicts`。这些调用在响应 body 出现之前可以一直没有 Data。其它已经写明的长调用都短于 65 分钟：`officecliInstall` 的 `officecli_install` 是 `630_000`（注释写明比后端 600 s 多 30 s），`acpInstallUvTool` / `acpInstallPiBinary` / `installHyperframesSkills` / `createHyperframesProject` 是 `600_000`，`translateDocument` 是 `540_000`（注释写明要盖住后端约 480 s），`acpAntigravityLoginFinish` 是 `240_000`，`acpAntigravityLoginStart` 与 `acpAntigravitySignOut` 是 `180_000`，`describeAgentOptions` 的 `acp_describe_agent_options` 是 `70_000`。`WEB_CALL_TIMEOUT_MS = 60_000` 只是 `web-transport.ts` 的默认值。持续有字节的 `backup_upload` 会不断重置计时，不受这 65 分钟限制；它仍受 64 MiB 请求体上限（或 `ready` 里申报的 `maxRequestBody`）约束。
- `kind: "ws"` **没有** 应用层空闲计时。不要因为缺少 Data 而 Reset。存活只看两件事：隧道级 Ping 间隔 `25_000` ms，`120_000` ms 无 Pong 则拆掉 **整条** 隧道（与现有 socket.io ping 同量级，但是另一条连接；同进程同步工作仍可能阻塞它；这条规则盖住所有池）；以及 loopback 上的 WS 关闭时，对该流发 Reset 或 End。浏览器和 axum 消化掉的协议层 ping 不是 Data。
- `Control(type=9, streamId=0)` UTF-8 JSON：`{"op":"ready","maxRequestBody":67108864}` 与 `{"op":"codeg-down","reason":"exited"}`。auth 成功后初始 ready=false；只有当前隧道代次发来的 ready 才能置真。down 立即置假并 Reset 该代次活跃流。新隧道替换旧隧道时递增代次，旧回调不得清掉新状态；所有变化通知 `machines:changed`，即使 IDE `/agent` 已断开。

### 桌面 web 服务才是后端

原生页要的 `/api` 与 `/ws/events` 已经由桌面进程提供。Lifeline 不携带、不安装、不监督 `server_bin`，也不读取 `codegServerBin`。本设计不运行共用数据目录的第二个完整应用进程：桌面持有 WAL 连接池，`db/mod.rs` 写明在活句柄下更换 `codeg.db` 会损坏它；ACP 会话也在这个进程里。`server_bin` 的默认数据目录是 `%APPDATA%\codeg`，和桌面的 `%APPDATA%\app.mycodebuddy` 不是同一个目录。共享数据库不会共享内存 ConnectionManager，且双进程的整库恢复可能破坏活跃句柄；另一个目录同样看不到桌面会话。所以不做第二种补偿。

隧道的拨号目标是 **已经在听** 的 `127.0.0.1` 加上交接文件里的端口。桌面 web 服务没在跑、交接文件不存在、权限不对、health 不是 200、或令牌等于哨兵：隧道可以完成 `/machine-io` 的 auth，但 **不发 `ready`**。`codegReady` 保持 false。文档导航是边缘自己的 503 离线 HTML，fetch 是 JSON `machine_offline`。不要为了补位去启动 `codeg-server`，不要改绑 `0.0.0.0`，也不要自动替用户打开桌面 web 服务。

#### 本模式只绑 loopback

「本模式」是 `app_metadata.web_service_lifeline_bind=loopback`。未设置时维持现有 `0.0.0.0` 默认且不发布交接文件。桌面设置页增加明确开关，Rust/TS `WebServiceConfig` 增加可选字段；更新请求缺字段必须保留原值，不能让旧客户端的 `{port,token,autoStart}` 自动保存静默关闭本模式。显式关闭才清掉值。运行时配置同步靠 `PORTABLE_PREFERENCE_KEYS` 允许列表排除此键；把它加入 `#[cfg(test)] FORBIDDEN_PREFERENCE_KEYS` 只为测试锁定意图，后者不是运行时过滤器。

要改的函数是这两处，默认都是 `host.unwrap_or_else(|| "0.0.0.0".to_string())`：

- `do_start_web_server_tauri`（`web/mod.rs`）。这是桌面真正在用的路径：`lib.rs` 在 `auto_start` 时以 `host: None` 调用它，Tauri command `start_web_server` 也调用它。
- `do_start_web_server_with_state`（同文件）。今天没有调用方，但默认主机与上面相同。两条路径必须一起改，避免以后接上它时又听 `0.0.0.0`。

本模式里，`host` 为 `None` 时绑定 `127.0.0.1`，不再落到 `0.0.0.0`。显式传入其它主机（包括 `0.0.0.0`、`::`、`::1`、`localhost`）返回 `InvalidInput`，不 bind。只接受 `127.0.0.1`，这样 agent 不必猜地址族。非本模式的默认绑定不变。`WebServerState::new` 和 `do_stop_web_server` 把内存里的 host 占位恢复成 `0.0.0.0`，那不是监听；停服务时同时删掉交接文件。HTTP handler 的 start 在已运行时返回含 token 的状态、停止后才拒绝启动；因此托管隧道必须拒绝整个命令，不能依赖现有分支。

已经以 `0.0.0.0` 在听时，保存模式/端口/令牌必须在桌面端完成 stop → 保存 → start，停止失败不得发布交接文件；启动失败保留安全模式并显示错误。仅写数据库不会改变正在运行的 router 捕获的令牌。UI 同步 TS 类型与 10 种语言文案，明确 autoStart 是独立选项：想在桌面重启后自动恢复须同时启用它，不隐式替用户打开服务。HTTP 隧道禁止配置变更及停服，agent 不重绑。

#### 令牌交接：所有者可读文件，不读 sqlite

`web_service_token` 存在 `app_metadata`，文件是 `{effective_data_dir}/codeg.db` 或 debug 的 `codeg-dev.db`。SQLite WAL 本身支持跨进程读；`db/mod.rs` 警告的是活跃句柄下替换数据库文件，并不能推出“另一个只读连接必损坏”。本设计仍不让 agent 读库：避免依赖数据库结构、目录探测与 restore 生命周期。桌面手里已有绑定使用的令牌，采用所有者文件交接即可。`tokens.json` 是另一份凭据且属于备份段，不能复用。loopback TCP 没有 OS 用户身份隔离，故不用它交接主令牌。

文件路径：`{effective_data_dir}/.codeg-lifeline-web-handoff`。

- `effective_data_dir` 与桌面相同：环境变量 `CODEG_DATA_DIR` 非空则用它的绝对路径，否则是 identifier `app.mycodebuddy` 的 Tauri `app_data_dir`。Windows 默认 `%APPDATA%\app.mycodebuddy\.codeg-lifeline-web-handoff`。不是 `%APPDATA%\codeg`，不是 `codeg.db`，不是 `tokens.json`，也不在 `~/.lifeline/logs`。
- 只在本模式、且监听已经是 `127.0.0.1` 之后写。端口用 bind 之后的 `actual_port`，不用请求里的端口（传 `0` 时两者不同），也不读 `web_service_port` 那一行来给 agent。
- 正文是 UTF-8，恰好三行，只用 LF：`v1`、十进制端口、令牌。令牌是 `resolve_web_service_token` 的原字符串。不轮换，不调用 `resolve_persisted_server_token`，不设置 `CODEG_TOKEN`。
- 桌面为此文件持有同目录排他租约直到服务停止，避免 debug/发布版或多个 debug 实例覆盖/删除彼此文件；无法取锁只使隧道交接失败，不杀桌面进程。在锁内清理旧交接，再用同目录随机名临时文件、独占创建、写完原子替换。Unix 创建时 0600、O_NOFOLLOW，并验证属主 UID；Windows 用原生安全 API 在创建时设置仅当前 SID 的受保护 DACL，不先写秘密再收紧权限。检查目标/父目录，拒绝符号链接与 reparse point。PR 5 补现有 windows-sys 的 Security/Authorization feature，不另引 ACL 库。
- 写失败不退出桌面进程；锁内确保旧交接不再可被误用，tracing 只含无秘密的错误分类。`WebServerState.inner` 保存本次租约和实际交接路径，stop 无需猜目录或新增 AppHandle 参数。非托管启动也应在能取得租约时清理崩溃残留；不能把旧文件配上非 loopback 的同令牌监听误报 ready。
- `do_stop_web_server` 在释放本次监听与租约前删除自己发布的文件并清空内存路径；未写过时 no-op，不删除另一个实例的交接。崩溃残留仍需重新验证，agent 不据此启动服务器。
- 不把这个文件加进 `MANAGED_SECTIONS`。备份本来就会 `VACUUM INTO` 整库，`app_metadata` 里的令牌行是既有事实；不要再把交接文件收进归档。`is_excluded_section_entry` 会丢掉以 `.codeg` 开头的名字，但本文件本来就不在任何段的遍历根上。
- 桌面不得把令牌写入日志；本机 Tauri 设置页保留显示能力。HTTP 设置页并非天然只对本机可见，必须执行前述托管路由限制，不能靠隐藏导航项保护令牌。agent 不调用配置/状态 API 拿令牌。

agent 的同一探测循环负责初次连接、掉线和恢复：

1. `CODEG_HANDOFF_POLL_MS = 5_000`，任意时刻只跑一次读取/探测。每轮重新打开文件，not-ready 时也继续；不依赖 fs.watch 的 rename 通知，不复用陈旧端口/令牌。配置的 CODEG_DATA_DIR 必须是双方相同的绝对路径；相对路径在桌面与计划任务的不同 CWD 下会分叉，隧道模式拒绝它。
2. Unix 用 no-follow 打开后的同一文件描述符做 fstat/读取，要求普通文件、当前 UID、组/其它权限全零。Windows 使用系统 PowerShell 的固定脚本和 .NET/Win32 句柄读取文件身份、属主 SID 与有效 DACL：参数用 LiteralPath/结构化传递，拒绝 reparse point、NULL DACL、继承/其它 SID 的允许 ACE。身份/ACL 校验与读取必须绑定同一文件句柄，不能先 Get-Acl 路径再普通 readFile 留替换竞态。由隐藏的子进程返回受限 JSON；失败闭合，不解析本地化的 icacls 文本，不把 Windows mode 当 Unix 权限。日志不记录返回正文，循环无重叠。
3. 读取至多 4 KiB，格式三行 LF（可允许一个末尾 LF），端口 1–65535，令牌非空且无 CR/LF/NUL，不能等于哨兵。文件变化或任何检查失败立即发 codeg-down、取消旧流并清除旧令牌内存，再重探。
4. `POST http://127.0.0.1:<端口>/api/health`，`Content-Type: application/json`、body `{}`、Bearer 为第三行，超时 5 s，禁止重定向/环境代理。只有 200 才发 ready；方法与现有 page.tsx / probeHealth 一致。GET 不受支持，不得用于 ready 判断。
5. 已 ready 仍每 5 s 复查文件和 health。loopback 拒绝连接、health 失败或注入后遇令牌 401，立即 down 并重读；后续成功才 ready。不重放失败的非幂等请求。连接失败且浏览器 Head 尚未提交时，按离线表回 503（文档 HTML、fetch JSON）；已提交后只能中断流。令牌错误的当前请求仍按表回 502，下次请求在恢复前是 503。
6. agent 读取的交接令牌只用于 loopback 认证，不记录、不放在隧道控制字段或 Open；不对用户授权的数据响应作令牌扫描或过滤。文件内容读取和端口/令牌切换作为一个代次，旧异步 health 回调不得把新失败状态改回 ready。

Windows：不新建服务、不改 `LogonType`。`daemon.ps1` 拉起的同一个 `lifeline start` 只维持 `/agent` 与 `/machine-io`，不拉起 `codeg-server`。用户注销或未登录 = 桌面应用和 Interactive 任务都不在 = 选择器上 `codegTunnelConnected: false`。锁屏但会话仍在时，Interactive 任务通常还在；桌面应用若被用户关掉，web 服务就没了，隧道 not-ready，这是正确的离线，不是去提权到 SYSTEM 把 router 留住。不把锁屏写成 SLA。CDP / IDE 逻辑断线不得主动拆掉 CodeG 隧道，反向同理；同一进程的致命退出或同步主线程阻塞仍会影响两者，不承诺进程级隔离。计划任务的 CWD 经常是 System32；交接路径按上面的数据目录解析，不靠 CWD。

### `/console` 还留下什么

`/console` 继续挂载，服务端 IDE 中继（`command:*`、`session:*`）不删。落地页 `/` 仍是安装与 `lifeline setup` 的说明（`LandingPage`、`enroll.ts` 的 `install.ps1` / `setup` 命令）。

CodeG 没有机器列表。新增薄选择器 `GET /machines`（Lifeline 的 `dist/client`，不是 CodeG 页面）：用现有浏览器 socket.io 的 `machines:list`，读新字段 `hostLabel`、`codegTunnelConnected`、`codegReady`。每行一个链接，指向同源 `/go/<hostLabel>`。离线行仍可点，机器源站回边缘自己的离线页（不进隧道）。

登录后的默认产品入口改为 `/machines`，不再把用户送进 `/console`。`App.tsx` 里 `/console` 路由留着，导航上拿掉。选择器不嵌 Cursor/CodeBuddy 会话 UI。

### 登记

不新造协议。`lifeline setup` 现有步骤原样：

1. 本机回调 `http://127.0.0.1:<port>/callback`（Windows 同样）。
2. 浏览器在已登录的 apex 上打开 `/cli-setup`，拿到 120 s code。
3. `POST /public/cli-setup/exchange` 用 code + `agentId` 换明文机器令牌，只此一次。
4. 令牌写入 `~/.lifeline/config.json` 的现有字段（今天已经这么做）。哈希在 `machine_tokens`。
5. `finishSetup` 调 `daemon install`。Windows 上是计划任务 `Lifeline Agent`。

exchange 继续只签发机器令牌，不提前创建 `machines` 行。首次 `/agent` register 先执行既有 `adoptStaleSibling`，无可接管行才由 `upsertMachine` 的 INSERT 分支生成 label；UPDATE/令牌轮换不改 label。`/machine-io` 可先完成 auth，但在首次登记持久化之前 not-ready，不能提前 INSERT 与 rename 的目标主键冲突。一次登记之后，IDE/CDP 掉线不影响已有机器的隧道可达性。旧行由启动迁移补 label，无需重新 setup。adoption 的“离线”必须同时检查 IDE socket 与机器隧道：活跃隧道不得被仅因 IDE 断线的同名机器接管。forget 同样拒绝任一通道仍在线的机器。登记/接管、持久化和内存索引需以成功事务为界，失败不发布半更新状态。

### 失败时用户看到什么

| 情况 | 用户看到 | 桌面 web 服务 |
| --- | --- | --- |
| 守护进程没连上 `/machine-io`（未登录 Windows、进程崩、令牌被吊销） | 选择器行离线。已打开标签里的 `/api` 探针得到 JSON 503 `machine_offline`，重连框（4 s 后），可点 Reconnect now，哨兵还在。同一时刻的文档导航得到 503 离线 HTML，不是这段 JSON | 不监听公网。没有「改绑 0.0.0.0 应急」，也不另起进程 |
| 隧道在，但交接文件缺失、权限不对，或 `127.0.0.1` 上 health 不是 200 | 文档导航：503 离线 HTML，不进隧道。fetch / `/api`：JSON 503 `machine_offline`，重连框，不删哨兵 | 可能根本没启动。agent 不替它启动 |
| 桌面应用退出或 web 服务停了 | 同上。用户再次打开桌面应用并让 web 服务在本模式里听 `127.0.0.1` 之后，health 200 才重新 `ready`。没有监督循环去拉起 `codeg-server`。WS 会从 `__ready__` 重新走，attach 协议自己重放 | 不在听。不要改绑 `0.0.0.0` |
| Lifeline 会话到期 | 文档导航：302 到绝对 `apexOrigin + "/go/<label>"`。不调用 `onHttpDenied`。API：401 `lifeline_session_expired`，按钮去同一个绝对 URL | 请求没进隧道 |
| 静态池满 | JSON 503 `static_saturated`。不要画成「机器离线」 | 请求没进隧道 |
| 注入的令牌和进程不一致 | 502，重连框，日志 `codeg_token_mismatch`。不把令牌打印出来 | 401 停在 loopback |

离线 HTML 由边缘生成，自包含（`forbidden-page.ts` 的同一约束：不能依赖被门挡住的静态资源）。上面写机器名（`displayName ?? hostname`）和「这台电脑上的 Lifeline 没连上」，不写 stack、不写 `web_service_token`、不写 loopback 端口。桌面应用没开时也用这页，不要写成「正在启动 codeg-server」。

### 架构总图

```mermaid
flowchart LR
  subgraph browser [Browser]
    UI[CodeG static UI]
  end
  subgraph lifeline [Lifeline public]
    Apex[apex: login /machines /cli-setup /agent-io]
    Edge[m-label host: cookie gate + meta]
    Hub[AgentHub socket.io /agent]
  end
  subgraph machine [Machine win32 / darwin / linux]
    Daemon[lifeline daemon]
    Codeg[Desktop web router 127.0.0.1]
  end
  UI -->|HTTPS lifeline_session| Apex
  UI -->|HTTPS lifeline_machine + sentinel Bearer| Edge
  Apex -->|bootstrap 302| Edge
  Edge -->|/machine-io binary frames| Daemon
  Daemon -->|inject web_service_token| Codeg
  Daemon -->|/agent-io agentToken unchanged| Hub
  Hub --> Apex
```

## API / Interface Changes

### 不改的

- `require_token` / `require_token_with_completion_authorizations` 的比较。
- `POST /api/<command>`、`GET /ws/events`、静态路径这些 **桌面 web router 路由**。
- `POST /public/cli-setup/exchange` 的 body。响应仍只有 `{ agentToken }`。label 不回给 CLI（CLI 不需要它；选择器从服务器列表读）。
- socket.io 事件名与 `COMMAND_EVENTS`。

### Lifeline 浏览器可见

`machines:list` 里每项在现有 `MachineInfo` 上增加，不改 `connected` 的含义：

```ts
interface MachineInfo {
  // ...现有字段，含 connected: IDE socket 是否连接
  hostLabel: string;             // ^[0-9a-f]{16}$
  codegTunnelConnected: boolean; // /machine-io 已 auth
  codegReady: boolean;           // 已收到 op=ready
}
```

新的页面路径（Lifeline 静态站，不是 CodeG）：

| 方法与路径 | 认证 | 行为 |
| --- | --- | --- |
| `GET /machines` | 浏览器会话 | 选择器。未登录走现有 `onHttpDenied`（password：302 `/login?next=`） |
| `GET /go/:hostLabel` | 浏览器会话 | `canSee` 后 302 到机器源站 bootstrap。label 非法或不可见：404 |
| `GET /__lifeline/bootstrap` | 一次性 code，仅机器主机 | 设 `lifeline_machine`，302 `/` |
| `GET /__lifeline/offline` | 机器 cookie | 有效 cookie：同一份 503 自包含离线 HTML，不进隧道，不管隧道是否已经 ready。没有 cookie：302 到绝对 `apexOrigin/go/<label>`，只要 hostname 上的 label 匹配 `HOST_LABEL_RE`，即使库里没有这一行。不用 200 离线页确认 label 存在。没有「或仅 host 匹配」。label 字符集非法：404 |

普通机器路径在 cookie 门通过后进入隧道；Office 前缀按派生能力门处理。`/agent-io`、`/socket.io`、`/machine-io`、`/public/*`、`/healthz`、`/cli-setup` 仅属 apex，在机器主机上一律 404，HTTP 与 Upgrade 都覆盖。

### Host 分派的实际位置

在 Node HTTP server 的入口按规范化 authority 分派，必须先于 Fastify 路由、gateHttp、body parser 以及 engine.io 路径接管。机器 HTTP 交原始 request/response 给流式网关（不走 Fastify 默认 JSON 收齐/1 MiB bodyLimit），apex 才进入原应用。Upgrade 按 Host+精确 path 交给对应处理器；未知 Host 拒绝，未知 Upgrade 立即关闭。禁止把 X-Forwarded-Host 当未经配置的路由依据。

现有两个 SocketServer 直接 attach 同一 http.Server。实现必须统一封装 request/upgrade 所有权（例如受控 serverFactory/分派器），不能只加一个后注册的 Fastify hook。两个 engine.io 实例均加 apex Host 的 allowRequest 防线并关闭 destroyUpgrade，由统一分派器负责未处理的升级；具体 attach 顺序和锁定版本行为用真 HTTP/WS 测试确认，不宣称 Fastify hook 足以保证。机器端 101 延迟超过 1 s 但低于握手预算仍必须成功；机器主机的 polling 与 upgrade 到 /socket.io、/agent-io 均不得进入控制面。测试使用 http.request / ws 连 loopback 并显式 Host，app.inject 不覆盖这个入口。

### 隧道控制消息

上文 Open / Head / `ready` JSON 即合同。实现放在 `packages/protocol/src/tunnel.ts` 并在 `limits.ts` 旁导出常量，服务器与 agent 都从这里引，禁止两边各写一份魔数。

建议常量：

```ts
export const TUNNEL_PATH = "/machine-io";
export const TUNNEL_PROTOCOL = "lifeline-machine-v1";
export const TUNNEL_CHUNK_BYTES = 256 * 1024;
export const TUNNEL_STREAM_WINDOW_BYTES = 1024 * 1024;
export const TUNNEL_MAX_STATIC_STREAMS = 160;
export const TUNNEL_MAX_APP_STREAMS = 32;
export const TUNNEL_MAX_WS_STREAMS = 8;
export const TUNNEL_AGGREGATE_BUFFER_BYTES = 8 * 1024 * 1024;
export const TUNNEL_MAX_REQUEST_BODY_BYTES = 64 * 1024 * 1024;
export const TUNNEL_MAX_WS_MESSAGE_BYTES = 8 * 1024 * 1024;
export const TUNNEL_HTML_REWRITE_MAX_BYTES = 4 * 1024 * 1024;
export const TUNNEL_MAX_HTML_REWRITES = 2;
export const CODEG_HANDOFF_POLL_MS = 5_000;
export const TUNNEL_HTTP_IDLE_MS = 65 * 60_000;
export const TUNNEL_PING_INTERVAL_MS = 25_000;
export const TUNNEL_PING_TIMEOUT_MS = 120_000;
export const TUNNEL_AUTH_TIMEOUT_MS = 5_000;
export const BOOTSTRAP_CODE_TTL_MS = 60_000;
export const HOST_LABEL_RE = /^[0-9a-f]{16}$/;
export const CODEG_LIFELINE_SENTINEL = "codeg-lifeline-session";
export const TUNNEL_FLAG_WS_TEXT = 0x01;
export const TUNNEL_FLAG_WS_BINARY = 0x02;
export const TUNNEL_FLAG_WS_CLOSE = 0x04;
export const TUNNEL_FLAG_FIN = 0x08;
```

没有 `TUNNEL_MAX_STREAMS`，也没有 `TUNNEL_STREAM_IDLE_MS`。WS 流不设空闲常量。

CodeG 客户端从 `web-auth.ts` 导出同名哨兵。两边字符串必须有测试钉死，不能靠注释同步。

### 桌面 web 服务（本模式）

没有 `CODEG_LIFELINE_TUNNEL`，没有 `CODEG_TOKEN_EXPORT_PATH`，也没有由守护进程设置的 `CODEG_HOST` / `CODEG_TOKEN` / `CODEG_PORT`。那一套是给被拉起的 `server_bin` 的，本设计不用。

| 项 | 本模式 |
| --- | --- |
| `web_service_lifeline_bind` | `app_metadata` 键。值 `loopback` 才生效。未设置则默认仍是 `0.0.0.0`，且不写交接文件 |
| 绑定函数 | `do_start_web_server_tauri` 与 `do_start_web_server_with_state`。本模式只接受 `127.0.0.1` |
| 令牌 | `resolve_web_service_token` 的返回值，即已有的 `web_service_token`。不轮换 |
| 交接文件 | `{effective_data_dir}/.codeg-lifeline-web-handoff`。Unix `0600`；Windows 仅当前用户 DACL |
| 拨号 | `127.0.0.1` 加文件里的 `actual_port`。服务没在跑则 not-ready，不另起进程 |

### 客户端函数

`src/lib/transport/web-auth.ts` 增加 `hostedAccess()` 与上面的 `getCodegToken` / `redirectToCodegLogin` 行为。`page.tsx`、`login/page.tsx` 只通过这两个函数分支。`web-connection-guard.tsx` 继续只调用 `redirectToCodegLogin()`，不必认识 apex。

## Data Model Changes

`packages/server/src/db/schema.ts` 的 `machines` 增加：

```ts
hostLabel: text("host_label").notNull().unique(), // 存量行迁移时补齐
```

迁移沿用 `packages/server/src/db/open.ts` 的手写 `applySchema` / migrateMachines* 模式，同时更新 `db/schema.ts` 类型和新建表 DDL；不是另引 drizzle 迁移执行器。存量表先加可空列，事务内补齐唯一 16 hex 并建立 UNIQUE 索引，再供服务读取。新行插入也必须提供 label，UPDATE 不改它。迁移可重复启动且先于机器列表加载。

`machine_tokens` 不改。令牌仍然是哈希，明文不落库。`host_label` 与令牌轮换无关。`renameMachine`（`identity-store.ts`）今天只在一个事务里把 `machines.agentId` 和 `machine_tokens.agentId` 改成新 id。加上 `host_label` 之后这个函数仍然不许写该列。`adoptStaleSibling` 继续只调用它，所以重新签名不会使机器 cookie 失效。

`forget(agentId)` 已经删 `machines` 与 `machine_tokens`。label 随行删除。之后同一 `agentId` 再登记会得到新 label（新行）。这是可以接受的：机器被主人删掉再装，书签失效，选择器出现新主机名。

bootstrap code 只放有容量上限、到期清理的内存表（沿用 CliSetupCodes 的思路），进程重启作废。`gateway_secret` 使用新增单行表 `gateway_auth(id=1, secret)`，32 字节由服务端随机产生并事务性 insert-if-absent；不复用 password 专用 `auth_local`。PR 7 同时更新 schema.ts 与 open.ts DDL，trusted-header 部署也能独立启动。

CodeG 的 `app_metadata` 不改表结构。新增的键 `web_service_lifeline_bind` 仍是这一张表现有的 `key` / `value` 行。交接文件是旁路，不是第二份权威：权威仍是桌面进程交给 `build_router` 的那个 `web_service_token`。agent 不打开 `codeg.db`。

## Alternatives Considered

### 1. 伪装 Lifeline `ide=cursor`，把 ACP 译成 `session:full` / `command:*`

产品已拒绝，技术上也不成立。

- 上行是固定的 `COMMAND_EVENTS` 与会话投影，不是 HTTP 方法加任意路径。CodeG 的 API 是 `POST /api/<command>` 加一条 attach WS，两边 schema 不同。
- 载体仍是 socket.io，单消息 20 MiB，并且和 sqlite 同步共享 120 s ping。CodeG 快照应用层就是 4 MiB，请求体到 64 MiB，备份上传更大。
- 用户看到的会是 `/console` 的会话壳，不是 CodeG 页面。`next` 静态导出、`x-codeg-completion-context`、下载票据、`office-watch-proxy` 都没有落点。
- 翻译层每加一个 CodeG 命令就要加一个伪造事件，两套产品会锁死在一起。

代价若硬做：少一条新连接。这个代价买不到 CodeG UI，也过不了缓冲上限。

### 2. 把桌面 web 服务公布到网上，Lifeline 只做反向代理，没有出站隧道

产品已拒绝。另起一个 `server_bin` 再公布，同样拒绝：那是第二个进程，不能共享桌面的 ACP 内存会话，整库替换还涉及活跃数据库句柄。

- 桌面 web 服务今天的默认就是 `0.0.0.0`（`do_start_web_server_tauri` / `do_start_web_server_with_state`）。`server_bin` 的默认也是 `CODEG_HOST=0.0.0.0`，但本设计不启动它。再公开一步，家里的机器就依赖端口转发和防火墙上的入站，和 Lifeline「机器主动出站」相反（落地页文案也是 daemon 出站、没有公网入口）。
- 反向代理能校验 `lifeline_session`，但 `require_token` 不认这张 cookie。代理要么把 `web_service_token` 放进自己的配置（公网进程持有每台机器的操作者令牌），要么改 router 去信任一个可被直连伪造的头。后者就是 `trusted-header` 已经在防的事。
- 机器在 NAT 后没有稳定入站地址。子域也没有可指的后端。

代价若硬做：少写复用流。换来的是入站面和令牌驻留在公网进程里。

### 3. Lifeline 登录成功后把真 `web_service_token` 交给浏览器

不采用自动分发方案。Lifeline 已可靠确认用户身份，浏览器用机器会话即可进入隧道；哨兵满足 CodeG 的非空令牌要求，无需把长期本机凭据写入 localStorage、HTML 或 machines:list。

机器侧注入保持现有 require_token 与 completion 能力校验。用户主动读取自己的配置或导出完整备份仍属授权操作，不以“浏览器绝对拿不到令牌”为目标，也不因此限制原生编程功能。

### 4. 只加 cookie，客户端原样不动

不行，见 Background。`require_token` 不读 cookie；空 `codeg_token` 会去 `/login` 并拒绝连 WS。若 cookie 里放哨兵、却指望边缘在看不到 localStorage 的情况下改写：客户端根本不会把哨兵放进 `Authorization`，因为空键在 `connectWs` 就返回了。所以哨兵必须进 `localStorage`，改写必须发生在机器上，边缘必须删掉浏览器带来的 Authorization。三件事少一件都会要么 401 循环，要么把真令牌送进浏览器。

### 5. 在现有 socket.io 上加二进制事件 `tunnel:frame`

少一个监听路径，复用 `agentToken` handshake。仍然拒绝：

- 单条事件受 `maxHttpBufferSize` 20 MiB 限制。64 MiB 的 regex 体和流式下载对不上，除非自己再分片，而分片之后的信用、队头阻塞要重做一套，socket.io 的 ack 帮不上。
- `/agent` 的 ping 超时被放宽到 120 s，就是因为主线程会被 `session:full` 堵住。再把 CodeG 的 WS 快照放进同一条 socket，IDE 投影和 CodeG UI 会互相判死。
- 浏览器侧的 socket.io（`packages/web/src/net/socket.ts`）`reconnectionDelayMax` 是 10 s、`maxHttpBufferSize` 20 MiB，和 agent 侧不是同一条连接。不存在「浏览器已经连着 socket.io，顺便承载 CodeG WS」的通道；浏览器 socket.io 是 Lifeline 页面用的，而且产品决定 CodeG 页面不走 `/console`。

新的 `/machine-io` 复用同一枚机器令牌和 `resolveHandshake`，分离 socket.io 的事件与发送队列；同一 Node 进程仍共享事件循环。v1 不新增 worker 或子进程，不承诺同步 sqlite 停顿期间的延迟隔离。必须压测 IDE 同步并记录 CodeG 探针/WS 延迟；若实测不能满足 5 s ready 和 8 s probe 预算，发布受阻，届时再将隧道移到 worker，不能只调高 ping 超时掩盖。

### 6. 路径前缀而不是子域

已在 Proposed Design 里按 Next 配置和 cookie 作用域否掉。补一条运维代价：子域要通配符证书。这是真实成本。它仍然小于「改 `basePath` + 每个 `fetch` + 静态资源，还是无法按机器变前缀」。若某个部署暂时没有通配符 DNS，机器网关应拒绝启动并打明确日志，而不是静默退回路径前缀。

## Security & Privacy Considerations

| 风险 | 严重性 | 缓解 |
| --- | --- | --- |
| 正常登录/连接额外分发真令牌，或服务设置误停当前连接 | 工程约束 | 浏览器使用哨兵、机器侧注入，五个服务管理接口留在桌面；不将真令牌写入登录结果、控制字段或日志。用户授权的文件/终端/代理输出及完整备份保持原样，不作为令牌泄漏测试失败 |
| 显式配置把令牌设成哨兵 | 高 | 生成器不可能生成哨兵，但 override/存量配置可能；ready 前相等检查，失败闭合，不退出桌面进程 |
| 桌面 web 服务按默认 `0.0.0.0` 暴露，用来「补偿」隧道故障，或另起 `server_bin` | 高 | 本模式里 `do_start_web_server_tauri` 与 `do_start_web_server_with_state` 只接受 `127.0.0.1`。服务没在跑则 503 离线页。没有第二条监听，也没有第二个进程 |
| Open 的 `path` 变成 SSRF（`http://`、`//`、反斜杠、`%2e%2e`） | 高 | 边缘规范化失败即 400。机器只拨配置里的 loopback 地址和端口，不用请求 `Host` |
| 同站页面从机器 A 打开机器 B 的 WebSocket 或带 cookie 的 fetch | 高 | host-only 只防止 A 的 cookie 被送到 B。B 自己的 cookie 在同站请求里仍会附上，`new WebSocket` 没有 CORS。机器主机要求 `Origin` 精确等于 `https://m-<该 label>.<apexHostname>[:port]`（loopback 开发才允许 `http`），否则 403 且不开隧道。`/ws/events` 缺 Origin 也 403。不调用 `checkOriginAgainstExpected`。password 模式下所有人的 `userId` 都是 `owner`，这条检查是横向移动的实际门 |
| 把 503 误做成 401，离线被客户端当成登出并清哨兵 | 中 | 状态表写死。`probeHealth` 只有 401 才 `markUnauthorized`。集成测试覆盖 503 时 `localStorage` 仍是哨兵 |
| 注入失败仍把 `codeg` 的令牌 401 交给页面，用户被送去 apex 登录，形成修不好的循环 | 中 | 该正文映射为 502 `codeg_token_mismatch` |
| `/machine-io` 与 `/agent-io` 一样在 HTTP 层公开，靠第一帧令牌 | 中 | 与今天 agent hub 相同的哈希查找。5 s 不 auth 则断开。连续失败按源 IP 限速（password 登录已有 5 次/60 s 的量级，复用同一思路，不共享那个计数器） |
| bootstrap code 经 Referer 或访问日志泄漏后、60 s 内被兑换 | 中 | 一次性、绑定 userId + label、`Referrer-Policy: no-referrer`、`Cache-Control: no-store`。code 不进隧道、不进 meta |
| 窃得的 `lifeline_machine` 在到期前一直有效；没有 logout | 中 | 无全局会话吊销表，到期上限 14 天；到期主动关闭既有流，owner 变化或删除同步失效。公网使用 `__Host-`、HttpOnly、Secure、SameSite=Strict；localhost HTTP 开发例外见 Cookie 一节 |
| `x-codeg-completion-context` 被日志记录 | 低 | 它不是 `web_service_token`，但是能力凭证。边缘访问日志只记方法、路径、状态、`streamId`，不记该头和 cookie |
| HTML 改写信用错误或缓冲死锁 | 高 | Window 按原始压缩 Data 字节归还；连接总信用 8 MiB，HTML 另预留最多两份 4 MiB；找不到 head 或解压失败回 502，不能回无托管标记的登录页 |
| 可信 Lifeline 身份与目标机器绑定错误 | 高 | `lifeline.woa.com` 登录鉴权是可信前提；每次请求仍必须使用验证后的 userId 校验 owner，子域、bootstrap 和 WS 不能绕过目标机器的授权范围。无需另做执行隔离或第二套用户鉴权 |
| password 模式所有登录者都是 `owner`，因此共享全部机器 | 中（既有） | 不在本设计改成多用户。需要按人分机器时用 `trusted-header`，`userId` 来自网关，`canSee` 已经按字符串相等隔离 |
| Windows 计划任务以 Interactive Limited 运行，注销后隧道消失；有人会想改成 SYSTEM 以便「始终在线」 | 中 | 保持 Interactive。SYSTEM 读不到当前用户的 `%APPDATA%\app.mycodebuddy` 交接文件，也会扩大令牌文件的读者。桌面应用本来就在用户会话里。离线是正确的失败表现 |
| CORS/Origin 规则破坏 Office 沙箱或扩大跨机器权限 | 高 | 普通 API/WS 严格同源；Office 前缀仅凭绑定 owner、label、期限及代次的派生 cap 放行 null Origin，无凭据 CORS，预检不授予权限 |

`CompletionAuthorizationRegistry` 仍是进程内存、重启即失效。隧道不持久化这些 cap。

## Observability

不要新造指标系统。Lifeline 用现有 `timingLog`（`packages/server/src/timing-log.ts`）。桌面 web router 已经有外层 `http` span（方法、路径、`request_id`），隧道注入之后 loopback 上的请求照旧进这个 span。

Lifeline 要打的事件（字段只有这些，禁止令牌、cookie、completion cap、bootstrap code）：

| 事件 | 字段 |
| --- | --- |
| `tunnel:auth` | `agentId`、`ok`、失败原因枚举（`unknown-token` / `timeout` / `owner-mismatch`） |
| `tunnel:ready` / `tunnel:codeg-down` | `agentId`、`reason` |
| `tunnel:open` | `agentId`、`streamId`、`kind`、`path`（规范化后）、`inject` |
| `tunnel:close` | `agentId`、`streamId`、`status`、`bytesIn`、`bytesOut`、`durMs` |
| `tunnel:reset` | `agentId`、`streamId`、`code` |
| `gateway:deny` | `hostLabel`、`reason`（`cookie` / `owner` / `origin` / `offline` / `busy` / `static_saturated` / `ws_saturated` / `body`） |
| `gateway:html-no-head` | `hostLabel`、解压后正文长度。不含正文 |
| `gateway:token-mismatch` | `agentId`。计数器，不含上游正文以外的分类 |

告警：

- 同一 `agentId` 的 `gateway:token-mismatch` 连续出现：交接文件里的令牌和桌面 web router 正在检查的 `web_service_token` 不一致（旧文件，或桌面侧轮换后还没重写文件）。用户看到的是无尽重连而不是登录页。不要把令牌打进这条告警。
- `tunnel:auth` 失败率突增：和 agent hub 今天的 unknown token 日志同一类问题（没跑 setup，或令牌被轮换）。
- HTTP 流 `durMs` 接近 `TUNNEL_HTTP_IDLE_MS`（65 分钟）且 status 为 Reset：调用在客户端 1 小时上限之后仍无 Data，或窗口耗尽。不要按 60 秒告警；那会把正常的备份和安装调用打成故障。WS 流没有这个空闲 Reset。

机器本地：桌面 web 服务继续用自己的 tracing，本模式的启动日志只有地址，没有令牌。守护进程日志沿用 `LOG_DIR`（`~/.lifeline/logs`）。交接文件的路径可以打，第三行不可以。没有 `codeg-server` 子进程，所以不要把它的 stderr 接进 `LOG_DIR`，也不要到这个目录里找令牌。Windows 上任务计划程序不收集 stdout；现有 supervisor 已经把 **agent 自己** 的输出重定向到日志文件（`win-daemon.ts` 记过绑定失败只进任务计划程序黑洞的同类故障）。这些文件只追加、不轮转。单测看的是 agent 与桌面本模式日志的字节里没有令牌，不是「日志目录不存在」。

健康：

- apex 的 `/healthz` 保持匿名，不代表任何机器在线。
- 选择器上的 `codegReady` 是用户可见的机器健康。不要做匿名的「按 label 查询是否在线」接口。

## Rollout Plan

前提（不做则机器网关不要监听）：

- `PUBLIC_ORIGIN` 是 `https://<apexHostname>`，非默认端口写成 `https://<apexHostname>:<port>`。端口不进 DNS。启动日志打印规范化后的 hostname 和 port；带 userinfo 的值拒绝启动。
- DNS `*.<apexHostname>` 与 apex 的 hostname 指向同一入口。不要写成 `*.<hostname>:<port>`。
- 证书覆盖 apex hostname 和 `*.<apexHostname>`。
- 外层代理转发 `Host`、`Upgrade`、`Connection`。读超时按连接拆开，不要用一个「大于 120 秒」覆盖全部：
  - `/machine-io`：读超时大于 `TUNNEL_PING_TIMEOUT_MS`（120_000 ms）。这条连接每 `TUNNEL_PING_INTERVAL_MS`（25_000 ms）有隧道 Ping，会刷新它。
  - 机器主机上的 HTTP，尤其是 `/api`：读超时大于 `TUNNEL_HTTP_IDLE_MS`（65 分钟）。`backup_create_ticket` 等在响应体出现之前可以沉默将近 60 分钟，这条 HTTP 上没有隧道 Ping。短于 65 分钟的读超时会先于客户端自己的 1 小时上限把连接拆掉。
  - `/ws/events`、`/machine-io` 以及下载响应关闭代理缓冲。浏览器上的 `/ws/events` 不靠把读超时调到 65 分钟来保活：边缘每 25 秒发送 WebSocket 协议 Ping，见 WebSocket attach。

顺序：

1. 合并协议类型与 `host_label`（无行为变化）。存量机器补 label。
2. 发布带哨兵逻辑的 CodeG 静态资源（PR 4），以及桌面本模式的 loopback 绑定和交接文件（PR 5）。没人把 `web_service_lifeline_bind` 设成 `loopback` 时，桌面默认仍是 `0.0.0.0`，不写交接文件。没有 meta 的页面仍是粘贴登录。
3. 发布 Lifeline agent：默认 **不** 拨 `/machine-io`。`install.ps1` / `install.sh` 不增加 `server_bin` 载荷，守护进程不启动它。用配置 `codegTunnel: false` 控制。Windows 仍是原来的计划任务，只是二进制变新。
4. 测试账户设 `codegTunnel:true`，桌面设置打开 loopback 模式，按需启用 autoStart；保存时停止并重启 web 服务。验证端口只绑 127.0.0.1、文件权限正确、POST health 成功、端口/令牌变化可恢复、无 server_bin 子进程；关闭桌面后文档 HTML 503、API JSON 503。验证 gzip HTML 三枚 meta、服务管理接口不回显 token、两种 provider 的 `/go` 恢复、跨机器 WS 拒绝与 Office sandbox 完整交互。用已鉴权且拥有目标机器的账号验证终端、文件、代理及完整备份可用；不因授权数据包含本机令牌而判失败。
5. 选择器与 `/go` 在标志关闭时仍可上线：`codegReady` 为 false 的行点进去是离线页，而不是半截 CodeG。
6. 先升级带 PR 4/5 的桌面，再启用隧道。旧静态页可能仍显示粘贴框；任意非空输入都会被边缘剥离，能否进入实际取决于 cookie 与机器注入，不能把它当作令牌验证。发布禁止这种误导性混搭。服务没开就离线，不安装独立服务器补位。
7. 默认打开 `codegTunnel` 只在桌面应用已经带上 PR 5、并且测试账户稳定之后。不要等 Lifeline 安装包去携带 `codeg-server`。已有的 IDE `/console` 用户不受这个默认值影响。默认值的开关放在以后单独的小 PR，不在本计划里预写死，方便单独回滚。本计划的 PR 9 不翻转默认值。

回滚：关闭并保存 `codegTunnel`，守护进程关闭现有 `/machine-io` 并停止重连；保持桌面运行，不切回公网监听。令牌疑似泄漏时在桌面设置通过既有 API 轮换并 stop/start，不指导用户直接修改活跃 sqlite。启动后重写交接，agent 重读恢复。公网网关被攻破时先断开并恢复可信入口，再轮换凭据。

Windows 验证要在真正的计划任务里做，不只在交互式 PowerShell 里做：工作目录、`APPDATA`、隐藏窗口的 supervisor、`ExecutionTimeLimit` 已设为 0 这些都是现有代码里踩过的。注销后选择器必须变离线。

## Open Questions

- 已决定：不另起 `codeg-server`；桌面进程持有 ACP 内存会话并提供 router，Lifeline 不打包或监督它。SQLite 的并发读取能力不改变这条进程架构决定。服务没开则离线。
- `/console` 的 IDE 投影要保留多久。本设计不删路由、不删 `/agent`。若产品决定 CodeG 隧道是唯一的浏览器面，删除 `/console` 应另案，避免和隧道 PR 绑在一起。这些 PR 不删除 `/console`。

URL 形状、令牌改写、可信 Lifeline 身份下的完整操作者权限均已确定；不另起 `codeg-server`。这些决定不再列为待确认的发布条件。

## References

- Lifeline `packages/server/src/auth/provider.ts`：浏览器 `AuthProvider` 与机器令牌分离。
- Lifeline `packages/server/src/auth/factory.ts`：`none` / `trusted-header` / password 的绑定护栏。`createAuthProvider` 的优先级。
- Lifeline `packages/server/src/auth/password.ts`：`lifeline_session`、14 天、`owner`、`SameSite=Strict`。`password-pages.ts` 的 `nextTarget()`。
- Lifeline `packages/server/src/auth/trusted-header.ts`：`userId` 最长 128。`none.ts`：`userId` 为 `owner`。
- Lifeline `packages/server/src/cli-setup.ts`：code TTL 120 s，`isAllowedRedirectUri`。`packages/cli/src/commands/setup.ts`：回调等待 180 s，`POST /public/cli-setup/exchange`。
- Lifeline `packages/server/src/identity-store.ts`：`hashToken` sha256，`mintMachineToken` 24 字节 hex，`resolveHandshake`。
- Lifeline `packages/server/src/agent-hub.ts`：`/agent` namespace，handshake，`agent-mismatch` / `owner-mismatch`，`canSee`，`MachineInfo`。
- Lifeline `packages/server/src/public-paths.ts`、`relay.ts`、`server-version.ts` 的 `clientDir()`、`socket-limits.ts`。
- Lifeline `packages/agent/src/uplink.ts`：`/agent-io`，重连 1 s–30 s，timeout 60 s。
- Lifeline `packages/protocol/src/limits.ts`：20 MiB、ping 25 s / 120 s。`schema.ts` 的 `exchangeBodySchema`。`wire.ts` 的 `MACHINE_NAME_MAX_LENGTH = 24`。`commands.ts` 的 `COMMAND_EVENTS`。
- Lifeline `packages/web/src/lib/routes.ts`：`/` 与 `/console`。`packages/cli/src/daemon/windows.ts` 与 `win-daemon.ts`：任务 `Lifeline Agent`，`AtLogOn`，`Interactive`，`Limited`。
- CodeG `src/lib/transport/web-transport.ts`：`WEB_CALL_TIMEOUT_MS` 60 s，`READY_TIMEOUT_MS` 5 s，`MAX_WS_FRAME_CHARS` 4 MiB，探针 8 s，退避 1 s–32 s。
- CodeG `src/lib/transport/web-auth.ts`、`ws-auth.ts`、`src/app/page.tsx`、`src/app/login/page.tsx`、`src/components/connection/web-connection-guard.tsx`（`RECONNECT_DIALOG_GRACE_MS` 4 s）。
- CodeG `src/lib/transport/index.ts`：web base URL 为 `window.location.origin`。`src/lib/api.ts` 上传同样使用 origin。
- CodeG `src-tauri/src/web/auth.rs`：`require_token_with_completion_authorizations`。
- CodeG `src-tauri/src/web/router.rs`：`/api` 嵌套、`/ws/events`、`ServeDir`、body 限制、`CorsLayer::allow_origin(Any)`、末尾 `compression_layer()`。`compression.rs`：`MIN_COMPRESS_BYTES = 32`，`text/html` 在压缩允许列表里。
- CodeG `src/lib/api.ts`：`BACKUP_LONG_CALL_TIMEOUT_MS`、`officecli_install`、`translate_document` 等长于 60 s 的 `timeoutMs`。`src/lib/transport/web-event-stream.ts`：`syncSharedHeartbeat` 只在 `shared` 订阅存在时每 30 s 发应用层 ping。
- Lifeline `packages/server/src/auth/origin.ts`：`checkOriginAgainstExpected` 在设置了 `PUBLIC_ORIGIN` 时只接受 apex。机器主机不要调用它。`password.ts` 的 `onHttpDenied` 是相对路径 302。`identity-store.ts` 的 `renameMachine` 只改 `agentId`。
- CodeG `src-tauri/src/web/ws.rs`、`ws_attach.rs`：`__ready__`、`OUTBOUND_CAPACITY` 64、`serialize_server_msg`。`session_state.rs` 的 `MAX_ATTACH_FRAME_BYTES`。
- CodeG `src-tauri/src/web/mod.rs`：`do_start_web_server_tauri`（桌面自动启动与 Tauri command 的实际绑定）、`do_start_web_server_with_state`（同一默认主机，必须一起改）、`resolve_web_service_token`、`WEB_SERVICE_TOKEN_KEY = "web_service_token"`。`lib.rs` 把 `resolve_effective_data_dir(app.path().app_data_dir())` 交给 `db::init_database`。`db/mod.rs` 的 `database_file_name()` 与 WAL。`server_bin/main.rs` 只说明独立二进制：默认 `0.0.0.0:3080`，`resolve_persisted_server_token`，`default_data_dir` = `dirs::data_dir()/codeg`。本设计不启动它。`commands/config_sync/portable_keys.rs` 已把 `web_service_token` 列在 `FORBIDDEN_PREFERENCE_KEYS`。`commands/backup/sections.rs` 的 `MANAGED_SECTIONS` 是允许列表，交接文件不得加入。
- CodeG `next.config.ts`：`output: "export"`，无 `basePath`，生产 `assetPrefix` 为空。
- axum 0.8 `WebSocketUpgrade` 默认 `max_message_size` 64 MiB、`max_frame_size` 16 MiB。`ws_handler` 没有覆盖它们。

## Key Decisions

1. 一台机器一个 `https://m-<16 hex>.<apexHostname>[:port]/` 源站，不做动态 basePath。
2. 信任 `lifeline.woa.com` 网页鉴权，通过机器归属检查后保留完整编程能力。正常连接使用机器会话、哨兵和机器侧令牌注入；五个服务管理接口留在桌面以避免误停及无必要回显。用户授权的数据通道不作主令牌内容过滤，不设绝对保密发布条件。
3. 公网机器入口在 Fastify/engine.io 之前分派，普通请求受 cookie+owner+Origin 门约束；Office 沙箱仅用有期限、限机器/端口/owner 的派生能力。
4. 会话恢复统一走 apex 的 `/go/<label>`，由 provider 决定是否登录。password 期限由验证后的身份接口传递，bootstrap 不延长。
5. 独立 `/machine-io` WebSocket 承载二进制复用帧；每流 1 MiB、每连接 8 MiB 原始 Data 信用。HTML 收齐缓冲另设两份 4 MiB 预留，控制帧不能因 Data 背压停读。
6. 桌面 router 只绑 loopback，所有者文件交接；每 5 s 重读并用 POST health 检测。ready/down 及流回收按连接代次处理。
7. `/console` 保留，`/machines` 为新入口；新增在线字段不能复用 IDE 的 connected。
8. label 在首次机器行 INSERT 时分配，登记与 adoption 保持事务一致；令牌兑换不提前造机器行。
9. 不新增 server_bin、Windows 服务或 worker。保留 Interactive 计划任务；共享主线程的延迟风险需压测，不声称 socket 分离等于执行隔离。
10. 现有 browser_bridge 的额外端口不随隧道发布；Office 同路径能力代理保留。HTTP/SSE 65 分钟空闲与 WS 8 MiB 上限是明确的兼容性约束。
11. 本轮只修订设计，以下 PR 的运行时实现与验收尚未执行。

## PR Plan

每个 PR 只改一个仓库；开关默认关闭。信任模型已确认，发布门只保留各 PR 的功能、身份接入与运行时验收，不再要求额外的令牌权限产品选择。

### PR 1 — Machine tunnel protocol

- Lifeline：`packages/protocol/src/tunnel.ts`、index 导出与单测。
- 定义完整帧布局与状态机：Control(type=9)、两级 Window、FIN/close、长度/credit 溢出、半关闭/取消、代次；常量与正文一致。
- 检查大消息分片、空消息、非法 opcode/长度、未知流、双向同时用尽信用后恢复。控制帧不受 Data 信用约束。

### PR 2 — Durable labels and directory lifecycle

- Lifeline：`db/schema.ts`、`db/open.ts`、identity-store、agent-hub。
- 手写幂等迁移回填唯一 label；只在 upsertMachine INSERT 生成，UPDATE/rename/轮换保留；不在 mintMachineToken 提前造行。
- 增加列表字段，初始 false；对双通道在线判定、adoption/forget、事务失败做测试。后续 PR 3 接入实际隧道状态并发 machines:changed。
- 依赖：无，可与 PR 1 并行。

### PR 3 — Tunnel connections and CLI persistence

- Lifeline：relay 的统一 HTTP/Upgrade 入口、隧道模块、agent 客户端、`packages/server/package.json` 显式声明已有版本的 ws，CLI config/start/setup。
- 依赖 PR 1、PR 2。CLI 类型、load 白名单、save、finishSetup 全链路保留 `codegTunnel`；旧配置默认 false，测试 start/setup/重启读写往返。
- 实现 auth、代次替换、目录在线广播、窗口、池、Ping/Pong、退出及轮换清理。机器首次登记前可 auth 不可 ready；IDE 后续断开不降已有 ready。
- 真 ws 测试：4 MiB 文本（超过流窗口）后跟短消息，在不手工注入 Window 的情况下解出恰好两条完整消息；另测 256 KiB 分片边界、超限只影响单流、慢消费者与相反方向并发、取消释放槽位。
- 普通 HTTP /machine-io 为 426；不扩 isPublicPath。engine.io attach/destroyUpgrade 行为按锁定版本实测，不能仅测 app.inject。

### PR 4 — Hosted client authentication

- CodeG：web-auth、page、login/page、web-service-settings 直接访问防护，以及 browser_bridge 的托管不支持提示。
- meta 必须与当前机器 origin 对应；getCodegToken 同步写哨兵，恢复跳 /go；无 meta 的直接访问保持既有行为。
- 测试首次渲染无登录闪跳、过期恢复、配置页不调用禁止 API、无额外 bridge 端口。复用既有组件与翻译体系。

### PR 5 — Desktop loopback and handoff lifecycle

- CodeG：web/mod.rs、WebServerState、api.ts 中 WebServiceConfig、设置开关及 10 种语言、portable_keys 测试表、现有 windows-sys 安全 feature。
- 缺失新配置字段保持存量值，显式切换经 stop/save/start；autoStart 独立。两条启动路径复用最小绑定/文件助手，设置页不手改数据库。
- 文件独占租约、安全创建/原子替换、实际端口、路径留存在 state、停止清理、失败不得留下可误用旧交接。发布/debug 同目录不能互相覆盖。
- 纯逻辑/文件权限用窄测试，真实路由集成优先 do_start_web_server_with_state + 测试 AppState/端口 0；Tauri 路径再做桌面冒烟。检查非托管默认不变、Unix/Windows 权限、写失败、重启、日志无协议令牌。不改 auth.rs 的相等逻辑，不启动 server_bin。

### PR 6 — Machine injection and recovery

- Lifeline：agent 的安全文件读取、5 s 探测、Open 处理；依赖 PR 3，运行时需要 PR 5。
- Windows 用系统工具及同句柄原生 ACL 校验，隐藏窗口、结构化传参，读取失败闭合。绝对目录、无重定向/代理、POST health、代次切换、旧回调、退出恢复均有测试。
- 在 loopback 之前拒绝五个服务管理命令，验证 API 不能意外回显令牌或误停本服务。文件、终端、代理及完整备份保持既有授权能力，不为令牌保密增设黑名单、执行隔离或响应内容过滤。
- 不安装/监督 codeg-server，不读 sqlite，不改变 Interactive 计划任务。IDE 同步压测暴露共享主线程延迟；不达预算不得发布。

### PR 7 — Machine gateway and capability adapters

- Lifeline：Host 分派、gateway_auth schema/DDL、bootstrap、AuthProvider.expiresAtMs/password、HTML 改写、Office 授权映射、双端路由/头校验与错误映射。
- 依赖 PR 2、3、6；实际可用发布须含 CodeG PR 4、5，并完成下列运行时验收。
- 真 HTTP/WS 入口测试：机器 Host 不进 apex gate/body parser/socket.io；升级延迟 >1 s 成功，机器主机的控制面路径拒绝；64 MiB/未知长度上传流式越限；取消不残留流。
- 验证两种 provider 的 /go、code 原子消费/过期/跨机器、Cookie 到期关闭既有 WS、no-Origin 跨机器子资源、无 Pong 回收；无合法身份不泄漏机器存在性。
- HTML：gzip/br、超过 1 MiB、解压超限、压缩信用守恒、HEAD/条件请求、缺 head；三篇并发时两篇完成、第三篇受控 html_rewrite_busy，不能整体死锁。
- Office：真实 sandbox 中的 null Origin、无 cookie 能力访问、预检、SSE/编辑、过期/错误 cap/错机器拒绝；网关重启需重开预览。不能用全局 CORS 或放开 cookie 门代替。

### PR 8 — Machine picker

- Lifeline：routes.ts、选择器、App、LandingPage、登录默认 next。
- 依赖 PR 2 字段；发布在 PR 7 可用之后。使用 codegTunnelConnected/codegReady，所有进入链接到 /go；/console 保留。
- 真实冷缓存单/双标签验证静态池和 API 池峰值，包含现有长下载/备份/SSE 占槽场景；不能把 URL 总数当并发保证。

### PR 9 — Operations and release evidence

- Lifeline：运维文档与验收记录；依赖 PR 7、8。
- 写明通配符 DNS/TLS、Host 原样转发、三类读超时、关闭流式缓冲、能力 query 日志脱敏、Windows 注销离线、CodeG 必须先升级并启用本模式。
- 不发布 browser_bridge 额外端口；本地 localhost 测试不代替真实 HTTPS 同站隔离测试。真实计划任务验证 APPDATA/CWD/ACL/恢复，记录同步 IDE 时的事件循环延迟、池峰值与内存预算。
- 默认 codegTunnel 仍关闭。默认启用另开小 PR，需完成路由、流控、恢复、兼容性及容量验收，不能以“设计已修订”代替。

## 2026-09-24 并行评审处置与核实

三个指定路由各委托一次且先全部发出。主审核逐项回查两仓源码；以下是合并后的处置，均为设计修改，非运行时已修复声明。

| 合并问题 | 核实依据 | 处置 |
| --- | --- | --- |
| 真令牌回显、远程停服、备份/终端提取 | CodeG web/handlers/web_server.rs:14、mod.rs:728；handlers/files.rs:115；handlers/terminal.rs:43；commands/backup/core.rs:89 | 按已确认的可信鉴权模型，授权读取自己的令牌不构成阻断；五个服务管理接口留在桌面，避免自动回显与误停，终端/文件/代理/备份保持可用 |
| GET health 永不 ready | CodeG web/router.rs:38、前端 probeHealth | 改 POST；未采纳 GET 会落入 501 fallback 的具体推断，已注册路径方法不匹配通常为 405 |
| 过期恢复与 provider 期限 | Lifeline auth/password.ts:171、235；provider.ts:8；trusted-header.ts:18 | /go 统一入口；传递验证后的期限，不假设 trusted-header 有 Lifeline cookie |
| HTML 信用膨胀、并发收齐死锁、复用 socket 停读 | Node zlib/流量算术检查：2102 压缩字节展开为 2097191；三流未完成即占满 8 MiB | 原始字节信用、连接级窗口、HTML 独立预留、控制面保持推进 |
| HTTP/Upgrade 分派与 Fastify 收齐 | Lifeline relay.ts:145、147、156、262 | 入口统一分派；engine.io 锁定版本的确切延迟行为留真实集成测试，未冒充已实测 |
| Office opaque origin 与 CORS | office-preview.tsx:44、246；office_watch_proxy.rs:23、33、215 | 保留 sandbox，绑定身份期限的单文档派生能力 |
| 交接重读、stop 路径、旧文件/实例竞争 | CodeG web/mod.rs:285、688、759 | 单一 5 s 循环、state 留路径、租约及安全创建、POST 复探 |
| 设置自动保存 / CLI 丢开关 | web-service-settings.tsx:374；api.ts:5440；Lifeline cli/config.ts:37、start.ts:12、setup.ts:122 | 新字段缺省保留、全链路持久化、补 UI/文案/测试 |
| label 时机、adoption、迁移与 secret 表 | Lifeline identity-store.ts:123、216、264；db/open.ts:33；agent-hub.ts:396 | 不提前 INSERT，手写迁移，双通道存活检查，明确新增单行表 |
| 独立 socket 并不隔离主线程 | Lifeline protocol/limits.ts:4、agent/index.ts:70 | 删除隔离承诺；先压测，失败再考虑 worker，不预增进程 |
| 静态请求数与 API 池容量 | out 实测 44 JS + 3 CSS + 51 字体 URL；web-transport.ts 非 2xx 抛错 | 静态上界 99；不把字体数当实际加载数；双池实测发布门 |
| WS 超限与关闭语义 | CodeG web/ws.rs:52、233 | 保留 8 MiB Reset 的显式兼容边界；不在已发送分片后丢尾片，补 Pong/取消/close 回收 |
| 工作区大文件、SSE | web/router.rs:649；office_watch_proxy.rs:283 | 两类上传默认 64 MiB；SSE 65 分钟可重连，增加浏览器验收 |
| browser_bridge 额外端口 | web/browser_bridge.rs:113；frontend browser-bridge.ts:121 | 托管客户端声明不支持，不把额外端口暴露公网 |
| IP apex、路径归一、相对数据目录 | Node WHATWG URL 拒绝机器 IP 子域；posix.normalize 会消掉越根段；paths.rs:214 | localhost-only 开发、归一前验证、转发原 query、隧道要求绝对目录 |
| forbidden 表与 SQLite 读并发表述 | portable_keys.rs:42、52、75；db/mod.rs 活跃句柄替换警告 | 纠正事实；仍保持不读库、不另起完整应用的方案 |

核实范围：源码与静态产物检查、Node 标准库断言已运行。未安装 Lifeline 依赖、未启动服务、未执行 Rust 构建或端到端测试；上述 PR 验收均是实施要求，不是本轮测试通过记录。`lifeline.woa.com` 可信登录与完整操作者权限已由用户确认；剩余运行时证据在各 PR 的发布门收集。
