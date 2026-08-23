# Grok Session Images in Chat Design

## Status

Draft for review (2026-08-23). Locked in conversation: both inline preview
and click-to-open; Grok conversations only; `images/` only; session directory
first, workspace fallback. Approach **A (shared resolver)** chosen after a
Codex review of A/B/C. Do not implement until this spec is approved.

## Problem

Grok Build TUI treats `images/N.ext` as a file under the current Grok
**session** directory:

```text
$GROK_HOME/sessions/<url-encoded-cwd>/<session-id>/images/2.png
```

Grok agents are instructed to cite those files as a short session-relative
path so the TUI can render them. In Codeg they appear in assistant Markdown
as either:

```md
![目标 DAG overlay](images/2.png)
```

or an autolinked file badge around `` `images/2.png` `` (completed assistant
prose; `png` is already on the relative-autolink extension whitelist).

Codeg then does two wrong things:

1. **Inline image is blocked.** Assistant Markdown goes through Streamdown
   `remark → sanitize → harden → React`. `rehype-harden`'s `parseUrl` only
   treats `/`, `./`, and `../` as relative when `defaultOrigin` is unset.
   Bare `images/2.png` fails to parse, so harden replaces `<img>` with
   `[Image blocked: {alt}]`. `remark-file-uri-links` deliberately does not
   rewrite image destinations. `rehype-allow-codeg` preserves local **`<a
   href>`** through harden, not `<img src>`.
2. **Click opens the workspace copy.** The file badge calls
   `openFilePreview("images/2.png")`. `resolveOpenAbsolutePath` joins the
   active folder, producing `{workspace}/images/2.png`. That file is missing,
   `readFileBase64` fails, and `failOpenTab` toasts `unableOpenFile` with
   only the basename (`无法打开 2.png`).

The bytes are on disk. `GrokParser::find_session_dir` already locates the
session directory from the conversation `external_id`. File preview already
loads local Markdown images (preprocess to `./{abs}` + `PreviewImage` +
`readFileBase64`); chat `MessageResponse` does not.

Codex image generation already becomes in-position `image_generation` /
`generated-image` cards. Grok `image_gen` / `image_edit` / a Chrome
screenshot copied into `images/2.png` typically only write a file and cite
it in prose.

## Decision

One backend resolver, two consumers.

Do not rewrite persisted transcripts. Do not inject `image_generation`
blocks as the primary fix. Do not preprocess Markdown to absolute paths as
the source of truth (that splits existence checks across preprocess and
click).

```text
Grok assistant Markdown
  ![alt](images/2.png)          autolinked `images/2.png` badge
           \                        /
            v                      v
     GrokSessionImageContext (conversationId)
            \                      /
             v                    v
  resolve_grok_session_image(conversation_id, href)
             |
   session {GROK_HOME}/sessions/…/<id>/images/<file>
   else workspace {folder}/images/<file>
             |
             v
   { path, origin: session | workspace }
             |
     ┌───────┴────────┐
     v                v
  inline data URL   openFilePreview(abs path)
```

## Goals

- In a Grok conversation, `![alt](images/2.png)` renders as an inline image
  when the file exists in the session `images/` directory or, if not, in the
  workspace `images/` directory.
- Clicking an autolinked / explicit Markdown file badge for the same href
  opens that resolved file in the files pane (absolute path, existing image
  tab).
- Non-Grok conversations keep today's harden image block and today's
  workspace-relative open.
- Path traversal and symlink escape out of the two `images/` directories
  fail closed (invalid / not found), never an unconfined read of a new class
  of paths.
- Autolink scanning stays shape-only. Existence is checked only at
  resolve/read time.

## Non-Goals

- Codex / Cursor / other agents' local Markdown images.
- Relative paths outside `images/` (including `src/foo.ts` and nested
  `images/foo/bar.png`).
- `file://`, `data:`, POSIX-absolute, home-relative, or UNC image srcs in
  chat Markdown (still blocked).
- Rewriting JSONL / ACP content blocks / injecting `generated-image` parts
  (optional later enhancement for native `image_gen` tool results; not this
  spec).
- SVG inline preview (data-URL SVG is an XSS vector). SVG stays blocked as
  an `<img>` and is not part of the resolver allow-list.
- Filesystem watches or unlimited retries. Capped in-component retry covers
  the cite-before-write race.
- New toast copy. Missing files reuse `workspaceContext.unableOpenFile`.
- Changing autolink activation, extension whitelist, or badge visuals.
- Letting the webview load `file:` or session paths as raw `<img src>`.

## Product choices (locked)

| Choice | Value |
|--------|--------|
| UX | Inline preview **and** click-to-open |
| Conversations | `agent_type = grok` only |
| Path shape | Single segment `images/<file>` with raster image extension |
| `./images/<file>` | Allowed (strip one leading `./`) |
| Nested `images/a/b.png` | Rejected |
| Resolve order | Session `images/` if that **file** exists, else workspace `images/` |
| Missing both | Inline: component fallback (not the harden span). Click: existing open-failed toast |
| Collision | Session wins |
| Transcript | Unchanged |
| Autolink scan | No existence check |
| Inline formats | `png`, `jpg`, `jpeg`, `webp`, `gif` (case-insensitive) |
| Streaming | Enabled on live Grok assistant Markdown, not only completed turns |

## Href gate (shared rule)

Frontend (rehype + click) and backend (resolver) implement the **same**
predicate. Duplicate the rule in TS and Rust; do not share a runtime. Test
the same fixtures on both sides.

A string is a Grok session image ref iff after trim + `decodeURIComponent`
(invalid encoding → reject) + `\` → `/`:

1. It is not empty and has no `://` scheme and is not protocol-relative
   (`//…`).
2. It does not start with `/`, `~`, or a Windows drive (`X:`).
3. Query and hash are stripped; if anything other than optional `#…` / `?…`
   remains besides the path, reject.
4. One leading `./` may be stripped. `../` is never accepted.
5. The path matches `^images/([^/]+)$` (exactly one extra segment).
6. That segment is a non-empty filename with no `.` or `..` as the whole
   name, and no additional `.` / `..` path components.
7. The extension (after the last `.`, which must not be at index 0) is one
   of `png` `jpg` `jpeg` `webp` `gif`, ASCII-folded.

Examples:

| Input | Gate |
|-------|------|
| `images/2.png` | pass |
| `./images/3.jpg` | pass |
| `images/2.PNG` | pass |
| `images/foo/bar.png` | reject |
| `../images/2.png` | reject |
| `/images/2.png` | reject |
| `images/../../etc/passwd` | reject (not a single segment) |
| `images/2.svg` | reject |
| `docs/a.md` | reject |
| `file:///tmp/x.png` | reject |

## Architecture

### Backend: `resolve_grok_session_image`

New command, not a folder of helpers stuffed into `folders.rs`.

```text
src-tauri/src/commands/grok_session_image.rs
  + Tauri command + `_core`
  + web handler + router POST `/resolve_grok_session_image`
  + generate_handler registration (desktop)
```

**Input**

```ts
{
  conversationId: number
  href: string
  includeData?: boolean // default false
}
```

**Output** (found)

```ts
{
  path: string // canonical absolute filesystem path
  origin: "session" | "workspace"
  mimeType: "image/png" | "image/jpeg" | "image/webp" | "image/gif"
  dataBase64?: string // only when includeData is true
}
```

**Errors**

| Case | Code |
|------|------|
| Conversation missing, not grok, or no `external_id` | `invalid_input` / `not_found` (do not leak existence of other agents' files) |
| Href fails the gate | `invalid_input` |
| Both candidates missing or not regular files | `not_found` |
| Canonical path escapes the allowed `images/` directory (symlink or `..`) | `invalid_input` |
| `includeData` and file exceeds existing `read_file_base64` size cap | same as `read_file_base64` (`invalid_input` + detail) |

**Algorithm**

1. Load conversation by id. Require `agent_type == grok` and a non-empty
   `external_id`. Load its folder (workspace root).
2. Run the href gate. Extract `filename`.
3. **Session candidate:** `GrokParser::find_session_dir(external_id)` (same
   loose fallback as `grok_updates_jsonl_path` if the jsonl is not there
   yet). Join `images/filename`. `canonicalize` (or equivalent
   `realpath`-style resolve). Require the result is a file and is still
   under `canonicalize(session_dir/images)` (prefix check on the canonical
   directory, not a string-prefix on the unnormalized join). Follow the
   same symlink discipline as `read_workspace_file_base64` / `open_no_follow`
   so a dangling or swapped symlink cannot point outside that directory.
4. If step 3 hits, return `origin: "session"`. Optionally read bytes.
5. **Workspace candidate:** join `{folder.path}/images/{filename}` with the
   existing workspace-confined resolver (`resolve_tree_path` /
   `read_workspace_file_base64` containment), not unconfined
   `read_file_base64` of a client-supplied absolute path. Return
   `origin: "workspace"` if it is a regular file.
6. Else `not_found`.

Do not search other session subdirectories. Do not search the workspace
outside `images/`.

`includeData: true` is the inline-preview path so the client does not take
the returned `path` and call unconfined `read_file_base64` for display.
Click-to-open uses `includeData: false` and `openFilePreview(path)` — that
absolute path is produced by this command, not by the model.

### Frontend context

```ts
// src/components/ai-elements/grok-session-image-context.tsx
type GrokSessionImageContextValue = {
  conversationId: number
} | null
```

`MessageListView` already receives `conversationId` and `agentType`. When
`agentType === "grok"`, wrap the transcript tree (historical + live) with
the provider. Other agents wrap nothing (`null`).

Consumers:

- `MessageResponse` — chooses the grok-image rehype array and `img`
  component when context is non-null.
- `useOpenLinkOrFile` — if context is set **and** the clicked href passes
  the gate, resolve then `openFilePreview(abs)`.

Live `MessageResponse` (including `LiveTextSegment`) sits under the same
provider, so streaming Grok turns get the same img pipeline without
threading a new prop through every memoized live row. React `memo` still
re-renders when a component's **own** `useContext` value changes.

### Harden: preserve only matching `<img src>`

Extend `rehypePluginsAllowingCodeg(defaults, { preserveGrokSessionImageSrc?:
boolean })`.

Today the harden wrapper snapshots `<a href>` that `shouldPreserveLocalPathHref`,
swaps in a placeholder, runs harden, restores. Add the same for `<img src>`
**only when the option is true** and `isGrokSessionImageRef(src)`.

Default remains false. Non-Grok `MessageResponse` keeps the current plugin
array (local images stay `[Image blocked: …]`).

Do not preserve arbitrary `isLocalPathLike` image srcs. That would silently
change policy for `![x](docs/foo.png)` in every conversation.

Sanitize: relative `images/2.png` has no scheme, so it already survives
`rehype-sanitize`. The only gate that currently destroys the node is harden.

`remark-file-uri-links` stays as-is (images untouched). A `file://` image
is still blocked.

### Inline component

`GrokSessionImage` implements Streamdown `components.img` when context is
set. Wired in `MessageResponse` the same way `markdownLinkComponents`
overrides `a` (merge so `a` still always wins).

Behavior:

1. If `src` fails the gate → render children/alt as plain text (do not
   attempt resolve).
2. Call `resolveGrokSessionImage({ conversationId, href: src, includeData:
   true })`.
3. On success, render `<img src="data:{mimeType};base64,{dataBase64}">`
   with the original alt, constrained like generated-image thumbnails
   (`max-width: 100%`, click can open the same abs path via
   `openFilePreview`).
4. On `not_found` while still mounted: retry at 400ms, 1200ms, 2500ms
   (three extra attempts). Covers cite-before-write in the same turn.
   Stop on success, unmount, or non-`not_found` errors.
5. After retries fail: keep alt visible in a compact muted line. Do **not**
   show `[Image blocked: …]` (that string is harden's; we already preserved
   the node). Do not toast on inline miss (the click path still toasts).

No raw filesystem `src`. No `blob:` of an unvalidated path.

### Click-to-open

`useOpenLinkOrFile` (shared by MarkdownLink file badges):

```text
if grok context && isGrokSessionImageRef(url):
  resolve includeData=false
  on hit: openFilePreview(result.path)  // absolute → skip folder join
  on not_found / invalid: fall through to today's relative open
    (workspace join + existing toast)
else:
  existing parseLocalFileTarget / external URL behavior
```

Do not change `resolveOpenAbsolutePath` globally. A non-Grok
`images/2.png` badge still opens the workspace file. A Grok badge that
failed the resolver still falls through, so a workspace-only file still
opens.

`FileReferenceActions` (reveal in file manager / copy path) calls the same
resolver when the menu opens under Grok context and a gated href. Copy/reveal
use `result.path` on hit; on miss they keep the badge href. Do not rewrite
the transcript href.

## Data flow

### Inline

```text
Grok MessageListView
  → GrokSessionImageProvider(conversationId)
  → MessageResponse / StreamingMarkdownDocument / LiveTextSegment
  → rehypePluginsAllowingCodeg(..., { preserveGrokSessionImageSrc: true })
  → <img src="images/2.png" alt="…">
  → GrokSessionImage
  → resolve_grok_session_image(includeData=true)
  → <img src="data:image/png;base64,…">
```

### Click

```text
MarkdownLink (file badge, href still "images/2.png")
  → useOpenLinkOrFile
  → resolve_grok_session_image(includeData=false)
  → openFilePreview("/home/…/sessions/…/images/2.png")
  → existing image tab (readFileBase64 on the abs path)
```

Historical replay and live streaming share this path. The provider is the
only Grok-specific branch; parsers do not grow Markdown scanners.

## Error handling and races

| Event | Inline | Click |
|-------|--------|-------|
| Href fails gate | Plain alt/text | Existing link/file logic |
| Session file exists | Show session image | Open session file |
| Only workspace file exists | Show workspace image | Open workspace file |
| Neither exists | Retry then muted alt | Relative open → toast `无法打开 2.png` |
| Cited before write | Retry window | User can click again after the tool finishes |
| Conversation is not grok | Unreachable (no provider / no img preserve) | Unreachable |
| Session dir deleted | Workspace fallback, else miss | Same |
| File too large | Same error as attach/preview cap | Open may still fail via existing cap |
| Resolver invalid_input | Treat as miss (no extra toasts on inline) | Fall through |

Do not retry invalid hrefs. Do not retry permission / size errors.

## Security

- Href gate is the first filter (no `..` segments, no nested paths, no
  schemes).
- Session read is confined to `canonicalize(session_dir/images)/`.
- Workspace read uses the existing workspace-confined path helper, not a
  new unconfined join.
- Inline bytes leave the backend only through `includeData` on this
  command, not by teaching Markdown to accept `file:`.
- `read_file_base64` remains the generic primitive for the files pane
  after a **resolver-produced** absolute path. This spec does not add a
  new public “read any path” surface.
- Non-Grok harden behavior is unchanged, including the intentional
  `[Image blocked: …]` for local Markdown images.

## Testing

### Pure gate (TS + Rust)

Table-driven cases from the href-gate table, plus `%2e%2e` / encoded slash
tricks, trailing spaces, `images/2.png#L1` (hash stripped → pass),
`images/2.png?x=1` (query stripped → pass).

### Resolver (Rust, `#[cfg(test)]` with temp GROK_HOME + temp folder)

- Grok conversation + session file → `origin=session`, canonical path.
- Grok conversation + only workspace file → `origin=workspace`.
- Both exist → session wins.
- Neither → `not_found`.
- Non-grok conversation id → error, no file read.
- `images/../../…` and symlink pointing outside `images/` → `invalid_input`.
- `includeData` returns correct mime + base64 for a tiny PNG.

### Harden (vitest, real Streamdown plugin, extend `rehype-allow-codeg.test.ts`)

- Default plugins: `![x](images/2.png)` still becomes `[Image blocked: x]`.
- `{ preserveGrokSessionImageSrc: true }`: `<img src="images/2.png">`
  survives with that src.
- Same option still blocks `![x](docs/foo.png)` and `file://` images.
- Local `<a href="docs/a.md">` preservation unchanged.

### Chat wiring (vitest)

- Grok `MessageResponse` under the provider does not render
  `[Image blocked: 目标 DAG overlay]` for `![目标 DAG overlay](images/2.png)`
  (mock resolver + data).
- Non-Grok `MessageResponse` still shows the blocked placeholder.
- File-badge click under the provider calls resolve then
  `openFilePreview` with the absolute session path (mock).
- `MessageResponse` memo / live row: provider present is enough; no
  autolink required for the **image** syntax path.

No full `pnpm test` / `cargo test --features test-utils` required for the
implementation plan's inner loop; targeted files plus the existing
MessageResponse Streamdown tests.

## Files to touch

| File | Role |
|------|------|
| `src-tauri/src/commands/grok_session_image.rs` | Gate + resolve + `_core` + unit tests |
| `src-tauri/src/commands/mod.rs` | Module |
| `src-tauri/src/lib.rs` | Tauri handler |
| `src-tauri/src/web/handlers/files.rs` (or a grok handler) | HTTP |
| `src-tauri/src/web/router.rs` | Route |
| `src/lib/markdown/grok-session-image.ts` | TS gate |
| `src/lib/markdown/grok-session-image.test.ts` | Gate tests |
| `src/lib/api.ts` / `src/lib/tauri.ts` | Client |
| `src/components/ai-elements/rehype-allow-codeg.ts` | Optional img preserve |
| `src/components/ai-elements/rehype-allow-codeg.test.ts` | Harden tests |
| `src/components/ai-elements/grok-session-image-context.tsx` | Provider |
| `src/components/ai-elements/grok-session-image.tsx` | `<img>` loader |
| `src/components/ai-elements/message.tsx` | Rehype + img override |
| `src/components/ai-elements/link-safety.tsx` | Click resolve |
| `src/components/message/message-list-view.tsx` | Provider wrap |
| `src/components/ai-elements/message-file-uri-pipeline.test.tsx` or a new grok-image pipeline test | End-to-end Streamdown |

Keep `remark-file-uri-links.ts` unchanged.

## Risks

| Risk | Mitigation |
|------|------------|
| Cite-before-write | Capped retries; click still works later |
| Harden option leaks to non-Grok | Default off; only MessageResponse under provider opts in |
| `read_file_base64` unconfined after open | Path is resolver output; workspace branch uses confined resolve |
| Workspace `images/` stolen in Grok chats | Intended: session wins if both exist |
| Encoded traversal in href | Decode before the single-segment match |
| Live `MessageResponse` memo | Context subscription, not a new ignored prop |
| Session 33 already on disk | Historical replay uses the same command; no parser change needed |

## Later, not this spec

Map Grok `image_gen` / `image_edit` **tool results** onto `generated-image`
cards (approach B) so generated pictures also show as Codex-style cards
without depending on Markdown. That still needs this resolver for
screenshot-then-cite and for badge clicks.
