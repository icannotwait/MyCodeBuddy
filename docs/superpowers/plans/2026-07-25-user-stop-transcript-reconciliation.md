# User Stop Transcript Reconciliation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve live Codex response content on user Stop, attach typed interruption metadata, and abort-fence reconcile persisted detail without pre-flush content loss.

**Architecture:** Extend `TurnComplete` with optional `termination_source`/`provider_turn_id`, stop the vendored codex-acp synthetic interrupt marker, emit `_meta.codex.activeTurnId` at turn start, parse `event_msg.turn_aborted` into `MessageTurn.outcome`, and run a frontend cancel coordinator that only applies detail through `RECONCILE_CANCELLED_TURN` after an exact provider-id fence.

**Tech Stack:** Rust (ACP connection, SeaORM models, Codex parser), TypeScript/React (conversation runtime store, ACP context, message list), vendored `@agentclientprotocol/codex-acp` submodule, vitest + cargo test + insta.

**Design baseline:** `docs/superpowers/specs/2026-07-24-user-stop-transcript-reconciliation-design.md`

**Worktree:** `D:\MyCodeBuddy\.worktrees\user-stop-transcript-reconciliation`  
**Branch:** `feature/user-stop-transcript-reconciliation`

## Global Constraints

- User Stop never resumes/retries/resends the interrupted prompt.
- Ordinary `end_turn` keeps no-refetch path.
- Idle Cancel emits no `TurnComplete` and starts no reconcile.
- Only `TurnFinalizationDisposition::UserCancelled` sets `termination_source = "user_stop"`.
- Coordinator must never apply via `FETCH_DETAIL_SUCCESS`; only `RECONCILE_CANCELLED_TURN` after fence match.
- Immediate next prompt / queue flush may cancel coordinator; recovery then via Reload/cold open.
- Wire fields optional with serde defaults; absent = legacy behavior.
- Parent session does not implement; each Task is Grok-implemented and Codex-reviewed under SDD.
- Prefer PowerShell-friendly commands on Windows.
- **Never `git add -A`.** Stage only task-owned paths. Do not stage unrelated dirty files.
- Commits are local only (no push/PR unless user asks).

## AC3 Adapter Delivery (LOCKED)

### Baseline facts

- Production **today** launches public npm `@agentclientprotocol/codex-acp@1.1.7` via Npx (`registry.rs`).
- The git submodule `src-tauri/vendor/codex-acp` is a **Codeg fork** currently versioned **`1.1.2-mycodebuddy.3`** (not an unmodified 1.1.7 tree). Do **not** merely rename it to `1.1.7-codeg.stop1` without a real upstream rebase.
- `tauri.conf.json` resources, server ZIP, and Docker images **do not** currently ship the vendor tree. Dev-only submodule edits are **not** production acceptance.

### Chosen mechanism (normative)

1. **Init submodule** `src-tauri/vendor/codex-acp` (hard-fail if empty).
2. **Baseline policy (pick one explicit path in Task 2 report; implement the chosen one):**
   - **Preferred:** Rebase/merge the fork onto the upstream `1.1.7` tag/commit that matches production’s public pin, then apply Stop patches; version as `1.1.7-mycodebuddy.stop1`.
   - **Allowed fallback if rebase is blocked:** keep the mycodebuddy fork lineage, apply Stop patches, version as `1.1.2-mycodebuddy.stop1`, and **replace** the production Npx pin so Codeg no longer launches public `1.1.7` for default Codex (document intentional pin change).
3. Apply Stop patches on the chosen baseline (marker removal + `activeTurnId` emit).
4. `npm ci` + `npm run build` in vendor; ensure `dist/index.js` (bin entry) exists; commit **inside submodule** with explicit paths (no parent `git add -A`).
5. Parent records submodule gitlink SHA.

### Packaging + runnable launch (LOCKED mechanism)

The vendor package’s `bin` points at **JavaScript** (`dist/index.js`) and needs an **installed** npm prefix (deps + platform shims). Shipping bare `package.json`+`dist` is **not** directly spawnable as a Windows program.

**Chosen mechanism: application-managed npm prefix**

1. **Build stage (clean checkout):** every desktop/server/Docker/local package path runs `npm ci && npm run build` in `src-tauri/vendor/codex-acp`.  
   - Desktop: own a durable hook — extend `tauri.conf.json` `build.beforeBuildCommand` (or a small `scripts/stage-codex-acp.mjs` invoked from it) so local `tauri build` always stages.  
   - Release.yml desktop + server jobs: same stage before artifact copy.  
   - Dockerfile: same stage before `COPY`.
2. **Seed payload in artifacts:** copy the **built package tree** (enough for `npm install` from a directory: `package.json`, `package-lock.json` or bundled deps strategy, `dist/`) into:
   - Desktop resources: e.g. `resources/codex-acp-seed/`
   - Server ZIP / Docker: same relative layout beside the binary.
3. **Managed install prefix (runtime):**  
   - `<app_data>` = `crate::paths::resolve_effective_data_dir` (or the same `CODEG_DATA_DIR` first pattern in `src-tauri/src/paths.rs`) with the process’s established Tauri/server data-dir fallback. Do not invent a parallel root.  
   - Target: `<app_data>/agent-runtimes/codex-acp-<lockedPin>/`.  
   - **Single-flight + atomic promotion:** install into a temp dir under the same parent, validate, then rename/promote into the final prefix. Concurrent first launches must not corrupt the prefix (mutex/file lock or equivalent).  
   - **Integrity:** after install, require installed `package.json` version equals locked pin and required bin/shim exists; on mismatch/partial install, delete and reinstall.  
   - Seed source: resource/dev seed directory with matching locked pin.
4. **Launch resolver:** `resolve_codex_acp_command()` returns the absolute **managed-prefix shim** when valid; else single-flight install from seed then resolve; else fall back to legacy PATH only if seed missing.  
   - **Ignore ambient PATH** public `1.1.7` when managed prefix or seed exists.  
   - Env escape hatch: `CODEG_CODEX_ACP_BIN` absolute executable wins.
5. **Minimal `connection.rs` change:** Npx Codex branch uses `resolve_codex_acp_command()` instead of bare `resolve_npx_command("codex-acp")`.
6. **Registry identity:** locked Stop pin string (Preferred `1.1.7-mycodebuddy.stop1` or Fallback `1.1.2-mycodebuddy.stop1`); tests updated; not bare public `1.1.7`.
7. **Prepare:** after successful managed install, set `installed_version` to locked pin for UI.
8. **Tests (Task 2 — include these names in code):**  
   - `codex_resolver_prefers_managed_prefix_over_path_public_1_1_7`  
   - `codex_resolver_survives_restart_with_managed_prefix`  
   - `codex_resolver_initialize_smoke_via_resolve_codex_acp_command` (ACP initialize using the path returned by the production resolver)  
   - `codex_managed_install_single_flight_concurrent`  
   - `codex_managed_install_repairs_partial_or_version_mismatch`  
   - `codex_resolver_codeg_codex_acp_bin_override`  
   - Seed absent → PATH fallback (documented).

### Task ownership

- Task 2 owns: vendor patches, seed packaging, managed-prefix install, resolver + connection one-liner, beforeBuildCommand/release/Docker stage hooks, unit/integration tests including initialize via production resolver.
- Task 8 owns: `smoke-codex-acp.mjs` expected pin update, multi-binary/FE verification, layout file checks, report.

## File Map

| Area | Files |
| --- | --- |
| Shared models | `src-tauri/src/models/message.rs`, all Rust `MessageTurn { ... }` literals (~50+ across parsers/commands), `src/lib/types.ts` |
| ACP event | `src-tauri/src/acp/types.rs`, all `AcpEvent::TurnComplete { ... }` sites, FE `AcpEvent` in `src/lib/types.ts` |
| Adapter | `src-tauri/vendor/codex-acp/**`, `src-tauri/src/acp/registry.rs`, Codex install helper in `src-tauri/src/commands/acp.rs` (vendor-path install) |
| Rust lifecycle | `src-tauri/src/acp/connection.rs`, optional `session_state.rs` |
| Parser | `src-tauri/src/parsers/codex.rs` (inline temp JSONL tests + insta under `src-tauri/tests/snapshots` if churned) |
| Runtime | `src/stores/conversation-runtime-store.ts` (+ tests) |
| Envelope | `src/contexts/acp-connections-context.tsx` (authoritative coordinator starter), `conversation-session-surface.tsx` (promotion-only + Manual Reload) |
| Presentation | `src/lib/adapters/ai-elements-adapter.ts`, `src/components/message/message-list-view.tsx`, `src/i18n/messages/*.json` |

## Fixed action names (Tasks 5–7)

| Action / API | Role |
| --- | --- |
| `RECORD_TURN_OUTCOME` | Attach/idempotent set `TurnOutcome` on current-turn assistant or outcome-only turn |
| `START_CANCEL_RECONCILE` | Start coordinator from accepted user_stop envelope |
| `RECONCILE_CANCELLED_TURN` | Fenced authoritative detail install + clear overlays |
| `CLEAR_CANCEL_RECONCILE` | Clear pending key + timers (lifecycle table) |
| `reloadDetail(runtimeConversationId, { reason: "manual_reload" })` | Manual Reload: clear cancel key then authoritative load |
| Internal raw fetch | Non-dispatching detail read used only by coordinator |

---

### Task 1: Shared `TurnOutcome` + optional `TurnComplete` wire fields

**Files:**
- Modify: `src-tauri/src/models/message.rs` — add `TurnOutcome` + `MessageTurn.outcome`
- Modify: **every** Rust `MessageTurn { ... }` construction site (`outcome: None` or default) across parsers (`codex`, `claude`, `gemini`, `opencode`, `grok`, `hermes`, `kimi_code`, `mod.rs`, etc.), commands, event-stream, delegation, fixtures (~50+)
- Modify: `src-tauri/src/acp/types.rs` — optional fields on `TurnComplete`
- Modify: **every** `AcpEvent::TurnComplete { ... }` site with `termination_source: None, provider_turn_id: None` unless Task 3 owns UserCancelled values (Task 1 may leave UserCancelled as None; Task 3 sets real values)
- Modify: `src/lib/types.ts`
- Test: serde tests in `acp/types.rs` module tests

**Interfaces:**
- Produces (Rust + TS) as in design:
  - `TurnOutcome { status, stop_reason, source?, provider_turn_id?, completed_at?, duration_ms? }`
  - `MessageTurn.outcome: Option<TurnOutcome>`
  - `TurnComplete.termination_source?: "user_stop"`, `provider_turn_id?: string`
- Consumes: nothing

- [ ] **Step 1: Write failing serde tests** (absent + present optional fields).

- [ ] **Step 2: Implement types** with `#[serde(default, skip_serializing_if = "Option::is_none")]`.

- [ ] **Step 3: Update all `MessageTurn` and `TurnComplete` construct sites** so `cargo test --features test-utils --lib` and `cargo check --all-targets --features test-utils` compile.

- [ ] **Step 4: Mirror TS types.**

- [ ] **Step 5: Verify + scoped commit**

```powershell
cd src-tauri
cargo test --features test-utils --lib
cargo check --all-targets --features test-utils
cd ..
pnpm exec tsc --noEmit
git add src-tauri/src/models/message.rs src-tauri/src/acp/types.rs src/lib/types.ts
# plus every other path this task actually touched for MessageTurn/TurnComplete constructors
git status
git commit -m "feat(types): add TurnOutcome and optional user-stop TurnComplete fields"
```

---

### Task 2: Vendored codex-acp patch + packaging + install pin (AC3)

**Files:**
- Submodule: `src-tauri/vendor/codex-acp` (init required; hard-fail if missing)
- Modify: `CodexAcpServer.ts`, `CodexEventHandler.ts` (+ vendor tests)
- Modify: `src-tauri/src/acp/registry.rs` version/package identity
- Modify: `src-tauri/src/commands/acp.rs` — discovery + default install from packaged/vendor path; custom version override rules
- Modify: `src-tauri/tauri.conf.json` resources mapping
- Modify: `.github/workflows/release.yml` **and** `Dockerfile` (both required) — build stage + copy
- Create: `src-tauri/scripts/stage-codex-acp.mjs` (mandatory; parent-committed)
- Possibly: `src-tauri/src/paths.rs` only if reusing data-dir helper needs export
- Possibly: `.gitignore` entry for generated `src-tauri/resources/codex-acp-seed/`
- Test: vendor vitest; registry tests; install helper tests (default / override / missing artifact)

**Interfaces:**
- Produces: no `*Conversation interrupted*` agent chunk on interrupt
- Produces: `_meta.codex.activeTurnId` on `turn/started` via `createCodexSessionInfoUpdate`
- Produces: production launches patched package for default pin (see AC3 locked section)
- Consumes: AC3 locked decision above

- [ ] **Step 1: Init submodule**; record baseline commit/version; choose Preferred vs Fallback baseline policy in task report.

- [ ] **Step 2: Write/extend vendor tests** for all four design cases:
  1. interrupted prompt `stopReason = "cancelled"`
  2. emits provider turn id once via `_meta.codex.activeTurnId` on `turn/started`
  3. no `Conversation interrupted` agent chunk
  4. still suppresses interrupt output while session is closing

- [ ] **Step 3: Baseline align (if Preferred) + implement adapter patches**; set `package.json` version to the locked Stop version string.

- [ ] **Step 4: `npm ci` + `npm run build`**; run vendor tests via package.json script (vitest).

- [ ] **Step 5: Submodule commit with explicit source paths** (`package.json`, `src/**`, tests, lockfile). Do **not** depend on committing gitignored `dist/` unless intentionally force-adding; packaging builds dist at stage time.

- [ ] **Step 6: Packaging + managed-prefix resolver + install helper + registry pin**  
  - `tauri.conf.json` resources: seed at `resources/codex-acp-seed/` (exact key Task 2 may adjust only if documented in report)  
  - `beforeBuildCommand` invokes `node src-tauri/scripts/stage-codex-acp.mjs` (create this script) which runs vendor `npm ci && npm run build` and copies seed into the resource path  
  - release.yml **and** Dockerfile: invoke the same stage script then copy `resources/codex-acp-seed`  
  - default-pin uses **managed-prefix shim** via `resolve_codex_acp_command()` (not ambient PATH)  
  - tests listed in AC3 §8

- [ ] **Step 7: Parent scoped commit**

```powershell
cd src-tauri/vendor/codex-acp
npm ci
npm run build
npm test
git add package.json package-lock.json src/CodexAcpServer.ts src/CodexEventHandler.ts
# + only other vendor source/test paths actually edited
git commit -m "feat: activeTurnId session meta; remove Conversation interrupted marker"
cd ../../..
# Mandatory: ignore generated seed
# (edit root or src-tauri .gitignore to include resources/codex-acp-seed/)
git add src-tauri/vendor/codex-acp src-tauri/src/acp/registry.rs src-tauri/src/commands/acp.rs src-tauri/src/acp/connection.rs src-tauri/tauri.conf.json Dockerfile .github/workflows/release.yml src-tauri/scripts/stage-codex-acp.mjs
# + .gitignore when seed ignore added; + paths.rs if touched
cd src-tauri
cargo test --features test-utils registry -- --nocapture
cargo test --features test-utils --lib -- acp
cd ..
git commit -m "feat(codex-acp): ship patched adapter pin with packaged vendor install"
```

---

### Task 3: Rust ACP lifecycle — activeTurnId, user_stop, ready drain, ID lifetime

**Files:**
- Modify: `src-tauri/src/acp/connection.rs`
- Possibly: `session_state.rs` for connection-local `active_provider_turn_id: Option<String>`

**Interfaces:**
- Consumes: Task 1 fields; Task 2 wire `_meta.codex.activeTurnId` (same as `_meta.codex.goal` → `info.meta`)
- Produces: UserCancelled TurnComplete with `termination_source=Some("user_stop")` and snapshot `provider_turn_id`
- **Clear active provider id on every terminal finalization** (UserCancelled, watchdog, ordinary natural-end, SuspensionFailed) and on new-turn reset; **retain across `DelegationSuspended` only**
- Ready drain **before every** `finalize_active_user_cancel` call site (or shared helper that receives session reader) using `ReadyUpdateSource` / `drain_ready_in_prompt_updates` / zero-timeout pattern

- [ ] **Step 1: Failing tests**
  1. UserCancelled sets user_stop + forwards id
  2. Watchdog cancel: no user_stop
  3. Natural-end / ordinary `cancelled`: no user_stop
  4. SuspensionFailed: no user_stop
  5. Idle Cancel: no TurnComplete
  6. Late activeTurnId after finalization ignored
  7. Cancel with ready activeTurnId drain preserves id
  8. End-turn clears id; subsequent Stop without new id does not reuse old id

- [ ] **Step 2: Implement store/accept/clear + drain + UserCancelled emit.**

- [ ] **Step 3: Verify + scoped commit**

```powershell
cd src-tauri
cargo test --features test-utils --lib -- --nocapture
git add src/acp/connection.rs
# + session_state.rs only if touched; run git add from repo root or use paths relative to cwd
git commit -m "feat(acp): typed user_stop TurnComplete with provider turn id lifecycle"
```

Also add test: **DelegationSuspended retains** stored provider turn id.

---

### Task 4: Codex parser — `event_msg.turn_aborted` → `TurnOutcome`

**Files:**
- Modify: `src-tauri/src/parsers/codex.rs` (inline temp-file tests in `#[cfg(test)]`, follow existing temp JSONL pattern ~line 3016+)
- Snapshots: `src-tauri/tests/snapshots/parsers_snapshot__codex_*.snap` only if churned

**Interfaces:**
- Consumes: Task 1 `TurnOutcome`
- Fence only if `reason == "interrupted"` and non-null non-empty `turn_id`
- No fence for `replaced` / `review_ended` / null `turn_id`
- Flush pending reasoning for that turn before attach
- Timing: `completed_at` from enclosing JSONL `timestamp` when present; `duration_ms` from payload **or** rollout-envelope/sibling fields when present

- [ ] **Step 1: Failing tests** — content survival, outcome attach, truncated line, empty assistant, reason filters, null turn_id, no synthetic marker, timestamp/duration extraction.

- [ ] **Step 2: Implement arm** (not text `<turn_aborted>` envelopes).

- [ ] **Step 3: Insta if needed; commit scoped paths**

```powershell
cd src-tauri
$env:INSTA_UPDATE="auto"
cargo test --features test-utils codex -- --nocapture
git add src/parsers/codex.rs
# + ../tests/snapshots/* only if changed; paths relative to src-tauri cwd
git commit -m "feat(parser): recognize codex turn_aborted as interrupt fence"
```

---

### Task 5: Frontend runtime store — coordinator, exclusive path, key lifecycle

**Files:**
- Modify: `src/stores/conversation-runtime-store.ts`
- Test: `src/stores/conversation-runtime-store.test.ts`, `src/stores/viewer-detail-sync.test.ts`, new `src/stores/cancel-reconcile.test.ts` as needed
- Manual Reload API used by Task 6 surfaces: implement `reloadDetail(runtimeId, { reason: "manual_reload" })` here

**Interfaces:**
- Fixed action names table above
- `CancelCompletionKey` + dedicated `cancelGeneration` (bump on new prompt, remove, rebind, backend identity reset, manual reload start, success, retry exhaustion)
- Raw non-dispatching detail fetch (extract internal helper shared with `refetchDetail` path without success reducer)
- While pending: block `syncViewerDetail` / background terminal sync / automatic `refetchDetail` destructive commits
- Manual Reload: `CLEAR_CANCEL_RECONCILE` then authoritative load (may use negative runtime id — resolve via runtime map, not only positive db id)

**Task 5 owns these design FE cases (store-level):** 1–10, 12–15, 16 (`syncTurnMetadata` interaction), 17 (competing cancelGeneration), 18 (key cleanup resumes sync).  
**Not Task 5:** 11 (envelope ordering → Task 6), 19 (adapter cache → Task 7).

- [ ] **Step 1: Write failing store tests** for Task 5 cases above (explicit list in test file names/comments).

- [ ] **Step 2: Implement reducers, coordinator (100/300/1000), merge, lifecycle clears, exclusive path, `reloadDetail`.**

- [ ] **Step 3: Run vitest by path; scoped commit**

```powershell
pnpm test src/stores/conversation-runtime-store.test.ts src/stores/viewer-detail-sync.test.ts src/stores/cancel-reconcile.test.ts
git add src/stores/conversation-runtime-store.ts src/stores/conversation-runtime-store.test.ts src/stores/viewer-detail-sync.test.ts src/stores/cancel-reconcile.test.ts
git commit -m "feat(runtime): abort-fenced cancel reconciliation coordinator"
```

---

### Task 6: Dual-path completion wiring (envelope owns outcome + coordinator)

**Files:**
- Modify: `src/contexts/acp-connections-context.tsx` — **sole** `START_CANCEL_RECONCILE` / `RECORD_TURN_OUTCOME` starter on accepted `turn_complete` with `termination_source === "user_stop"` (prefer existing afterCommit envelope path ~2938+)
- Modify: `src/components/conversations/conversation-session-surface.tsx` — status-edge promotion only; Manual Reload calls `reloadDetail(..., { reason: "manual_reload" })`
- Modify: `src/components/conversations/conversation-detail-panel.tsx` — **verify** background listener cannot double-start; keep promotion/cleanup only (document finding in task report)

**Interfaces:**
- Consumes: Task 1 event fields; Task 5 actions
- `completion_seq = EventEnvelope.seq`
- Preferred order: envelope starts coordinator with pending key; status-edge only promotes
- Owns design FE case **11** (status-edge then late envelope and reverse)

- [ ] **Step 1: Failing tests** for both orderings + single coordinator.

- [ ] **Step 2: Implement envelope acceptance; status-edge remains promotion-only; wire Manual Reload.**

- [ ] **Step 3: Audit detail-panel listener; prevent double-start.**

- [ ] **Step 4: Verify by test file paths; scoped commit**

```powershell
pnpm test src/contexts src/components/conversations
git add src/contexts/acp-connections-context.tsx src/components/conversations/conversation-session-surface.tsx src/components/conversations/conversation-detail-panel.tsx
# + tests touched
git commit -m "feat(ui): accept typed user_stop turn_complete for cancel reconcile"
```

---

### Task 7: Presentation footer, full outcome cache fingerprint, i18n

**Files:**
- Modify: `src/lib/adapters/ai-elements-adapter.ts` (`adaptMessageTurn`, `TurnCacheEntry` full-outcome fingerprint including `duration_ms` / all fields)
- Modify: `src/components/message/message-list-view.tsx` (footer, non-transparent outcome-only turns)
- Modify: all 10 `src/i18n/messages/*.json` — key `responseInterrupted` distinct from `statusInterrupted`
- Test: adapter + message-list tests — owns design FE case **19** including duration-only updates

- [ ] **Step 1: Failing tests** (footer, copy exclusion, grouping, outcome-only + duration-only cache miss).

- [ ] **Step 2: Implement adapter + list footer + i18n.**

- [ ] **Step 3: Verify + scoped commit**

```powershell
pnpm test src/lib/adapters src/components/message/message-list-view.test.tsx
git add src/lib/adapters/ai-elements-adapter.ts src/components/message/message-list-view.tsx src/i18n/messages
git commit -m "feat(ui): render response-interrupted footer with cache-safe outcome"
```

---

### Task 8: End-to-end verification sweep

**Files:**
- Modify: `src-tauri/scripts/smoke-codex-acp.mjs` (exists; currently hardcodes old identity like `1.1.2-mycodebuddy.3` — update expected pin/package-bin form; keep ACP **initialize** smoke, not mere `--help`)
- Create (optional): thin PowerShell wrapper only if needed for temp-prefix install before invoking the smoke script
- Create: `.superpowers/sdd/task-8-report.md`

- [ ] **Step 1: Rust desktop + server + mcp**

```powershell
cd src-tauri
cargo test --features test-utils --lib
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
cd ..
```

- [ ] **Step 2: Frontend**

```powershell
pnpm test
pnpm exec tsc --noEmit
pnpm eslint .
pnpm build
```

- [ ] **Step 3: AC3 executable verification (exact, throwing)**

```powershell
$ErrorActionPreference = "Stop"
# A) Stage seed the same way desktop/CI does
node src-tauri/scripts/stage-codex-acp.mjs
if ($LASTEXITCODE -ne 0) { throw "stage-codex-acp.mjs failed with exit $LASTEXITCODE" }
if (-not (Test-Path "src-tauri/resources/codex-acp-seed/package.json")) {
  throw "missing desktop seed package.json after stage-codex-acp.mjs"
}
if (-not (Test-Path "src-tauri/resources/codex-acp-seed/dist/index.js")) {
  throw "missing desktop seed dist/index.js after stage-codex-acp.mjs"
}
# B) Production resolver + initialize + install integrity (Task 2 test names)
cd src-tauri
cargo test --features test-utils --lib codex_resolver_initialize_smoke_via_resolve_codex_acp_command -- --nocapture
if ($LASTEXITCODE -ne 0) { throw "resolver initialize smoke failed" }
cargo test --features test-utils --lib codex_managed_install_single_flight_concurrent -- --nocapture
if ($LASTEXITCODE -ne 0) { throw "single-flight install test failed" }
cargo test --features test-utils --lib codex_managed_install_repairs_partial_or_version_mismatch -- --nocapture
if ($LASTEXITCODE -ne 0) { throw "repair install test failed" }
cargo test --features test-utils --lib codex_resolver_prefers_managed_prefix_over_path_public_1_1_7 -- --nocapture
if ($LASTEXITCODE -ne 0) { throw "PATH preference test failed" }
cargo test --features test-utils --lib codex_resolver_survives_restart_with_managed_prefix -- --nocapture
if ($LASTEXITCODE -ne 0) { throw "restart test failed" }
cargo test --features test-utils --lib codex_resolver_codeg_codex_acp_bin_override -- --nocapture
if ($LASTEXITCODE -ne 0) { throw "env override test failed" }
cd ..
# C) Packaging wiring present (must match Task 2 edits)
if (-not (Select-String -Path "src-tauri/tauri.conf.json" -Pattern "codex-acp-seed" -Quiet)) {
  throw "tauri.conf.json missing codex-acp-seed resource mapping"
}
if (-not (Select-String -Path "Dockerfile" -Pattern "codex-acp-seed|stage-codex-acp" -Quiet)) {
  throw "Dockerfile missing codex-acp seed/stage wiring"
}
if (-not (Select-String -Path ".github/workflows/release.yml" -Pattern "stage-codex-acp|codex-acp-seed" -Quiet)) {
  throw "release.yml missing codex-acp seed/stage wiring"
}
# D) JS smoke script pin + execute initialize smoke via production-resolver path from cargo test in B
# Also update and run JS smoke if Task 2 left a print helper; otherwise B is sufficient for AC3.
$pin = (Get-Content src-tauri/vendor/codex-acp/package.json -Raw | ConvertFrom-Json).version
if (-not (Select-String -Path "src-tauri/scripts/smoke-codex-acp.mjs" -Pattern [regex]::Escape($pin) -Quiet)) {
  throw "smoke-codex-acp.mjs missing exact locked pin $pin"
}
```

Expected: stage succeeds; all cargo tests exit 0; layout wiring greps hit; smoke script expects exact vendor package.json version.

- [ ] **Step 4: Write** `.superpowers/sdd/task-8-report.md` with full command lines + results; scoped commit if needed.

---

## Spec Coverage Checklist

| Design requirement | Task |
| --- | --- |
| Typed `TurnComplete` user_stop + provider id | 1, 3 |
| Provider id clear on all terminals (not only UserCancelled) | 3 |
| Adapter marker removal + activeTurnId | 2 |
| Production pin + vendor install (AC3) | 2, 8 |
| Ready drain on every user-cancel path | 3 |
| Non-user cancel emit matrix | 3 |
| Parser turn_aborted fence + reason/null/timing | 4 |
| Dual-path completion | 5, 6 |
| Raw detail + RECONCILE only | 5 |
| Exclusive destructive path + full key lifecycle | 5 |
| Manual Reload vs automatic refetch | 5, 6 |
| Current-turn / outcome-only boundary | 5, 7 |
| Presentation + full outcome fingerprint | 7 |
| FE cases 1–19 | 5 (most), 6 (#11), 7 (#19) |
| No auto-resume | global + 5/6 |

## Dependency Order

```text
1 → 2 → 3 → 5 → 6 → 7 → 8
1 → 4 → 5
2 → 8   (AC3 smoke depends on Task 2 packaging + pin)
4 may run after 1 in parallel with 2 only if workers do not touch the same files; default serial order remains 1,2,3,4,5,6,7,8
```

**Hard edges:** Task 2 completes before Task 3 (Task 2 owns default-pin launch preference in `commands/acp.rs`; Task 3 owns cancel lifecycle in `connection.rs`).

## Plan Review Disposition

- Round 1: GLM5.2 / KimiK3 APPROVE_WITH_MINORS; Grok / Codex REQUEST_CHANGES.
- Round 2: KimiK3 / Grok APPROVE_WITH_MINORS; Codex REQUEST_CHANGES (AC3 baseline + packaging).
- Round 3 plan edit: AC3 baseline policy, packaging, discovery, custom-version rules.
- Round 4 plan edit: public-1.1.7 migration/launch-identity, clean-checkout dist stage, executable Task 8 smoke, release.yml **and** Dockerfile.
- Round 5 plan edit: Option A; smoke script ownership; `pnpm build`.
- Round 6–9 plan edit: hard `2→3`; managed npm prefix as runnable launch mechanism; beforeBuildCommand stage; initialize via production resolver; path/cwd staging fixes.
