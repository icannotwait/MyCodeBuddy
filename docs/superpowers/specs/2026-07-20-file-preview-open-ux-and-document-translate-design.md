# File Preview Open UX and Document Translation

Date: 2026-07-20

Status: Design written; awaiting user review before implementation plan

## Summary

This change improves the file workspace open/close experience and adds an
on-demand document translation action that reuses the configured automatic-title
agent (`auto_title_agent`).

1. **Open UX**: clicking a file that cannot be loaded must not leave an error
   tab (“error window”). Show a toast only. Successfully opening a **new** file
   tab defaults the files pane to maximized. `Escape` closes the active file
   tab.
2. **Document translation**: for Markdown / plain-text documents, a toolbar
   action translates content into the current application interface language via
   the same global title agent, opens a **read-only preview tab**, and offers
   **Save as…**. Code blocks and proper/technical English terms must not be
   translated away.

Both desktop (Tauri) and server (Axum) transports expose the same behavior.

## Goals

- On cold-load failure of a newly opened file tab: toast + remove the tab; never
  surface the current `unableLoadContent` error body as a sticky error window.
- On first successful seed of a **new** file tab: set `filesMaximized = true`.
- Re-activating an already-open tab does **not** force maximize.
- `Escape` closes the current file tab when the files workspace is interactive
  (fusion + files pane active, or files maximized). Dialogs, composers, and
  other focus traps that already consume Escape keep precedence.
- Toolbar “Translate to current language” for Markdown / plain-text file tabs
  only, using `auto_title_agent` and the current UI locale.
- If the title agent is unset or unavailable: toast that points the user to
  conversation-experience settings; do not start a runner.
- Translation result lands in a new read-only tab; optional Save as… writes a
  new path without overwriting the source.
- Preserve fenced/inline code and proper English terminology (APIs, brands,
  identifiers, paths, commands).

## Non-goals

- Auto-translating on every file open.
- Translating source code, images, Office binaries, or HTML preview shells as a
  first-class path (HTML may be text-openable but is out of the translate
  button whitelist unless listed below).
- Changing the automatic conversation title job coordinator, job table, or
  title-finalized persistence model.
- Side-by-side original/translation split view.
- Offline / non-agent machine translation.
- Per-project translation-agent overrides.
- Streaming partial translation into the editor (one-shot result is enough).
- Replacing native OS “open with” for unsupported binaries beyond toast messaging.

## Confirmed Product Decisions

| Area | Decision |
| --- | --- |
| Unopenable file | Any cold-load failure → toast only, no error tab retained |
| Expand default | New file tab → `filesMaximized = true` |
| Existing tab click | Activate only; do not force maximize |
| Escape | Close current file tab (empty workspace → conversation mode) |
| Translate entry | Toolbar button after open |
| Translate types | Markdown / plain text only (`.md`, `.mdx`, `.markdown`, `.txt`) |
| Translate agent | Global `auto_title_agent` (same setting as auto titles) |
| Agent off / missing | Toast → configure in settings |
| Translate output | Read-only new tab + Save as… |
| Code / terms | Do not translate code blocks; keep proper/technical English |
| Open architecture | Seed loading tab first; on cold failure close + toast (Approach 1) |

## Current Behavior (baseline)

### Open path

`openFilePreview` in `workspace-context.tsx`:

1. Resolves absolute path, builds a seed tab, runs `decideLoad`.
2. Cold open calls `seedLoadingTab` → tab appears, files pane activates, mode
   becomes `fusion`.
3. Fetch succeeds → content settled on the tab.
4. Fetch fails → `rejectTab` writes `unableLoadContent` into tab content and
   `saveState: "error"`. The tab remains open (the “error window”).

`filesMaximized` defaults to `false`. It resets when all file tabs close.
`seedLoadingTab` already defaults Markdown/HTML to **preview** mode
(`previewFileTabIds`); that is independent of maximize.

### Close / shortcuts

`FileWorkspaceTabBar` handles close-current / close-all via configurable
shortcuts (`close_current_tab` defaults to `Mod+W`). There is no Escape handler
for file tabs today.

### Title agent

Backend owns `auto_title` with:

- Settings: `auto_title_agent: Option<AgentType>`
- `HiddenAgentRunner`: isolated one-turn ACP session
  (`ConnectionPurpose::InternalTitle`, `EventEmitter::Noop`)
- Sessions registered in `InternalAgentSessionRegistry` so they never appear in
  Codeg conversation lists
- Prompt language driven by `AppLocale` (system / interface language)

Document translation reuses this agent setting and the same class of hidden
runner, not the durable auto-title job table.

---

## Architecture

### Component map

| Unit | Responsibility | Depends on |
| --- | --- | --- |
| `openFilePreview` / load settle path | Cold-fail close + toast; maximize on new seed | `toast`, `setFilesMaximized`, tab state |
| `seedLoadingTab` | When inserting a **new** file tab, maximize | files maximized state |
| `FileWorkspaceTabBar` keyboard handler | Escape → `closeFileTab(active)` with guards | shortcuts, view state |
| Translate toolbar control | Visibility, busy state, invoke translate | file tab, settings, API |
| `translate_document` command/API | Validate, protect code, run agent, return text | title agent, locale, runner |
| `DocumentTranslateRunner` (or extended hidden runner) | One-shot internal ACP prompt/collect | ConnectionManager, internal registry |
| Markdown protect/restore helpers | Fence + inline code placeholders | pure functions, unit-tested |
| Translation result tab | Read-only in-memory tab + Save as… | file tabs, `saveFileCopy` / save dialog |

### Data flow — open failure

```text
User clicks file
  → openFilePreview
  → seedLoadingTab (new) → filesMaximized = true
  → readFileForEdit / image / office path
  → success: settle content
  → cold failure:
        closeFileTab(tabId)   // or remove without rejectTab body
        toast.error(message)
        // do NOT rejectTab with unableLoadContent for cold opens
```

### Data flow — translate

```text
User clicks Translate
  → FE checks: file kind whitelist, non-empty content, agent configured
  → FE optional busy UI on source tab / button
  → FE calls translate_document({ path?, content, locale?, sourceLanguage? })
  → BE:
        load auto_title_agent; if None → error code AgentNotConfigured
        protect markdown code spans → placeholders
        build translate prompt (target locale + rules)
        Hidden-style runner one turn
        restore placeholders
        return { translatedText, locale, titleHint }
  → FE opens read-only tab with translated content (preview mode for md)
  → User may Save as… → path picker / derived name → write new file → open that path
```

---

## 1. File open / close UX

### 1.1 Cold-load failure: no error tab

**Definition — cold load**: the tab was created in this open attempt and has
never reached a successful ready state with real content (only loading or empty
seed). Includes first open and first open after the user closed the tab.

**Definition — warm failure**: the tab already had successfully loaded content
(or the user is forcing `reload` on an existing ready tab).

| Case | Behavior |
| --- | --- |
| Cold load failure | Remove the tab; `toast.error` with a short message (path basename + reason). Do not call today’s `rejectTab` content-write path. |
| Warm reload failure | Keep the previous content; `toast.error`. Do **not** replace the body with `unableLoadContent` and do **not** close the tab. |
| In-flight superseded (stale gen) | No toast, no UI change (existing `settleFetch` false). |
| User closed tab mid-load | No resurrection (existing reload-skip); no toast. |

Implementation sketch:

- Introduce `rejectColdOpen(tabId, errorMessage)` or a flag on reject:
  - cold: if tab still exists and never ready → `closeFileTab` + toast
  - warm: toast only; leave content; clear `loading`
- Track “ever successfully loaded” per tab (e.g. `readyOnce` on the tab, or
  infer: cold seed has empty content + loading, never settled success). Prefer
  an explicit `hasLoadedSuccessfully: boolean` on `FileWorkspaceTab` defaulting
  false, set true on successful settle for file/image/office.

Office and image paths follow the same cold/warm rules.

Toast copy (i18n, all 10 locales): e.g. `unableOpenFile` —
“Could not open {name}: {message}”.

### 1.2 Default maximize on new file tab

In `seedLoadingTab` (or immediately after deciding a **new** tab is created):

```text
setFilesMaximized(true)
```

Rules:

- Only when a **new** tab is inserted (not cache-hit activate, not in-flight
  dedup activate).
- Diff / rich-diff tabs: apply the same maximize-on-new-seed for consistency
  when opened from the tree/git UI (single layout rule: any new file-workspace
  tab seeds maximized).
- Existing effect that clears maximize when `fileTabs.length === 0` remains.
- `activateConversationPane` still clears maximize (existing).
- Mobile layout has no maximize overlay; maximize state may still flip but the
  mobile shell continues to show the files section when `activePane === "files"`.
  No separate mobile-only maximize UI is required.

### 1.3 Escape closes current file tab

Extend the keyboard handler in `FileWorkspaceTabBar` (or a co-located hook used
by the files chrome):

```text
if key !== Escape → return
if should not handle files shortcuts → return
  (fusion && (activePane === "files" || filesMaximized))  // same as close tab
if event defaultPrevented or target is editable inside a modal → respect
if no activeFileTabId → return
preventDefault
closeFileTab(activeFileTabId)
```

Precedence:

1. Open modal/dialog/sheet that uses Escape (Radix) — do not steal if focus is
   inside dialog content, or if a higher-priority listener already handled it.
2. Prefer listening in **bubble** phase after dialogs, or skip when
   `event.defaultPrevented`.
3. Do not close when focus is in the chat composer even if files are maximized
   and conversation is `inert` — when maximized, conversation is inert so
   Escape on files is correct.
4. Configurable shortcut for close remains; Escape is an **additional** fixed
   binding for close-current file tab in the files context (not rebindable in
   v1 unless shortcuts system already supports a free key — keep Escape fixed
   to match user request).

When the last tab closes, existing logic returns mode to `conversation` and
clears maximize.

### 1.4 Loading flash on cold failure

Approach 1 accepts a brief loading tab that disappears on failure. No extra
global spinner is required. If the failure is faster than paint, the user may
only see the toast.

---

## 2. Document translation

### 2.1 Eligibility (toolbar)

Show the Translate control when **all** hold:

- Active tab `kind === "file"`
- Not loading, not cold-error path
- Path extension in whitelist (case-insensitive):

  | Extension | Included |
  | --- | --- |
  | `.md`, `.mdx`, `.markdown` | yes |
  | `.txt` | yes |
  | others | no |

- Content is non-empty after trim (or file has length > 0)
- Not an image/office language shell

Hide (do not disable-with-tooltip only) when ineligible, to reduce chrome noise.
Optional: if agent is off, still show the button; click → toast to configure
(so users discover the dependency).

### 2.2 Locale and agent

- **Target language**: current application interface language
  (`AppLocale` / system language settings already used by auto-title). Frontend
  may pass the active UI locale wire id; backend validates and falls back to
  loaded system language if missing/invalid.
- **Agent**: `load_auto_title_agent_from` — same as titles. No second setting.
- **Unavailable agent**: if set but not enabled/installed at run time → error
  `AgentUnavailable` with toast; do not silently pick another agent.

### 2.3 API surface

Shared core + Tauri command + Axum handler:

```rust
// Request
pub struct TranslateDocumentParams {
    /// Absolute path for display / default Save-as name; optional if content-only.
    pub path: Option<String>,
    /// Full document text (UTF-8). Required.
    pub content: String,
    /// Optional wire locale; backend resolves AppLocale.
    pub locale: Option<String>,
}

// Response
pub struct TranslateDocumentResult {
    pub translated_content: String,
    pub locale: String, // wire id
    pub source_path: Option<String>,
}
```

Errors (typed / string messages consistent with project patterns):

| Code / message key | When |
| --- | --- |
| `agent_not_configured` | `auto_title_agent` is None |
| `agent_unavailable` | agent not enabled or not installed |
| `content_empty` | empty content |
| `content_too_large` | over size limit |
| `unsupported_type` | optional BE check if path extension present and not whitelisted |
| `cancelled` / `timeout` | runner cancelled or deadline |
| `translate_failed` | runner/normalize failure |

**Size limit**: reject when `content` char length > **120_000** Unicode scalars
(~large README). Toast explains the limit. No chunking in v1.

**Timeout**: overall deadline **180s** (documents are larger than titles; titles
use 90s). Cancellation token if the client disconnects or user cancels (v1:
button shows busy; optional cancel if transport supports abort — at least
disable double-submit).

### 2.4 Runner design

Reuse the hidden-agent pattern without coupling to `auto_title_jobs`:

- Add `ConnectionPurpose::InternalTranslate` (or a shared `InternalUtility`
  with purpose tag in `InternalSessionPurpose`).
- Register external session IDs in `InternalAgentSessionRegistry` with a
  translate purpose so discovery ignores them (same as titles).
- `EventEmitter::Noop`; unlinked one-turn prompt; no tools required in prompt
  instructions (“Do not use tools”).
- Working directory: source file’s parent if path known and under a workspace
  root; else a neutral temp/workdir policy consistent with title runner
  (prefer the active folder root when available from FE).
- Collect final visible assistant text only (same delta collection style as
  title runner, but **do not** apply 80-scalar title normalization).
- Light post-process: trim outer markdown fences if the model wraps the entire
  document in a single ``` pair incorrectly; do not strip internal structure.

Extract shared “spawn internal → register → prompt → collect → disconnect”
from title runner only if the diff stays small; otherwise duplicate a thin
`HiddenDocumentRunner` beside `HiddenAgentRunner` to avoid risky title
regressions. Prefer shared private helpers over a forced mega-refactor.

### 2.5 Code and proper-English protection

#### Structural protection (Markdown)

Before the model sees the text:

1. Replace fenced code blocks (```` ``` ```` / `~~~`, with optional language
   tag) with stable tokens: `⟦CODE_0⟧`, `⟦CODE_1⟧`, …
2. Replace inline code `` `...` `` with `⟦INLINE_n⟧`.
3. Do **not** protect indented-code-only edge cases in v1 if ambiguous; fenced
   + inline cover the common case.

After the model returns:

1. Restore all placeholders by index. If any placeholder is missing or altered,
   attempt fuzzy restore (exact token search); on failure, append a short note
   in toast that some code regions may need manual check, still show best-effort
   text.
2. Placeholders that appear **extra** in the output are stripped.

Helpers live in a pure module (Rust preferred so BE is source of truth;
optional TS mirror only for tests if FE previews protection — default **BE
only**).

#### Prompt rules (semantic protection)

Prompt sketch (locale name from `locale_display_name`):

```text
Translate the following document into {Language}.
Return only the full translated document body.
Do not use tools. Do not wrap the entire answer in an outer code fence.
Do not add a preface or commentary.

Rules:
- Preserve Markdown structure (headings, lists, links, tables).
- Leave every placeholder like ⟦CODE_0⟧ and ⟦INLINE_1⟧ exactly unchanged.
- Do not translate source code, shell commands, file paths, URLs, or
  identifiers.
- Keep proper nouns, product names, API names, and established technical
  English terms in English when that is standard practice
  (e.g. pull request, commit, endpoint names, library names).
- Translate surrounding prose and documentation narrative into {Language}.

Document:
{protected_content}
```

“专有英语” is enforced primarily by these instructions; structural protection
hard-guarantees code. No glossary UI in v1.

### 2.6 Frontend result tab and Save as…

**Result tab**:

- New `FileWorkspaceTab` with `kind: "file"`, `readonly: true`,
  `path: null` or a synthetic display path.
- Prefer a dedicated id scheme: `translate:{sourceTabId}:{locale}:{stamp}` so it
  does not collide with real paths.
- `title`: `{basename} ({localeDisplay})` e.g. `README.md (简体中文)` or
  `README.md (zh-CN)` — use short locale label from existing i18n helpers.
- `content`: translated markdown/text; for `.md*` enable preview mode by default
  (same as normal md seed).
- Opening a result tab counts as a **new** tab → maximize (consistent rule).
- Dirty state: false; editing disabled until Save as… produces a real path tab.

**Save as…**:

- Button on the translation tab toolbar (or file tab bar overflow when the
  active tab is a translation result).
- Default filename: `{stem}.{localeWire}{ext}` e.g. `README.zh_cn.md` or
  `README.zh-CN.md` — pick one convention and document it:
  **`{stem}.{localeWire}{ext}`** with wire id as stored (`zh_cn`) →
  `README.zh_cn.md`.
- Default directory: same directory as source path when known; else active
  folder root.
- Use existing write APIs (`save_file_copy` / create + save) after path
  confirmation. On web, follow existing file-write permission patterns.
- After successful write: open the real file via `openFilePreview` (new tab or
  replace flow per existing open rules) and optionally close the ephemeral
  translation tab.

**Concurrency**:

- One in-flight translation per source tab id (button disabled / spinner).
- A second click while busy is ignored.
- Global multi-tab: allow different source tabs to translate in parallel up to
  a small backend semaphore (e.g. 2) to avoid spawning many CLIs; excess waits
  or returns busy error.

### 2.7 Settings / discovery

No new setting. Conversation-experience settings copy may add one sentence:
“The automatic title agent is also used for document translation.”

Toast when not configured: link-like text is plain toast in v1 (“Set an
automatic title agent in Settings → …”). Exact settings path string uses
existing navigation labels.

---

## 3. Error handling matrix

| Situation | UX |
| --- | --- |
| Cold open fail | Close tab + error toast |
| Warm reload fail | Keep content + error toast |
| Translate, agent off | Error toast, stay on source tab |
| Translate, agent unavailable | Error toast |
| Translate, too large | Error toast with limit |
| Translate, timeout/fail | Error toast; no empty result tab |
| Translate, placeholder restore partial | Open tab + warning toast |
| Save as cancelled | No-op |
| Save as fail | Error toast; keep ephemeral tab |

---

## 4. Internationalization

Add keys to all 10 locale catalogs under `Folder.fileWorkspace` (or adjacent):

- `unableOpenFile`
- `translateToCurrentLanguage` (button)
- `translating`
- `translateAgentNotConfigured`
- `translateAgentUnavailable`
- `translateContentTooLarge`
- `translateFailed`
- `translatePlaceholderWarning`
- `saveTranslationAs`
- `translationTabTitle` (`{name}`, `{locale}`)

Reuse existing maximize/close strings where possible.

---

## 5. Testing

### Frontend

- `openFilePreview` cold failure: no residual tab; toast called; maximize reset
  if no tabs remain.
- Warm reload failure: tab remains with prior content; toast called.
- New seed sets `filesMaximized` true; activate existing does not toggle.
- Escape closes active file tab when files context active; ignored when no
  active tab.
- Translate button visibility for md/txt vs `.rs` / image.
- Translate success opens readonly tab; agent-null path toasts.

### Backend / pure

- Markdown protect/restore round-trip with nested fences, inline code, multiple
  blocks.
- Prompt includes locale display name and placeholder rule (unit on builder).
- `translate_document` core: agent none → error; empty → error; oversize →
  error.
- Runner integration tests with fake connection driver (mirror title runner
  style) for happy path and timeout.
- Internal session registration uses translate purpose and is filtered from
  discovery.

### Manual smoke

- Open missing file from tree → toast only.
- Open README.md → maximized; Escape closes.
- Translate README with code fences → code unchanged; prose in UI language.
- Save as → new file on disk opens correctly.

---

## 6. Implementation sequence (for later planning)

1. Open UX: cold-fail close + toast; `hasLoadedSuccessfully`; maximize on seed;
   Escape handler; tests.
2. Protect/restore pure helpers + unit tests.
3. Translate runner + command/handler + API client.
4. Toolbar button, busy state, result tab, Save as….
5. i18n strings (10 locales).
6. Docs tweak on conversation-experience settings blurb.

---

## 7. Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Model still translates code despite prompt | Placeholder protection |
| Model corrupts placeholders | Restore validation + warning toast |
| Long docs hit agent limits | 120k scalar cap + 180s deadline |
| Title runner refactors break titles | Prefer thin parallel runner or shared helpers with title tests green |
| Escape steals from dialogs | defaultPrevented / focus checks |
| Maximize surprises power users | Only on **new** tab seed; manual restore still available |
| Ephemeral tab has no path for watchers | No workspace watch; readonly |

---

## 8. Open implementation notes (resolved defaults)

These are fixed defaults for implementers; change only if implementation hits a
hard platform constraint:

- Size limit: **120_000** Unicode scalars.
- Translate deadline: **180** seconds.
- Save-as default name: `{stem}.{locale_wire}{ext}`.
- Whitelist: `.md`, `.mdx`, `.markdown`, `.txt`.
- Escape: fixed key, files context only.
- No chunked translation in v1.

---

## Spec self-review

- No TBD/TODO placeholders left for product decisions.
- Open UX and translation share layout (maximize) but not failure paths.
- Scope is one feature slice suitable for a single implementation plan with
  ordered tasks.
- “专有英语” interpreted as proper nouns + established technical English, with
  structural code protection explicit.
