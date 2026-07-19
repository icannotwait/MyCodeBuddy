# File Preview Open UX and Document Translation

Date: 2026-07-20

Status: Design revised after Codex review (NEEDS_REWORK → fixes applied);
awaiting implementation plan

## Revision note (Codex review)

Addressed Critical and Important findings from Codex CLI review of the first
draft. Major corrections:

- Maximize only on **successful settle** of a creating generation (not at seed).
- Cold/warm failure matrix per call site; Office runtime errors excluded.
- Translation uses reserved cwd + centralized hidden-generation policy (not
  workspace cwd); parallel runner, not title-trait branches.
- SQLite purpose migration for `translate`; registry retention/rate limits.
- Fail-closed placeholder integrity; no fuzzy restore; no result tab on mismatch.
- `format: Markdown | PlainText`; **drop `.mdx` from v1**.
- Dedicated `save_translation_as` with workspace isolation and exclusive create.
- Transport `timeoutMs` ≥ backend deadline + cleanup; conservative size cap.
- Typed `AppErrorCode` / i18n keys; no client-disconnect cancel in v1.
- Transient translation tab model; request generation; click-time snapshot.
- Escape precedence; dirty-tab confirm; post-save exclusive path rules.
- Provider disclosure; complete i18n; locale wire conversion.

## Summary

This change improves the file workspace open/close experience and adds an
on-demand document translation action that reuses the configured automatic-title
agent (`auto_title_agent`).

1. **Open UX**: cold open failures toast and remove the tab (no sticky error
   body). After a **successful first settle** of a newly created file tab,
   maximize the files pane. `Escape` closes the active file tab (with dirty
   confirm and overlay precedence).
2. **Document translation**: for Markdown / plain text only, a toolbar action
   translates a **click-time content snapshot** into the current UI language
   via a hidden one-shot agent run (same agent setting as auto-title), opens a
   **transient read-only tab**, and offers **Save as…** through a dedicated
   write API. Fenced/inline code is structurally protected (fail-closed).
   Technical English preservation is **best-effort via prompt**, not a hard
   guarantee.

Desktop and server transports share the same commands/handlers.

## Goals

- Cold-load failure of a newly opened **file** tab: toast + remove tab; never
  leave `unableLoadContent` as a sticky error window for that path.
- Maximize files pane only after the **first successful settle** of a creating
  generation, while that tab is still active.
- Re-activating an already-open tab does **not** force maximize.
- `Escape` closes the current file tab in files context, after overlay/dialog
  precedence and using existing dirty confirmation.
- Translate toolbar for `.md` / `.markdown` / `.txt` only; uses
  `auto_title_agent` + current UI locale.
- Agent unset/unavailable: toast; no runner spawn.
- Result: transient readonly tab + Save as… (exclusive create, never overwrite
  source or existing files in v1).
- Hard guarantee: protected code placeholders restore 1:1 or fail with no tab.
- Soft quality: prompt asks to keep proper/technical English terms.

## Non-goals

- Auto-translate on open; chunked multi-call translation in v1.
- MDX, HTML, source code, images, Office as translate targets.
- Changing auto-title job coordinator / `auto_title_jobs` / finalized flags.
- Side-by-side split view; streaming translation UI.
- User cancel mid-run or client-disconnect cancel in v1 (runner always cleans up).
- Per-project translation agent; glossary UI.
- Claiming “hidden from Codeg” means deleted from the agent CLI or private from
  the model provider.

## Confirmed Product Decisions

| Area | Decision |
| --- | --- |
| Cold file open fail | Toast + close tab (no error body) |
| Warm user reload fail | Keep prior content + toast; clear loading |
| Watcher external delete | Keep existing `rejectFileTab` policy (replace with error) |
| Office runtime fail | OfficePreview owns UI; not cold-close |
| Maximize | On first **successful** settle of creating gen if tab still active |
| Maximize scope | User-initiated file opens (file tree / openFilePreview); not auto-office, not silent |
| Existing tab click | Activate only |
| Escape | Close current file tab via existing close path |
| Translate types | `.md`, `.markdown`, `.txt` only |
| Translate agent | Global `auto_title_agent` |
| Agent off | Toast → settings |
| Output | Transient readonly tab + Save as… |
| Code protection | Structural placeholders; fail-closed |
| Technical English | Best-effort prompt only |
| Open architecture | Seed loading first; cold fail close + toast |
| Save as | Dedicated API; exclusive create; no overwrite |
| Cancellation (v1) | No user cancel; busy rejection if at capacity |
| Same UI language as source | Still run model (no auto language detection) |

---

## Architecture

### Components

| Unit | Responsibility |
| --- | --- |
| `openFilePreview` settle/reject paths | Cold close + toast; warm toast-only; set `hasLoadedSuccessfully`; maximize on success |
| Keyboard handler (files chrome) | Escape → close with precedence |
| Translate toolbar | Eligibility, busy, invoke, request gen |
| `DocumentTranslationService` | Process-wide admission (capacity 1 or 2), run lifecycle |
| `DocumentTranslateRunner` | Parallel to title runner; shared hidden lifecycle helpers |
| Markdown protect/restore | Pure; fail-closed integrity |
| `save_translation_as` | Workspace-scoped exclusive write |
| Transient tab insert | Typed `transient` metadata; no disk watch/reload/evict as pathless dirty |

### Hidden generation policy

Title and translate share a single predicate, e.g.
`ConnectionPurpose::is_hidden_generation()` (or equivalent on purpose enum),
used everywhere `InternalTitle` is special-cased today:

- MCP injection suppressed
- Prompt admission / tools disabled for utility runs
- Title-capture bypass
- Terminal-prefix / background-watch suppression
- Permission and question rejection
- Noop event emitter
- Internal session registry purpose filter

**Working directory**: always under `InternalAgentSessionRegistry::reserved_root()`
+ unique UUID subdir (same as titles). **Never** the source file parent or
workspace root. Source path is frontend display metadata only; full document
text is in the prompt payload.

Add `InternalSessionPurpose::Translate` and migrate
`internal_agent_sessions.purpose` CHECK to allow `title|translate`.

### Data flow — open

```text
openFilePreview
  → resolve path (pre-seed failure → toast only, no tab)
  → seedLoadingTab (loading; hasLoadedSuccessfully=false; NO maximize yet)
  → fetch
  → success settle:
        hasLoadedSuccessfully=true
        if this gen created the tab and tab still active → filesMaximized=true
  → cold failure (never successfully loaded):
        closeFileTab(tabId) + toast.error(i18n)
  → warm failure (hasLoadedSuccessfully):
        clear loading + toast; keep content
```

### Data flow — translate

```text
Click Translate (snapshot content + requestGen++)
  → FE: agent configured? size OK? → else toast
  → call translate_document({ content, format, locale, folderId? }, timeoutMs=195000)
  → BE DocumentTranslationService:
        admit or translation_busy
        protect → run hidden agent in reserved dir → restore exact
        integrity fail → error, no partial body
  → FE: if requestGen still current and source tab still open context:
        insert transient translation tab (readonly)
     else: drop late result
  → Save as… → save_translation_as → open real path → close transient after load OK
```

---

## 1. File open / close UX

### 1.1 Failure matrix

| Call site | Behavior |
| --- | --- |
| `openFilePreview` cold fail (never `hasLoadedSuccessfully`) | Close tab + toast `unableOpenFile` |
| `openFilePreview` warm fail / user reload | Keep content + toast; `loading=false` |
| Path resolve / split fail **before** seed | Toast only; no tab |
| `reloadOpenFileBackground` | Keep existing background semantics; do not invent error tabs for closed tabs; toast only if tab still open and warm |
| `rejectFileTab` (watcher: external delete / unreadable) | **Unchanged**: mark error body so user does not keep believing stale disk content |
| Diff / rich-diff open fail | Close cold diff tab + toast (same cold rule); warm rare for diffs |
| Office shell seed success then OfficePreview runtime error | **Excluded** from cold-close; OfficePreview retry/setup UI stays |
| Stale gen (`settleFetch` false) | No toast |

Add `hasLoadedSuccessfully: boolean` on `FileWorkspaceTab` (default false;
true after successful file/image/office settle).

Toast: localized generic message + basename; put raw technical detail in
console/log only (avoid dumping absolute paths in toast when possible). Use
`<bdi>` for filenames in UI chrome where mixed scripts appear.

### 1.2 Maximize

**Normative rule:** set `filesMaximized = true` only when:

1. A successful settle completes for generation G that was started by a **new
   tab insert** for that tab id, and
2. The active file tab id is still that tab, and
3. The open was **user-initiated** via `openFilePreview` / explicit open
   actions (not office auto-preview, not automatic silent opens).

Do **not** maximize at `seedLoadingTab` time (avoids maximize flash on failure
when other tabs already exist).

Provide a single internal helper used by insert paths:

```ts
insertFileTab(tab, { maximizeOnSuccess: boolean }): void
```

Translation result insert uses `maximizeOnSuccess: true` (user-initiated
translate). Auto-office open uses `maximizeOnSuccess: false`.

### 1.3 Escape

In files keyboard handler (same gate as close-current:
`mode === "fusion" && (activePane === "files" || filesMaximized)`):

1. If `event.defaultPrevented` → return.
2. If an open dialog/sheet/alertdialog focus scope contains focus → return
   (Radix owns Escape).
3. If focus is inside a portaled popover/menu that handles Escape → return.
4. Monaco suggest/find widgets: if they consume Escape first, respect
   `defaultPrevented`; do not force-close the tab over editor chrome.
5. Else `preventDefault` + `closeFileTab(activeFileTabId)`.

`closeFileTab` already `window.confirm`s dirty tabs — Escape uses that path;
confirm cancel leaves the tab open. Test confirm/cancel.

Escape is a **fixed** additional binding (not rebindable in v1), independent of
`close_current_tab` shortcut.

### 1.4 Loading flash

Accepted: brief loading tab may paint then disappear on cold failure.

---

## 2. Document translation

### 2.1 Eligibility

Show Translate when all hold:

- `kind === "file"` and not transient-translation (do not re-translate result)
- Not loading
- Extension in `{ .md, .markdown, .txt }` (case-insensitive)
- Non-empty content (trim)
- Not image/office languages

If agent is off, **show** button; click → `translateAgentNotConfigured` toast.

### 2.2 Locale and agent

- Target: current UI language. FE converts BCP-47-like intl locale to backend
  wire via existing/`fromIntlLocale` mapping before call; BE re-validates with
  `parse_supported_app_locale` and falls back to system language settings if
  missing/invalid.
- Agent: `load_auto_title_agent_from` only; no silent fallback agent.
- Response returns typed locale wire id used for the run.

### 2.3 API

```rust
pub enum DocumentTranslateFormat {
    Markdown,
    PlainText,
}

pub struct TranslateDocumentParams {
    pub content: String,
    pub format: DocumentTranslateFormat,
    /// Optional wire locale (snake_case).
    pub locale: Option<String>,
    /// Optional display basename only (not used for FS access).
    pub display_name: Option<String>,
}

pub struct TranslateDocumentResult {
    pub translated_content: String,
    pub locale: String,
    pub format: DocumentTranslateFormat,
}
```

**Size limits (authoritative on backend):**

- Input: max **24_000** Unicode scalars (conservative for one-shot default
  models without chunking). FE may pre-check with same constant shared or
  duplicated and documented.
- Output: max **96_000** UTF-8 bytes collected from the runner; over →
  `TaskExecutionFailed` / translate failed.
- Backend scalar count is authoritative (not JS UTF-16 `.length` alone).

**Deadline:** backend overall **120s**. Frontend transport call uses
`timeoutMs: 195_000` on Web and remote-desktop proxies (Tauri invoke has no
client timeout). Document this in the API client helper.

**Admission:** process-wide `DocumentTranslationService` with capacity **1**
in-flight. Additional requests return immediately with busy error (no queue
wait in v1).

**No v1 cancel API.** Runner always disconnects and removes run dir on all
exit paths even if the HTTP client is gone.

### 2.4 Errors (typed)

Map domain outcomes to `AppCommandError` with existing or added codes:

| Outcome | AppErrorCode | HTTP | i18n key |
| --- | --- | --- | --- |
| Agent None | `ConfigurationMissing` | 400 | `translateAgentNotConfigured` |
| Agent unavailable | `DependencyMissing` | 400 | `translateAgentUnavailable` |
| Empty content | `InvalidInput` | 400 | `translateContentEmpty` |
| Too large | `InvalidInput` | 400 | `translateContentTooLarge` |
| Unsupported format | `InvalidInput` | 400 | `translateUnsupportedFormat` |
| Busy | `TurnInProgress` or new `RegistryOverloaded`-style if preferred; **use `TaskExecutionFailed` with stable `i18n_key` in detail if no perfect code** — prefer adding `Busy` only if project accepts enum growth; v1: **`InvalidRequest` is wrong**; use **`TurnInProgress` (409)** for busy | 409 | `translateBusy` |
| Timeout | `SourceTimeout` is search-specific; prefer **`TaskExecutionFailed`** + i18n `translateTimeout` | 500/408 | `translateTimeout` |
| Placeholder integrity | `InvalidInput` or `TaskExecutionFailed` + i18n | 500 | `translatePlaceholderIntegrityFailed` |
| Runner/spawn fail | `TaskExecutionFailed` | 500 | `translateFailed` |
| Save path invalid | `InvalidInput` / `PermissionDenied` | 400 | `translateSavePathRejected` |
| Save exists | `AlreadyExists` | 409 | `translateSaveAlreadyExists` |

FE maps `error.i18n_key` / code to toasts; never parse English `message` for
control flow.

### 2.5 Runner

- New `DocumentTranslateRunner` (parallel type), not methods on
  `TitleAgentRunner` / `HiddenAgentRunner` title trait.
- Extract shared helpers only where low-risk: reserved dir, discovery lease,
  disconnect cleanup, private stream collect with **output byte cap**.
- Purpose: `InternalTranslate` under `is_hidden_generation()`.
- Register purpose `translate` in DB.
- Prompt: no tools; return full body only; no outer commentary; placeholder
  rules; best-effort technical English retention; target language display name.
- **No** 80-scalar title normalization.
- **No** outer-fence stripping heuristic (unsafe for legitimate fence-only
  docs). Rely on prompt.
- Rate limit: max **10** successful translate runs per process hour soft
  counter optional; hard: capacity 1. Document that each run leaves a durable
  internal session registry row (same class as titles); do not redesign
  registry GC in v1 beyond purpose migration + tests that translate purpose
  is filtered.

### 2.6 Code protection (fail-closed)

**Markdown format:**

1. Generate a per-request nonce; tokens like `⟦CGCODE_{nonce}_{n}⟧` and
   `⟦CGINLINE_{nonce}_{n}⟧` that must not appear in source (collision check:
   if source contains the nonce, regenerate).
2. Replace fenced blocks (``` / ~~~ with optional info string) then inline
   backticks (single-level; no nested fancy cases in v1).
3. After model output: require **exactly** the same multiset of tokens in the
   **same order** of first occurrence as emitted; restore 1:1.
4. Any missing, duplicated, reordered, or altered token →
   `translatePlaceholderIntegrityFailed`; **no result tab**.

**PlainText format:** no structural protect; prompt-only for “do not alter
code-like lines” best-effort.

### 2.7 Frontend result tab

```ts
transient: {
  type: "translation"
  sourceTabId: string
  sourcePath: string | null
  sourceContentHash: string // hash of snapshot
  locale: string
  format: "markdown" | "plainText"
  suggestedName: string // e.g. README.zh_cn.md
}
```

- `path: null`, `readonly: true`, `kind: "file"`
- Tab id: `translate:{sourceTabId}:{locale}:{requestGen}`
- Title: i18n `translationTabTitle` with basename + locale label
- Exclude from disk watch, stale reload, path-based eviction of clean
  pathless tabs (pin transient tabs until user closes)
- Maximize on successful insert (user action)
- Request generation: ignore late responses when gen mismatch or user closed
  source context (still allow showing result if only source closed? **Normative:
  show result if gen matches and user did not start a newer translate on that
  sourceTabId**; closing source tab does not cancel in-flight — result may still
  open. Closing app mid-flight: drop.)

### 2.8 Save as…

**API** `save_translation_as`:

```rust
pub struct SaveTranslationAsParams {
    pub folder_id: i32,
    /// Relative path under that folder root (no absolute, no `..`).
    pub relative_path: String,
    pub content: String,
}
```

Server:

1. Resolve `folder_id` → registered workspace root.
2. Join and **canonicalize** parent; require parent is still under root.
3. Reject symlink parents; reject absolute segments; reject `..`.
4. Exclusive create (`create_new` / O_EXCL); if exists → `AlreadyExists`.
5. Never overwrite source or any existing file in v1.
6. Atomic write (write temp in same dir + rename) where OS allows; else
   exclusive create + write + fsync best-effort.

Default relative name: `{stem}.{locale_wire}{ext}` with backend wire id
(`zh_cn` → `README.zh_cn.md`).

**Post-save:** open absolute path via `openFilePreview` (reload if tab exists);
on successful load, **close** the transient translation tab. Do not leave
duplicate transient + real tabs.

### 2.9 Provider disclosure

Settings blurb and/or first-use toast: document text is sent to the configured
title agent / its provider; sessions may remain in that CLI’s storage; Codeg
only hides internal sessions from Codeg lists.

---

## 3. Internationalization

Namespaces:

- Open errors: `Folder.workspaceContext` (or existing workspace keys)
- Controls: `Folder.fileWorkspace`

Keys (all 10 catalogs):

- `unableOpenFile` — `{name}`
- `translateToCurrentLanguage`
- `translating`
- `translateAgentNotConfigured`
- `translateAgentUnavailable`
- `translateContentEmpty`
- `translateContentTooLarge` — `{limit}`
- `translateUnsupportedFormat`
- `translateBusy`
- `translateTimeout`
- `translateFailed`
- `translatePlaceholderIntegrityFailed`
- `translateSavePathRejected`
- `translateSaveAlreadyExists`
- `saveTranslationAs`
- `translationTabTitle` — `{name}`, `{locale}`
- `translateProviderDisclosure` (settings help)

---

## 4. Testing

### Frontend

- Cold open fail: no residual tab; toast; no maximize stuck when other tabs
  existed.
- Warm reload fail: content preserved.
- Pre-resolve fail: toast, no tab.
- Maximize only after success; not on failed cold open.
- Escape: closes; dirty confirm cancel; defaultPrevented skips.
- Translate button visibility; agent-null toast; busy double-click.
- Late result gen mismatch dropped.
- Transient tab not disk-watched; save-as flow closes transient after open.

### Backend

- Protect/restore happy path; missing/dup/reorder/collision fail.
- Agent none / unavailable / empty / oversize.
- Busy second concurrent call.
- Timeout path disconnects and removes run dir.
- Hidden policy guards for `InternalTranslate` (each manager special case).
- DB migration purpose check; discovery filters translate purpose.
- `save_translation_as`: traversal, symlink, absolute, exists, happy exclusive.
- Output byte cap.
- Transport client uses 195s timeout (unit on API wrapper).

### Manual smoke

- Missing file → toast only.
- Open md → maximize after load; Escape closes.
- Translate with fences → code identical; prose translated.
- Save as new name → file on disk; transient gone.

---

## 5. Implementation phases (single plan, ordered)

Plan may be one file with tasks ordered so each is shippable:

1. Open UX (fail matrix, maximize-on-success, Escape) + tests  
2. Hidden purpose migration + `is_hidden_generation` generalization + tests  
3. Protect/restore pure module + tests  
4. DocumentTranslateRunner + service + command/handler + API client  
5. FE toolbar, transient tab, request gen  
6. `save_translation_as` + FE Save as…  
7. i18n (10 locales) + settings disclosure  

---

## 6. Risks

| Risk | Mitigation |
| --- | --- |
| Model context too small | 24k scalar input cap |
| Placeholder corruption | Fail-closed, no tab |
| Title regressions | Parallel runner; shared helpers only with title tests green |
| Escape vs Monaco | defaultPrevented |
| Path escape on save | folder_id + relative + canonicalize + exclusive |
| Provider privacy | Disclosure copy |
| Registry growth | Capacity 1; accept title-like retention in v1 |

---

## 7. Fixed defaults

| Parameter | Value |
| --- | --- |
| Input max scalars | 24_000 |
| Output max UTF-8 bytes | 96_000 |
| Backend deadline | 120s |
| FE transport timeoutMs | 195_000 |
| In-flight capacity | 1 |
| Extensions | `.md`, `.markdown`, `.txt` |
| Save-as name | `{stem}.{locale_wire}{ext}` |
| Save overwrite | Never in v1 |
| Cwd | reserved_root only |

---

## Spec self-review

- Critical Codex items addressed with normative text (no optional forks).
- Open and translate remain one plan with phased tasks.
- Hard vs soft guarantees for code vs terminology explicit.
- No reliance on non-existent `save_file_copy` destination semantics.
