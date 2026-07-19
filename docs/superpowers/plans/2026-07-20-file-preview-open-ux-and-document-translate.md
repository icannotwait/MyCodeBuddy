# File Preview Open UX and Document Translation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cold-open failures toast without error tabs; maximize files pane only after successful open; Escape closes the active file tab; on-demand Markdown/plain-text translation via the auto-title agent into a transient tab with exclusive Save as….

**Architecture:** Frontend load settle paths gain `hasLoadedSuccessfully` and cold/warm reject behavior. Backend adds `ConnectionPurpose::InternalTranslate` under a shared `is_hidden_generation()` policy, a parallel `DocumentTranslateRunner` in a reserved directory, Markdown placeholder protection (fail-closed), `translate_document` + `save_translation_as` commands, and FE toolbar/transient-tab/Save-as UI. Spec: `docs/superpowers/specs/2026-07-20-file-preview-open-ux-and-document-translate-design.md`.

**Tech Stack:** Next.js/React 19, TypeScript, Vitest, Tauri 2 / Axum, Rust 2021, SeaORM SQLite, existing `auto_title` hidden runner patterns, sonner toasts, next-intl (10 locales).

## Global Constraints

- Input max: **24_000** Unicode scalars (backend authoritative).
- Output max: **96_000** UTF-8 bytes.
- Backend deadline: **120s**; FE `timeoutMs`: **195_000**.
- In-flight translate capacity: **1** (busy reject, no queue).
- Extensions: **`.md`**, **`.markdown`**, **`.txt`** only (no `.mdx` in v1).
- Translate cwd: **`registry.reserved_root()` only** (never workspace).
- Save as: **`folder_id` + relative path**, exclusive create, **never overwrite**.
- Placeholder integrity: **fail-closed** (no result tab on mismatch).
- Maximize: only after **first successful settle** of a creating generation while tab still active; user-initiated opens only.
- Escape: fixed key; respect `defaultPrevented` and dirty `window.confirm`.
- Agent: global **`auto_title_agent`** only; no silent fallback.
- Do not change auto-title job coordinator / `auto_title_jobs` semantics.
- Parallel runner type — do **not** add translate branches onto `TitleAgentRunner` trait methods beyond shared helpers.
- i18n: all **10** locale catalogs.
- TDD: failing test first for each task; commit after green.

## File map

| Path | Role |
| --- | --- |
| `src/contexts/workspace-context.tsx` | Tab model, open/reject, maximize-on-success, transient insert |
| `src/contexts/workspace-context.test.tsx` | Open UX + maximize + cold/warm tests |
| `src/components/files/file-workspace-tab-bar.tsx` | Escape close; translate/save controls if placed here |
| `src/components/files/file-workspace-panel.tsx` | Toolbar translate / save as when needed |
| `src/lib/document-translate.ts` | FE helpers: eligibility, hash, API wrappers |
| `src/lib/document-translate.test.ts` | FE unit tests |
| `src/lib/api.ts` | `translateDocument`, `saveTranslationAs` |
| `src-tauri/src/auto_title/types.rs` | `InternalTranslate`, `is_hidden_generation()` |
| `src-tauri/src/auto_title/internal_sessions.rs` | Purpose enum + filtering |
| `src-tauri/src/db/migration/*_translate_purpose.rs` | CHECK migration |
| `src-tauri/src/acp/manager.rs` | All InternalTitle special cases → hidden generation |
| `src-tauri/src/document_translate/mod.rs` | Module root |
| `src-tauri/src/document_translate/protect.rs` | Markdown protect/restore |
| `src-tauri/src/document_translate/runner.rs` | DocumentTranslateRunner |
| `src-tauri/src/document_translate/service.rs` | Admission + run |
| `src-tauri/src/commands/document_translate.rs` | Cores |
| `src-tauri/src/web/handlers/document_translate.rs` | HTTP |
| `src-tauri/src/web/router.rs` / `lib.rs` | Routes + commands |
| `src/i18n/messages/*.json` | Strings |

---

### Task 1: File tab load flags and cold/warm open UX

**Files:**
- Modify: `src/contexts/workspace-context.tsx`
- Modify: `src/contexts/workspace-context.test.tsx`
- Modify: `src/i18n/messages/en.json` (and other 9 locales in Task 7 if deferred — **add en + zh-CN minimum here**, rest in Task 7)

**Interfaces:**
- Produces: `FileWorkspaceTab.hasLoadedSuccessfully: boolean`
- Produces: cold open failure closes tab + toast; warm keeps content
- Produces: maximize on successful settle when `pendingMaximizeTabId === tabId`

- [ ] **Step 1: Write failing tests** in `workspace-context.test.tsx`

```tsx
it("cold open failure removes the tab and does not leave error content", async () => {
  // mock readFileForEdit reject
  // openFilePreview("missing.ts")
  // expect fileTabs empty or without that path
  // expect no tab with saveState === "error"
})

it("warm reload failure keeps prior content", async () => {
  // open success with content "hello"
  // mock next read to fail
  // openFilePreview(path, { reload: true })
  // expect content still "hello"
})

it("maximize becomes true only after successful settle of a new tab", async () => {
  // open success
  // expect filesMaximized true after content settled
})

it("failed cold open does not leave maximize true when no tabs remain", async () => {
  // open fail with empty workspace
  // expect filesMaximized false and no tabs
})
```

- [ ] **Step 2: Run tests — expect FAIL**

Run: `pnpm exec vitest run src/contexts/workspace-context.test.tsx -t "cold open failure|warm reload|maximize becomes"`

- [ ] **Step 3: Implement**

On `FileWorkspaceTab` add:

```ts
hasLoadedSuccessfully?: boolean
/** Internal: maximize files pane when this tab first settles successfully */
// Track via ref pendingMaximizeTabIds: Set<string> in provider, not necessarily on tab
```

In `seedLoadingTab` for **new** user-initiated file seeds from `openFilePreview`, add tab id to `pendingMaximizeOnSuccessRef`.

On successful settle (file/image/office ready paths): set `hasLoadedSuccessfully: true`; if `pendingMaximizeOnSuccessRef` has tabId and `activeFileTabId === tabId`, `setFilesMaximized(true)` and clear pending for that id.

Replace cold `rejectTab` in `openFilePreview` catch with:

```ts
const existing = fileTabsRef.current.find((t) => t.id === tabId)
if (!existing?.hasLoadedSuccessfully) {
  // close tab without error body
  setFileTabs((prev) => prev.filter((t) => t.id !== tabId))
  // fix active id like closeFileTab
  toast.error(t("unableOpenFile", { name: fileName(absPath) }))
  pendingMaximizeOnSuccessRef.current.delete(tabId)
} else {
  // warm: clear loading, toast, keep content
  setFileTabs((prev) =>
    prev.map((tab) =>
      tab.id === tabId
        ? { ...tab, loading: false, saveState: "idle", saveError: null }
        : tab
    )
  )
  toast.error(t("unableOpenFile", { name: fileName(absPath) }))
}
```

Do **not** change `rejectFileTab` (watcher deletion).

Pre-seed resolve failures: toast without tab.

Office auto-preview: do **not** add to `pendingMaximizeOnSuccessRef`.

- [ ] **Step 4: Run tests — expect PASS**

- [ ] **Step 5: Commit**

```bash
git add src/contexts/workspace-context.tsx src/contexts/workspace-context.test.tsx src/i18n/messages/en.json src/i18n/messages/zh-CN.json
git commit -m "fix(files): cold-open toast without error tab; maximize on success"
```

---

### Task 2: Escape closes active file tab

**Files:**
- Modify: `src/components/files/file-workspace-tab-bar.tsx`
- Create or modify: `src/components/files/file-workspace-tab-bar.test.tsx` (create if missing)

**Interfaces:**
- Consumes: `closeFileTab`, `activeFileTabId`, view mode/pane/filesMaximized
- Produces: Escape → `closeFileTab` when files context active and not defaultPrevented

- [ ] **Step 1: Write failing test**

```tsx
it("Escape closes the active file tab when files pane is active", () => {
  // render tab bar with mocked workspace hooks
  // dispatch keydown Escape on window
  // expect closeFileTab called with active id
})

it("Escape does nothing when event.defaultPrevented", () => {
  // listener that preventDefault first in capture? or dispatch Event with defaultPrevented
  // expect closeFileTab not called
})
```

- [ ] **Step 2: Run — expect FAIL**

- [ ] **Step 3: Implement** in existing `onKeyDown` in `file-workspace-tab-bar.tsx`:

```ts
if (event.key === "Escape") {
  if (event.defaultPrevented) return
  const shouldHandle =
    mode === "fusion" && (activePane === "files" || filesMaximized)
  if (!shouldHandle) return
  if (!activeFileTabId) return
  event.preventDefault()
  closeFileTab(activeFileTabId)
  return
}
```

Keep existing shortcut handling for Mod+W.

- [ ] **Step 4: Run — expect PASS**

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(files): Escape closes active file workspace tab"
```

---

### Task 3: Hidden generation purpose + DB migration

**Files:**
- Modify: `src-tauri/src/auto_title/types.rs`
- Modify: `src-tauri/src/auto_title/internal_sessions.rs`
- Create: `src-tauri/src/db/migration/m20260720_000001_internal_session_translate_purpose.rs` (timestamp style matching repo)
- Modify: migration registrar
- Modify: `src-tauri/src/acp/manager.rs` (all `InternalTitle`-only checks that should include translate)

**Interfaces:**
- Produces:

```rust
impl ConnectionPurpose {
    pub fn is_hidden_generation(self) -> bool {
        matches!(self, Self::InternalTitle | Self::InternalTranslate)
    }
}
```

- Produces: `InternalSessionPurpose::Translate` wire `"translate"`
- Produces: DB CHECK allows `title|translate`

- [ ] **Step 1: Write failing Rust tests**

```rust
#[test]
fn hidden_generation_includes_title_and_translate() {
    assert!(ConnectionPurpose::InternalTitle.is_hidden_generation());
    assert!(ConnectionPurpose::InternalTranslate.is_hidden_generation());
    assert!(!ConnectionPurpose::User.is_hidden_generation());
}
```

Update manager tests that assert InternalTitle-only admission to also cover InternalTranslate where policy requires.

- [ ] **Step 2: `cargo test` relevant modules — FAIL for missing variant**

Run (from `src-tauri`):  
`cargo test --features test-utils is_hidden_generation -- --nocapture`

- [ ] **Step 3: Implement enum + migration + replace comparisons**

Replace patterns like:

```rust
launch_context.purpose == ConnectionPurpose::InternalTitle
```

with:

```rust
launch_context.purpose.is_hidden_generation()
```

**only** where the design requires both (MCP skip, unlinked prompt admission, permission/question reject, title-capture bypass, etc.). Keep probe-only paths as probe-only.

Migration: rebuild `internal_agent_sessions` or alter CHECK per project migration style for SQLite.

- [ ] **Step 4: `cargo test --features test-utils` for auto_title + manager purpose tests — PASS**

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(acp): InternalTranslate purpose and hidden generation policy"
```

---

### Task 4: Markdown protect / restore (fail-closed)

**Files:**
- Create: `src-tauri/src/document_translate/mod.rs`
- Create: `src-tauri/src/document_translate/protect.rs`
- Wire module in `src-tauri/src/lib.rs` or `auto_title` sibling

**Interfaces:**
- Produces:

```rust
pub struct ProtectedDocument {
    pub text: String,
    pub tokens: Vec<String>, // ordered
}

pub fn protect_markdown(source: &str) -> Result<ProtectedDocument, ProtectError>;
pub fn restore_markdown(model_output: &str, protected: &ProtectedDocument) -> Result<String, ProtectError>;
```

- [ ] **Step 1: Write unit tests in `protect.rs`**

```rust
#[test]
fn round_trip_fenced_and_inline() {
    let src = "Hello `x`\n\n```rs\nfn main(){}\n```\n";
    let p = protect_markdown(src).unwrap();
    assert!(p.text.contains("⟦CG"));
    let out = restore_markdown(&p.text.replace("Hello", "你好"), &p).unwrap();
    assert!(out.contains("```rs\nfn main(){}\n```"));
    assert!(out.contains("`x`"));
}

#[test]
fn missing_token_fails() {
    let p = protect_markdown("a `b` c").unwrap();
    let bad = p.text.replace(&p.tokens[0], "GONE");
    assert!(restore_markdown(&bad, &p).is_err());
}

#[test]
fn reordered_tokens_fail() { /* swap two tokens in output */ }
```

- [ ] **Step 2: Run — FAIL**

`cargo test --features test-utils protect_markdown -- --nocapture`

- [ ] **Step 3: Implement** nonce tokens `⟦CGCODE_{nonce}_{n}⟧` / `⟦CGINLINE_{nonce}_{n}⟧`; collision regenerate; fence then inline; restore ordered exact multiset.

- [ ] **Step 4: PASS + Commit**

```bash
git commit -m "feat(translate): fail-closed markdown code placeholder protect/restore"
```

---

### Task 5: DocumentTranslateRunner + service + translate_document API

**Files:**
- Create: `src-tauri/src/document_translate/runner.rs`
- Create: `src-tauri/src/document_translate/service.rs`
- Create: `src-tauri/src/document_translate/types.rs`
- Create: `src-tauri/src/commands/document_translate.rs`
- Create: `src-tauri/src/web/handlers/document_translate.rs`
- Modify: `src-tauri/src/web/router.rs`, `src-tauri/src/lib.rs`, `AppState` if service stored
- Modify: `src/lib/api.ts`
- Create: Rust tests with fake driver (mirror title runner style, smaller)

**Interfaces:**
- Produces:

```rust
pub struct TranslateDocumentParams {
    pub content: String,
    pub format: DocumentTranslateFormat, // Markdown | PlainText
    pub locale: Option<String>,
    pub display_name: Option<String>,
}

pub struct TranslateDocumentResult {
    pub translated_content: String,
    pub locale: String,
    pub format: DocumentTranslateFormat,
}

// FE
export async function translateDocument(
  params: TranslateDocumentParams
): Promise<TranslateDocumentResult> {
  return getTransport().call("translate_document", params, {
    timeoutMs: 195_000,
  })
}
```

Constants: `MAX_INPUT_SCALARS = 24_000`, `MAX_OUTPUT_BYTES = 96_000`, `DEADLINE_SECS = 120`.

- [ ] **Step 1: Service unit tests** — agent none → ConfigurationMissing; empty → InvalidInput; oversize → InvalidInput; second concurrent → busy (TurnInProgress).

- [ ] **Step 2: FAIL then implement service admission + runner skeleton** that:
  1. Loads agent
  2. Protects if Markdown
  3. Builds prompt with locale display name
  4. Spawns in reserved_root UUID dir with InternalTranslate
  5. Collects with byte cap
  6. Restores or integrity error
  7. Always disconnect + remove dir

Use capacity-1 mutex/semaphore on service.

- [ ] **Step 3: Wire Tauri command + Axum POST `/translate_document`**

- [ ] **Step 4: FE api helper with timeoutMs 195_000**

- [ ] **Step 5: cargo test + commit**

```bash
git commit -m "feat(translate): document translate runner, service, and API"
```

---

### Task 6: Frontend translate toolbar + transient result tab

**Files:**
- Modify: `src/contexts/workspace-context.tsx` (insert transient tab API)
- Modify: `src/components/files/file-workspace-tab-bar.tsx` and/or `file-workspace-panel.tsx`
- Create: `src/lib/document-translate.ts` + tests
- Modify: workspace tests

**Interfaces:**
- Produces:

```ts
export type TranslationTransient = {
  type: "translation"
  sourceTabId: string
  sourcePath: string | null
  sourceContentHash: string
  locale: string
  format: "markdown" | "plainText"
  suggestedName: string
}

// on FileWorkspaceTab:
transient?: TranslationTransient
// path: null, readonly: true
```

- Produces: `openTranslationResultTab(result, meta)`
- Produces: request gen map `sourceTabId → number`

- [ ] **Step 1: Unit tests for eligibility**

```ts
expect(isTranslatablePath("a.md")).toBe(true)
expect(isTranslatablePath("a.mdx")).toBe(false)
expect(isTranslatablePath("a.rs")).toBe(false)
```

- [ ] **Step 2: Component/integration test** — click translate with mocked API opens readonly tab with content.

- [ ] **Step 3: Implement button + busy + gen guard + toast on errors via i18n keys**

Hash snapshot with simple djb2 or Web Crypto; store on transient.

Markdown results: add to `previewFileTabIds` like normal md seed.

Maximize: insert with maximize on success (content already present → set maximized immediately on insert).

- [ ] **Step 4: PASS + Commit**

```bash
git commit -m "feat(files): translate toolbar and transient translation tabs"
```

---

### Task 7: save_translation_as + Save as UI + i18n complete

**Files:**
- Create/modify: `src-tauri/src/commands/document_translate.rs` (or folders)
- Handler + router
- FE API + UI dialog/prompt for relative path default
- All `src/i18n/messages/*.json`
- Settings blurb in conversation-experience settings

**Interfaces:**
- Produces:

```rust
pub struct SaveTranslationAsParams {
    pub folder_id: i32,
    pub relative_path: String,
    pub content: String,
}
// returns absolute path string or FileSaveResult-like
```

- [ ] **Step 1: Rust tests** — `../`, absolute, existing file, happy exclusive create under temp folder root.

- [ ] **Step 2: Implement exclusive create under resolved folder root**

- [ ] **Step 3: FE Save as button on transient tab → call API → `openFilePreview(abs)` → close transient on success**

- [ ] **Step 4: Fill all 10 locale catalogs with keys from spec §3**

- [ ] **Step 5: Settings one-line disclosure about provider**

- [ ] **Step 6: Tests PASS + Commit**

```bash
git commit -m "feat(translate): exclusive save_translation_as and full i18n"
```

---

### Task 8: Verification sweep

**Files:** none new; run suites

- [ ] **Step 1:** `pnpm exec vitest run src/contexts/workspace-context.test.tsx src/lib/document-translate.test.ts src/components/files/`

- [ ] **Step 2:** From `src-tauri`:  
  `cargo test --features test-utils document_translate`  
  `cargo test --features test-utils is_hidden_generation`  
  `cargo clippy --all-targets --features test-utils -- -D warnings` (if time; fix warnings introduced)

- [ ] **Step 3:** Fix any failures

- [ ] **Step 4:** Commit only if fixes needed

```bash
git commit -m "test(translate): fix verification failures for open UX and translate"
```

---

## Spec coverage checklist

| Spec requirement | Task |
| --- | --- |
| Cold open toast, no error tab | 1 |
| Warm reload keep content | 1 |
| Watcher rejectFileTab unchanged | 1 (explicit non-change) |
| Maximize on successful settle | 1 |
| Escape close + dirty confirm | 2 |
| InternalTranslate + hidden policy + migration | 3 |
| Protect/restore fail-closed | 4 |
| Runner reserved cwd, service capacity, API, timeoutMs | 5 |
| Toolbar, transient tab, request gen | 6 |
| save_translation_as exclusive | 7 |
| i18n 10 locales + disclosure | 7 |
| Verification | 8 |

## Placeholder / consistency self-review

- No TBD steps; constants match revised spec.
- `.mdx` excluded consistently.
- `save_file_copy` not used for Save as.
- Title job table untouched.

## Execution

User requested: **Subagent-Driven Development** with **Grok** implementers and **Codex** reviewers per task.
