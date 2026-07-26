# User Stop Transcript Reconciliation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close remaining HEAD gaps so user Stop preserves live Codex content, attaches typed interruption metadata, and abort-fence reconciles under the Round 4e design contract (soft fence, `owner_preserve`, Branch A/B, no-bump migration, AC3 managed pin).

**Architecture:** HEAD already lands shared types, Rust `user_stop`/`activeTurnId`, parser fence arm, basic FE coordinator, presentation fingerprint, and i18n. This plan **amends** incomplete semantics and **re-lands** the retired AC3 managed-prefix path. Do not re-land RETAIN types/lifecycle.

**Tech Stack:** Rust (ACP, parser, registry), TypeScript/React (runtime store, ACP context), vendored `src-tauri/vendor/codex-acp`, vitest + cargo.

**Design baseline:** `docs/superpowers/specs/2026-07-24-user-stop-transcript-reconciliation-design.md` (Round 4e approved)

**Worktree:** `D:\MyCodeBuddy\.worktrees\b2d-user-stop-transcript-reconciliation`  
**Branch:** `feature/b2d-user-stop-transcript-reconciliation`  
**Reviewed baseline SHA:** `4e23b90542d4366a293f22c138792a40a196e071` (main tip at worktree create)

## Global Constraints

- User Stop never resumes/retries/resends the interrupted prompt.
- Ordinary `end_turn` keeps no-refetch path; idle Cancel emits no `TurnComplete` / no soft fence arm.
- Only `UserCancelled` sets `termination_source = "user_stop"`.
- Coordinator never applies via `FETCH_DETAIL_SUCCESS`; only `RECONCILE_CANCELLED_TURN` after fence match.
- Recovery is **best-effort under append-order**.
- `cancelDestructiveSuppress = softFenceActive OR pendingCancel OR ownerPreserve`.
- AC3 pin: **`1.1.7-mycodebuddy.stop1`** only; if 1.1.7 rebase blocked → **stop and report** (no silent 1.1.2).
- PowerShell-friendly; **never `git add -A`**; local commits only for parent repo.
- Parent session does not implement; Grok implementer + Codex reviewer per Task under SDD.
- Live outcome-only id: **RETAIN** tree spelling `cancel-outcome:${connectionId}:${completionSeq}` (design `outcome-…` spelling is non-normative for this amend; do not rename).
- Commit approved design/plan docs before implementation commits (or include them in first doc commit).

## Dependency DAG (normative)

```text
Task 0 (AC3 preflight / reachable gitlink)  ──┬──► Task 5a (vendor) ──► Task 5b (parent pin/resolver) ──► Task 5c (packaging/smoke)
Task 1 (parser)  ─────────────────────────────┤
Task 2 (suppress SM) ──► Task 3 (Branch A/B) ──► Task 4 (migration + envelope)
                                                    │
Task 6 (presentation RETAIN audit) ◄───────────────┘  (after 1–4; may run // 5*)
Task 7 (full verification) ◄── all of the above
```

- **Serial on shared FE files:** Task 2 → 3 → 4 only.
- **Parallel lanes:** {Task 1} ∥ {Task 2→3→4} ∥ {Task 0→5a→5b→5c} once Task 0 passes.
- Task 0 failure **blocks only AC3 lane**; Tasks 1–4 may proceed if user accepts AC3 deferred — default is **do not claim delivery complete without AC3**.

## Fixed symbols

| Symbol | Role |
| --- | --- |
| `cancelDestructiveSuppress(session)` | softFence OR pendingCancel OR ownerPreserve |
| `noteUserStopTurnOwnership(runtimeId)` | Stop-time ownership + soft fence enter (same call path as HEAD ~context L7476) |
| `ownerPreserve` | Durable suppress |
| Branch A / Branch B | reconcile merge |
| `resolve_codex_acp_command()` | Production Codex launch resolver (ADD; exact name) |
| Seed path | `src-tauri/resources/codex-acp-seed/` |
| Stage script | `src-tauri/scripts/stage-codex-acp.mjs` (un-retire; wire into beforeBuild) |

## File Map

| Area | Files |
| --- | --- |
| Parser | `src-tauri/src/parsers/codex.rs` |
| Runtime | `src/stores/conversation-runtime-store.ts`, `src/stores/cancel-reconcile.test.ts` |
| Envelope / dual-path | `src/contexts/acp-connections-context.tsx`, `src/contexts/user-stop-dual-path.test.ts`, `src/components/conversations/conversation-session-surface.tsx`, spot-check `conversation-detail-panel.tsx` |
| Presentation | `src/lib/adapters/ai-elements-adapter.ts`, `src/components/message/message-list-view.tsx`, `src/i18n/messages/*.json` |
| AC3 vendor | `src-tauri/vendor/codex-acp/**` (submodule) |
| AC3 parent | `src-tauri/src/acp/registry.rs`, new/restore `src-tauri/src/acp/codex_acp_runtime.rs` (or equivalent single module owning managed install), `src-tauri/src/commands/acp.rs` / launch sites, `src-tauri/tauri.conf.json`, `src-tauri/scripts/stage-codex-acp.mjs`, `src-tauri/scripts/smoke-codex-acp.mjs`, `docs/releasing/bundled-codex-acp.md`, Dockerfile / release.yml as needed |

---

### Task 0: AC3 preflight — reachable vendor gitlink + publish authority

**Files:** none (investigation + report). Optional: doc-only notes in `.superpowers/sdd/task-0-ac3-preflight.md`

**Interfaces:** Produces go/no-go for Task 5a.

- [ ] **Step 1: Record baseline**

```powershell
git rev-parse HEAD
git status -sb
git submodule status src-tauri/vendor/codex-acp
```

Expected baseline: `4e23b905…` or descendant with only design/plan docs dirty.

- [ ] **Step 2: Probe remote without relying on parent-dir git discovery**

```powershell
# Prefer direct remote object probe (does not need initialized submodule):
git ls-remote https://github.com/icannotwait/codex-acp.git HEAD
git ls-remote https://github.com/icannotwait/codex-acp.git "refs/heads/*"

# Temp bare clone to test object reachability of parent gitlink SHA:
$tmp = Join-Path $env:TEMP "codex-acp-reach-$([guid]::NewGuid().ToString('n'))"
git clone --bare https://github.com/icannotwait/codex-acp.git $tmp
git -C $tmp cat-file -t 841b03525f85c931ac079ad7d3dea60521ddc60c 2>&1
# cleanup: Remove-Item -Recurse -Force $tmp

# Only after object exists: initialize real submodule
git submodule update --init src-tauri/vendor/codex-acp
git -C src-tauri/vendor/codex-acp remote -v   # must be codex-acp remote, not MyCodeBuddy
```

Do **not** use `git -C src-tauri/vendor/codex-acp …` while the directory is an
empty gitlink placeholder (Git may walk up into the parent repo and hit
MyCodeBuddy origin).

- [ ] **Step 3: Decide path**
  - If SHA unreachable: plan **repair** — writable remote + publish authority for `1.1.7-mycodebuddy.stop1`; parent gitlink only after a **named branch** on the remote advertises the new SHA (`git ls-remote <url> refs/heads/<branch>` equals local commit).
  - If reachable: init submodule and proceed to Task 5a after Tasks 1–4 as DAG allows.
  - Write `.superpowers/sdd/task-0-ac3-preflight.md` with evidence.

- [ ] **Step 4: Hard rule** — Task 5a/5b/5c must not mark AC3 complete with a parent-only local submodule commit that clean checkout cannot fetch.

---

### Task 1: Parser — display-only null id + post-abort residual fixture

**Files:**
- Modify: `src-tauri/src/parsers/codex.rs`

**Interfaces:**
- null/empty `turn_id` + `interrupted` → display-only outcome (no `provider_turn_id`)
- non-empty id → matchable fence (RETAIN)
- post-abort in-scope record fixture documents v1 first-match residual (may miss post-abort content)

- [ ] **Step 1: Failing tests**
  1. Rewrite null/empty id test: outcome **present**, `provider_turn_id` absent.
  2. **Two-phase residual fixture** (deterministic contract, not “may drop”):
     - Phase A fixture: full pre-abort content + matching `turn_aborted` only.
       Parse → assert matchable fence present; assert specific post-abort marker
       content **absent**.
     - Phase B: same file with one additional in-scope agent_message after abort.
       Parse → assert that later content **is present**.
     - Document in test: v1 coordinator first-match apply uses Phase A snapshot;
       design does **not** auto re-apply after first accepted reconcile. (If a
       dedicated FE first-apply test is clearer for the residual, put Phase A
       “authorize apply without later content” in Task 3/4 runtime tests; keep
       both parser snapshots deterministic here.)

- [ ] **Step 2: Run**

```powershell
cd src-tauri
cargo test --features test-utils turn_aborted_null -- --nocapture
cargo test --features test-utils turn_aborted_two_phase -- --nocapture
```

- [ ] **Step 3: Implement** display-only branch; implement two-phase tests with **hard asserts** for each phase.

- [ ] **Step 4: Full turn_aborted suite**

```powershell
cargo test --features test-utils turn_aborted -- --nocapture
```

- [ ] **Step 5: Commit** `src-tauri/src/parsers/codex.rs` (+ snaps if any)

---

### Task 2: Runtime — soft fence, `owner_preserve`, `cancelDestructiveSuppress`

**Files:**
- Modify: `src/stores/conversation-runtime-store.ts`, `src/stores/cancel-reconcile.test.ts`
- Consumes: nothing from Task 1
- Produces: suppress predicate + states for Tasks 3–4

**Interfaces:**
- Extend `noteUserStopTurnOwnership` (or same Stop call path) to arm **soft fence** only when cancel targets an **active prompt** (idle Cancel must not arm).
- Soft-fence age-out **30s** → `ownerPreserve` (still suppresses).
- `cancelDestructiveSuppress` used at **all** automatic destructive commit sites (replace `sessionHasPendingCancel` alone): store fetch apply, viewer sync, delegate terminal sync.
- Explicit clear of `ownerPreserve`: new prompt, Manual Reload, session remove, identity reset.

- [ ] **Step 1: Failing tests**
  1. Soft fence on Stop ownership; destructive no-op.
  2. Idle Cancel does **not** arm soft fence.
  3. 30s age-out → ownerPreserve; still suppressed.
  4. `user_stop` without `provider_turn_id` (simulate via store API or test helper): outcome recorded path + ownerPreserve; no pending coordinator key.
  5. pendingCancel still suppresses (regression).
  6. Manual Reload / new prompt / remove restore eligibility.
  7. Retry exhaustion clears pending key, **keeps** ownerPreserve.

- [ ] **Step 2–4: TDD implement + vitest + eslint on touched files**

```powershell
pnpm exec vitest run src/stores/cancel-reconcile.test.ts
pnpm exec eslint src/stores/conversation-runtime-store.ts src/stores/cancel-reconcile.test.ts
```

- [ ] **Step 5: Commit**

---

### Task 3: Runtime — Branch A/B reconcile

**Files:** same store + cancel-reconcile tests  
**Consumes:** Task 2 suppress states  
**Produces:** Branch A/B semantics for Task 4

- Branch A: detail cancelled-turn non-empty OR both empty → replace detail, clear overlays, clear suppress.
- Branch B: fence match + detail empty + local non-empty → skip detail install, keep overlays, clear pending/timers, **keep ownerPreserve**.
- Empty = no non-empty text/thinking/tool blocks (outcome-only ≠ content).
- **Plan-lock generation:** Branch A success need not bump `cancelGeneration` if suppress is fully cleared; exhaustion must not clear `ownerPreserve`.

- [ ] **Step 1: Failing tests** Branch A non-dup replace; Branch B retain + post-apply automatic destructive still suppressed; thinking/tool-only non-empty classification.

- [ ] **Step 2–4: Implement + pass tests**

- [ ] **Step 5: Commit**

---

### Task 4: Migration no-bump, unbound id, dual-path envelope/surface

**Files:**
- Modify: `src/stores/conversation-runtime-store.ts`, `src/stores/cancel-reconcile.test.ts`
- Modify: `src/contexts/acp-connections-context.tsx`, `src/contexts/user-stop-dual-path.test.ts`
- Modify: `src/components/conversations/conversation-session-surface.tsx`
- Spot-check: `conversation-detail-panel.tsx` (must not double-start coordinator)

**Consumes:** Tasks 2–3  
**Produces:** final runtime+envelope behavior for Task 6/7

**HEAD invert (normative):**
- Today `MIGRATE_CONVERSATION` sets `pendingCancel: null` and bumps generations so late envelopes are **stale**.
- Required: runtime-key migration **migrates** pendingCancel (rewrite `runtimeConversationId`), soft fence, ownerPreserve, ownership, timers, **`recordedTurnOutcomeKeys`** (both ids), and **does not bump** `cancelGeneration` (move counter value).
- **Invert** existing tests that expect post-migrate stale fence / cleared pending.
- Identity replacement / true rebind still bumps + clears.

- [ ] **Step 1: Failing tests**
  1. migrate: same cancelGeneration; pending rewritten not null; soft/owner/timers migrated; **recordedTurnOutcomeKeys** migrated; duplicate envelope after migrate does **not** second footer.
  2. In-flight deferred reconcile applies against **post-migration** identity (no gen bump).
  3. Identity replacement: bump + clear suppress + cancel coordinator.
  4. Unbound detail id (`<=0`): outcome, no coordinator, ownerPreserve.
  5. Late envelope after 30s age-out still current → may start coordinator; stale gen no-ops.
  6. Status-edge / viewer / delegate destructive under suppress no-ops.
  7. Panel does not start coordinator for open owner tabs (spot assert).

- [ ] **Step 2–4: Implement +**

```powershell
pnpm exec vitest run src/stores/cancel-reconcile.test.ts src/contexts/user-stop-dual-path.test.ts
```

- [ ] **Step 5: Commit**

---

### Task 5a: Vendor Stop patches + publish reachable gitlink

**Depends:** Task 0 go  
**Files:** `src-tauri/vendor/codex-acp/**` (submodule only)

- [ ] **Step 1: Init submodule on reachable baseline; rebase/merge to 1.1.7 lineage** → package version **`1.1.7-mycodebuddy.stop1`**. If rebase blocked: **stop, write report, do not ship 1.1.2 silently**.

- [ ] **Step 2: Vendor tests (all four design items)**
  1. interrupted prompt `stopReason === "cancelled"`
  2. emits `_meta.codex.activeTurnId` exactly once per turn start
  3. no `Conversation interrupted` agent message chunk
  4. closing-session interruption output still suppressed

- [ ] **Step 3: Implement patches + `npm ci` + `npm test` + `npm run build`**

- [ ] **Step 4: Commit inside submodule first** (explicit paths), note local SHA.

```powershell
cd src-tauri/vendor/codex-acp
git status
git add <explicit paths>
git commit -m "fix(codex-acp): stop marker removal and activeTurnId for 1.1.7-mycodebuddy.stop1"
$sha = git rev-parse HEAD
```

- [ ] **Step 5: Publish named branch** then prove reachability:

```powershell
$branch = "codex/codeg-stop1"   # or agreed branch
$remoteUrl = git remote get-url origin
if (-not $remoteUrl) { throw "submodule origin URL missing" }
git push origin "HEAD:refs/heads/$branch"
$advertised = (git ls-remote $remoteUrl "refs/heads/$branch").Split("`t")[0]
if ($advertised -ne $sha) { throw "advertised $advertised != local $sha" }
$tmp = Join-Path $env:TEMP "codex-acp-clean-$([guid]::NewGuid().ToString('n'))"
try {
  git clone --branch $branch --single-branch $remoteUrl $tmp
  $clean = git -C $tmp rev-parse HEAD
  if ($clean -ne $sha) { throw "clean clone $clean != local $sha" }
} finally {
  if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
}
```

- [ ] **Step 6: Parent records gitlink only after Step 5 succeeds.**

---

### Task 5b: Parent pin + `resolve_codex_acp_command` + managed install

**Depends:** Task 5a  
**Files:**
- `src-tauri/src/acp/registry.rs` → pin `1.1.7-mycodebuddy.stop1` / package coordinate matching managed install
- ADD/restore: `src-tauri/src/acp/codex_acp_runtime.rs` (managed prefix under app data dir via `paths::resolve_effective_data_dir` pattern)
- Launch site: Codex path uses `resolve_codex_acp_command()` (exact name)
- Tests (run **each** by name):
  - `codex_resolver_prefers_managed_prefix_over_path_public_1_1_7`
  - `codex_resolver_survives_restart_with_managed_prefix`
  - `codex_resolver_initialize_smoke_via_resolve_codex_acp_command`
  - `codex_managed_install_single_flight_concurrent`
  - `codex_managed_install_repairs_partial_or_version_mismatch`
  - `codex_resolver_codeg_codex_acp_bin_override`

- [ ] **Step 1: Failing cargo tests** (scaffold names)

- [ ] **Step 2: Implement managed prefix + single-flight install + integrity check**

- [ ] **Step 3: Registry pin + launch wiring**

- [ ] **Step 4: Run all six tests by name + lib check**

```powershell
cd src-tauri
cargo test --features test-utils codex_resolver_prefers_managed_prefix_over_path_public_1_1_7 -- --nocapture
cargo test --features test-utils codex_resolver_survives_restart_with_managed_prefix -- --nocapture
cargo test --features test-utils codex_resolver_initialize_smoke_via_resolve_codex_acp_command -- --nocapture
cargo test --features test-utils codex_managed_install_single_flight_concurrent -- --nocapture
cargo test --features test-utils codex_managed_install_repairs_partial_or_version_mismatch -- --nocapture
cargo test --features test-utils codex_resolver_codeg_codex_acp_bin_override -- --nocapture
```

- [ ] **Step 5: Commit** explicit parent paths + gitlink if not committed in 5a

---

### Task 5c: Packaging hooks, seed stage, smoke, release docs

**Depends:** Task 5b  
**Files:**
- Un-retire / rewire: **`src-tauri/scripts/stage-codex-acp.mjs`** into `package.json` tauri beforeBuild / fast-build as applicable
- `src-tauri/tauri.conf.json` beforeBuildCommand chain
- Seed: `src-tauri/resources/codex-acp-seed/`
- `src-tauri/scripts/smoke-codex-acp.mjs` — expected version/string from Task 5a build `--version` observation; CLI still takes the binary path
- `docs/releasing/bundled-codex-acp.md` — reverse “forbid seed” guidance to match LOCKED design
- Dockerfile / `.github` release jobs: stage seed before copy
- **Always** update `src-tauri/resources/THIRD_PARTY_LICENSES.txt` (and any version table) for the custom pin

**Locked smoke path:** production helper prints absolute shim → smoke CLI consumes it. Do not leave `$bin` as free text.

- [ ] **Step 1: Stage + fail-fast seed asserts**

```powershell
node src-tauri/scripts/stage-codex-acp.mjs
$seedPkg = "src-tauri/resources/codex-acp-seed/package.json"
if (-not (Test-Path -LiteralPath $seedPkg)) { throw "missing seed package.json" }
$ver = (Get-Content $seedPkg -Raw | ConvertFrom-Json).version
if ($ver -ne "1.1.7-mycodebuddy.stop1") { throw "seed version $ver != 1.1.7-mycodebuddy.stop1" }
$entry = "src-tauri/resources/codex-acp-seed/dist/index.js"
if (-not (Test-Path -LiteralPath $entry)) { throw "missing seed dist/index.js" }
```

- [ ] **Step 2: Resolve shim via production resolver then smoke**

```powershell
# Prefer a small test bin or `cargo test` helper that prints the absolute path
# returned by resolve_codex_acp_command() after managed install from seed.
# Example (implement helper in Task 5b if missing):
$bin = cargo run --quiet --features test-utils --bin print-codex-acp-command 2>$null
# OR capture from a focused cargo test that writes the path to a temp file.
if (-not $bin) { throw "resolve_codex_acp_command produced empty path" }
$bin = $bin.Trim()
if (-not (Test-Path -LiteralPath $bin)) { throw "shim missing: $bin" }
node src-tauri/scripts/smoke-codex-acp.mjs $bin
# smoke must assert version identity for 1.1.7-mycodebuddy.stop1
```

- [ ] **Step 3: Fail-fast packaging wiring checks**

```powershell
$tc = Get-Content src-tauri/tauri.conf.json -Raw
if ($tc -notmatch "stage-codex-acp") { throw "tauri.conf.json missing stage-codex-acp hook" }
$pkg = Get-Content package.json -Raw
if ($pkg -notmatch "stage-codex-acp") { throw "package.json missing stage-codex-acp wiring" }
$df = Get-Content Dockerfile -Raw
if ($df -notmatch "codex-acp-seed") { throw "Dockerfile missing codex-acp-seed" }
$rel = Get-ChildItem -Recurse .github -Filter "*.yml" -ErrorAction SilentlyContinue | Select-String -Pattern "codex-acp-seed|stage-codex-acp" -List
if (-not $rel) { throw "release workflow missing seed/stage wiring" }
$lic = Get-Content src-tauri/resources/THIRD_PARTY_LICENSES.txt -Raw
if ($lic -notmatch "1\.1\.7-mycodebuddy\.stop1") { throw "THIRD_PARTY_LICENSES missing locked pin" }
```

- [ ] **Step 4: Commit** packaging + docs + license paths only

---

### Task 6: Presentation RETAIN audit

**Depends:** Tasks 1–4 preferred (stable outcome shape)

- [ ] **Step 1: Evidence report** — fingerprint 6 fields; FE19 duration-only cache; all 10 locales `responseInterrupted`; outcome-only grouping; copy exclusion. Paths + greps in `.superpowers/sdd/task-6-presentation-retain.md`.

- [ ] **Step 2: Only if gap found, TDD fix**

```powershell
pnpm exec vitest run src/lib/adapters/ai-elements-adapter.test.ts
```

Do **not** use `--passWithNoTests` as a green pass for missing suites.

- [ ] **Step 3: Commit only if code changed**

---

### Task 7: Full verification sweep

**Depends:** all tasks claimed complete for delivery

- [ ] **Frontend**

```powershell
pnpm eslint .
pnpm test
pnpm build
```

- [ ] **Desktop Rust**

```powershell
cd src-tauri
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
```

- [ ] **Server**

```powershell
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
```

- [ ] **codeg-mcp**

```powershell
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

- [ ] **Vendor** (if Task 5a done): `npm test` + `npm run build` in vendor

- [ ] **AC3 smoke** — copy-paste re-run Task 5c Steps 1–3 PowerShell blocks exactly (throw-on-fail); do not paraphrase

- [ ] **RETAIN spot-check** existing Rust user_stop / drain / watchdog negatives:

```powershell
cargo test --features test-utils --lib user_stop -- --nocapture
```

- [ ] Write `.superpowers/sdd/user-stop-b2d-verification.md` with outcomes. Fix only plan-owned failures.

---

## Spec coverage checklist

| Design requirement | Task |
| --- | --- |
| Display-only null abort id | Task 1 |
| Post-abort first-match residual fixture | Task 1 |
| Soft fence + owner_preserve + cancelDestructiveSuppress | Task 2 |
| user_stop without provider id → owner_preserve | Task 2 |
| Branch A/B | Task 3 |
| Migration no-bump + recordedTurnOutcomeKeys + late envelope | Task 4 |
| Unbound detail id | Task 4 |
| Dual-path / surface / panel | Task 4 |
| AC3 reachable publish | Task 0 + 5a |
| Managed pin + resolver tests | Task 5b |
| Packaging / smoke | Task 5c |
| Presentation RETAIN | Task 6 |
| Full AGENTS verification | Task 7 |

## Placeholder scan

No TBD. Task 0/5a may **block and report** on unreachable remote / blocked rebase — explicit hard gates.

## Execution

After plan review Critical/Important = 0, run **subagent-driven-development** in this worktree with the DAG above. Final global Codex review after all Tasks.
