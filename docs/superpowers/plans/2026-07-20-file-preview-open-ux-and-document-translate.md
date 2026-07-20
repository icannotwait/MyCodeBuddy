# File Preview Open UX and Document Translation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cold-open failures toast without error tabs; maximize files pane only after successful open; Escape closes the active file tab; on-demand Markdown/plain-text translation via the auto-title agent into a transient tab with exclusive Save as….

**Architecture:** Frontend load settle paths gain required `hasLoadedSuccessfully` and a full cold/warm failure matrix. Backend adds `ConnectionPurpose::InternalTranslate` under `is_hidden_generation()` across manager **and** connection, a parallel `DocumentTranslateRunner` in a reserved directory, fail-closed Markdown protection, process-wide `Arc<DocumentTranslationService>`, `translate_document` + `save_translation_as`, and FE toolbar/transient-tab/Save-as UI.

**Spec:** `docs/superpowers/specs/2026-07-20-file-preview-open-ux-and-document-translate-design.md`  
**Plan revision:** Codex plan review (NEEDS_REWORK) fixes applied — full matrix, Escape overlays, AppState wiring, protect tests, Save-as contract, camelCase transport, all-10 i18n parity per task.

**Tech Stack:** Next.js/React 19, TypeScript, Vitest, Tauri 2 / Axum, Rust 2021, SeaORM SQLite, `auto_title` hidden runner patterns, sonner, next-intl (10 locales).

## Global Constraints

- Input max: **24_000** Unicode scalars (backend authoritative).
- Output max: **96_000** UTF-8 bytes.
- Backend deadline: **120s**; FE `timeoutMs`: **195_000**.
- In-flight translate capacity: **1** (busy reject, no queue).
- Extensions: **`.md`**, **`.markdown`**, **`.txt`** only (no `.mdx`).
- Translate cwd: **`registry.reserved_root()` only**.
- Save as: **`folder_id` + relative path**, exclusive create, **never overwrite**.
- Placeholder integrity: **fail-closed**.
- Maximize: first **successful settle** of creating gen while tab still active; **user-initiated** only (`maximizeOnSuccess: true`).
- Escape: fixed; `defaultPrevented` + overlay focus guard; dirty uses `closeFileTab` confirm.
- Agent: **`auto_title_agent` only**.
- Do not change auto-title job coordinator semantics.
- Parallel runner — no translate branches on `TitleAgentRunner` trait.
- i18n: **all 10 catalogs** whenever a key is introduced (parity test).
- Wire JSON: **camelCase** flat bodies (project convention).
- TDD + commit per task.
- One process-wide `Arc<DocumentTranslationService>` shared by Tauri + Axum AppState.

## File map

| Path | Role |
| --- | --- |
| `src/contexts/workspace-context.tsx` | Tabs, failure matrix, maximize, transient tabs, open settle result |
| `src/contexts/workspace-context.test.tsx` | Open UX tests |
| `src/components/files/file-workspace-tab-bar.tsx` | Escape + Translate + Save as buttons |
| `src/components/files/file-workspace-tab-bar.test.tsx` | Keyboard / button tests |
| `src/lib/document-translate.ts` | Eligibility, djb2 hash, locale wire, constants |
| `src/lib/document-translate.test.ts` | Pure FE tests |
| `src/lib/api.ts` + `src/lib/api.test.ts` | API + timeoutMs 195000 |
| `src-tauri/src/auto_title/types.rs` | Purpose + `is_hidden_generation` |
| `src-tauri/src/auto_title/internal_sessions.rs` | Purpose filter |
| `src-tauri/src/db/entities/internal_agent_session.rs` | SeaORM purpose enum |
| `src-tauri/src/db/migration/m20260720_000001_internal_session_translate_purpose.rs` | Table rebuild CHECK |
| `src-tauri/src/acp/manager.rs` | Hidden policy |
| `src-tauri/src/acp/connection.rs` | Hidden policy (3 sites) |
| `src-tauri/src/document_translate/*` | protect, runner, service, types |
| `src-tauri/src/commands/document_translate.rs` | Cores |
| `src-tauri/src/web/handlers/document_translate.rs` | HTTP |
| `src-tauri/src/app_state.rs`, `lib.rs`, `web/*`, `bin/codeg_server.rs` | Wire service Arc |
| `src/i18n/messages/*.json` | All 10 |
| `src/components/settings/conversation-experience-settings.tsx` | Disclosure |

---

### Task 1: Failure matrix, maximize-on-success, open settle result

**Files:**
- Modify: `src/contexts/workspace-context.tsx`
- Modify: `src/contexts/workspace-context.test.tsx`
- Modify: **all 10** `src/i18n/messages/*.json` — add `unableOpenFile` under the same namespace used by workspace file strings (`Folder.fileWorkspace` or existing `unableLoadContent` parent — match `unableLoadContent` location)

**Interfaces:**
- Produces on `FileWorkspaceTab`:
  - `hasLoadedSuccessfully: boolean` **required**, default `false` on every seed/loading constructor
- Produces:

```ts
type OpenFileOptions = {
  line?: number
  reload?: boolean
  folderId?: number
  /** default true for openFilePreview; false for office auto-preview */
  maximizeOnSuccess?: boolean
}

// openFilePreview returns settle outcome for Save-as chaining (Task 7)
type OpenFileSettleResult =
  | { ok: true; tabId: string }
  | { ok: false; reason: "resolve" | "load" | "closed" | "stale" }

openFilePreview(...): Promise<OpenFileSettleResult>
```

- Produces internal helpers (names may vary but must exist):
  - `removeFileTabId(tabId)` — fixes active id like `closeFileTab` without dirty confirm
  - `pendingMaximizeOnSuccessRef: Set<string>`
  - `activeFileTabIdRef` kept in sync every active-id change
- **Unchanged:** `rejectFileTab` watcher deletion body
- **Office auto-preview** calls `openFilePreview(abs, { maximizeOnSuccess: false })`

**Failure matrix (implement all rows):**

| Site | Behavior |
| --- | --- |
| `openFilePreview` cold fail | remove tab + toast; clear pending maximize |
| `openFilePreview` warm fail | keep content, `loading=false`, toast |
| Pre-seed resolve/split fail | toast only, no tab, return `{ok:false,reason:"resolve"}` |
| Stale gen | silent, return `{ok:false,reason:"stale"}` |
| Diff/rich-diff cold fail | remove tab + toast |
| `reloadOpenFileBackground` fail, tab open + hasLoaded | warm toast keep content (no rejectTab error body) |
| `reloadOpenFileBackground` tab closed | no-op |
| `rejectFileTab` | unchanged |

**Maximize:** on successful settle, if tabId in pending set AND `activeFileTabIdRef.current === tabId`, set maximized and delete from pending. Never maximize at seed. Existing-tab activate: no pending entry.

- [ ] **Step 1: Add i18n key to all 10 catalogs** (parity)

```json
"unableOpenFile": "Could not open {name}"
```

(zh-CN: `"无法打开 {name}"`, etc. for remaining locales)

- [ ] **Step 2: Write failing tests** (real assertions, mock `readFileForEdit` / toast)

Use existing test harness patterns in `workspace-context.test.tsx`. Required cases:

1. `cold open failure removes the tab and does not leave saveState error`
2. `warm reload failure keeps prior content`
3. `pre-seed resolve failure toasts without creating a tab` (force resolve to null/throw)
4. `maximize true only after successful settle of new tab`
5. `failed cold open with pre-existing other tab does not steal maximize incorrectly` (other tab open, maximize false, cold fail new file → maximize still false)
6. `auto office open path does not maximize` if testable via maximizeOnSuccess false
7. `rejectFileTab still writes error body` (regression)
8. `openFilePreview returns ok true after settle`

- [ ] **Step 3: Run**

```bash
pnpm exec vitest run src/contexts/workspace-context.test.tsx src/i18n/messages.test.ts
```

Expected: new tests FAIL; messages parity PASS after keys added.

- [ ] **Step 4: Implement matrix + required flag + pending maximize + activeFileTabIdRef + return type**

- [ ] **Step 5: Re-run same command — all PASS**

- [ ] **Step 6: Commit**

```bash
git add src/contexts/workspace-context.tsx src/contexts/workspace-context.test.tsx src/i18n/messages
git commit -m "fix(files): cold-open toast matrix and maximize-on-success settle"
```

---

### Task 2: Escape closes file tab with overlay precedence

**Files:**
- Modify: `src/components/files/file-workspace-tab-bar.tsx`
- Create/Modify: `src/components/files/file-workspace-tab-bar.test.tsx`

**Interfaces:**
- Helper (export for tests):

```ts
export function shouldHandleFilesEscape(event: KeyboardEvent, ctx: {
  mode: string
  activePane: string
  filesMaximized: boolean
  activeFileTabId: string | null
}): boolean
```

Logic:

1. `event.key === "Escape"`
2. not `event.defaultPrevented`
3. fusion && (files pane || maximized)
4. `activeFileTabId` present
5. Overlay guard: if `document` has open `[role="dialog"][data-state="open"]`, `[role="alertdialog"]`, or focus inside `[data-radix-popper-content-wrapper]` / menu content, return false
6. else true → caller `preventDefault` + `closeFileTab`

- [ ] **Step 1: Unit tests for `shouldHandleFilesEscape`** + integration:

- Escape → closeFileTab called
- defaultPrevented → not called
- open dialog in document → not called
- wrong pane → not called
- dirty tab: mock closeFileTab that records confirm path (spy) — Escape still **calls** closeFileTab (confirm is inside closeFileTab)

- [ ] **Step 2: FAIL then implement**

- [ ] **Step 3: PASS + commit**

```bash
git commit -m "feat(files): Escape closes file tab with overlay guard"
```

---

### Task 3: InternalTranslate + is_hidden_generation everywhere

**Files:**
- `src-tauri/src/auto_title/types.rs`
- `src-tauri/src/auto_title/internal_sessions.rs`
- `src-tauri/src/db/entities/internal_agent_session.rs`
- `src-tauri/src/db/migration/m20260720_000001_internal_session_translate_purpose.rs` + registrar
- `src-tauri/src/acp/manager.rs`
- `src-tauri/src/acp/connection.rs` (all InternalTitle purpose checks that are generation policy)

**Migration (exact):** SQLite table rebuild:

1. CREATE new table with CHECK `(purpose IN ('title','translate'))`
2. INSERT SELECT from old (existing title rows preserved)
3. DROP old; RENAME new
4. down: rebuild with title-only CHECK (translate rows deleted or blocked)

**Policy matrix:** User/Delegation: not hidden. Probe: probe-only paths stay probe. Title+Translate: `is_hidden_generation()`.

Where today: `purpose == InternalTitle` for MCP/prompt/permission/question/terminal-prefix/background-watch/capture → use `is_hidden_generation()` **or** `probe || is_hidden_generation()` if probe shared that path.

- [ ] **Step 1: Tests**

```rust
#[test]
fn hidden_generation_matrix() { /* title+translate true; user/delegation false; */ }

// migration test: seed title row, migrate, still present; insert translate ok; garbage purpose fails
// manager: InternalTranslate admitted to send_prompt_unlinked_internal like title
// existing title runner tests still pass
```

- [ ] **Step 2: Implement + `cargo test --features test-utils` for affected modules**

- [ ] **Step 3: Commit**

```bash
git commit -m "feat(acp): InternalTranslate purpose, migration, hidden generation policy"
```

---

### Task 4: Fail-closed markdown protect/restore

**Files:**
- `src-tauri/src/document_translate/mod.rs`
- `src-tauri/src/document_translate/protect.rs`

**API:**

```rust
pub fn protect_markdown_with_nonce(source: &str, nonce: &str) -> Result<ProtectedDocument, ProtectError>;
pub fn protect_markdown(source: &str) -> Result<ProtectedDocument, ProtectError>; // random nonce
pub fn restore_markdown(output: &str, protected: &ProtectedDocument) -> Result<String, ProtectError>;
```

Tokens: `⟦CGCODE_{nonce}_{n}⟧`, `⟦CGINLINE_{nonce}_{n}⟧`. If source contains nonce, regenerate (or error in with_nonce).

- [ ] **Step 1: Complete tests** (deterministic nonce `"n0"`):

1. round_trip fenced ``` and ~~~ and inline
2. missing token fails
3. duplicate token in output fails
4. reordered tokens fail
5. altered token fails
6. collision: source already contains chosen nonce → with_nonce err or auto regen path tested
7. fenced block containing backticks preserved

Run:  
`cargo test --features test-utils --lib document_translate::protect -- --nocapture`

- [ ] **Step 2: Implement until PASS**

- [ ] **Step 3: Commit**

```bash
git commit -m "feat(translate): fail-closed markdown code placeholders"
```

---

### Task 5: DocumentTranslationService + runner + translate_document

**Files:**
- `src-tauri/src/document_translate/{types,runner,service}.rs`
- `src-tauri/src/commands/document_translate.rs`
- `src-tauri/src/web/handlers/document_translate.rs`
- `src-tauri/src/commands/mod.rs`, `web/handlers/mod.rs`, `web/router.rs`, `web/mod.rs`
- `src-tauri/src/app_state.rs`, `src-tauri/src/lib.rs`, `src-tauri/src/bin/codeg_server.rs` (all AppState constructors)
- `src/lib/api.ts`, `src/lib/api.test.ts`

**Types (serde camelCase):**

```rust
#[serde(rename_all = "camelCase")]
pub struct TranslateDocumentParams {
    pub content: String,
    pub format: DocumentTranslateFormat, // "markdown" | "plainText"
    pub locale: Option<String>,
    pub display_name: Option<String>,
}
```

**Service:** single `Arc` in AppState; `try_acquire` capacity 1 → busy `TurnInProgress` + i18n_key `translateBusy`. Detached cleanup: permit held until runner finishes disconnect+rmdir even if request future dropped (spawn owned task; request awaits oneshot result).

**Prompt builder** (exact content in code + unit test contains fragments):

```text
Translate the following document into {Language}.
Return only the full translated document body.
Do not use tools. Do not wrap the entire answer in an outer code fence.
Do not add a preface or commentary.
Leave every placeholder like ⟦CGCODE_…⟧ and ⟦CGINLINE_…⟧ exactly unchanged.
Do not translate source code, shell commands, file paths, URLs, or identifiers.
Keep proper nouns, product names, API names, and established technical English terms in English when standard.
Translate surrounding prose into {Language}.

Document:
{body}
```

No title normalization; no outer-fence strip.

**Locale:** `parse_supported_app_locale` or `load_system_language_settings`.

**FE:**

```ts
export async function translateDocument(params: {
  content: string
  format: "markdown" | "plainText"
  locale?: string
  displayName?: string
}) {
  return getTransport().call("translate_document", params, { timeoutMs: 195_000 })
}
```

Test: api wrapper passes `timeoutMs: 195_000`.

**Tests required:** agent none; unavailable; empty; oversize 24001 scalars; busy second call; timeout cleanup; output >96k fails; placeholder fail; unsupported; spawn fail; fake driver happy path; no spawn when agent none; same-language still calls runner.

- [ ] **Step 1–N: TDD each group; commit**

```bash
git commit -m "feat(translate): document translation service, runner, and API"
```

---

### Task 6: FE eligibility, toolbar, transient tab, request gen

**Files:**
- `src/lib/document-translate.ts` + test
- `src/contexts/workspace-context.tsx` (+ tests)
- `src/components/files/file-workspace-tab-bar.tsx` (+ tests)

**Hash:** djb2 on UTF-16 code units of snapshot string; export `hashDocumentContent(s: string): string` (hex). Test vector: `hashDocumentContent("abc")` equals fixed expected hex constant recorded once implementation stabilizes (assert stability + non-empty).

**Eligibility `isTranslationEligible(tab)`:** kind file, not transient translation, not loading, extension whitelist, trim content non-empty, not image/office language.

**Transient:**

```ts
transient: {
  type: "translation"
  sourceTabId: string
  sourcePath: string | null
  sourceContentHash: string
  locale: string
  format: "markdown" | "plainText"
  suggestedName: string // `${stem}.${localeWire}${ext}`
}
id: `translate:${sourceTabId}:${locale}:${requestGen}`
path: null
readonly: true
hasLoadedSuccessfully: true
```

**Generation:** `Map<sourceTabId, number>`; ignore late results if gen mismatch. **Source tab closed:** still open result if gen matches. **Unmount provider:** drop late. **Eviction:** skip tabs with `transient?.type === "translation"`.

**Locale wire:** map next-intl locale to snake_case wire (`zh-CN` → `zh_cn`) via existing helpers if present, else small map in `document-translate.ts`.

**Agent off:** button visible; click toasts `translateAgentNotConfigured` without API call (read conversation-experience store).

**Busy:** disable button while in flight; double-click does not second-call.

**Snapshot:** capture content at click; later editor edits do not change in-flight payload (test).

Place **Translate** and later **Save as** buttons in `file-workspace-tab-bar.tsx` next to preview/maximize.

- [ ] **Step 1: pure tests + component tests**

- [ ] **Step 2: implement**

- [ ] **Step 3: commit**

```bash
git commit -m "feat(files): translate toolbar and transient translation result tabs"
```

---

### Task 7: save_translation_as + Save as UI + remaining i18n + disclosure

**Files:**
- Backend save core + handler + router
- FE api + tab bar Save as
- All 10 message catalogs for remaining keys
- `conversation-experience-settings.tsx` disclosure

**API:**

```rust
#[serde(rename_all = "camelCase")]
pub struct SaveTranslationAsParams {
    pub folder_id: i32,
    pub relative_path: String,
    pub content: String,
}
#[serde(rename_all = "camelCase")]
pub struct SaveTranslationAsResult {
    pub absolute_path: String,
}
```

Security: resolve folder_id → root; reject absolute/`..`/symlink parents (walk components, `symlink_metadata`); canonicalize parent under root; `OpenOptions::create_new(true)`; write; fsync best-effort; on failure remove partial if created.

**UI:** prompt or simple dialog for relative path defaulting to `suggestedName`; folder_id = active folder id from workspace; on success `const r = await openFilePreview(absolutePath)`; if `r.ok` close transient tab.

**Tests:** traversal, exists, concurrent two creates one wins, happy path; FE closes transient only on ok open.

**i18n keys** (all 10): full list from spec §3.

**Disclosure string** (en):  
`The automatic title agent is also used for document translation. Document text is sent to that agent/provider; Codeg hides internal sessions from its own lists but does not delete the agent CLI’s storage.`

- [ ] **Steps: TDD + implement + commit**

```bash
git commit -m "feat(translate): exclusive save_translation_as, Save as UI, i18n, disclosure"
```

---

### Task 8: Mandatory verification sweep

All commands must exit 0 (or document pre-existing failures outside touched paths only after confirming):

```bash
pnpm exec vitest run src/contexts/workspace-context.test.tsx src/lib/document-translate.test.ts src/components/files/file-workspace-tab-bar.test.tsx src/i18n/messages.test.ts src/lib/api.test.ts
pnpm eslint src/contexts/workspace-context.tsx src/components/files/file-workspace-tab-bar.tsx src/lib/document-translate.ts src/lib/api.ts
```

From `src-tauri`:

```bash
cargo test --features test-utils document_translate
cargo test --features test-utils is_hidden_generation
cargo test --features test-utils auto_title
cargo check
cargo check --no-default-features --bin codeg-server
cargo clippy --all-targets --features test-utils -- -D warnings
```

Manual smoke (operator): missing file toast; md maximize+Escape; translate fences; save as new file.

- [ ] **Step 1: Run all**
- [ ] **Step 2: Fix failures in scope**
- [ ] **Step 3: Commit fixes if any**

---

## Spec coverage

| Spec item | Task |
| --- | --- |
| Full failure matrix | 1 |
| Maximize-on-success + user-initiated | 1 |
| open settle result | 1 |
| Escape + overlays + dirty | 2 |
| Hidden purpose + connection.rs + migration | 3 |
| Protect fail-closed full tests | 4 |
| Service Arc, runner, API, errors, timeout | 5 |
| Transient tab lifecycle | 6 |
| Save as security + post-save | 7 |
| i18n + disclosure | 1 partial + 7 |
| Verification matrix | 8 |

## Execution model (this session)

1. Implementer: **Grok** (`codeg://agent/grok` / `agent_type: grok`) per task  
2. After each task: **Codex** review of that task’s commits; fix Critical/Important  
3. Continuous until Task 8 done
