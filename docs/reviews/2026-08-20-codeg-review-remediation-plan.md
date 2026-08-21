# Codeg 残留审查问题修复实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** 关闭 2026-08-20 本地修改复审中仍开放的问题，并用确定性回归测试证明会话数据、连接生命周期和资源上限在压力及竞态下保持正确。

**Architecture:** 修复分为四条相互隔离的轨道：有界数据投影与传输、ACP 有序生命周期、会话发现与文件监听、外围安全与发布卫生。每条轨道先建立失败测试，再修改单一所有权边界；跨轨道唯一硬依赖是“服务端快照预算”必须先于“客户端超帧恢复”。

**Tech Stack:** Rust 2021、Tokio、Axum WebSocket、SeaORM、notify/ignore、Next.js 16、React 19、TypeScript strict、Vitest、pnpm。

**Spec:** `docs/reviews/2026-08-20-codeg-crash-logic-perf-review.md`

## Global Constraints

- 不得通过删除未持久化数据来满足内存上限；只允许删除已由明确 watermark、ID 或窗口边界证明已持久化的数据。
- 所有 wire snapshot 必须有总字节预算、集合数量预算和机器可读的截断元数据；单字段上限不能代替总量上限。
- 同一 connection/destination 的 lifecycle-critical 事件必须 FIFO，队列饱和时不得静默丢弃。
- rebind、disconnect 和 cleanup 对 map entry 的修改都必须校验 connection incarnation；批量 rebind 还必须对全部目标做 owner/generation CAS。
- ApplicationShutdown 必须关闭所有连接创建入口，等待已获准的创建结束，然后反复 drain，直到连接表和 admission 计数同时为零。
- 传输层错误不得删除仍存活的 canonical connection；错误必须可恢复且有稳定错误码。
- URL 处理不得按未经验证的 UTF-8 字节偏移切片；日志不得包含 userinfo、token、query 或 fragment。
- 新增前端文案必须进入全部 10 个 `src/i18n/messages/*.json` 文件。
- 每个任务独立提交；任务自己的定向测试通过后才能开始下一个任务。

## Current Baseline

本计划以 2026-08-20 当前工作树为准。前次复审后，`retireCoveredLocalTurns` 已改为只删除 detail 中同 ID 的回合，不再按 80 条截断。`conversation-runtime-store.test.ts` 已固定 81 条未持久化回合全部保留的契约；2026-08-20 15:11 运行该文件，24/24 测试通过。因此原 transcript 丢失项已关闭，执行本计划从 Task 2 开始。

| ID | 状态 | 合并门槛 |
|---|---|---|
| R0 | 已关闭 | 24/24 定向测试通过，81 条未持久化回合全部保留 |
| R1 | 开放 | 快照总预算、超帧 attach 恢复、remote proxy 同限额 |
| R2 | 开放 | 大 Write/Edit 输入在有界内存下只解析一次 |
| R3 | 开放 | critical lifecycle FIFO、无丢失、目标间互不阻塞 |
| R4 | 开放 | 删除 `raw_input_chunks`，增量解析保持 O(n) |
| R5 | 开放 | rebind 对根及全部子连接执行完整 CAS |
| R6 | 开放 | manager-wide admission fence 和真正的先停接入后 drain |
| R7 | 部分完成 | Cline/Gemini 单次恢复扫描、窄时间窗、排名前排除内部会话 |
| R8 | 开放 | 非递归、剪枝后的 watcher 注册及动态目录更新 |
| R9 | 开放 | URL 结构化解析、UTF-8 安全、大小写一致 |
| R10 | 次要 | 大文件复制文案国际化，复制失败有反馈 |
| R11 | 次要 | 第三方许可证生成不随开发机平台漂移 |

## Delivery Order

1. Phase A: Tasks 2-5，完成有界数据路径。
2. Phase B: Tasks 6-8，修复事件顺序、rebind CAS 和 shutdown admission。
3. Phase C: Tasks 9-11，修复 parser recovery、watcher 注册和 URL 安全。
4. Phase D: Tasks 12-13，完成 UI/许可证卫生及最终回归。

---

### Task 1: Transcript overlay 基线确认（已完成）

**Files:**
- Modify: `src/stores/conversation-runtime-store.test.ts:850`
- Verify: `src/stores/conversation-runtime-store.ts:860`

**Interfaces:**
- Consumes: `retireCoveredLocalTurns(localTurns, detail)`（文件内私有函数）。
- Produces: “仅持久化证明可退休 local turn”的稳定 reducer 契约。

- [x] **Step 1: 固定不按数量截断的契约**

当前测试包含以下断言：

```ts
expect(local).toHaveLength(81)
expect(local.some((t) => t.id === "live-42-cap-0")).toBe(true)
expect(local.some((t) => t.id === "live-42-cap-80")).toBe(true)
```

- [x] **Step 2: 固定已持久化 ID 才退休的契约**

测试先把 `live-42-keep-me` 放入 detail，再完成同 ID live turn，断言该 overlay 被删除；其余 81 个未出现在 detail 中的 ID 全部保留。

- [x] **Step 3: 运行定向测试**

```powershell
pnpm test -- src/stores/conversation-runtime-store.test.ts
```

Observed: 1 test file passed，24 tests passed，exit 0。

**Acceptance:** owner 会话连续完成 81 个未被 detail 覆盖的回合后，第一个和最后一个回合都可见；detail 返回持久化 ID 后只退休对应 overlay。

---

### Task 2: 建立服务端 aggregate snapshot budget

**Files:**
- Modify: `src-tauri/src/acp/session_state.rs:2229`
- Modify: `src-tauri/src/web/ws_attach.rs:87`
- Modify: `src-tauri/src/web/ws.rs:167`
- Test: `src-tauri/src/acp/session_state.rs` test module
- Test: `src-tauri/src/web/ws_attach.rs` test module

**Interfaces:**
- Produces: `SnapshotLimits`、`SnapshotTruncation`、`SessionState::to_snapshot_with_limits`。
- Produces: `ServerMsg::AttachError { code: AttachErrorCode::SnapshotBudgetExceeded }`。
- Consumed by: Task 3 的 Web/Tauri remote 客户端恢复路径。

- [ ] **Step 1: 写失败测试，覆盖总字节和总数量**

新增三个 Rust 测试：

| Test name | Required assertion |
|---|---|
| `snapshot_enforces_aggregate_budget_and_reports_omissions` | snapshot 的估算 payload 不超过 limits，`truncation` 精确报告图片、文本和 tool omission |
| `snapshot_caps_tool_failure_and_watchdog_collections` | 每个集合不超过对应 count limit，保留项按确定性顺序选择 |
| `serialized_snapshot_frame_never_exceeds_attach_frame_limit` | `serde_json::to_vec(ServerMsg::Snapshot)` 的实际长度不超过 `MAX_ATTACH_FRAME_BYTES` |

测试数据至少包含：300 张小图、300 个 tool calls、1 MiB `input`、1 MiB `output`、1 MiB `live_message`、1,000 个 `session_failures` 和 1,000 个 watchdog tombstones。断言 live `SessionState` 未被投影过程修改。

- [ ] **Step 2: 引入统一预算类型**

建议接口固定为：

```rust
pub const MAX_ATTACH_FRAME_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy)]
pub struct SnapshotLimits {
    pub payload_bytes: usize,
    pub tool_calls: usize,
    pub images: usize,
    pub failures: usize,
    pub watchdog_tombstones: usize,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotTruncation {
    pub omitted_tool_calls: usize,
    pub omitted_images: usize,
    pub omitted_failures: usize,
    pub omitted_watchdog_tombstones: usize,
    pub truncated_text_fields: usize,
}

impl SessionState {
    pub fn to_snapshot_with_limits(&self, limits: SnapshotLimits)
        -> LiveSessionSnapshot;
}
```

`payload_bytes` 计算 UTF-8 字节，不计算字符数。字符串截断必须落在 `is_char_boundary`，保留字段尾部时要携带明确的 `truncated` 信息。`LiveSessionSnapshot` 增加可选 `truncation` 字段，零遗漏时不序列化。

- [ ] **Step 3: 对所有可增长字段使用同一个 budget**

预算必须覆盖 `live_message`、tool input/output/content/meta/locations、图片数据与数量、`active_tool_calls` 数量、`session_failures` 和 `tool_watchdog_max_versions`。不得只继续扩大 `MAX_SNAPSHOT_IMAGE_TOTAL_CHARS`。

actionable watchdog projections 优先于 tombstones；只有后端已拒绝对应 stale producer 的 tombstone 才能省略。若仍可能收到旧 projection，先在后端 gate 掉 stale event，再允许 snapshot 省略其 floor。

- [ ] **Step 4: 在实际序列化点执行最终硬校验**

`ws.rs` 对 `ServerMsg` 执行 `serde_json::to_vec` 后校验长度。正常 snapshot 必须小于 `MAX_ATTACH_FRAME_BYTES`；若投影逻辑失守，发送小型 `AttachError`，不得发送超帧，也不得断开或删除 agent connection。

```rust
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachErrorCode {
    SnapshotBudgetExceeded,
}
```

- [ ] **Step 5: 运行定向 Rust 测试**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils snapshot_enforces_aggregate_budget_and_reports_omissions
cargo test --lib --features test-utils snapshot_caps_tool_failure_and_watchdog_collections
cargo test --lib --features test-utils serialized_snapshot_frame_never_exceeds_attach_frame_limit
```

- [ ] **Step 6: 建议提交**

```bash
git add src-tauri/src/acp/session_state.rs src-tauri/src/web/ws_attach.rs src-tauri/src/web/ws.rs
git commit -m "fix: enforce aggregate ACP snapshot budgets"
```

**Acceptance:** 任意构造的 attach snapshot 实际序列化后不超过 4 MiB；遗漏均有计数；创建 snapshot 不修改 live state；连接保持存活。

---

### Task 3: 统一 direct Web 与 remote desktop 的超帧恢复

**Files:**
- Modify: `src/lib/transport/types.ts:18`
- Modify: `src/lib/transport/web-event-stream.ts:116`
- Modify: `src/lib/transport/web-transport.ts:25`
- Modify: `src/contexts/acp-connections-context.tsx:6328`
- Modify: `src-tauri/src/commands/remote_proxy.rs:1780`
- Test: `src/lib/transport/web-transport.test.ts`
- Test: `src/contexts/acp-connections-context.test.tsx`
- Test: `src-tauri/src/commands/remote_proxy.rs` test module

**Interfaces:**
- Consumes: Task 2 的 `AttachErrorCode` 和 4 MiB 服务端上限。
- Produces: 前端 `onAttachError(code, retryable)` 回调，不再把 oversize 伪装成 terminal detach。

- [ ] **Step 1: 写三个失败测试**

1. Direct Web：收到 `snapshot_budget_exceeded` 后 canonical connection 仍存在，subscription 进入 recoverable error 状态。
2. Client backstop：收到超过 `MAX_WS_FRAME_CHARS` 的未知/旧服务端帧时，不执行 `JSON.parse`，不触发 `connection_gone`。
3. Remote desktop：超限文本在 `serde_json::from_str` 之前被拒绝，并向目标 webview 发出小型 attach error envelope。

- [ ] **Step 2: 分离 attach error 与 detach reason**

在 `AttachHandlers` 中增加：

```ts
onAttachError: (
  code: "snapshot_budget_exceeded" | "oversized_frame",
  retryable: boolean
) => void
```

`WebEventStream.notifyOversizedFrame` 可以停止对应 wire subscription，但不得调用 `onDetached`，不得删除 canonical connection。Context 将错误保存在连接状态并提供显式 retry；自动重试最多一次，且只允许在新的 WS ready 或用户触发 retry 后执行，禁止针对同一超帧立即循环 attach。

- [ ] **Step 3: remote proxy 在解析前执行同一字节限制**

在 `forward_text_message` 开头检查 `text.len()`。常量与服务端共享或保持 `MAX_REMOTE_WS_FRAME_BYTES == MAX_WS_FRAME_CHARS` 的测试契约；超限分支不得构造 `serde_json::Value`。

- [ ] **Step 4: 运行定向测试**

```powershell
pnpm test -- src/lib/transport/web-transport.test.ts src/contexts/acp-connections-context.test.tsx
Set-Location src-tauri
cargo test --lib --features test-utils remote_proxy_rejects_oversized_frame_before_parse
```

- [ ] **Step 5: 建议提交**

```bash
git add src/lib/transport src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx src-tauri/src/commands/remote_proxy.rs
git commit -m "fix: recover safely from oversized attach frames"
```

**Acceptance:** direct Web 和 remote desktop 都不会解析超限帧；不会删除 live connection；用户可以在状态变化后重试且无 attach 热循环。

---

### Task 4: 为大工具输入增加 owner-scoped parse cache

**Files:**
- Modify: `src/lib/try-parse-json.ts`
- Modify: `src/lib/try-parse-json.test.ts`
- Modify: `src/lib/plan-parse.ts:206`
- Modify: `src/lib/tool-call-normalization.ts:227`
- Modify: `src/lib/collab-tool.ts`
- Modify: `src/components/message/live-turn-stats.tsx:64`
- Verify: `src/stores/conversation-runtime-store.ts`

**Interfaces:**
- Retains: 小字符串使用 byte-weighted global LRU。
- Produces: `parseJsonForOwner(owner, input)`，大字符串只绑定到活动 tool object 的弱引用生命周期。

- [ ] **Step 1: 把当前“大输入解析两次”的测试改成失败契约**

```ts
const owner = {}
const huge = `{"content":"${"x".repeat(128 * 1024)}"}`
expect(parseJsonForOwner(owner, huge)).toBeDefined()
expect(parseJsonForOwner(owner, huge)).toBeDefined()
expect(JSON.parse).toHaveBeenCalledTimes(1)
```

再增加 owner 的 `input` 字符串变化后恰好重新解析一次，以及 owner 被替换时不复用旧值的测试。

- [ ] **Step 2: 实现弱引用 owner cache**

```ts
type OwnerParseEntry = {
  input: string
  value: unknown | typeof MISS
}

const ownerParseCache = new WeakMap<object, OwnerParseEntry>()

export function parseJsonForOwner(
  owner: object,
  input: string
): unknown | undefined
```

大输入不得进入以字符串为 key 的全局 Map。活动 tool object 消失后，key 和 parsed graph 都可被 GC。全局 LRU 继续服务短字符串，并按 UTF-8 估算字节或保守按 `length * 2` 计权。

- [ ] **Step 3: 让所有 live hot path 传入稳定 owner**

`kimiTodoWriteEntries`、tool name inference、collab detection 和 `LiveTurnStatsBanner` 必须复用同一个 tool-call/info object。不得为 banner 创建新的 `{ type: "tool_call", info }` 包装对象后再作为 cache owner。

- [ ] **Step 4: 运行定向测试和相关 reducer 测试**

```powershell
pnpm test -- src/lib/try-parse-json.test.ts src/lib/plan-parse.test.ts src/lib/tool-call-normalization.test.ts src/stores/conversation-runtime-store.test.ts
```

- [ ] **Step 5: 建议提交**

```bash
git add src/lib/try-parse-json.ts src/lib/try-parse-json.test.ts src/lib/plan-parse.ts src/lib/tool-call-normalization.ts src/lib/collab-tool.ts src/components/message/live-turn-stats.tsx
git commit -m "perf: cache large tool input by active owner"
```

**Acceptance:** 同一个 128 KiB Write/Edit 输入在后续 100 个 token 更新中只调用一次 `JSON.parse`；全局 cache 不强持有该字符串或 parsed graph。

---

### Task 5: 删除 raw input 双份历史并改为单遍增量解析

**Files:**
- Modify: `src-tauri/src/acp/session_state.rs:190`
- Test: `src-tauri/src/acp/session_state.rs` test module

**Interfaces:**
- Removes: `ToolCallState::raw_input_chunks`。
- Produces: serde-skip 的 `RawJsonAccumulator`，每个新 chunk 只扫描一次。

- [ ] **Step 1: 写失败的规模测试**

测试输入 10,000 个小 chunk，总计低于 1 MiB。断言最终只 parse 一次完整 JSON、buffer 不超过 1 MiB、snapshot 不包含 accumulator。再测试超过上限后 sticky freeze，后续 `}` 不得拼成截断 JSON。

- [ ] **Step 2: 用状态机替换 `raw_input_chunks`**

```rust
#[derive(Debug, Clone, Default)]
struct RawJsonAccumulator {
    buffer: String,
    depth: i32,
    in_string: bool,
    escaped: bool,
    frozen: bool,
}

impl RawJsonAccumulator {
    fn push(&mut self, chunk: &str, max_bytes: usize)
        -> Option<serde_json::Value>;
}
```

`push` 只扫描传入 chunk 来更新 depth/string/escape 状态；结构闭合时才对整份 buffer 调用一次 `serde_json::from_str`。单个 chunk 本身是完整 JSON 时允许 fast path。

- [ ] **Step 3: 删除所有 join、chunk count 和重复长度求和**

删除 `raw_input_chunks` 的字段、构造初始化、`iter().map(String::len).sum()` 和 `join("")`。snapshot 仍不得序列化 accumulator。

- [ ] **Step 4: 运行定向测试**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils raw_input
cargo test --lib --features test-utils snapshot_excludes_internal_chunk_buffers_and_carries_negotiated_caps
```

- [ ] **Step 5: 建议提交**

```bash
git add src-tauri/src/acp/session_state.rs
git commit -m "perf: parse streamed tool input in one pass"
```

**Acceptance:** 内存中不存在第二份 chunk 历史；处理 n 字节分片的扫描工作为 O(n)；1 MiB 上限和 sticky freeze 行为有测试固定。

---

### Task 6: 将 lifecycle critical delivery 改为 per-destination ordered ingress

**Files:**
- Modify: `src-tauri/src/acp/internal_bus.rs:39`
- Modify: `src-tauri/src/acp/lifecycle.rs:43`
- Modify: `src-tauri/src/web/event_bridge.rs:406`
- Test: `src-tauri/src/acp/internal_bus.rs` test module
- Test: `src-tauri/src/acp/lifecycle.rs` test module

**Interfaces:**
- Removes: `overflow_tx`、`last_resort_tx`、process-wide lifecycle overflow worker、5 秒丢弃分支。
- Produces: 每 connection 一个 `LifecycleIngress`，唯一 sender、唯一 FIFO consumer。

- [ ] **Step 1: 写确定性饱和测试**

新增以下测试，使用容量 1 和 barrier 控制 worker：

| Test name | Required assertion |
|---|---|
| `critical_ingress_preserves_fifo_through_saturation` | 先阻塞 consumer，依次提交 A/B/C，释放后收到严格的 A/B/C |
| `saturated_connection_does_not_block_other_connection` | A 的 consumer 保持阻塞时，B 的唯一事件仍在测试 deadline 内到达 |
| `critical_ingress_never_drops_before_shutdown` | 多 producer 饱和后，drain 前提交数与 consumer 收到数完全相等 |

禁止用 sleep 推测顺序；必须用 channel/barrier 明确控制交错。

- [ ] **Step 2: 建立目标级入口**

建议边界：

```rust
struct LifecycleIngress {
    destinations: Mutex<HashMap<String, Arc<DestinationIngress>>>,
}

struct DestinationIngress {
    incarnation: u64,
    tx: mpsc::Sender<Arc<InternalEventEnvelope>>,
}

impl LifecycleIngress {
    async fn send(&self, env: Arc<InternalEventEnvelope>)
        -> Result<(), LifecycleClosed>;
    async fn drain_and_close(&self);
}
```

同一 connection 的所有 critical event 只经过该 `tx`；不得存在 direct/overflow 两条并行路径。容量满时只对该 destination 施加 backpressure，其他 connection 使用自己的 sender。

- [ ] **Step 3: 在释放 SessionState 写锁后 await critical enqueue**

`emit_with_state_gated` 在锁内只更新 state 和构造 envelope；drop guard 后再 await `LifecycleIngress::send`。这样 backpressure 不会持有 SessionState 写锁。

- [ ] **Step 4: 处理 worker 关闭和 terminal drain**

worker 意外关闭时，在 map lock 下按 destination incarnation 替换并重试原事件一次；terminal event 进入同一 FIFO，worker 处理完它及此前事件后退出。旧 worker 的 cleanup 不得删除替换后的 ingress。

- [ ] **Step 5: 运行定向测试**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils critical_ingress
cargo test --lib --features test-utils lifecycle
```

- [ ] **Step 6: 建议提交**

```bash
git add src-tauri/src/acp/internal_bus.rs src-tauri/src/acp/lifecycle.rs src-tauri/src/web/event_bridge.rs
git commit -m "fix: preserve lifecycle FIFO under saturation"
```

**Acceptance:** A/B/C 在同一 connection 永远按 A/B/C 到达；connection A 的满队列不阻止 B；生产代码不存在“dropping critical lifecycle event”分支。

---

### Task 7: 对 rebind 全目标执行原子 CAS

**Files:**
- Modify: `src-tauri/src/acp/manager.rs:7550`
- Test: `src-tauri/src/acp/manager.rs` test module

**Interfaces:**
- Produces: `ExpectedRebindTarget`，记录 incarnation、owner label、operation ID、generation。
- Guarantees: validate-all-then-mutate-all，不允许跳过已变化的 descendant 后部分成功。

- [ ] **Step 1: 写两个失败竞态测试**

1. snapshot 后把 child ID 替换为新 incarnation；rebind 必须失败且 replacement owner 不变。
2. snapshot 后只修改 child owner/generation；即使 incarnation 相同，也必须失败，root 和所有 sibling 都不得被部分修改。

再补充 idempotent early-return 竞态：snapshot 显示已在目标 owner，随后替换 root；返回成功前必须重新校验 live root。

- [ ] **Step 2: 扩展目标快照**

```rust
struct ExpectedRebindTarget {
    id: String,
    connection_incarnation: String,
    owner_window_label: String,
    owner_operation_id: Option<String>,
    ownership_generation: u64,
}
```

- [ ] **Step 3: 最终 map lock 内先验证全部目标，再统一修改**

任何目标缺失或任一预期字段不匹配都返回 coded CAS error；不得使用 `continue` 跳过 descendant。只有验证循环全部成功后才进入 mutation 循环。SessionState 写操作继续在 map lock 释放后执行，但收集的 state Arc 必须来自已验证的同一批 live entries。

- [ ] **Step 4: 移动 idempotent 判断**

“已经在目标 owner + operation”只能在重新取得 map lock并验证 root incarnation/owner/generation 后返回；不能信任最初 snapshot。

- [ ] **Step 5: 运行 rebind 测试**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils rebind_
```

- [ ] **Step 6: 建议提交**

```bash
git add src-tauri/src/acp/manager.rs
git commit -m "fix: make owner rebind an all-target CAS"
```

**Acceptance:** ID 复用、child owner 改变和 idempotent 竞态都 fail closed；失败时没有任何 root/child 被部分 rebind。

---

### Task 8: 增加 manager-wide shutdown admission fence

**Files:**
- Modify: `src-tauri/src/acp/manager.rs:709`
- Modify: `src-tauri/src/server_bin/main.rs:720`
- Modify: 所有 `ConnectionManager` direct/shared/internal spawn entry points
- Test: `src-tauri/src/acp/manager.rs` test module
- Test: `src-tauri/tests/shared_session_http.rs`

**Interfaces:**
- Produces: `ConnectionAdmissionGate`、RAII `ConnectionAdmissionPermit`。
- Produces: `ConnectionManager::begin_shutdown()` 和 `drain_for_shutdown()`。

- [ ] **Step 1: 写 admission 竞态测试**

使用 barrier 暂停一个已获得 permit 但尚未插 map 的 spawn；同时开始 shutdown，再发 direct connect、shared connect 和 delegated child spawn。断言新请求得到稳定 `server_shutting_down`，已获准请求结束后也被 drain，最终 `connections.is_empty()` 且 `in_flight == 0`。

- [ ] **Step 2: 实现计数型 admission gate**

```rust
struct AdmissionState {
    accepting: bool,
    in_flight: usize,
}

pub struct ConnectionAdmissionGate {
    state: std::sync::Mutex<AdmissionState>,
    drained: tokio::sync::Notify,
}

impl ConnectionAdmissionGate {
    fn admit(&self) -> Result<ConnectionAdmissionPermit, AcpError>;
    async fn close_and_wait(&self);
}
```

permit 必须从进入 spawn 流程前一直持有到进程启动失败或 connection 已插入 map。`clone_ref` 必须共享同一个 gate。

- [ ] **Step 3: 覆盖全部创建入口**

至少核对 `connect_or_attach_shared`、`start_shared_attempt`、`spawn_agent_with_attach_mode_and_workflow_binding`、`ConnectionManagerSpawner::spawn`、resume/fork/internal probe 路径。不得只在 HTTP handler 设 flag。

- [ ] **Step 4: 修正 Axum shutdown 顺序**

当前把 ACP drain 放在 `with_graceful_shutdown` future 内，Axum 只会在该 future 返回后停止 accept。改为由外层 supervisor 驱动：

```rust
manager.begin_shutdown();          // 拒绝所有 manager spawn
shutdown_signal.trigger();         // 让 HTTP accept/WS 停止
manager.wait_for_admissions().await;
server.await?;                     // 等在途 HTTP 请求退出
manager
    .drain_for_shutdown(AcpDisconnectOrigin::ApplicationShutdown)
    .await;
```

`drain_for_shutdown` 不使用固定“两次快照”；循环条件必须是 admission 已关闭、in-flight 为零、connection map 为空。每轮取走当前 map entries，并保留现有 PID/terminal backstop。

- [ ] **Step 5: 运行桌面 core 与 server 测试**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils shutdown_admission
cargo test --test shared_session_http --features test-utils shutdown_fences_admission_keeps_release_available_and_restart_is_empty
cargo check --no-default-features --features server --bin codeg-server
```

- [ ] **Step 6: 建议提交**

```bash
git add src-tauri/src/acp/manager.rs src-tauri/src/server_bin/main.rs src-tauri/tests/shared_session_http.rs
git commit -m "fix: fence all ACP admission during shutdown"
```

**Acceptance:** shutdown signal 后没有任何 direct/shared/internal connection 可以被接纳；Axum/WS 先停止接入；drain 返回时没有 map entry、in-flight permit、ACP terminal 或已发布 PID。

---

### Task 9: 收紧 Cline/Gemini stale-session recovery

**Files:**
- Modify: `src-tauri/src/parsers/mod.rs:196`
- Modify: `src-tauri/src/parsers/cline.rs:135`
- Modify: `src-tauri/src/parsers/gemini.rs:750`
- Modify: `src-tauri/src/commands/conversations.rs:1460`
- Test: parser modules and `src-tauri/src/commands/conversations.rs` test module

**Interfaces:**
- Replaces: generic `list_conversations_for_recovery`。
- Produces: parser-specific `recover_conversation(query, accept)`，每个 parser 最多扫描 store 一次。

- [ ] **Step 1: 写恢复选择失败测试**

覆盖：候选超过 5 分钟拒绝、两个候选相差不足 60 秒拒绝、最近候选为 internal 时先排除再排名、Gemini stale ID 在一次 walk 内恢复、Cline 不读取所有 history body。

- [ ] **Step 2: 定义窄 recovery query**

```rust
pub struct RecoveryQuery<'a> {
    pub cwd: &'a str,
    pub approx: DateTime<Utc>,
    pub max_skew: chrono::Duration,       // 5 minutes
    pub ambiguity: chrono::Duration,      // 60 seconds
}

fn recover_conversation(
    &self,
    query: &RecoveryQuery<'_>,
    accept: &dyn Fn(&ConversationSummary) -> bool,
) -> Result<Option<ConversationDetail>, ParseError>;
```

默认实现返回 `Ok(None)`，避免其他 parser 意外回退到全量 list。

- [ ] **Step 3: Cline 只读 `taskHistory.json`，Gemini 单次 walk**

Cline 先生成轻量 summary 并在 `accept` 后排名，再只打开胜者 history。Gemini 在一次 `list_chat_files` walk 中解析 summary、先执行 `accept`、维护 best/second-best；确定唯一胜者后复用该轮已读 value 生成 detail，不再调用 `get_conversation` 二次扫描。

- [ ] **Step 4: 排名前过滤 internal session**

command 层传入：

```rust
|summary| {
    !filter.contains(
        agent_type,
        Some(summary.id.as_str()),
        summary.folder_path.as_deref(),
    )
}
```

禁止先选中 internal winner 再 `reject_internal_detail`；那会屏蔽合法第二候选。

- [ ] **Step 5: 运行 parser 与 command 测试**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils recovery_
cargo test --lib --features test-utils cline
cargo test --lib --features test-utils gemini
```

- [ ] **Step 6: 建议提交**

```bash
git add src-tauri/src/parsers/mod.rs src-tauri/src/parsers/cline.rs src-tauri/src/parsers/gemini.rs src-tauri/src/commands/conversations.rs
git commit -m "fix: recover stale parser sessions with narrow identity bounds"
```

**Acceptance:** Cline/Gemini stale ID 均可恢复；每次请求最多扫描一次 store；internal candidates 在排名前移除；超过 5 分钟或存在 60 秒内歧义时 fail closed。

---

### Task 10: 用剪枝后的非递归 watcher 注册替代全树递归

**Files:**
- Modify: `src-tauri/src/workspace_state/mod.rs:626`
- Test: `src-tauri/src/workspace_state/mod.rs` test module

**Interfaces:**
- Produces: `collect_watch_directories(root)` 和 `WorkspaceWatchRegistration`。
- Retains: 现有 callback 过滤作为第二道防线。

- [ ] **Step 1: 写目录注册测试**

fixture 包含 `src/`、`.github/`、`node_modules/`、`target/`、`.next/`、被 `.gitignore` 排除的目录和 linked-worktree metadata。断言只返回 root 与允许目录；重目录内即使有上千子目录也不出现在注册列表。

- [ ] **Step 2: 使用 `ignore::WalkBuilder` 做初始剪枝**

```rust
fn collect_watch_directories(root: &Path)
    -> Result<Vec<PathBuf>, AppCommandError>;
```

`WalkBuilder` 启用 repository ignore 规则，同时保留普通 hidden source 目录；`WATCH_IGNORED_DIRS` 通过 `filter_entry` 硬排除。`.git` 使用现有 `is_allowed_git_watch_path` 进行显式 metadata 注册，不递归 watch 整棵 object store。

- [ ] **Step 3: 全部工作树目录使用 `RecursiveMode::NonRecursive`**

删除对 root 的 `RecursiveMode::Recursive`。`WorkspaceWatchRegistration` 持有 `HashSet<PathBuf>`，确保重复 rescan 不重复注册。

- [ ] **Step 4: 处理新增目录和 ignore 配置变化**

目录 create/rename 事件触发对新 subtree 的剪枝注册；`.gitignore`、`.ignore` 或 `.git/info/exclude` 变化触发 registration rebuild。watcher 所有权放在单独 control task 中，事件 callback 只发送 `RegisterSubtree`/`Rebuild`，不得从 callback 并发修改 watcher。

- [ ] **Step 5: 运行 workspace tests**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils workspace_state::tests
```

- [ ] **Step 6: 建议提交**

```bash
git add src-tauri/src/workspace_state/mod.rs
git commit -m "perf: prune workspace watcher registrations"
```

**Acceptance:** `node_modules`、`target`、`.next` 和 gitignored tree 不注册 watch descriptor；新增允许目录可实时更新；ignore 文件变化后注册集合收敛。

---

### Task 11: 结构化并 UTF-8 安全地处理 credential URL

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/src/git_credential.rs:386`
- Test: `src-tauri/src/git_credential.rs` test module

**Interfaces:**
- Replaces: `split_http_url` 的手工字符串切片。
- Produces: `parse_http_remote` 和只输出 scheme/host/port/path 的 sanitizer。

- [ ] **Step 1: 写 panic 与大小写回归测试**

测试输入包括 `ééééhttps://host/x`、`HTTPS://token@host/x?secret=1#frag`、短字符串、无效 UTF-8 边界对应的合法 Rust `str`、HTTP 大小写 clone URL。用 `catch_unwind` 明确证明 sanitizer 不 panic。

- [ ] **Step 2: 使用 `url::Url` 或安全 byte-prefix API**

推荐增加直接依赖 `url = "2"` 并统一解析 HTTP(S)。若不增加依赖，scheme 检测必须使用：

```rust
url.as_bytes()
    .get(..8)
    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"https://"))
```

不得再出现 `url[..7]` 或 `url[..8]`。

- [ ] **Step 3: 统一日志和注入 eligibility**

sanitizer 只允许输出 scheme、host、显式 port 和 path；移除 username/password、query、fragment。`try_inject_for_repo`、`try_inject_for_url`、`extract_host` 和 account `server_url` 日志全部调用同一 parser。scheme 比较使用 ASCII case-insensitive 语义。

- [ ] **Step 4: 运行测试和 server check**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils git_credential
cargo check --no-default-features --features server --bin codeg-server
```

- [ ] **Step 5: 建议提交**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/git_credential.rs
git commit -m "fix: parse and sanitize credential URLs safely"
```

**Acceptance:** 任意 Unicode URL 字符串不 panic；大小写 HTTP(S) 行为一致；测试捕获的日志文本不包含 userinfo/query/fragment。

---

### Task 12: 国际化大文件操作并反馈复制失败

**Files:**
- Modify: `src/components/message/content-parts-renderer.tsx:1510`
- Modify: `src/i18n/messages/{en,zh-CN,zh-TW,ja,ko,es,de,fr,pt,ar}.json`
- Test: `src/components/message/content-parts-renderer.test.tsx`

**Interfaces:**
- Consumes: `copyTextToClipboard` from `@/lib/utils`。
- Produces: `Folder.chat.contentParts.moreLines`、`copyAll`、`copyFailed`。

- [ ] **Step 1: 写失败 UI 测试**

渲染 401 行，断言隐藏行数和 copy action 使用翻译文本；mock `copyTextToClipboard` 返回 false，断言出现 localized error feedback。

- [ ] **Step 2: 替换硬编码文案与直接 clipboard 调用**

```ts
const copied = await copyTextToClipboard(content)
if (!copied) toast.error(t("copyFailed"))
```

`moreLines` 使用 ICU count 参数；不要直接调用 `navigator.clipboard.writeText`。

- [ ] **Step 3: 补齐全部语言 key**

以英文语义为准：

```json
{
  "moreLines": "{count} more lines",
  "copyAll": "Copy all",
  "copyFailed": "Could not copy the full file content"
}
```

其他 9 个 locale 必须提供本地化值，不能复制英文占位。

- [ ] **Step 4: 运行定向测试与 i18n key 检查**

```powershell
pnpm test -- src/components/message
pnpm eslint src/components/message/content-parts-renderer.tsx
```

- [ ] **Step 5: 建议提交**

```bash
git add src/components/message/content-parts-renderer.tsx src/i18n/messages
git commit -m "fix: localize large file copy controls"
```

**Acceptance:** 10 个 locale 均无硬编码英文；复制 API 失败时用户得到明确反馈；完整内容仍可复制。

---

### Task 13: 稳定许可证输出并执行最终合并门槛

**Files:**
- Modify: `pnpm-workspace.yaml`
- Modify: `scripts/third-party-licenses.mjs`
- Modify: `scripts/third-party-licenses.test.mjs`
- Regenerate: `src-tauri/resources/THIRD_PARTY_LICENSES.txt`
- Format: 所有本计划涉及的 Rust/TS/JSON 文件

**Interfaces:**
- Produces: 与 host OS 无关的 supported-platform npm package union。

- [ ] **Step 1: 为 npm platform union 写失败测试**

fixture 同时包含 Darwin/Windows/Linux optional native packages。分别模拟当前平台报告，断言 `collectNpmPackageUnion` 输出完全相同且包含所有声明支持的目标。

- [ ] **Step 2: 固定支持架构集合**

在 `pnpm-workspace.yaml` 明确 Windows/macOS/Linux 与 x64/arm64 的 supported architectures，使安装和许可证收集不依赖执行生成命令的主机。生成器对多份 npm report 做与现有 `collectCargoPackageUnion` 相同的 identifier union 和稳定排序。

- [ ] **Step 3: 运行许可证测试并重新生成**

```powershell
node --test scripts/third-party-licenses.test.mjs
pnpm licenses:generate
git diff -- src-tauri/resources/THIRD_PARTY_LICENSES.txt
```

Expected: 输出同时保留声明支持平台的 optional packages；在 Windows 与 macOS 重跑不会互相替换条目。

- [ ] **Step 4: 修复格式和 lint，不使用自动删除业务代码的方式降错**

```powershell
pnpm prettier --write src docs/reviews/2026-08-20-codeg-review-remediation-plan.md
Set-Location src-tauri
cargo fmt
```

- [ ] **Step 5: 运行前端最终门槛**

```powershell
Set-Location ..
pnpm eslint .
pnpm test
pnpm build
```

Expected: 三条命令均 exit 0；测试报告无 skipped regression。

- [ ] **Step 6: 运行 Rust 三运行面检查**

```powershell
Set-Location src-tauri
cargo check
cargo check --no-default-features --features server --bin codeg-server
cargo check --no-default-features --bin codeg-mcp
cargo test --lib --features test-utils
cargo test --no-default-features --features server --bin codeg-server --lib
cargo clippy --all-targets --features test-utils -- -D warnings
cargo clippy --no-default-features --features server --bin codeg-server --lib -- -D warnings
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: 全部 exit 0。若 `cargo test --lib` 再次超过 20 分钟，不得标记完成；使用 test threads、最后输出和进程堆栈定位具体 hanging test，修复后重新完整运行。

- [ ] **Step 7: 运行 diff 卫生检查**

```powershell
Set-Location ..
git diff --check HEAD
git status --short
```

人工确认只包含预期源码、测试、计划与平台无关许可证输出，没有 `target/`、coverage、构建产物或本地平台 churn。

- [ ] **Step 8: 建议提交**

```bash
git add pnpm-workspace.yaml scripts/third-party-licenses.mjs scripts/third-party-licenses.test.mjs src-tauri/resources/THIRD_PARTY_LICENSES.txt
git commit -m "build: make license notices platform independent"
```

**Acceptance:** lint、format、frontend test/build、三个 Rust 运行面 check、Rust lib tests 和 clippy 全部完成且 exit 0；许可证输出跨 host OS 稳定。

## Final Review Checklist

- [x] R0: 81+ 未持久化回合不丢失；24/24 定向测试通过。
- [ ] R1: snapshot 实际帧有硬上限和截断元数据；direct/remote 超帧可恢复。
- [ ] R2: 大 Write/Edit 在 live token 热路径只解析一次且不进入强引用 global cache。
- [ ] R3: saturation 测试证明 per-destination FIFO、无 critical loss、目标间隔离。
- [ ] R4: `raw_input_chunks` 已删除，增量解析 O(n)。
- [ ] R5: rebind validate-all-then-mutate-all，所有竞态 fail closed。
- [ ] R6: manager admission 已关闭后才能 drain，最终连接/permit/PID/terminal 均为空。
- [ ] R7: Cline/Gemini 单次扫描，5 分钟边界，internal 先过滤。
- [ ] R8: watcher 不递归注册 ignored/heavy tree，并支持动态目录。
- [ ] R9: Unicode URL 不 panic，所有日志 URL 已去敏。
- [ ] R10: 大文件操作已国际化且复制失败可见。
- [ ] R11: 许可证报告不随开发机平台替换 optional package。
- [ ] 完整验证命令均有本次运行的 exit 0 证据。

## Merge Decision

只有 Final Review Checklist 全部勾选，且 Task 13 的完整验证在同一最终工作树上运行通过，才能把本轮本地修改标记为 ready to merge。定向测试通过不能替代完整 lint、build、Rust lib test 和 clippy。
