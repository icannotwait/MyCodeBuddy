# Grok Session Images in Chat Design

## Status

Draft for review (2026-08-23), revised after a repository-backed design
review. The product choices already locked in conversation remain unchanged:
inline preview and click-to-open, Grok conversations only, the single-level
`images/` namespace only, and session-first/workspace-second resolution.

Approach A remains selected: one backend resolver is authoritative for both
rendering and opening. Do not implement until this revised spec is approved.

## Repository-backed review findings closed

The original draft had several implementation and security gaps. This revision
closes them explicitly:

- replaces the possibly virtual runtime conversation id with the positive
  persisted database id;
- narrows activation from the whole transcript tree to top-level assistant text,
  so nested tool/plan/reasoning Markdown cannot inherit the feature;
- retags only matching image nodes instead of overriding every `img`, preserving
  Streamdown's existing remote-image behavior;
- makes a gated resolver rejection terminal, removing the generic-open symlink
  bypass;
- opens files-pane images from resolver-returned bytes, removing the second
  unconfined read and its validation/read race;
- resolves the session before loading the folder and honors `origin_cwd` for
  reparented conversations;
- replaces lossy `Option` session lookup with fallible, injectable, ambiguity-
  rejecting lookup and validates `external_id` before joining it;
- defines percent decoding, suffix splitting, byte limits, portable filename
  rules, and cross-language fixtures precisely;
- distinguishes absent/not-ready files from rejected/error states, and makes
  live retries bounded, single-flight, and collision-aware;
- validates raster headers and dimensions rather than trusting an extension and
  compressed byte count alone;
- makes context-menu resolution asynchronous and fail-closed; and
- marks files-pane results as snapshots so watchers or memory eviction cannot
  silently route them back through the generic reader.

## Problem

Grok Build stores images produced during a session below the session directory:

```text
$GROK_HOME/sessions/<encoded-cwd>/<session-id>/images/2.png
```

Grok cites the image from assistant Markdown using a session-relative path:

```md
![目标 DAG overlay](images/2.png)
```

It may also cite the same path as inline code or an explicit link. Completed
top-level assistant prose already autolinks `` `images/2.png` `` because `png`
is on the relative-path extension whitelist.

Codeg currently mishandles both forms:

1. Streamdown renders assistant Markdown through `remark -> sanitize -> harden
   -> React`. With no `defaultOrigin`, `rehype-harden` does not accept the bare
   relative image source `images/2.png`, so it replaces the image with
   `[Image blocked: ...]`. `rehype-allow-codeg` preserves selected local
   `<a href>` values, but not `<img src>` values.
2. A file badge routes `images/2.png` to `openFilePreview`. That action joins a
   relative path to the active folder, not the Grok session directory, so it
   either opens the wrong workspace collision or fails with the existing
   `unableOpenFile` toast.

The bytes already exist. `GrokParser` can locate a session directory from the
conversation's `external_id`; the missing piece is a conversation-scoped,
confined resolver that both frontend paths use.

Codex image generation already produces structured `generated-image` cards.
Grok `image_gen`, `image_edit`, and screenshot workflows commonly only write a
file and cite it in prose, so parser-side generated-image synthesis is not a
complete substitute.

## Decision

Use one backend command to validate and resolve the model-authored reference.
It optionally returns the validated bytes. The frontend activates it only in
top-level Grok assistant Markdown and never rewrites persisted transcripts.

```text
MessageListView(runtime conversation id)
  -> durable DB conversation id
  -> Grok conversation context
  -> top-level assistant Markdown scope
       |                                  |
       | ![alt](images/2.png)              | file badge images/2.png
       v                                  v
  preserve + retag matching img       useOpenLinkOrFile
       |                                  |
       +------ resolve_grok_session_image-+
                         |
              session images/<file>
              else conversation workspace images/<file>
                         |
              confined path + optional bytes
                    /               \
                   v                 v
             inline data URL   resolved image tab
```

The resolver, not the frontend, decides whether the session or workspace file
wins. A resolver failure for an otherwise gated Grok image is terminal for
that action; it must never fall through to the generic workspace-relative
reader, because that would bypass the resolver's symlink and containment
checks.

## Goals

- Render a valid `![alt](images/2.png)` inline in top-level Grok assistant
  Markdown when the image exists in the Grok session or conversation
  workspace.
- Open the same resolved image from its inline preview, explicit Markdown link,
  or autolinked file badge in the existing files pane.
- Use the persisted database conversation id even while the UI is keyed by a
  negative or otherwise virtual runtime id.
- Keep non-Grok behavior unchanged.
- Keep non-matching Grok images unchanged: existing remote image behavior
  remains intact, while other local images remain subject to today's harden
  policy.
- Reject traversal, unsupported formats, unsafe session ids, and symlink
  escapes before reading bytes or handing a path to the files pane.
- Keep autolink scanning shape-only. Filesystem existence is checked only when
  resolving a render, click, or context-menu action.
- Support both desktop/Tauri and server/web transports through the same core.

## Non-Goals

- Local Markdown images for Codex, Cursor, or other agents.
- User, system, reasoning, plan, tool-result, permission-dialog,
  collaboration-card, or nested structured Markdown. Only top-level assistant
  text in the conversation transcript is activated.
- Relative paths outside the exact `images/<file>` shape, including nested
  `images/a/b.png`.
- `file:`, `data:`, `blob:`, POSIX-absolute, home-relative, UNC, or Windows
  drive image sources authored by the model.
- SVG, BMP, TIFF, ICO, or AVIF inline preview. SVG in particular remains
  outside the allow-list because it is an active-content format.
- Rewriting JSONL, ACP blocks, or persisted Markdown.
- Converting Grok tool results into structured `generated-image` blocks.
- Filesystem watches for inline images or indefinite retry loops.
- New user-facing translations or toast copy.
- Changing the existing relative-path autolink scanner, its activation rules,
  or badge visuals.

## Product choices (previously locked)

| Choice | Value |
|--------|-------|
| UX | Inline preview; both the preview and file badge open the files pane |
| Conversation gate | Persisted `agent_type = grok` only |
| Live behavior | Explicit image/link Markdown works live; prose autolinking remains completed-turn only |
| Path shape | One filename directly under `images/` |
| `./images/<file>` | Allowed; exactly one leading `./` is stripped |
| Nested path | Rejected |
| Resolve order | Session file first, then the conversation workspace |
| Collision | Session wins |
| Missing both | Inline fallback; click uses the existing open-failed wording |
| Transcript | Unchanged |
| Autolink scan | No filesystem existence check |
| Inline formats | `png`, `jpg`, `jpeg`, `webp`, `gif`, ASCII case-insensitive |

Review-derived implementation decisions are explicit rather than presented as
previously approved product choices:

- Activate only top-level assistant text. This is the smallest scope that covers
  the cited Markdown without changing tool, plan, reasoning, or nested card
  rendering.
- For workspace fallback, use an existing `origin_cwd` first because it records
  where a reparented conversation actually ran; use the current conversation
  folder only when that original directory is genuinely gone.
- Apply a fixed 20,000,000-byte read cap and a 40,000,000-pixel header cap to
  every candidate, not a caller-controlled limit.
- Retry only live rendering, with four total single-flight attempts. Historical
  replay performs one attempt.

## Shared href parser

The frontend and backend implement the same pure parser, not merely two loose
boolean checks:

```ts
type GrokSessionImageRef = {
  path: string // canonical href form: images/<decoded filename>
  filename: string
  extension: "png" | "jpg" | "jpeg" | "webp" | "gif"
}
```

The backend remains authoritative and re-parses the unmodified `src` or `href`
string received by the frontend component. Both test suites consume one
checked-in JSON fixture file so the two implementations cannot silently drift.

Parsing is ordered and deterministic:

1. Measure the original input as UTF-8 and reject more than 1,024 bytes. Reject
   U+0000-U+001F and U+007F before trimming, trim only U+0020 SPACE from both
   ends, then reject an empty value. Defining the exact code points avoids
   JavaScript/Rust Unicode-whitespace drift.
2. Split the path from the first *literal* `?` or `#`, whichever comes first.
   Ignore that URL suffix. Do this before percent-decoding so encoded `%3F` and
   `%23` remain filename data rather than becoming delimiters.
3. Strictly percent-decode the path as UTF-8. Invalid escapes or invalid UTF-8
   reject. `+` stays a literal plus. Normalize `\` to `/` after decoding so
   encoded separators are checked too.
4. Reject an RFC 3986-style ASCII scheme prefix
   (`^[A-Za-z][A-Za-z0-9+.-]*:`), protocol-relative `//`, a leading `/` or `~`,
   a Windows drive prefix, and a `..` path component. Strip at most one leading
   `./`.
5. Require exactly `^images/([^/]+)$`.
6. Require a filename of at most 255 UTF-8 bytes. Reject `.` and `..`, leading
   or trailing U+0020 SPACE, a trailing `.`, path separators, U+0000-U+001F,
   U+007F, and the portable-invalid characters `< > : " | ? *`. Reject Windows
   device stems (`CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9`)
   case-insensitively; compare the portion before the first dot so an added
   extension does not bypass the rule.
7. Read the extension after the last dot; the dot cannot be the first or last
   filename character. ASCII-fold and require the raster allow-list above.

Representative fixtures:

| Input | Result |
|-------|--------|
| `images/2.png` | `images/2.png` |
| ` ./images/My%20Image.PNG#preview ` | `images/My Image.PNG` |
| `images/2.png?cache=1` | `images/2.png` |
| `images/%E7%9B%AE%E6%A0%87.webp` | pass |
| `images/foo/bar.png` | reject |
| `images/foo%2Fbar.png` | reject |
| `images/foo%5Cbar.png` | reject |
| `../images/2.png` | reject |
| `/images/2.png` | reject |
| `images/a%3Fb.png` | reject (`?` is not a portable filename character) |
| `images/2.svg` | reject |
| `images/%ZZ.png` | reject |
| `file:///tmp/2.png` | reject |

The parser returns the canonical path and filename. Callers use the canonical
path for cache keys and copied workspace-relative paths, but send the unmodified
component value to the backend for independent validation. The fixture schema
contains `accepted` entries with the full expected object and `rejected` entries
with a reason label; the 1,024/1,025-byte input and 255/256-byte filename
boundaries are generated in each language rather than represented by giant JSON
strings.

## Backend architecture

### Command boundary

Add `src-tauri/src/commands/grok_session_image.rs` with a transport-neutral
core plus a Tauri wrapper. Add a dedicated web handler and authenticated POST
route named `/resolve_grok_session_image`.

The core accepts the database handle, an explicit sessions root, and the
request. Production wrappers use `resolve_grok_home_dir().join("sessions")`;
tests inject that root. The Tauri wrapper obtains `State<AppState>`, while the Axum
handler follows the repository convention of `Extension<Arc<AppState>>` plus
`Json`, then delegates without duplicating resolution logic.

Input:

```ts
{
  conversationId: number
  href: string
  includeData?: boolean // default false
}
```

Found response:

```ts
{
  path: string // canonical absolute path, simplified for Windows clients
  origin: "session" | "workspace"
  mimeType: "image/png" | "image/jpeg" | "image/webp" | "image/gif"
  dataBase64?: string
}
```

Response invariants:

- `path`, `origin`, and `mimeType` are always present on success.
- `conversationId` must deserialize as a positive Rust `i32`; zero and negative
  ids fail before the database lookup.
- `includeData = true` guarantees a non-empty base64 field.
- `includeData = false` omits the base64 field but still opens the candidate,
  enforces the same byte/pixel limits, and validates its actual image header.
- `mimeType` comes from the validated header and must agree with the href
  extension (`jpg` and `jpeg` both map to JPEG).
- Convert a canonical path with `simplify_verbatim_path` before serialization
  so Windows does not expose a `\\?\` path shape the frontend misclassifies.
- Reject a non-UTF-8 result path rather than serialize a lossy path that cannot
  be opened again.

Use `#[serde(rename_all = "camelCase")]` on the request/response structs and
`skip_serializing_if = "Option::is_none"` on `data_base64`. The
frontend type lives in `src/lib/types.ts`, and the transport call lives only in
`src/lib/api.ts`; do not add a second direct-invoke implementation to
`src/lib/tauri.ts`.

### Error contract

| Case | Error code and behavior |
|------|-------------------------|
| Non-positive id, invalid href, unsafe `external_id`, ambiguous duplicate session directories, invalid workspace root, or non-UTF-8 result path | `invalid_input` |
| Missing/deleted conversation, non-Grok row, or empty `external_id` | `not_found` for all three, without exposing which gate failed |
| Session and workspace candidates both absent or not yet a decodable image | `not_found` |
| Candidate is a directory/non-regular file, has a mismatched supported-image header, uses a broken/escaping symlink, or escapes its authority root | `invalid_input`; do not try a lower-priority fallback after a rejected candidate |
| Permission failure | existing `permission_denied`; do not disguise it as not found |
| Other filesystem failure | existing `io_error`; do not disguise it as not found |
| Database failure other than an absent row | existing `database_error` |
| File exceeds 20,000,000 bytes | `invalid_input` with the same size detail shape as base64 file preview |
| Declared dimensions exceed 40,000,000 pixels | `invalid_input` |

The internal distinction among **Absent**, **NotReady**, **Rejected**, and
**Error** is load-bearing. `NotReady` covers an empty, too-short, or incomplete
header that may be observed while Grok is still writing. Only `Absent` and
`NotReady` permit the workspace candidate; if neither candidate is usable they
become public `not_found` so live rendering can retry. A rejected session escape,
oversize image, definite extension/header mismatch, permission error, or scan
error must not be hidden by a lower-priority workspace file.

### Resolution algorithm

1. Require a positive id, then query the non-deleted database conversation by
   that durable id. Use a narrow `conversation::Entity::find_by_id(...,
   deleted_at IS NULL)` lookup returning `Option<Model>`; do not use
   `conversation_service::get_by_id`, whose absent case is a `DbError::Migration`
   and whose summary path performs an unrelated child-count query. Map only
   `None` to the privacy-preserving `not_found`; preserve real database errors.
   Require Grok and a non-empty `external_id`. Before any join,
   require `external_id` to be 1-255 UTF-8 bytes and match
   `^[A-Za-z0-9][A-Za-z0-9._-]*$`; also reject `.`, `..`, a trailing dot, and a
   Windows device stem. Do not trim or percent-decode this database identity.
2. Run the shared href parser and extract the canonical path, filename, and
   expected image format.
3. Resolve the Grok session independently of the conversation folder. Add a
   fallible parser-layer locator that accepts an injected sessions root,
   performs the existing strict lookup (`updates.jsonl` exists) across all
   shallow group directories, then performs the loose directory lookup only if
   no strict match exists. Unlike the current `Option` helpers, this locator
   must preserve `read_dir` and entry errors and reject multiple matches at the
   selected strictness instead of choosing filesystem iteration order. Run the
   scan in the shared blocking file-I/O limiter, not on the async executor. A
   missing sessions root is `Absent`; permission and all other scan errors are
   preserved.
4. Validate the session directory below the canonical
   `$GROK_HOME/sessions` authority root and at the expected two-level
   `<group>/<session-id>` depth. Reject symlinked group/session components.
   Evaluate `<session-dir>/images/<filename>` with the confined-file helper and
   raster validator. Return immediately on a safe hit; retain `Absent` or
   `NotReady` only long enough to consider workspace fallback.
5. Only after an `Absent`/`NotReady` session result, choose one conversation
   workspace root. A non-empty absolute `origin_cwd` is authoritative when its
   metadata says it is a directory. Fall back to the conversation's current
   folder only when `origin_cwd` is unset or returns filesystem `NotFound`.
   Relative/non-directory roots reject, and permission/other I/O errors remain
   errors. Load a non-deleted folder row lazily by `folder_id`; it need not be
   the active UI folder. A missing/deleted folder produces the final workspace
   `not_found` but does not invalidate an earlier session hit.
6. Evaluate `<workspace-root>/images/<filename>` with the same confined helper
   and validator. Return `origin = workspace` on a safe hit. Otherwise preserve
   a rejected/error result, or collapse the two absent/not-ready candidates to
   final `not_found`.
7. For `includeData = true`, bounded-read once from the same opened handle used
   for metadata and header validation, then base64-encode those bytes. Do not
   canonicalize and later re-open a client-controlled path.

Do not search other session ids, arbitrary workspace subdirectories, both
workspace roots, or any active folder supplied by the frontend. Scanning all
shallow Grok groups for the exact external id is allowed only to preserve the
existing layout and detect ambiguous duplicates.

### Confined-file helper

The current `read_workspace_file_base64` contains the right primitives, but
copying its private implementation would create two subtly different security
boundaries. Move the semaphore, cap constants, no-follow opener, and a generic
confined reader into `src-tauri/src/commands/confined_file.rs`. Both
`read_workspace_file_base64` and the new resolver call it:

```text
read_confined_regular_file(
  authority_root,
  validated_relative_path,
  required_direct_parent?,
  max_bytes,
  read_bytes,
)
  -> canonical_path + metadata + optional bytes
```

Required behavior:

- Treat the caller-provided authority root as the only trusted starting point.
- Reject absolute/parent components before lookup. For the Grok call, require
  the lexical path to be exactly `images/<filename>` and the final canonical
  parent to equal canonical `<authority_root>/images`, not merely share a
  string prefix.
- Canonicalize root, required parent, and target; compare path components, not
  strings. An in-authority symlink may resolve to another direct file in the
  same canonical `images` directory. A dangling symlink, an `images` alias that
  leaves the authority root, or a final symlink to another directory/outside
  the canonical `images` directory is `Rejected`, not `Absent`.
- Open the canonical target with the existing `O_NOFOLLOW` / Windows
  reparse-point-aware final-component semantics, then use metadata and a
  `take(limit + 1)` bounded read from that handle. The resolver never reopens
  the original model-authored path.
- Reuse the existing file-I/O semaphore and 20 MB default constant rather than
  introduce an unlimited parallel blocking path or a divergent cap.
- Return an internal `Absent` only for a genuinely missing ordinary component.
  Preserve rejected, permission, and other I/O results so callers cannot
  accidentally fall through.
- Keep `read_workspace_file_base64`'s public behavior and max-byte clamping
  unchanged; its existing confinement tests must pass after delegation.

This protects against model-controlled traversal and pre-existing symlink
escapes. As with the rest of Codeg's local file APIs, a privileged hostile local
process continuously replacing every ancestor during the operation is outside
the application threat model; the design must not claim stronger cross-platform
TOCTOU guarantees than the implementation provides.

### Raster validation

Every candidate, including `includeData = false`, is checked from the opened
handle before success:

- sniff PNG, JPEG, WebP, or GIF from the bytes rather than trusting the suffix;
- require the sniffed format to agree with the allowed suffix;
- read dimensions from the header without a full pixel decode and reject more
  than 40,000,000 pixels;
- treat an empty/incomplete/unrecognized header as `NotReady`, but treat a
  recognized disallowed format, definite supported-format mismatch, or
  pixel/byte cap violation as `Rejected`.

The repository already depends on `image` with PNG/JPEG/WebP support. Enable its
GIF feature and use header-only dimension parsing. A browser decode error can
still catch a body truncated after a valid header; live retry handles that case.

## Frontend architecture

### Durable identity and narrow activation scope

`MessageListView` can be keyed by a virtual runtime id. It already derives:

```ts
dbConversationId ?? conversationId
```

Use that durable value only when it is positive and `agentType === "grok"`.
Before a draft is persisted, the Grok image context is null and performs no
resolver calls. When `dbConversationId` is bound, React context updates the
consumers.

Use two contexts instead of making every Markdown renderer Grok-aware:

1. `GrokConversationContext` at `MessageListView` carries the stable durable id
   for a Grok conversation.
2. `GrokSessionImageScope` reads that id and activates it only around a
   top-level assistant text part. Its value also records `phase = live |
   complete` for retry policy.

`HistoricalMessageGroup` knows `agentType` and `isResponseComplete`; pass a
nullable phase into `ContentPartsRenderer`, which already knows role and
top-level nesting. Only its `isTopLevel && role === "assistant"` text branch
mounts `GrokSessionImageScope`. Do not wrap the whole renderer, because tool and
goal cards below it contain their own `MessageResponse` instances. For live
output, thread the existing `agentType` through `LiveTranscriptSegmentView` and
wrap only `LiveTextSegment` / `LiveIncrementalTextSegment`. The compatibility
live-as-history row passes `phase = live` through the historical path. These are
the only activation sites; tool, goal, plan, reasoning, system, user, and nested
Markdown remain outside the active scope even though they descend from
`MessageListView`.

Use primitive or memoized context values so ordinary streaming renders do not
invalidate every historical row. `MessageResponse` may read the active scope;
React context updates still penetrate its `memo` wrapper. Any new explicit prop
included in a custom memo comparator must also be compared.

### Preserve and retag only matching images

Keep two module-stable rehype arrays and two module-stable component maps in
`message.tsx`:

- the current default Codeg array;
- a Grok-session-image array used only inside the active scope;
- the current link component map; and
- a Grok map that adds the private image tag while keeping the app-selected
  `a` component authoritative.

Extend `rehypePluginsAllowingCodeg` with an option that does two things:

1. Around harden, temporarily replace only a matching `<img src>` with the
   safe placeholder already used for preserved links, run harden, and restore
   the original source.
2. After harden, retag only those validated nodes as a private HAST tag such as
   `<codeg-grok-session-image>`. Map that private tag to
   `GrokSessionImage` in Streamdown's `components` map.

Do **not** override `components.img` for the whole Grok conversation. Such an
override would also intercept existing `https:` images and either suppress or
restyle them. Retagging lets Streamdown's exact default image component keep
handling every non-matching remote image, while non-matching local images still
follow the default harden behavior.

The private tag is introduced only after sanitize and harden. The React
component accepts only the sanitized `src` and `alt`; it does not spread
arbitrary model-authored properties onto an interactive element. Because
react-markdown types component keys as JSX intrinsic names, contain the private
tag's type extension/cast in `message.tsx` rather than globally loosening JSX
element props.

`remark-file-uri-links` remains unchanged and still ignores image nodes.

### Inline component

`GrokSessionImage` owns one request generation keyed by active conversation id
and canonical href. The generation observes phase as retry policy: an id/href
change resets content, while a live-to-complete transition cancels future
retries without issuing a duplicate fetch or discarding a successful preview
within the same mounted instance. If the live/history handoff remounts the
component, the new completed instance performs its normal single historical
request; no cross-component cache is required.

1. Re-check the frontend parser defensively. If it fails, render the original
   alt text without I/O (normally unreachable because the retag plugin uses the
   same parser).
2. Resolve with `includeData = true`.
3. Ignore every response from an obsolete generation after href/context change
   or unmount.
4. On success, construct `data:<mime>;base64,<data>` and render a constrained
   image. The image sits in a keyboard-accessible button whose accessible label
   is the original alt text or filename. Clicking it calls the resolved-image
   files-pane action with the already returned bytes and path.
5. While loading, keep a compact alt-text placeholder with `aria-busy`. On final
   failure, keep a compact muted alt line; never reuse harden's misleading
   `[Image blocked: ...]` text and never toast from passive rendering.

Retry only in a live scope. Additional attempts are due at 400 ms, 1,200 ms,
and 2,500 ms measured from the initial attempt. Maintain at most one request in
flight: if a deadline passes during a request, coalesce it into one immediate
next attempt after settlement. Four total attempts is the hard limit regardless
of whether retries were triggered by a timer or browser decode failure. Retry
`not_found` and a browser image decode error; stop on size, permission,
invalid-input, database, or transport errors. Clear timers and ignore late
promise results on phase/href/context change or unmount; do not claim transport
cancellation where the transport has no abort support.

A live `origin = workspace` result is provisional during the same retry window:
show it immediately, but re-resolve at the remaining scheduled attempts. If
the session file appears, replace the preview so the session-wins collision
rule remains true even when the workspace file existed before the session
writer finished. A decoded session-origin image stops provisional-workspace
revalidation; a decode error may still consume the remaining attempt budget.
Transitioning from live to complete clears future retry timers while preserving
the current successful preview. Completed history accepts the first safe result
without retries.

### Badge click routing

`useOpenLinkOrFile` checks the active Grok image scope **before** generic local
path parsing:

```text
if active Grok scope and parseGrokSessionImageRef(url) succeeds:
  resolve includeData=true
  success -> openResolvedImagePreview(result, canonical href)
  failure -> show existing open-failed wording and stop
else:
  run today's parseLocalFileTarget / external URL behavior
```

Never fall through after the Grok gate succeeds. The backend already checks the
conversation workspace; a second generic open is unnecessary and would turn a
resolver-rejected workspace symlink into an unconfined read.

Use a per-canonical-href in-flight guard for rapid repeated clicks; clicking a
different link is not blocked. A `not_found` uses
`Folder.workspaceContext.unableOpenFile` with the canonical filename. Other
errors use the existing local-file-open failure toast plus `toErrorMessage`.
No new translations are needed.

Non-Grok links and Grok links that do not match the exact image-ref parser keep
today's routing. In particular, a nested `images/a/b.png` or `docs/a.png` link
still behaves as an ordinary workspace-relative link even though it cannot be
rendered inline by this feature.

### Resolved image tab

Do not call ordinary `openFilePreview(path)` after fetching the resolved bytes.
That would perform a second unconfined `read_file_base64` and reopen a race
between validation and display. Add a workspace action:

```ts
openResolvedImagePreview({
  path,
  mimeType,
  dataBase64,
  source: { type: "grok-session-image", conversationId, href },
})
```

The action defensively requires an absolute resolver path, creates or refreshes
the normal image-shaped `FileWorkspaceTab` (same normalized absolute-path tab
id, title, files pane, maximize behavior, and image renderer) directly from the
data URL, marks it read-only, and records the in-memory source metadata. It must
participate in the existing per-tab load generation: invoking it invalidates an
older generic read for that tab, and a later ordinary open invalidates an older
snapshot update, so late work cannot overwrite the newer action.

Represent that metadata as a dedicated optional `snapshotSource` discriminated
union with the `grok-session-image` member shown above. Do not overload the
existing `transient` field, whose invariant is a pathless document-translation
result. Automatic paths test `tab.snapshotSource` explicitly.

Resolved-image tabs are snapshot-backed:

- exclude them from every automatic stale/reload path (watch candidates,
  `markTabsStale` variants, background reload/reject) and from hidden-tab
  content eviction, all of which could otherwise reach the generic path reader;
- a repeat chat click resolves again and refreshes the tab;
- closing the tab releases the snapshot;
- an explicit ordinary `openFilePreview` for the same path treats the source
  marker as a forced reload; only a successful ordinary read clears the marker
  and converts the tab back to normal path-backed behavior. A failed conversion
  keeps the last validated snapshot.

The per-file backend cap bounds each snapshot. Pinning applies only after an
explicit user open, not to passive inline previews.

### File badge context menu

`FileReferenceActionsMenu` currently computes paths synchronously from the
active folder. Under an active Grok image scope and matching href it instead
resolves with `includeData = false` on menu mount:

- disable reveal/copy actions while loading or after resolution failure;
- ignore a late result after the menu unmounts or target changes;
- reveal and copy-absolute use the resolver path;
- copy-relative is the canonical `images/<filename>` only for
  `origin = workspace`; it is relative to the resolver-selected conversation
  workspace (`origin_cwd` or current folder), not necessarily the active UI
  folder. A session-origin image has no workspace-relative form;
- do not toast merely because a context menu was opened;
- do not fall back to a guessed active-folder path on resolver rejection.

React context remains available through the Radix portal, so the menu uses the
same active scope as its badge.

## Data flow

### Inline render

```text
MessageListView
  -> GrokConversationContext(durable DB id)
  -> top-level GrokSessionImageScope(phase)
  -> MessageResponse with Grok rehype array
  -> matching img preserved through harden and retagged
  -> GrokSessionImage
  -> resolve_grok_session_image(includeData=true)
  -> data URL image
```

### Inline or badge open

```text
GrokSessionImage cached result OR MarkdownLink badge
  -> resolve_grok_session_image(includeData=true) when needed
  -> openResolvedImagePreview(path + validated bytes + source metadata)
  -> existing files pane image tab, with no second filesystem read
```

### Context menu

```text
FileReferenceActionsMenu
  -> resolve_grok_session_image(includeData=false)
  -> canonical absolute path + origin
  -> reveal/copy actions
```

Historical replay and both live rendering paths use the same backend command.
Parsers do not scan or mutate Markdown.

## Error handling and races

| Event | Inline | Click | Context menu |
|-------|--------|-------|--------------|
| Href fails frontend parser | Default harden/alt behavior | Existing link/file logic | Existing path logic |
| Session file exists | Show session image | Open resolved session snapshot | Session absolute actions |
| Only workspace file exists | Show workspace image | Open resolved workspace snapshot | Workspace absolute + relative actions |
| Both exist | Session wins | Session wins | Session wins |
| Neither exists | Live retry, then muted alt | Existing unable-open wording; no generic fallback | Actions disabled, no toast |
| Session file exists but its header is incomplete | Treat as not ready; try workspace and keep live retry budget | Current usable workspace result or unable-open wording | Current usable workspace result or disabled actions |
| Workspace result precedes session write | Show workspace, bounded live revalidation can replace it | Current click result; user can click again | Current menu result |
| Conversation folder is missing | Session can still render/open | Same | Same |
| Resolver `invalid_input` / escaping-symlink reject | No retry; muted alt | Error toast; stop | Actions disabled |
| Oversize / pixel cap / permission / DB / transport error | No retry or toast | Existing error toast with detail | Actions disabled |
| Component unmount, href change, or live completion | Clear retry timers; ignore stale results | N/A | Ignore stale result |

The backend response is a point-in-time result. Inline live revalidation is the
only automatic session-file retry. No infinite polling or filesystem watcher is
introduced.

## Security

- Both runtimes parse the same narrow href grammar; the backend never trusts the
  frontend result.
- `conversationId` is a durable DB id. The backend verifies the row is Grok and
  takes `external_id`, `origin_cwd`, and folder id from the database rather than
  accepting roots from the client.
- `external_id` is validated before any join.
- Session paths are confined below the canonical Grok sessions root; workspace
  paths are confined below one database-derived conversation workspace.
- Candidate resolution distinguishes absence from rejection. Unsafe candidates
  never trigger a lower-priority or generic fallback.
- Only a regular, direct child of an allowed `images` directory can be read.
- Actual raster type and header dimensions are validated from the opened file;
  a suffix alone never authorizes browser decoding.
- Inline and resolved-tab bytes come from the confined command. Markdown never
  receives `file:` or a raw filesystem path as an image source.
- The resolved-tab path is display/identity metadata; initial content comes from
  the same validated file handle, not a later generic path read.
- Remote images retain the existing Streamdown policy. The feature does not
  widen sanitize/harden for arbitrary `<img>` sources.
- SVG remains excluded. Raster bytes are rendered only in an image context.
- Fixed input, filename, file-size, pixel, and retry bounds, plus an explicit
  user action before retaining a files-pane snapshot, limit work from a single
  model-authored reference.

## Performance and lifecycle

- All directory scans, canonicalization, file opens, and reads run through the
  existing bounded blocking file-I/O path.
- Rehype arrays and component maps are module-stable. Context values are
  primitive or memoized.
- Passive history performs one resolver call per mounted image. The message
  list's existing virtualization limits mounted historical rows.
- Live retries are capped at three additional attempts, never overlap, and only
  continue for `not_found`, decode failure, or a provisional workspace result.
- No global success cache is required in v1; it would need invalidation when a
  session file replaces a workspace fallback. Each component ignores stale
  responses locally.
- Base64 has the same expansion cost as the existing files-pane image preview.
  The 20 MB input cap and explicit-user-open requirement bound each retained
  resolved tab.

## Testing

### Shared parser fixtures (TS and Rust)

Add one JSON fixture file containing accepted canonical results and rejected
inputs. Both suites must cover:

- all accepted extensions and ASCII case variants;
- plain and one-leading-`./` forms;
- literal query/hash suffixes;
- valid UTF-8 percent encoding and spaces;
- invalid percent escapes/UTF-8;
- encoded slash and backslash traversal;
- schemes, absolute/home/drive/UNC forms;
- nested paths, `.`/`..`, controls/NUL, portable-invalid characters, trailing
  space/dot, reserved device names, and exact/pass-one boundaries for both
  length limits;
- literal `+`, encoded `?`/`#`, raw/encoded backslash, two leading `./`
  segments, and the exact ASCII trimming/control rules.

### Backend resolver

Use an injected sessions root/internal locator in unit tests instead of mutating
process-global `GROK_HOME`; tests can run in parallel safely.

- Grok conversation + safe session file -> session origin, canonical/simplified
  path.
- Strict and loose session lookup both work.
- The fallible locator reports an unreadable sessions root/group and rejects
  duplicate strict or duplicate loose matches deterministically.
- Missing folder does not prevent a session hit.
- `origin_cwd` wins when it exists; missing origin falls back to the current
  folder. A relative/non-directory origin rejects, and permission/I/O errors do
  not fall back.
- Workspace-only file -> workspace origin; both files -> session origin.
- Missing both -> `not_found`.
- Missing/non-Grok/no-external-id rows share the same `not_found` code.
- Non-positive ids and every invalid external-id boundary reject before a path
  join.
- Symlinked session group/session identity, broken links, and links escaping the
  authority/direct `images` parent reject and do not fall back. A link resolving
  to a regular direct file inside the same canonical `images` directory passes.
  Unix symlink tests are mandatory; add Windows reparse coverage where CI
  permissions allow it.
- Directory/non-regular candidates reject. Empty/incomplete candidates become
  not-ready, allow a workspace candidate, and end as `not_found` if neither is
  usable.
- `includeData = true` returns correct MIME/base64 from the validated handle;
  `false` omits bytes but still enforces header, byte, and pixel validation.
- Uppercase extension maps to the correct MIME; a definite suffix/header
  mismatch rejects.
- Exact 20 MB boundary passes; one byte over fails.
- Exact 40,000,000-pixel boundary passes; one pixel over fails without a full
  decode.
- Non-UTF-8 output path rejects on Unix.
- Existing `read_workspace_file_base64` confinement, cap-clamping, and symlink
  tests remain green after it delegates to the shared helper.

### Rehype and rendering

- Default Codeg plugins still turn `![x](images/2.png)` into the current blocked
  fallback.
- The Grok array preserves and retags only a valid session image ref.
- `docs/foo.png`, nested `images/a/b.png`, `file:`, SVG, and malformed encoded
  sources remain blocked.
- An `https:` image under Grok scope still uses Streamdown's default image
  component and behavior.
- Local `<a href>` preservation remains unchanged.
- Private-tag rendering does not forward arbitrary raw HTML attributes.
- The private-tag component typing remains narrow; no global JSX index
  signature is introduced.

### Scope and identity wiring

- Grok top-level historical assistant text resolves; user, tool, reasoning,
  plan, nested goal, and non-Grok Markdown do not.
- Both incremental live text and the compatibility live path activate the live
  scope.
- A negative runtime id bound to a positive `dbConversationId` sends the
  positive id. An unpersisted draft sends nothing; binding later activates the
  image.
- `MessageResponse` memo/context changes reselect the module-stable rehype array
  without requiring unrelated history rows to re-render.

### Inline lifecycle

- Success renders the returned data URL and accessible open button.
- Complete history performs one attempt.
- Live `not_found` retries at the specified elapsed times, coalesces deadlines
  while one request is in flight, and never exceeds four attempts.
- A provisional workspace result is displayed and then replaced by a later
  session result.
- Invalid/oversize/permission errors do not retry or toast.
- Unmount, href change, and conversation change cancel timers and ignore late
  results.
- Browser decode failure, including after a session-origin response, consumes
  the remaining live retry budget and ends in the muted-alt fallback.
- A live-to-complete transition in the same mounted instance clears pending
  retries without a second request or loss of a successful preview; a handoff
  remount performs exactly one completed-history request.

### Click, menu, and files pane

- A gated badge resolves with bytes and opens the resolver path through
  `openResolvedImagePreview`.
- Resolver `not_found`, invalid-input, and symlink errors never call generic
  `openFilePreview`.
- Ungated/non-Grok links retain the existing opener behavior.
- Rapid repeated clicks share the in-flight guard.
- Inline-preview click reuses its cached resolution without another backend
  read.
- Menu actions are disabled while resolving/on failure and use the resolved path
  on success; session origin has no copy-relative action.
- Resolved image tabs use the normal absolute-path tab identity and renderer,
  participate in last-open-wins generations, skip every automatic stale/read
  and eviction path, refresh on a repeated resolved open, and release content
  on close.
- A successful ordinary open converts a snapshot tab back to path-backed state;
  a failed conversion preserves the validated snapshot.

### Transport and regression checks

- Web handler request camelCase and response shape match the Tauri command.
- An API integration test exercises the route through the authenticated router,
  including a rejected unauthenticated request and an authenticated structured
  `not_found` that exits before filesystem lookup. Core serialization tests
  assert the successful camelCase response shape without mutating global
  `GROK_HOME`.
- Tauri production handler registration includes the command.
- Existing `message-file-uri`, local-path-autolink, file-action, and workspace
  image-preview tests remain green.

The implementation inner loop should run the narrow Vitest files and the new
Rust module tests. Before handoff, run frontend lint/type-relevant tests plus
desktop, server, and `codeg-mcp` `cargo check`; reserve the repository-wide
suites for the normal branch-completion regression stage.

## Files to touch

| File | Role |
|------|------|
| `fixtures/grok-session-image-href-cases.json` | Cross-language href fixtures |
| `src-tauri/Cargo.toml` | Enable the existing `image` dependency's GIF header support |
| `src-tauri/Cargo.lock` | Lock any dependency added by the GIF feature |
| `src-tauri/src/commands/confined_file.rs` | Shared limiter, caps, canonical confinement, no-follow open, bounded read |
| `src-tauri/src/commands/grok_session_image.rs` | Parser, DB gate, resolution core, command, unit tests |
| `src-tauri/src/commands/folders.rs` | Delegate existing confined base64 reads to the shared helper without API changes |
| `src-tauri/src/commands/mod.rs` | Command module registration |
| `src-tauri/src/parsers/grok.rs` | Fallible, injectable strict/loose session-directory locator |
| `src-tauri/src/lib.rs` | Module and Tauri production handler registration |
| `src-tauri/src/web/handlers/grok_session_image.rs` | Axum request adapter |
| `src-tauri/src/web/handlers/mod.rs` | Handler module registration |
| `src-tauri/src/web/router.rs` | Authenticated POST route |
| `src/lib/types.ts` | Resolution response type |
| `src/lib/api.ts` | Transport-neutral resolver client |
| `src/lib/markdown/grok-session-image.ts` | Frontend parser |
| `src/lib/markdown/grok-session-image.test.ts` | Shared-fixture parser tests |
| `src/components/ai-elements/grok-session-image-context.tsx` | Root identity + narrow active scope |
| `src/components/ai-elements/grok-session-image.tsx` | Inline resolver, retry, fallback, open action |
| `src/components/ai-elements/rehype-allow-codeg.ts` | Matching image preservation + retagging |
| `src/components/ai-elements/rehype-allow-codeg.test.ts` | Harden/retag regressions |
| `src/components/ai-elements/message.tsx` | Scope-selected rehype/components arrays |
| `src/components/ai-elements/link-safety.tsx` | Authoritative gated badge click routing |
| `src/components/ai-elements/link-safety.test.tsx` | No-fallback and click tests |
| `src/components/message/content-parts-renderer.tsx` | Top-level historical activation scope |
| `src/components/message/live-transcript-row.tsx` | Incremental live activation scope |
| `src/components/message/message-list-view.tsx` | Durable Grok conversation provider |
| `src/components/message/file-reference-actions.tsx` | Async resolved menu paths |
| `src/contexts/workspace-context.tsx` | Snapshot-backed resolved image tab action/source marker |
| `src/hooks/use-open-file-tabs-watch.ts` | Exclude resolved snapshot tabs from generic disk reload |
| `src/components/ai-elements/grok-session-image-pipeline.test.tsx` | End-to-end scoped Streamdown image pipeline |
| existing message/workspace test files beside changed modules | Scope, menu, tab generation/lifecycle, runtime-id mapping |
| `src-tauri/tests/api_integration.rs` | Authenticated web-route contract |

Keep `remark-file-uri-links.ts`, Grok transcript parsing behavior, persistence
formats, and i18n message files unchanged. The `grok.rs` change is path-location
infrastructure only; it must not change conversation listing/detail semantics.

## Risks and mitigations

| Risk | Mitigation |
|------|------------|
| Runtime id is not a DB id | Root provider uses positive `dbConversationId ?? conversationId` and tests virtual-id binding |
| Grok scope changes tool/plan Markdown | A nested active scope exists only around top-level assistant text |
| Custom image component swallows remote images | Retag only matching local refs; never override all `img` nodes |
| Resolver rejection is bypassed | A gated click never falls through to generic open |
| Validation/read race | Read bytes from the confined handle and seed a snapshot-backed tab |
| Session file appears after workspace collision | Live workspace result is provisional and revalidated within the bounded window |
| Stale async result paints the wrong message | Generation token plus timer/promise cleanup |
| Global `GROK_HOME` tests race | Inject the sessions root in tests |
| Parser locator hides I/O or picks a duplicate arbitrarily | Fallible scan with explicit ambiguity rejection |
| Windows canonical path breaks frontend parsing | `simplify_verbatim_path` before response serialization |
| Folder was removed/reparented | Resolve session before folder; use existing `origin_cwd` when available |
| Renamed binary or decompression bomb reaches the webview | Header/extension agreement plus 20 MB and 40 MP caps |
| Large base64 retention | Fixed 20 MB/file cap; passive preview remains virtualized; pinned snapshot only follows user open |
| Session scan blocks async runtime | Shared bounded `spawn_blocking` file-I/O path |

## Later, not this spec

Map Grok `image_gen` and `image_edit` tool results to structured
`generated-image` cards so generated pictures can render even when the model
does not cite them in Markdown. The resolver remains necessary for screenshots,
ordinary file citations, historical transcripts, and file-badge actions.
